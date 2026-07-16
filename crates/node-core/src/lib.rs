//! Bounded orchestration between the Reticulum protocol owner and interface
//! actors.
//!
//! This slice owns a concrete RNS node, fixed external-buffer dispatch
//! metadata, and a fixed DATA attempt ledger. A receipt is registered only
//! after both final metadata slots have been reserved. Proofs and timeouts
//! become acknowledged terminal tombstones before RNS releases its receipt
//! state. The crate deliberately has no device-API, executor, radio, board, or
//! persistence dependency.
//!
//! Construction and unrelated RNS paths may still allocate inside the current
//! Rete integration. The external-buffer DATA transaction implemented here
//! uses only caller-owned arrays; it is not a claim that the entire node is
//! allocation free.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use rand_core::{CryptoRng, RngCore};
use reticulum_rns_rete::{
    DestHash as RnsDestinationHash, EmbeddedNode as RnsNode, EmbeddedNodeConfig as RnsNodeConfig,
    Identity as RnsIdentity, IngressReport, InterfaceId as RnsInterfaceId, NodeActions,
    NodeRole as RnsNodeRole, PrepareDataError as RnsPrepareDataError,
    PreparedData as RnsPreparedData, RNS_MTU, ReceiptCandidate, ReceiptKind,
    ReceiptReservationUnavailable, ReceiptTerminal, ReceiptTerminalReservation,
    ReceiptTerminalSink, TxTarget as RnsTxTarget,
};

/// Complete packet storage reserved for one base-MTU Reticulum transmission.
pub const PACKET_CAPACITY: usize = RNS_MTU;

/// Largest destination-DATA plaintext accepted by the current RNS adapter.
pub const MAX_DATA_PAYLOAD: usize = reticulum_rns_rete::MAX_DATA_PAYLOAD;

/// Whether Rete's current heapless backend can instantiate these table
/// capacities.
///
/// Path and Link maps use `FnvIndexMap`, whose capacity must be a power of two
/// greater than one. Announce and deduplication storage use non-empty deques.
/// External packet-buffer dispatch metadata is independent and may have zero
/// slots.
pub const fn capacity_profile_is_supported(
    paths: usize,
    announces: usize,
    deduplication: usize,
    links: usize,
) -> bool {
    paths > 1
        && paths.is_power_of_two()
        && announces > 0
        && deduplication > 0
        && links > 1
        && links.is_power_of_two()
}

/// Opaque Reticulum identity owned by the node core.
pub struct NodeIdentity(RnsIdentity);

impl NodeIdentity {
    /// Import Reticulum's combined 64-byte X25519-plus-Ed25519 private key.
    pub fn from_private_key(private_key: &[u8]) -> Result<Self, IdentityError> {
        reticulum_rns_rete::identity_from_private_key(private_key)
            .map(Self)
            .map_err(|_| IdentityError::InvalidPrivateKey)
    }

    /// Public X25519-plus-Ed25519 key bytes.
    pub fn public_key(&self) -> [u8; 64] {
        self.0.public_key()
    }
}

/// Failure to import a node identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityError {
    /// The private key length or key material is invalid.
    InvalidPrivateKey,
}

/// Product-owned complete 128-bit Reticulum destination hash.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DestinationHash([u8; 16]);

impl DestinationHash {
    /// Construct a destination hash from its complete wire bytes.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Borrow the complete destination hash bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    fn into_rns(self) -> RnsDestinationHash {
        RnsDestinationHash::from(self.0)
    }
}

impl AsRef<[u8]> for DestinationHash {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Caller-provided identity for one in-memory node-owner incarnation.
///
/// A value must not be reused when reconstructing the same Reticulum identity
/// while messages carrying old TX leases can still exist. Firmware should
/// derive this from a boot/session nonce or a persisted monotonic boot epoch.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct NodeInstanceId([u8; 16]);

impl NodeInstanceId {
    /// Construct an instance identifier from caller-owned epoch/nonce bytes.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Borrow the complete instance identifier.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Monotonic time in whole seconds for RNS path and receipt state.
///
/// This is deliberately distinct from executor ticks, milliseconds and Unix
/// wall time. Callers must convert at the platform boundary.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MonotonicSeconds(u64);

impl MonotonicSeconds {
    /// Construct a monotonic-seconds sample.
    pub const fn new(seconds: u64) -> Self {
        Self(seconds)
    }

    /// Raw whole seconds supplied to RNS.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Executor-independent monotonic time in whole milliseconds.
///
/// This clock scopes packet-owner deadlines. It is deliberately distinct
/// from RNS's whole-second protocol clock and from Unix wall time.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MonotonicMillis(u64);

impl MonotonicMillis {
    /// Construct a monotonic-millisecond sample.
    pub const fn new(milliseconds: u64) -> Self {
        Self(milliseconds)
    }

    /// Raw whole milliseconds supplied by the platform monotonic clock.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Deadline by which an externally owned packet buffer should return to the
/// node owner or enter retained recovery.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TxLeaseDeadline(MonotonicMillis);

impl TxLeaseDeadline {
    /// Construct a packet-owner deadline from a monotonic instant.
    pub const fn new(deadline: MonotonicMillis) -> Self {
        Self(deadline)
    }

    /// Monotonic instant represented by this deadline.
    pub const fn instant(self) -> MonotonicMillis {
        self.0
    }
}

/// Product-owned packet-interface identifier.
///
/// RNS interface identifiers are one byte. The initial compact
/// [`InterfaceSet`] can represent identifiers zero through 63; larger values
/// remain representable here so a native target is never truncated while a
/// later route-resolution step can reject an unsupported profile explicitly.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PacketInterfaceId(u8);

impl PacketInterfaceId {
    /// Construct a packet-interface identifier from its complete wire value.
    pub const fn new(value: u8) -> Self {
        Self(value)
    }

    /// Raw one-byte interface identifier.
    pub const fn get(self) -> u8 {
        self.0
    }

    fn into_rns(self) -> RnsInterfaceId {
        RnsInterfaceId(self.0)
    }
}

/// Fixed set of the first 64 packet interfaces.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InterfaceSet(u64);

impl InterfaceSet {
    /// Empty interface set.
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Construct a set from its complete bit representation.
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// Complete bit representation of this set.
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Whether this set contains no interfaces.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether this compact set contains an interface.
    ///
    /// Identifiers above 63 are outside this profile and return `false`.
    pub const fn contains(self, interface: PacketInterfaceId) -> bool {
        let index = interface.get();
        index < 64 && self.0 & (1u64 << index) != 0
    }

    /// Add an interface when it fits the compact set.
    ///
    /// `None` reports an identifier above 63 without altering the set.
    pub const fn with(self, interface: PacketInterfaceId) -> Option<Self> {
        let index = interface.get();
        if index < 64 {
            Some(Self(self.0 | (1u64 << index)))
        } else {
            None
        }
    }
}

/// Interface authorization retained with one prepared RNS packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxTarget {
    /// Send on every eligible packet interface.
    All,
    /// Send only on one packet interface.
    Only(PacketInterfaceId),
    /// Send on every eligible interface except one packet interface.
    AllExcept(PacketInterfaceId),
}

impl TxTarget {
    fn from_rns(target: RnsTxTarget) -> Self {
        match target {
            RnsTxTarget::All => Self::All,
            RnsTxTarget::Only(interface) => Self::Only(PacketInterfaceId::new(interface.0)),
            RnsTxTarget::AllExcept(interface) => {
                Self::AllExcept(PacketInterfaceId::new(interface.0))
            }
        }
    }
}

/// Whether this node terminates only local traffic or also forwards RNS
/// traffic for other nodes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeRole {
    /// Local endpoint only.
    Endpoint,
    /// Reticulum transport node.
    Transport,
}

/// Construction policy for the bounded node owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeConfig {
    /// Endpoint or transport behavior.
    pub role: NodeRole,
    /// Maximum destinations registered in addition to the primary one.
    pub max_additional_destinations: usize,
}

impl NodeConfig {
    /// Conservative endpoint profile.
    pub const fn endpoint() -> Self {
        Self {
            role: NodeRole::Endpoint,
            max_additional_destinations: 4,
        }
    }

    /// Transport profile with the same initial local-destination quota.
    pub const fn transport() -> Self {
        Self {
            role: NodeRole::Transport,
            max_additional_destinations: 4,
        }
    }
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self::endpoint()
    }
}

/// Failure to construct the concrete RNS owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeConstructionError {
    /// RNS rejected the application name, aspects, identity, or destination.
    InvalidRnsConfiguration,
}

/// Failure to cache one peer identity for deterministic direct sending.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeerRegistrationError {
    /// RNS rejected the peer name, aspects, or identity.
    InvalidPeer,
}

/// Full RNS hash used to correlate one prepared DATA packet with its delivery
/// proof or timeout.
///
/// This is the Reticulum hashable-part digest covered by a proof. It is not a
/// SHA-256 digest of every byte in the encoded interface packet.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AttemptToken([u8; 32]);

impl AttemptToken {
    /// Borrow the complete proof-correlation hash.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Opaque, generation-checked identity of one submitted DATA attempt.
///
/// Unlike [`AttemptToken`], this handle remains unambiguous if packet hashes
/// are ever repeated after an earlier attempt has been acknowledged. It is
/// scoped to both the Reticulum identity and this in-memory node incarnation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AttemptHandle {
    owner_identity: [u8; 16],
    instance: NodeInstanceId,
    slot: usize,
    generation: u64,
}

/// Application-visible outcome for one destination-DATA attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttemptOutcome {
    /// A valid Reticulum delivery proof was accepted.
    Delivered,
    /// The Reticulum delivery receipt expired without a proof.
    DeliveryTimeout,
}

/// One terminal attempt retained until the application acknowledges it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalAttempt {
    handle: AttemptHandle,
    token: AttemptToken,
    outcome: AttemptOutcome,
}

impl TerminalAttempt {
    /// Opaque handle required to acknowledge this terminal record.
    pub const fn handle(self) -> AttemptHandle {
        self.handle
    }

    /// Complete RNS proof-correlation hash for this attempt.
    pub const fn token(self) -> AttemptToken {
        self.token
    }

    /// Delivery outcome retained by the tombstone.
    pub const fn outcome(self) -> AttemptOutcome {
        self.outcome
    }
}

/// Stable identifier assigned to one externally owned packet buffer.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PacketSlotId(u16);

impl PacketSlotId {
    /// Numeric slot index assigned by the node owner.
    pub const fn get(self) -> u16 {
        self.0
    }

    fn index(self) -> usize {
        usize::from(self.0)
    }
}

/// Scalar metadata for one packet committed to an external buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedPacket {
    packet_len: u16,
    attempt: AttemptToken,
    handle: AttemptHandle,
    slot: PacketSlotId,
    target: TxTarget,
    deadline: TxLeaseDeadline,
}

impl PreparedPacket {
    /// Encoded packet length in the external packet buffer.
    pub const fn packet_len(self) -> u16 {
        self.packet_len
    }

    /// Proof-correlation token for this attempt.
    pub const fn attempt(self) -> AttemptToken {
        self.attempt
    }

    /// Generation-scoped handle for later terminal acknowledgement.
    pub const fn handle(self) -> AttemptHandle {
        self.handle
    }

    /// Stable external packet-buffer slot containing these bytes.
    pub const fn slot_id(self) -> PacketSlotId {
        self.slot
    }

    /// RNS interface authorization retained from packet preparation.
    pub const fn target(self) -> TxTarget {
        self.target
    }

    /// Monotonic return/recovery deadline bound to this dispatch.
    pub const fn deadline(self) -> TxLeaseDeadline {
        self.deadline
    }

    fn from_rns(
        prepared: RnsPreparedData,
        handle: AttemptHandle,
        slot: PacketSlotId,
        deadline: TxLeaseDeadline,
    ) -> Self {
        Self {
            packet_len: prepared.packet_len(),
            attempt: AttemptToken(*prepared.receipt().as_bytes()),
            handle,
            slot,
            target: TxTarget::from_rns(prepared.target()),
            deadline,
        }
    }
}

/// Failure to reserve and prepare one DATA packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmitError {
    /// The supplied packet buffer was never registered with this node owner.
    UnregisteredPacketBuffer,
    /// The supplied packet buffer belongs to another node incarnation.
    ForeignPacketBuffer,
    /// The supplied packet buffer is already bound to an active dispatch.
    PacketBufferBusy,
    /// The registered packet slot and node dispatch metadata disagree.
    PacketBufferInvariant,
    /// Every attempt slot is active or retained as an unacknowledged
    /// terminal tombstone.
    AttemptLedgerFull {
        /// Configured DATA-attempt limit.
        limit: usize,
    },
    /// The non-repeating attempt-generation space has been exhausted.
    AttemptIdentifierExhausted,
    /// The non-repeating dispatch-generation space has been exhausted.
    DispatchIdentifierExhausted,
    /// Plaintext cannot fit in one encrypted base-MTU DATA packet.
    PayloadTooLarge {
        /// Supplied plaintext length.
        actual: usize,
        /// Maximum accepted plaintext length.
        maximum: usize,
    },
    /// No cached identity exists for the destination.
    UnknownDestination,
    /// The bounded RNS receipt table has no free entry.
    ReceiptTableFull {
        /// Configured receipt limit.
        limit: usize,
    },
    /// A live receipt already uses the generated hash.
    ReceiptHashAlreadyTracked,
    /// Destination encryption failed.
    Cryptography,
    /// RNS could not construct the packet.
    PacketBuild,
    /// The RNS adapter reported an impossible failure class.
    Invariant,
}

impl From<RnsPrepareDataError> for SubmitError {
    fn from(error: RnsPrepareDataError) -> Self {
        match error {
            RnsPrepareDataError::PayloadTooLarge { actual, maximum } => {
                Self::PayloadTooLarge { actual, maximum }
            }
            RnsPrepareDataError::UnknownDestination => Self::UnknownDestination,
            RnsPrepareDataError::ReceiptTableFull { limit } => Self::ReceiptTableFull { limit },
            RnsPrepareDataError::ReceiptHashAlreadyTracked => Self::ReceiptHashAlreadyTracked,
            RnsPrepareDataError::Crypto => Self::Cryptography,
            RnsPrepareDataError::PacketBuild => Self::PacketBuild,
            RnsPrepareDataError::Invariant => Self::Invariant,
        }
    }
}

/// Failure to register one external packet buffer with a node owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BufferRegistrationError {
    /// Every configured dispatch slot already has a registered buffer.
    PoolFull {
        /// Configured external packet-buffer limit.
        limit: usize,
    },
    /// This buffer is already registered or bound to a dispatch.
    AlreadyRegistered,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BufferBinding {
    Unregistered,
    Available {
        owner_identity: [u8; 16],
        instance: NodeInstanceId,
        slot: PacketSlotId,
    },
    Bound {
        dispatch_generation: u64,
        prepared: PreparedPacket,
    },
}

/// One complete externally owned Reticulum packet buffer.
///
/// This value is intentionally neither `Copy` nor `Clone`. Firmware allocates
/// it in static storage, registers it once, and then moves its unique mutable
/// reference through the owning handoff. Packet bytes have no public accessor;
/// a later interface-authorization slice will expose them only with a matching
/// scalar permit.
pub struct TxPacketBuffer {
    bytes: [u8; PACKET_CAPACITY],
    binding: BufferBinding,
}

impl TxPacketBuffer {
    /// Construct one zero-filled, unregistered packet buffer.
    pub const fn new() -> Self {
        Self {
            bytes: [0; PACKET_CAPACITY],
            binding: BufferBinding::Unregistered,
        }
    }

    /// Stable slot assigned during registration, if any.
    pub const fn slot_id(&self) -> Option<PacketSlotId> {
        match self.binding {
            BufferBinding::Unregistered => None,
            BufferBinding::Available { slot, .. } => Some(slot),
            BufferBinding::Bound { prepared, .. } => Some(prepared.slot_id()),
        }
    }
}

impl Default for TxPacketBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Opaque identity of one external-buffer dispatch generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DispatchHandle {
    instance: NodeInstanceId,
    slot: PacketSlotId,
    generation: u64,
}

/// Unique ownership of one prepared packet awaiting the async handoff.
///
/// The job carries the only mutable reference to its packet buffer. It exposes
/// scalar metadata but deliberately does not expose packet bytes before the
/// separate interface-authorization state machine exists.
#[must_use = "dropping a TX job quarantines its packet buffer, dispatch, attempt, and receipt"]
pub struct TxJob<'a> {
    buffer: &'a mut TxPacketBuffer,
    dispatch: DispatchHandle,
}

impl TxJob<'_> {
    fn bound_prepared(&self) -> PreparedPacket {
        match self.buffer.binding {
            BufferBinding::Bound {
                dispatch_generation,
                prepared,
            } if dispatch_generation == self.dispatch.generation
                && prepared.slot_id() == self.dispatch.slot =>
            {
                prepared
            }
            BufferBinding::Unregistered
            | BufferBinding::Available { .. }
            | BufferBinding::Bound { .. } => {
                unreachable!("private TX job and packet-buffer binding diverged")
            }
        }
    }

    /// Stable slot of the uniquely owned packet buffer.
    pub fn slot_id(&self) -> PacketSlotId {
        self.bound_prepared().slot_id()
    }

    /// Encoded packet length retained as scalar metadata.
    pub fn packet_len(&self) -> u16 {
        self.bound_prepared().packet_len()
    }

    /// Complete proof-correlation token for this dispatch.
    pub fn attempt(&self) -> AttemptToken {
        self.bound_prepared().attempt()
    }

    /// Attempt handle used for terminal acknowledgement.
    pub fn attempt_handle(&self) -> AttemptHandle {
        self.bound_prepared().handle()
    }

    /// RNS interface authorization retained at preparation time.
    pub fn target(&self) -> TxTarget {
        self.bound_prepared().target()
    }

    /// Deadline bound to this external-buffer owner generation.
    pub fn deadline(&self) -> TxLeaseDeadline {
        self.bound_prepared().deadline()
    }

    /// Copy all scalar metadata without separating it from buffer ownership.
    pub fn prepared(&self) -> PreparedPacket {
        self.bound_prepared()
    }
}

/// Preparation rejection that returns the caller's still-available buffer.
pub struct PrepareFailure<'a> {
    reason: SubmitError,
    buffer: &'a mut TxPacketBuffer,
}

impl<'a> PrepareFailure<'a> {
    /// Typed reason the preparation transaction was rejected.
    pub const fn reason(&self) -> SubmitError {
        self.reason
    }

    /// Recover the exact unique packet buffer supplied by the caller.
    pub fn into_buffer(self) -> &'a mut TxPacketBuffer {
        self.buffer
    }
}

/// Failure while rolling back a queued, definitely-unsent dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RollbackErrorKind {
    /// The job belongs to another node owner, incarnation, slot, or generation.
    StaleOrUnknown,
    /// Buffer, dispatch, and attempt metadata disagree.
    Invariant,
    /// The corresponding RNS receipt was already absent. The job remains
    /// bound so the inconsistency cannot be hidden by buffer reuse.
    ReceiptMissing {
        /// Attempt whose receipt could not be cancelled.
        attempt: AttemptToken,
    },
}

/// Rollback failure retaining ownership of the exact bound job.
#[must_use = "a rollback failure retains the only job capable of recovering the bound buffer"]
pub struct RollbackFailure<'a> {
    reason: RollbackErrorKind,
    job: TxJob<'a>,
}

impl<'a> RollbackFailure<'a> {
    /// Typed invariant or receipt-cancellation failure.
    pub const fn reason(&self) -> RollbackErrorKind {
        self.reason
    }

    /// Recover the still-bound unique job for diagnosis or the correct owner.
    pub fn into_job(self) -> TxJob<'a> {
        self.job
    }
}

/// Failure to acknowledge a terminal DATA attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcknowledgeError {
    /// The handle belongs to another node/incarnation or no longer identifies
    /// the same attempt generation.
    StaleOrUnknown,
    /// The attempt is still awaiting a proof or timeout.
    NotTerminal,
    /// An external job still owns the packet bytes for this terminal attempt.
    PacketStillBound,
    /// Private packet/attempt state violates the fixed-pool invariant.
    Invariant,
}

/// Correlation fault between RNS receipt state and the fixed attempt ledger.
///
/// These are invariants, not retryable capacity failures. The triggering
/// proof or timeout remains uncommitted in RNS for diagnosis.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiptCorrelationError {
    /// RNS presented a channel receipt, which this DATA-only owner cannot
    /// represent.
    UnsupportedChannel,
    /// RNS presented a DATA receipt absent from the fixed ledger.
    UnknownData {
        /// Complete proof-correlation hash RNS presented.
        attempt: AttemptToken,
    },
    /// RNS presented a receipt already retained as a terminal tombstone.
    AlreadyTerminal {
        /// Complete proof-correlation hash RNS presented.
        attempt: AttemptToken,
    },
    /// RNS refused the sink without presenting a classifiable candidate.
    ReservationInvariant,
}

/// Result of one timer-maintenance pass through RNS and the attempt ledger.
#[derive(Debug)]
#[must_use = "timer actions and receipt correlation faults must be handled"]
pub struct MaintenanceReport {
    /// Ordinary RNS events and resolved outbound packets.
    pub actions: NodeActions,
    /// Number of DATA timeouts committed to terminal tombstones in this pass.
    pub timed_out_attempts: usize,
    /// A non-retryable mismatch between native receipts and the fixed ledger.
    pub correlation_fault: Option<ReceiptCorrelationError>,
}

/// Scalar occupancy snapshot for external dispatch metadata and DATA receipts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapacitySnapshot {
    /// Packet buffers registered with this owner incarnation.
    pub packet_buffers_registered: usize,
    /// Registered buffers currently bound to queued jobs.
    pub dispatches_queued: usize,
    /// Registered buffers reserved or queued by a transaction.
    pub dispatches_used: usize,
    /// Configured dispatch-metadata limit.
    pub dispatches_limit: usize,
    /// Outstanding destination-DATA receipts retained by RNS.
    pub receipts_used: usize,
    /// Configured destination-DATA receipt limit.
    pub receipts_limit: usize,
    /// Attempts still awaiting a proof or timeout.
    pub attempts_active: usize,
    /// Terminal attempts retained until explicit acknowledgement.
    pub attempts_terminal: usize,
    /// Total reserved, active, or terminal attempt slots.
    pub attempts_used: usize,
    /// Configured attempt-ledger limit.
    pub attempts_limit: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AttemptRef {
    slot: usize,
    generation: u64,
}

#[derive(Clone, Copy)]
enum DispatchState {
    Unregistered,
    Free,
    Reserved {
        attempt: AttemptRef,
    },
    Queued {
        prepared: RnsPreparedData,
        attempt: AttemptRef,
        generation: u64,
        deadline: TxLeaseDeadline,
    },
}

#[derive(Clone, Copy)]
struct DispatchSlot {
    state: DispatchState,
}

impl DispatchSlot {
    const EMPTY: Self = Self {
        state: DispatchState::Unregistered,
    };
}

#[derive(Clone, Copy)]
enum AttemptState {
    Free,
    Reserved {
        generation: u64,
    },
    Active {
        generation: u64,
        token: AttemptToken,
    },
    Terminal {
        generation: u64,
        token: AttemptToken,
        outcome: AttemptOutcome,
    },
}

#[derive(Clone, Copy)]
struct AttemptSlot {
    state: AttemptState,
}

impl AttemptSlot {
    const EMPTY: Self = Self {
        state: AttemptState::Free,
    };
}

/// Iterator over terminal DATA attempts awaiting acknowledgement.
pub struct TerminalAttempts<'a> {
    owner_identity: [u8; 16],
    instance: NodeInstanceId,
    attempts: &'a [AttemptSlot],
    next: usize,
}

impl Iterator for TerminalAttempts<'_> {
    type Item = TerminalAttempt;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some((slot, attempt)) = self.attempts.get(self.next).map(|value| {
            let slot = self.next;
            self.next += 1;
            (slot, value)
        }) {
            if let AttemptState::Terminal {
                generation,
                token,
                outcome,
            } = attempt.state
            {
                return Some(TerminalAttempt {
                    handle: AttemptHandle {
                        owner_identity: self.owner_identity,
                        instance: self.instance,
                        slot,
                        generation,
                    },
                    token,
                    outcome,
                });
            }
        }
        None
    }
}

/// Single owner of a concrete bounded RNS node and external-buffer metadata.
///
/// `PATHS` also bounds destination-DATA receipts in the current Rete storage
/// backend and this owner's attempt ledger. `PATHS` and `LINKS` must be powers
/// of two greater than one; `ANNOUNCES` and `DEDUPLICATION` must be nonzero.
/// `PACKET_BUFFERS` independently bounds registered external packet buffers
/// and their dispatch metadata and may be zero. The 500-byte arrays live
/// outside this value.
pub struct NodeCore<
    const PATHS: usize,
    const ANNOUNCES: usize,
    const DEDUPLICATION: usize,
    const LINKS: usize,
    const PACKET_BUFFERS: usize,
> {
    rns: RnsNode<PATHS, ANNOUNCES, DEDUPLICATION, LINKS>,
    owner_identity: [u8; 16],
    instance: NodeInstanceId,
    dispatches: [DispatchSlot; PACKET_BUFFERS],
    attempts: [AttemptSlot; PATHS],
    next_registration: usize,
    next_attempt: usize,
    dispatch_generation: u64,
    attempt_generation: u64,
}

impl<
    const PATHS: usize,
    const ANNOUNCES: usize,
    const DEDUPLICATION: usize,
    const LINKS: usize,
    const PACKET_BUFFERS: usize,
> NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, PACKET_BUFFERS>
{
    /// Construct the concrete RNS owner and empty external-buffer metadata.
    pub fn new(
        identity: NodeIdentity,
        app_name: &str,
        aspects: &[&str],
        instance: NodeInstanceId,
        config: NodeConfig,
    ) -> Result<Self, NodeConstructionError> {
        const {
            assert!(
                PATHS > 1 && PATHS.is_power_of_two(),
                "node-core PATHS must be a power of two greater than one"
            );
            assert!(
                ANNOUNCES > 0,
                "node-core ANNOUNCES must be greater than zero"
            );
            assert!(
                DEDUPLICATION > 0,
                "node-core DEDUPLICATION must be greater than zero"
            );
            assert!(
                LINKS > 1 && LINKS.is_power_of_two(),
                "node-core LINKS must be a power of two greater than one"
            );
            assert!(
                PACKET_BUFFERS <= (u16::MAX as usize) + 1,
                "node-core PACKET_BUFFERS must fit the 16-bit packet-slot namespace"
            );
        }
        let role = match config.role {
            NodeRole::Endpoint => RnsNodeRole::Endpoint,
            NodeRole::Transport => RnsNodeRole::Transport,
        };
        let rns = RnsNode::new(
            identity.0,
            app_name,
            aspects,
            RnsNodeConfig {
                role,
                max_additional_destinations: config.max_additional_destinations,
            },
        )
        .map_err(|_| NodeConstructionError::InvalidRnsConfiguration)?;
        let owner_identity = *rns.identity_hash().as_bytes();

        Ok(Self {
            rns,
            owner_identity,
            instance,
            dispatches: [DispatchSlot::EMPTY; PACKET_BUFFERS],
            attempts: [AttemptSlot::EMPTY; PATHS],
            next_registration: 0,
            next_attempt: 0,
            dispatch_generation: 0,
            attempt_generation: 0,
        })
    }

    /// Primary local destination hash.
    pub fn destination_hash(&self) -> DestinationHash {
        DestinationHash::new(*self.rns.destination_hash().as_bytes())
    }

    /// Cache one peer identity for deterministic direct sending or tests.
    /// Learned announces will eventually drive the same native identity table.
    pub fn register_peer(
        &mut self,
        peer: &NodeIdentity,
        app_name: &str,
        aspects: &[&str],
        now: MonotonicSeconds,
    ) -> Result<(), PeerRegistrationError> {
        self.rns
            .register_peer(&peer.0, app_name, aspects, now.get())
            .map_err(|_| PeerRegistrationError::InvalidPeer)
    }

    /// Register one externally allocated packet buffer with this owner.
    ///
    /// Registration assigns the next stable slot exactly once. A buffer must
    /// be registered before it can enter the available/completion channel.
    pub fn register_packet_buffer(
        &mut self,
        buffer: &mut TxPacketBuffer,
    ) -> Result<PacketSlotId, BufferRegistrationError> {
        if !matches!(buffer.binding, BufferBinding::Unregistered) {
            return Err(BufferRegistrationError::AlreadyRegistered);
        }
        if PACKET_BUFFERS == 0 {
            return Err(BufferRegistrationError::PoolFull {
                limit: PACKET_BUFFERS,
            });
        }

        let mut index = self.next_registration;
        let mut free = None;
        for _ in 0..PACKET_BUFFERS {
            if matches!(self.dispatches[index].state, DispatchState::Unregistered) {
                free = Some(index);
                break;
            }
            index = Self::next_dispatch_index(index);
        }
        let index = free.ok_or(BufferRegistrationError::PoolFull {
            limit: PACKET_BUFFERS,
        })?;
        let raw = u16::try_from(index).map_err(|_| BufferRegistrationError::PoolFull {
            limit: PACKET_BUFFERS,
        })?;
        let slot = PacketSlotId(raw);
        self.dispatches[index].state = DispatchState::Free;
        buffer.binding = BufferBinding::Available {
            owner_identity: self.owner_identity,
            instance: self.instance,
            slot,
        };
        self.next_registration = Self::next_dispatch_index(index);
        Ok(slot)
    }

    /// Prepare encrypted destination DATA directly into one registered
    /// external buffer and bind it to a unique job.
    ///
    /// Buffer, dispatch and attempt capacity plus identifier exhaustion are
    /// rejected before entropy or RNS state mutation. Native preparation
    /// errors restore both metadata reservations and return the same available
    /// buffer. On success no packet bytes move or copy.
    pub fn prepare_data_into_slot<'a, R: RngCore + CryptoRng>(
        &mut self,
        buffer: &'a mut TxPacketBuffer,
        destination: &DestinationHash,
        plaintext: &[u8],
        now: MonotonicSeconds,
        deadline: TxLeaseDeadline,
        rng: &mut R,
    ) -> Result<TxJob<'a>, PrepareFailure<'a>> {
        let reservation = self.reserve_external_submission(buffer);
        let (slot, attempt, dispatch_generation) = match reservation {
            Ok(reservation) => reservation,
            Err(reason) => return Err(PrepareFailure { reason, buffer }),
        };

        let result = self.rns.prepare_data_into(
            &destination.into_rns(),
            plaintext,
            now.get(),
            rng,
            &mut buffer.bytes,
        );

        let prepared = match result {
            Ok(prepared) => prepared,
            Err(error) => {
                self.dispatches[slot.index()].state = DispatchState::Free;
                self.attempts[attempt.slot].state = AttemptState::Free;
                return Err(PrepareFailure {
                    reason: error.into(),
                    buffer,
                });
            }
        };

        let token = AttemptToken(*prepared.receipt().as_bytes());
        self.attempts[attempt.slot].state = AttemptState::Active {
            generation: attempt.generation,
            token,
        };
        self.dispatches[slot.index()].state = DispatchState::Queued {
            prepared,
            attempt,
            generation: dispatch_generation,
            deadline,
        };
        let scalar = PreparedPacket::from_rns(prepared, self.handle_for(attempt), slot, deadline);
        buffer.binding = BufferBinding::Bound {
            dispatch_generation,
            prepared: scalar,
        };
        self.next_attempt = Self::next_attempt_index(attempt.slot);
        self.attempt_generation = attempt.generation;
        self.dispatch_generation = dispatch_generation;

        Ok(TxJob {
            buffer,
            dispatch: DispatchHandle {
                instance: self.instance,
                slot,
                generation: dispatch_generation,
            },
        })
    }

    /// Roll back a queued job after a handoff proves it was never accepted for
    /// transmission.
    ///
    /// The exact receipt is cancelled before attempt, dispatch or buffer state
    /// becomes reusable. Any mismatch returns the still-bound unique job.
    pub fn rollback_queued<'a>(
        &mut self,
        job: TxJob<'a>,
    ) -> Result<&'a mut TxPacketBuffer, RollbackFailure<'a>> {
        let (prepared, attempt) = match self.validate_queued_job(&job) {
            Ok(values) => values,
            Err(reason) => return Err(RollbackFailure { reason, job }),
        };

        match self.terminal_for(attempt) {
            Some(terminal) if terminal.token().as_bytes() == prepared.receipt().as_bytes() => {}
            Some(_) => {
                return Err(RollbackFailure {
                    reason: RollbackErrorKind::Invariant,
                    job,
                });
            }
            None => {
                if !self.attempt_is_active(attempt, prepared) {
                    return Err(RollbackFailure {
                        reason: RollbackErrorKind::Invariant,
                        job,
                    });
                }
                if !self.rns.cancel_data_receipt(prepared.receipt()) {
                    return Err(RollbackFailure {
                        reason: RollbackErrorKind::ReceiptMissing {
                            attempt: AttemptToken(*prepared.receipt().as_bytes()),
                        },
                        job,
                    });
                }
                self.attempts[attempt.slot].state = AttemptState::Free;
            }
        }

        self.dispatches[job.dispatch.slot.index()].state = DispatchState::Free;
        job.buffer.binding = BufferBinding::Available {
            owner_identity: self.owner_identity,
            instance: self.instance,
            slot: job.dispatch.slot,
        };
        Ok(job.buffer)
    }

    /// Process one inbound packet while committing any proof to the exact
    /// fixed-ledger attempt before RNS mutates its receipt state.
    ///
    /// Correlation failures are invariant faults, not capacity backpressure;
    /// the packet remains retryable inside RNS for diagnosis or recovery.
    pub fn ingest<R: RngCore + CryptoRng>(
        &mut self,
        raw: &[u8],
        now: MonotonicSeconds,
        interface: PacketInterfaceId,
        rng: &mut R,
    ) -> Result<IngressReport, ReceiptCorrelationError> {
        let (result, fault) = {
            let (rns, attempts) = (&mut self.rns, &mut self.attempts);
            let mut sink = AttemptReceiptSink::new(attempts);
            let result =
                rns.ingest_with_receipt_sink(raw, now.get(), interface.into_rns(), rng, &mut sink);
            (result, sink.fault)
        };

        match (result, fault) {
            (Ok(report), None) => Ok(report),
            (Ok(_), Some(fault)) | (Err(ReceiptReservationUnavailable), Some(fault)) => Err(fault),
            (Err(ReceiptReservationUnavailable), None) => {
                Err(ReceiptCorrelationError::ReservationInvariant)
            }
        }
    }

    /// Run RNS timer maintenance and transactionally retain DATA timeouts.
    ///
    /// Ordinary maintenance actions are always returned, including when a
    /// receipt/ledger correlation invariant is detected.
    pub fn tick<R: RngCore + CryptoRng>(
        &mut self,
        now: MonotonicSeconds,
        rng: &mut R,
    ) -> MaintenanceReport {
        let (report, fault) = {
            let (rns, attempts) = (&mut self.rns, &mut self.attempts);
            let mut sink = AttemptReceiptSink::new(attempts);
            let report = rns.tick_with_receipt_sink(now.get(), rng, &mut sink);
            (report, sink.fault)
        };
        MaintenanceReport {
            actions: report.actions,
            timed_out_attempts: report.timed_out_receipts,
            correlation_fault: fault.or_else(|| {
                report
                    .receipt_terminals_deferred
                    .then_some(ReceiptCorrelationError::ReservationInvariant)
            }),
        }
    }

    /// Iterate terminal attempt tombstones in stable ledger-slot order.
    pub fn terminal_attempts(&self) -> TerminalAttempts<'_> {
        TerminalAttempts {
            owner_identity: self.owner_identity,
            instance: self.instance,
            attempts: &self.attempts,
            next: 0,
        }
    }

    /// Release one terminal tombstone after its outcome has been projected or
    /// persisted by the caller.
    pub fn acknowledge_terminal(
        &mut self,
        handle: AttemptHandle,
    ) -> Result<TerminalAttempt, AcknowledgeError> {
        let attempt_ref = self.validate_attempt_handle(handle)?;
        let state = self.attempts[attempt_ref.slot].state;
        let AttemptState::Terminal {
            generation,
            token,
            outcome,
        } = state
        else {
            return match state {
                AttemptState::Active { .. } | AttemptState::Reserved { .. } => {
                    Err(AcknowledgeError::NotTerminal)
                }
                AttemptState::Free | AttemptState::Terminal { .. } => {
                    Err(AcknowledgeError::StaleOrUnknown)
                }
            };
        };

        for slot in &self.dispatches {
            match slot.state {
                DispatchState::Queued { attempt, .. } if attempt == attempt_ref => {
                    return Err(AcknowledgeError::PacketStillBound);
                }
                DispatchState::Reserved { attempt, .. } if attempt == attempt_ref => {
                    return Err(AcknowledgeError::Invariant);
                }
                DispatchState::Unregistered
                | DispatchState::Free
                | DispatchState::Reserved { .. }
                | DispatchState::Queued { .. } => {}
            }
        }

        debug_assert_eq!(generation, attempt_ref.generation);
        let terminal = TerminalAttempt {
            handle,
            token,
            outcome,
        };
        self.attempts[attempt_ref.slot].state = AttemptState::Free;
        Ok(terminal)
    }

    /// Read scalar dispatch and receipt occupancy without exposing RNS
    /// internals.
    pub fn capacities(&self) -> CapacitySnapshot {
        let mut registered = 0;
        let mut queued = 0;
        let mut used = 0;
        for slot in &self.dispatches {
            match slot.state {
                DispatchState::Unregistered => {}
                DispatchState::Free => registered += 1,
                DispatchState::Reserved { .. } => {
                    registered += 1;
                    used += 1;
                }
                DispatchState::Queued { .. } => {
                    registered += 1;
                    queued += 1;
                    used += 1;
                }
            }
        }
        let mut attempts_active = 0;
        let mut attempts_terminal = 0;
        let mut attempts_used = 0;
        for slot in &self.attempts {
            match slot.state {
                AttemptState::Free => {}
                AttemptState::Reserved { .. } => attempts_used += 1,
                AttemptState::Active { .. } => {
                    attempts_active += 1;
                    attempts_used += 1;
                }
                AttemptState::Terminal { .. } => {
                    attempts_terminal += 1;
                    attempts_used += 1;
                }
            }
        }
        let rns = self.rns.metrics();
        CapacitySnapshot {
            packet_buffers_registered: registered,
            dispatches_queued: queued,
            dispatches_used: used,
            dispatches_limit: PACKET_BUFFERS,
            receipts_used: rns.capacity.receipts.used,
            receipts_limit: rns.capacity.receipts.limit,
            attempts_active,
            attempts_terminal,
            attempts_used,
            attempts_limit: PATHS,
        }
    }

    fn reserve_external_submission(
        &mut self,
        buffer: &TxPacketBuffer,
    ) -> Result<(PacketSlotId, AttemptRef, u64), SubmitError> {
        let (owner_identity, instance, slot) = match buffer.binding {
            BufferBinding::Unregistered => return Err(SubmitError::UnregisteredPacketBuffer),
            BufferBinding::Available {
                owner_identity,
                instance,
                slot,
            } => (owner_identity, instance, slot),
            BufferBinding::Bound { prepared, .. } => {
                return if prepared.handle.owner_identity == self.owner_identity
                    && prepared.handle.instance == self.instance
                {
                    Err(SubmitError::PacketBufferBusy)
                } else {
                    Err(SubmitError::ForeignPacketBuffer)
                };
            }
        };
        if owner_identity != self.owner_identity || instance != self.instance {
            return Err(SubmitError::ForeignPacketBuffer);
        }
        if !matches!(
            self.dispatches.get(slot.index()).map(|entry| entry.state),
            Some(DispatchState::Free)
        ) {
            return Err(SubmitError::PacketBufferInvariant);
        }

        let mut attempt_slot = self.next_attempt;
        let mut free_attempt = None;
        for _ in 0..PATHS {
            if matches!(self.attempts[attempt_slot].state, AttemptState::Free) {
                free_attempt = Some(attempt_slot);
                break;
            }
            attempt_slot = Self::next_attempt_index(attempt_slot);
        }
        let attempt_slot = free_attempt.ok_or(SubmitError::AttemptLedgerFull { limit: PATHS })?;
        let attempt_generation = self
            .attempt_generation
            .checked_add(1)
            .ok_or(SubmitError::AttemptIdentifierExhausted)?;
        let dispatch_generation = self
            .dispatch_generation
            .checked_add(1)
            .ok_or(SubmitError::DispatchIdentifierExhausted)?;
        let attempt = AttemptRef {
            slot: attempt_slot,
            generation: attempt_generation,
        };
        self.attempts[attempt_slot].state = AttemptState::Reserved {
            generation: attempt_generation,
        };
        self.dispatches[slot.index()].state = DispatchState::Reserved { attempt };
        Ok((slot, attempt, dispatch_generation))
    }

    fn validate_queued_job(
        &self,
        job: &TxJob<'_>,
    ) -> Result<(RnsPreparedData, AttemptRef), RollbackErrorKind> {
        if job.dispatch.instance != self.instance {
            return Err(RollbackErrorKind::StaleOrUnknown);
        }
        let scalar = match job.buffer.binding {
            BufferBinding::Bound {
                dispatch_generation,
                prepared,
            } if dispatch_generation == job.dispatch.generation
                && prepared.slot_id() == job.dispatch.slot =>
            {
                prepared
            }
            BufferBinding::Unregistered
            | BufferBinding::Available { .. }
            | BufferBinding::Bound { .. } => return Err(RollbackErrorKind::Invariant),
        };
        if scalar.handle.owner_identity != self.owner_identity
            || scalar.handle.instance != self.instance
            || scalar.handle.instance != job.dispatch.instance
        {
            return Err(RollbackErrorKind::StaleOrUnknown);
        }
        let Some(slot) = self.dispatches.get(job.dispatch.slot.index()) else {
            return Err(RollbackErrorKind::StaleOrUnknown);
        };
        match slot.state {
            DispatchState::Queued {
                prepared,
                attempt,
                generation,
                deadline,
            } if generation == job.dispatch.generation
                && prepared.packet_len() == scalar.packet_len()
                && prepared.receipt().as_bytes() == scalar.attempt().as_bytes()
                && TxTarget::from_rns(prepared.target()) == scalar.target()
                && deadline == scalar.deadline()
                && self.handle_for(attempt) == scalar.handle() =>
            {
                Ok((prepared, attempt))
            }
            DispatchState::Unregistered
            | DispatchState::Free
            | DispatchState::Reserved { .. }
            | DispatchState::Queued { .. } => Err(RollbackErrorKind::StaleOrUnknown),
        }
    }

    fn attempt_is_active(&self, attempt: AttemptRef, prepared: RnsPreparedData) -> bool {
        matches!(
            self.attempts.get(attempt.slot).map(|slot| slot.state),
            Some(AttemptState::Active { generation, token })
                if generation == attempt.generation
                    && token.as_bytes() == prepared.receipt().as_bytes()
        )
    }

    fn terminal_for(&self, attempt: AttemptRef) -> Option<TerminalAttempt> {
        let AttemptState::Terminal {
            generation,
            token,
            outcome,
        } = self.attempts.get(attempt.slot)?.state
        else {
            return None;
        };
        (generation == attempt.generation).then_some(TerminalAttempt {
            handle: self.handle_for(attempt),
            token,
            outcome,
        })
    }

    fn handle_for(&self, attempt: AttemptRef) -> AttemptHandle {
        AttemptHandle {
            owner_identity: self.owner_identity,
            instance: self.instance,
            slot: attempt.slot,
            generation: attempt.generation,
        }
    }

    fn validate_attempt_handle(
        &self,
        handle: AttemptHandle,
    ) -> Result<AttemptRef, AcknowledgeError> {
        if handle.owner_identity != self.owner_identity || handle.instance != self.instance {
            return Err(AcknowledgeError::StaleOrUnknown);
        }
        let Some(slot) = self.attempts.get(handle.slot) else {
            return Err(AcknowledgeError::StaleOrUnknown);
        };
        let generation = match slot.state {
            AttemptState::Reserved { generation }
            | AttemptState::Active { generation, .. }
            | AttemptState::Terminal { generation, .. } => generation,
            AttemptState::Free => return Err(AcknowledgeError::StaleOrUnknown),
        };
        if generation != handle.generation {
            return Err(AcknowledgeError::StaleOrUnknown);
        }
        Ok(AttemptRef {
            slot: handle.slot,
            generation,
        })
    }

    fn next_dispatch_index(index: usize) -> usize {
        if index + 1 == PACKET_BUFFERS {
            0
        } else {
            index + 1
        }
    }

    fn next_attempt_index(index: usize) -> usize {
        if index + 1 == PATHS { 0 } else { index + 1 }
    }
}

struct AttemptReceiptSink<'a> {
    attempts: &'a mut [AttemptSlot],
    fault: Option<ReceiptCorrelationError>,
}

impl<'a> AttemptReceiptSink<'a> {
    fn new(attempts: &'a mut [AttemptSlot]) -> Self {
        Self {
            attempts,
            fault: None,
        }
    }

    fn reject(
        &mut self,
        fault: ReceiptCorrelationError,
    ) -> Result<AttemptTerminalReservation<'_>, ReceiptReservationUnavailable> {
        if self.fault.is_none() {
            self.fault = Some(fault);
        }
        Err(ReceiptReservationUnavailable)
    }
}

struct AttemptTerminalReservation<'a> {
    state: &'a mut AttemptState,
    generation: u64,
    token: AttemptToken,
}

impl ReceiptTerminalSink for AttemptReceiptSink<'_> {
    type Reservation<'a>
        = AttemptTerminalReservation<'a>
    where
        Self: 'a;

    fn try_reserve(
        &mut self,
        candidate: ReceiptCandidate,
    ) -> Result<Self::Reservation<'_>, ReceiptReservationUnavailable> {
        if candidate.kind() == ReceiptKind::Channel {
            return self.reject(ReceiptCorrelationError::UnsupportedChannel);
        }
        let token = AttemptToken(*candidate.receipt().as_bytes());
        let active_slot = self.attempts.iter().position(|slot| {
            matches!(slot.state, AttemptState::Active { token: active, .. } if active == token)
        });
        if let Some(index) = active_slot {
            let generation = match self.attempts[index].state {
                AttemptState::Active { generation, .. } => generation,
                AttemptState::Free
                | AttemptState::Reserved { .. }
                | AttemptState::Terminal { .. } => {
                    return self.reject(ReceiptCorrelationError::ReservationInvariant);
                }
            };
            return Ok(AttemptTerminalReservation {
                state: &mut self.attempts[index].state,
                generation,
                token,
            });
        }
        if self.attempts.iter().any(|slot| {
            matches!(
                slot.state,
                AttemptState::Terminal {
                    token: terminal_token,
                    ..
                } if terminal_token == token
            )
        }) {
            self.reject(ReceiptCorrelationError::AlreadyTerminal { attempt: token })
        } else {
            self.reject(ReceiptCorrelationError::UnknownData { attempt: token })
        }
    }
}

impl ReceiptTerminalReservation for AttemptTerminalReservation<'_> {
    fn commit(self, terminal: ReceiptTerminal) {
        let candidate = terminal.candidate();
        debug_assert_eq!(candidate.kind(), ReceiptKind::Data);
        debug_assert_eq!(candidate.receipt().as_bytes(), self.token.as_bytes());
        let outcome = match terminal {
            ReceiptTerminal::Delivered(_) => AttemptOutcome::Delivered,
            ReceiptTerminal::TimedOut(_) => AttemptOutcome::DeliveryTimeout,
        };
        *self.state = AttemptState::Terminal {
            generation: self.generation,
            token: self.token,
            outcome,
        };
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

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

    type TestNode<const PATHS: usize, const BUFFERS: usize> = NodeCore<PATHS, 2, 8, 2, BUFFERS>;

    fn identity(tag: u8) -> NodeIdentity {
        NodeIdentity::from_private_key(&[tag; 64]).unwrap()
    }

    fn node<const PATHS: usize, const BUFFERS: usize>(
        tag: u8,
        aspect: &str,
    ) -> TestNode<PATHS, BUFFERS> {
        node_with_instance(tag, aspect, tag.wrapping_add(0x80))
    }

    fn node_with_instance<const PATHS: usize, const BUFFERS: usize>(
        tag: u8,
        aspect: &str,
        instance_tag: u8,
    ) -> TestNode<PATHS, BUFFERS> {
        TestNode::new(
            identity(tag),
            "reticulum",
            &[aspect],
            NodeInstanceId::new([instance_tag; 16]),
            NodeConfig::endpoint(),
        )
        .unwrap()
    }

    const fn time(seconds: u64) -> MonotonicSeconds {
        MonotonicSeconds::new(seconds)
    }

    const fn deadline(milliseconds: u64) -> TxLeaseDeadline {
        TxLeaseDeadline::new(MonotonicMillis::new(milliseconds))
    }

    fn register_receiver<const PATHS: usize, const BUFFERS: usize>(
        sender: &mut TestNode<PATHS, BUFFERS>,
        tag: u8,
        aspect: &str,
    ) {
        sender
            .register_peer(&identity(tag), "reticulum", &[aspect], time(0))
            .unwrap();
    }

    fn registered_buffer<const PATHS: usize, const BUFFERS: usize>(
        owner: &mut TestNode<PATHS, BUFFERS>,
    ) -> TxPacketBuffer {
        let mut buffer = TxPacketBuffer::new();
        owner.register_packet_buffer(&mut buffer).unwrap();
        buffer
    }

    fn prepare<'a, const PATHS: usize, const BUFFERS: usize>(
        sender: &mut TestNode<PATHS, BUFFERS>,
        buffer: &'a mut TxPacketBuffer,
        destination: DestinationHash,
        plaintext: &[u8],
        now: u64,
        rng: &mut CounterRng,
    ) -> TxJob<'a> {
        match sender.prepare_data_into_slot(
            buffer,
            &destination,
            plaintext,
            time(now),
            deadline(now * 1_000 + 500),
            rng,
        ) {
            Ok(job) => job,
            Err(failure) => panic!("preparation failed: {:?}", failure.reason()),
        }
    }

    fn proof_for(receiver_tag: u8, attempt: AttemptToken) -> Vec<u8> {
        let signature = identity(receiver_tag).0.sign(attempt.as_bytes()).unwrap();
        let mut proof = std::vec![0u8; 19 + 32 + 64];
        proof[0] = 0x03;
        proof[2..18].copy_from_slice(&attempt.as_bytes()[..16]);
        proof[19..51].copy_from_slice(attempt.as_bytes());
        proof[51..].copy_from_slice(&signature);
        proof
    }

    fn implicit_proof_for(receiver_tag: u8, attempt: AttemptToken) -> Vec<u8> {
        let signature = identity(receiver_tag).0.sign(attempt.as_bytes()).unwrap();
        let mut proof = std::vec![0u8; 19 + 64];
        proof[0] = 0x03;
        proof[2..18].copy_from_slice(&attempt.as_bytes()[..16]);
        proof[19..].copy_from_slice(&signature);
        proof
    }

    #[test]
    fn capacity_and_project_routing_types_have_explicit_bounds() {
        assert!(capacity_profile_is_supported(2, 1, 1, 2));
        assert!(capacity_profile_is_supported(16, 4, 32, 4));
        assert!(!capacity_profile_is_supported(3, 1, 1, 2));
        assert!(!capacity_profile_is_supported(2, 0, 1, 2));
        assert!(!capacity_profile_is_supported(2, 1, 0, 2));
        assert!(!capacity_profile_is_supported(2, 1, 1, 3));

        let first = PacketInterfaceId::new(0);
        let last = PacketInterfaceId::new(63);
        let outside = PacketInterfaceId::new(64);
        let set = InterfaceSet::empty()
            .with(first)
            .unwrap()
            .with(last)
            .unwrap();
        assert!(set.contains(first));
        assert!(set.contains(last));
        assert!(!set.contains(outside));
        assert_eq!(set.with(outside), None);
        assert_eq!(InterfaceSet::from_bits(set.bits()), set);

        assert_eq!(TxTarget::from_rns(RnsTxTarget::All), TxTarget::All);
        assert_eq!(
            TxTarget::from_rns(RnsTxTarget::Only(RnsInterfaceId(7))),
            TxTarget::Only(PacketInterfaceId::new(7))
        );
        assert_eq!(
            TxTarget::from_rns(RnsTxTarget::AllExcept(RnsInterfaceId(9))),
            TxTarget::AllExcept(PacketInterfaceId::new(9))
        );
        assert_eq!(deadline(123).instant().get(), 123);
    }

    #[test]
    fn registration_assigns_stable_unique_slots_exactly_once() {
        let mut owner = node::<2, 2>(1, "owner");
        let mut first = TxPacketBuffer::new();
        let mut second = TxPacketBuffer::new();
        let mut extra = TxPacketBuffer::new();

        assert_eq!(owner.register_packet_buffer(&mut first).unwrap().get(), 0);
        assert_eq!(owner.register_packet_buffer(&mut second).unwrap().get(), 1);
        assert_eq!(
            owner.register_packet_buffer(&mut first),
            Err(BufferRegistrationError::AlreadyRegistered)
        );
        assert_eq!(
            owner.register_packet_buffer(&mut extra),
            Err(BufferRegistrationError::PoolFull { limit: 2 })
        );
        assert_eq!(first.slot_id().unwrap().get(), 0);
        assert_eq!(second.slot_id().unwrap().get(), 1);
        assert_eq!(extra.slot_id(), None);
        assert_eq!(owner.capacities().packet_buffers_registered, 2);
    }

    #[test]
    fn zero_capacity_owner_rejects_registration_without_mutating_buffer() {
        let mut owner = node::<2, 0>(2, "owner");
        let mut buffer = TxPacketBuffer::new();
        assert_eq!(
            owner.register_packet_buffer(&mut buffer),
            Err(BufferRegistrationError::PoolFull { limit: 0 })
        );
        assert_eq!(buffer.slot_id(), None);
        assert_eq!(owner.capacities().packet_buffers_registered, 0);
    }

    #[test]
    fn successful_preparation_is_no_copy_parseable_and_preserves_scalar_metadata() {
        let mut sender = node::<2, 1>(3, "sender");
        let receiver = node::<2, 1>(4, "receiver");
        register_receiver(&mut sender, 4, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let original = core::ptr::from_ref(&buffer);
        let mut rng = CounterRng::default();

        let job = prepare(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"external packet",
            1,
            &mut rng,
        );
        assert!(core::ptr::eq(core::ptr::from_ref(job.buffer), original));
        assert_eq!(job.slot_id().get(), 0);
        assert_eq!(job.target(), TxTarget::All);
        assert_eq!(job.deadline(), deadline(1_500));
        assert_eq!(job.prepared().slot_id(), job.slot_id());

        let bytes = &job.buffer.bytes[..usize::from(job.packet_len())];
        let parsed = reticulum_rns_rete::parse_packet(bytes).unwrap();
        assert_eq!(&parsed.compute_hash(), job.attempt().as_bytes());
        assert_eq!(sender.capacities().dispatches_queued, 1);
        assert_eq!(sender.capacities().receipts_used, 1);
        assert_eq!(sender.capacities().attempts_active, 1);
    }

    #[test]
    fn dropping_a_job_quarantines_its_buffer_and_dispatch() {
        let mut sender = node::<2, 1>(40, "sender");
        let receiver = node::<2, 1>(41, "receiver");
        register_receiver(&mut sender, 41, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();

        let attempt = {
            let job = prepare(
                &mut sender,
                &mut buffer,
                receiver.destination_hash(),
                b"lost owner",
                1,
                &mut rng,
            );
            job.attempt()
        };
        let entropy_before = rng.0;

        let failure = match sender.prepare_data_into_slot(
            &mut buffer,
            &receiver.destination_hash(),
            b"must not reuse",
            time(2),
            deadline(3_000),
            &mut rng,
        ) {
            Ok(_) => panic!("a bound buffer was reused after its job was lost"),
            Err(failure) => failure,
        };
        assert_eq!(failure.reason(), SubmitError::PacketBufferBusy);
        assert_eq!(failure.into_buffer().slot_id().unwrap().get(), 0);
        assert_eq!(rng.0, entropy_before);
        assert_eq!(sender.capacities().dispatches_queued, 1);
        assert_eq!(sender.capacities().attempts_active, 1);
        assert_eq!(sender.capacities().receipts_used, 1);
        assert!(matches!(
            sender.dispatches[0].state,
            DispatchState::Queued { prepared, .. }
                if prepared.receipt().as_bytes() == attempt.as_bytes()
        ));
    }

    #[test]
    fn node_dispatch_slots_do_not_embed_packet_arrays() {
        let no_dispatch = core::mem::size_of::<TestNode<2, 0>>();
        let one_dispatch = core::mem::size_of::<TestNode<2, 1>>();
        let dispatch_metadata = one_dispatch - no_dispatch;

        assert!(core::mem::size_of::<TxPacketBuffer>() >= PACKET_CAPACITY);
        assert!(
            dispatch_metadata < PACKET_CAPACITY,
            "one node dispatch slot unexpectedly embeds packet-sized storage"
        );
    }

    #[test]
    fn unregistered_and_foreign_buffers_reject_before_entropy_or_rns_mutation() {
        let mut first = node::<2, 1>(5, "sender");
        let mut second = node::<2, 1>(6, "other");
        let receiver = node::<2, 1>(7, "receiver");
        register_receiver(&mut first, 7, "receiver");
        register_receiver(&mut second, 7, "receiver");
        let mut unregistered = TxPacketBuffer::new();
        let mut foreign = registered_buffer(&mut second);
        let mut rng = CounterRng::default();

        let unregistered_failure = match first.prepare_data_into_slot(
            &mut unregistered,
            &receiver.destination_hash(),
            b"no registration",
            time(1),
            deadline(2_000),
            &mut rng,
        ) {
            Ok(_) => panic!("unregistered buffer was accepted"),
            Err(failure) => failure,
        };
        assert_eq!(
            unregistered_failure.reason(),
            SubmitError::UnregisteredPacketBuffer
        );
        let unregistered = unregistered_failure.into_buffer();
        assert_eq!(unregistered.slot_id(), None);

        let foreign_failure = match first.prepare_data_into_slot(
            &mut foreign,
            &receiver.destination_hash(),
            b"wrong owner",
            time(1),
            deadline(2_000),
            &mut rng,
        ) {
            Ok(_) => panic!("foreign buffer was accepted"),
            Err(failure) => failure,
        };
        assert_eq!(foreign_failure.reason(), SubmitError::ForeignPacketBuffer);
        assert_eq!(foreign_failure.into_buffer().slot_id().unwrap().get(), 0);
        assert_eq!(rng.0, 0);
        assert_eq!(first.capacities().receipts_used, 0);
        assert_eq!(first.capacities().attempts_used, 0);
    }

    #[test]
    fn native_preparation_failures_return_the_same_available_buffer() {
        let mut sender = node::<2, 1>(8, "sender");
        let receiver = node::<2, 1>(9, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let pointer = core::ptr::from_ref(&buffer);
        let mut rng = CounterRng::default();

        let unknown = match sender.prepare_data_into_slot(
            &mut buffer,
            &DestinationHash::new([0xa5; 16]),
            b"unknown",
            time(1),
            deadline(2_000),
            &mut rng,
        ) {
            Ok(_) => panic!("unknown destination was accepted"),
            Err(failure) => failure,
        };
        assert_eq!(unknown.reason(), SubmitError::UnknownDestination);
        let returned = unknown.into_buffer();
        assert!(core::ptr::eq(core::ptr::from_ref(returned), pointer));
        assert_eq!(rng.0, 0);
        assert_eq!(sender.capacities().dispatches_used, 0);
        assert_eq!(sender.capacities().attempts_used, 0);

        register_receiver(&mut sender, 9, "receiver");
        let oversized = [0x5a; MAX_DATA_PAYLOAD + 1];
        let failure = match sender.prepare_data_into_slot(
            returned,
            &receiver.destination_hash(),
            &oversized,
            time(2),
            deadline(3_000),
            &mut rng,
        ) {
            Ok(_) => panic!("oversized payload was accepted"),
            Err(failure) => failure,
        };
        assert_eq!(
            failure.reason(),
            SubmitError::PayloadTooLarge {
                actual: MAX_DATA_PAYLOAD + 1,
                maximum: MAX_DATA_PAYLOAD,
            }
        );
        let returned = failure.into_buffer();
        assert!(core::ptr::eq(core::ptr::from_ref(returned), pointer));
        assert_eq!(rng.0, 0);

        let job = prepare(
            &mut sender,
            returned,
            receiver.destination_hash(),
            b"valid after failures",
            3,
            &mut rng,
        );
        assert_eq!(job.slot_id().get(), 0);
    }

    #[test]
    fn attempt_and_dispatch_exhaustion_reject_before_entropy() {
        let receiver = node::<2, 3>(11, "receiver");

        let mut attempt_full = node::<2, 3>(10, "sender");
        register_receiver(&mut attempt_full, 11, "receiver");
        let mut a = registered_buffer(&mut attempt_full);
        let mut b = registered_buffer(&mut attempt_full);
        let mut c = registered_buffer(&mut attempt_full);
        let mut rng = CounterRng::default();
        let first = prepare(
            &mut attempt_full,
            &mut a,
            receiver.destination_hash(),
            b"a",
            1,
            &mut rng,
        );
        let second = prepare(
            &mut attempt_full,
            &mut b,
            receiver.destination_hash(),
            b"b",
            2,
            &mut rng,
        );
        let rng_before = rng.0;
        let failure = match attempt_full.prepare_data_into_slot(
            &mut c,
            &receiver.destination_hash(),
            b"c",
            time(3),
            deadline(4_000),
            &mut rng,
        ) {
            Ok(_) => panic!("full ledger accepted another attempt"),
            Err(failure) => failure,
        };
        assert_eq!(
            failure.reason(),
            SubmitError::AttemptLedgerFull { limit: 2 }
        );
        assert_eq!(rng.0, rng_before);
        assert_eq!(attempt_full.capacities().dispatches_queued, 2);
        let _ = (first, second);

        let mut exhausted = node::<2, 1>(12, "sender");
        register_receiver(&mut exhausted, 11, "receiver");
        let mut buffer = registered_buffer(&mut exhausted);
        exhausted.dispatch_generation = u64::MAX;
        let mut rng = CounterRng::default();
        let failure = match exhausted.prepare_data_into_slot(
            &mut buffer,
            &receiver.destination_hash(),
            b"no dispatch id",
            time(1),
            deadline(2_000),
            &mut rng,
        ) {
            Ok(_) => panic!("exhausted dispatch generation was accepted"),
            Err(failure) => failure,
        };
        assert_eq!(failure.reason(), SubmitError::DispatchIdentifierExhausted);
        assert_eq!(rng.0, 0);
        assert_eq!(exhausted.capacities().dispatches_used, 0);
    }

    #[test]
    fn receipt_hash_collision_rolls_back_only_the_rejected_transaction() {
        let mut sender = node::<2, 2>(13, "sender");
        let receiver = node::<2, 2>(14, "receiver");
        register_receiver(&mut sender, 14, "receiver");
        let mut first_buffer = registered_buffer(&mut sender);
        let mut second_buffer = registered_buffer(&mut sender);
        let mut first_rng = CounterRng::default();
        let first = prepare(
            &mut sender,
            &mut first_buffer,
            receiver.destination_hash(),
            b"repeat",
            1,
            &mut first_rng,
        );
        let mut repeated_rng = CounterRng::default();

        let failure = match sender.prepare_data_into_slot(
            &mut second_buffer,
            &receiver.destination_hash(),
            b"repeat",
            time(1),
            deadline(1_500),
            &mut repeated_rng,
        ) {
            Ok(_) => panic!("duplicate receipt hash was accepted"),
            Err(failure) => failure,
        };
        assert_eq!(failure.reason(), SubmitError::ReceiptHashAlreadyTracked);
        let returned = failure.into_buffer();
        assert_eq!(returned.slot_id().unwrap().get(), 1);
        assert_eq!(sender.capacities().dispatches_queued, 1);
        assert_eq!(sender.capacities().attempts_active, 1);
        assert_eq!(sender.capacities().receipts_used, 1);
        assert_eq!(first.slot_id().get(), 0);
    }

    #[test]
    fn queue_handoff_failure_rolls_back_exact_receipt_and_reuses_same_buffer() {
        let mut sender = node::<2, 1>(15, "sender");
        let receiver = node::<2, 1>(16, "receiver");
        register_receiver(&mut sender, 16, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let pointer = core::ptr::from_ref(&buffer);
        let mut rng = CounterRng::default();

        let first = prepare(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"queue full",
            1,
            &mut rng,
        );
        let old_handle = first.attempt_handle();
        let returned = match sender.rollback_queued(first) {
            Ok(buffer) => buffer,
            Err(failure) => panic!("rollback failed: {:?}", failure.reason()),
        };
        assert!(core::ptr::eq(core::ptr::from_ref(returned), pointer));
        assert_eq!(sender.capacities().dispatches_used, 0);
        assert_eq!(sender.capacities().attempts_used, 0);
        assert_eq!(sender.capacities().receipts_used, 0);
        assert_eq!(
            sender.acknowledge_terminal(old_handle),
            Err(AcknowledgeError::StaleOrUnknown)
        );

        let second = prepare(
            &mut sender,
            returned,
            receiver.destination_hash(),
            b"reused",
            2,
            &mut rng,
        );
        assert_eq!(second.slot_id().get(), 0);
        assert_ne!(second.attempt_handle(), old_handle);
    }

    #[test]
    fn missing_rollback_receipt_retains_the_bound_unique_job() {
        let mut sender = node::<2, 1>(17, "sender");
        let receiver = node::<2, 1>(18, "receiver");
        register_receiver(&mut sender, 18, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"invariant",
            1,
            &mut rng,
        );
        let attempt = job.attempt();
        let DispatchState::Queued { prepared, .. } = sender.dispatches[0].state else {
            panic!("expected queued dispatch")
        };
        assert!(sender.rns.cancel_data_receipt(prepared.receipt()));

        let failure = match sender.rollback_queued(job) {
            Ok(_) => panic!("missing receipt was hidden"),
            Err(failure) => failure,
        };
        assert_eq!(
            failure.reason(),
            RollbackErrorKind::ReceiptMissing { attempt }
        );
        let job = failure.into_job();
        assert_eq!(job.attempt(), attempt);
        assert!(matches!(job.buffer.binding, BufferBinding::Bound { .. }));
        assert_eq!(sender.capacities().dispatches_queued, 1);
        assert_eq!(sender.capacities().attempts_active, 1);
    }

    #[test]
    fn cross_incarnation_rollback_returns_job_to_the_correct_owner() {
        let receiver = node::<2, 1>(20, "receiver");
        let mut first = node_with_instance::<2, 1>(19, "sender", 1);
        let mut reconstructed = node_with_instance::<2, 1>(19, "sender", 2);
        register_receiver(&mut first, 20, "receiver");
        register_receiver(&mut reconstructed, 20, "receiver");
        let mut buffer = registered_buffer(&mut first);
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut first,
            &mut buffer,
            receiver.destination_hash(),
            b"owned by first",
            1,
            &mut rng,
        );

        let failure = match reconstructed.rollback_queued(job) {
            Ok(_) => panic!("cross-incarnation job was accepted"),
            Err(failure) => failure,
        };
        assert_eq!(failure.reason(), RollbackErrorKind::StaleOrUnknown);
        let job = failure.into_job();
        assert_eq!(first.capacities().dispatches_queued, 1);
        assert_eq!(reconstructed.capacities().dispatches_used, 0);
        assert!(first.rollback_queued(job).is_ok());
    }

    #[test]
    fn terminal_token_mismatch_retains_the_bound_job() {
        let mut sender = node::<2, 1>(42, "sender");
        let receiver = node::<2, 1>(43, "receiver");
        register_receiver(&mut sender, 43, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"terminal invariant",
            1,
            &mut rng,
        );
        let handle = job.attempt_handle();
        sender
            .ingest(
                &proof_for(43, job.attempt()),
                time(2),
                PacketInterfaceId::new(2),
                &mut rng,
            )
            .unwrap();
        sender.attempts[handle.slot].state = AttemptState::Terminal {
            generation: handle.generation,
            token: AttemptToken([0xa5; 32]),
            outcome: AttemptOutcome::Delivered,
        };

        let failure = match sender.rollback_queued(job) {
            Ok(_) => panic!("a mismatched terminal token released the packet buffer"),
            Err(failure) => failure,
        };
        assert_eq!(failure.reason(), RollbackErrorKind::Invariant);
        let job = failure.into_job();
        assert_eq!(job.attempt_handle(), handle);
        assert_eq!(sender.capacities().dispatches_queued, 1);
        assert_eq!(sender.capacities().attempts_terminal, 1);
    }

    #[test]
    fn valid_proof_commits_terminal_while_job_ownership_remains_bound() {
        let mut sender = node::<2, 1>(21, "sender");
        let receiver = node::<2, 1>(22, "receiver");
        register_receiver(&mut sender, 22, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"proof race",
            1,
            &mut rng,
        );
        let scalar = job.prepared();
        let proof = proof_for(22, job.attempt());

        sender
            .ingest(&proof, time(2), PacketInterfaceId::new(2), &mut rng)
            .unwrap();
        let terminal = sender.terminal_attempts().next().unwrap();
        assert_eq!(terminal.handle(), scalar.handle());
        assert_eq!(terminal.outcome(), AttemptOutcome::Delivered);
        assert_eq!(sender.capacities().receipts_used, 0);
        assert_eq!(sender.capacities().dispatches_queued, 1);
        assert_eq!(
            sender.acknowledge_terminal(scalar.handle()),
            Err(AcknowledgeError::PacketStillBound)
        );

        let returned = sender
            .rollback_queued(job)
            .unwrap_or_else(|failure| panic!("terminal rollback failed: {:?}", failure.reason()));
        assert_eq!(returned.slot_id().unwrap(), scalar.slot_id());
        assert_eq!(sender.capacities().dispatches_used, 0);
        assert_eq!(sender.acknowledge_terminal(scalar.handle()), Ok(terminal));
    }

    #[test]
    fn timeout_commits_terminal_while_job_ownership_remains_bound() {
        let mut sender = node::<2, 1>(23, "sender");
        let receiver = node::<2, 1>(24, "receiver");
        register_receiver(&mut sender, 24, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"timeout race",
            100,
            &mut rng,
        );
        let scalar = job.prepared();

        let report = sender.tick(time(132), &mut rng);
        assert_eq!(report.timed_out_attempts, 1);
        assert_eq!(report.correlation_fault, None);
        let terminal = sender.terminal_attempts().next().unwrap();
        assert_eq!(terminal.outcome(), AttemptOutcome::DeliveryTimeout);
        assert_eq!(
            sender.acknowledge_terminal(scalar.handle()),
            Err(AcknowledgeError::PacketStillBound)
        );
        let returned = sender
            .rollback_queued(job)
            .unwrap_or_else(|failure| panic!("terminal rollback failed: {:?}", failure.reason()));
        assert_eq!(returned.slot_id().unwrap(), scalar.slot_id());
        assert_eq!(sender.acknowledge_terminal(scalar.handle()), Ok(terminal));
    }

    #[test]
    fn terminal_tombstones_backpressure_until_exact_acknowledgement() {
        let mut sender = node::<2, 2>(44, "sender");
        let receiver = node::<2, 2>(45, "receiver");
        register_receiver(&mut sender, 45, "receiver");
        let mut first_buffer = registered_buffer(&mut sender);
        let mut second_buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();

        let first = prepare(
            &mut sender,
            &mut first_buffer,
            receiver.destination_hash(),
            b"first tombstone",
            1,
            &mut rng,
        );
        let first_scalar = first.prepared();
        sender
            .ingest(
                &proof_for(45, first.attempt()),
                time(2),
                PacketInterfaceId::new(2),
                &mut rng,
            )
            .unwrap();
        let first_buffer = sender.rollback_queued(first).unwrap_or_else(|failure| {
            panic!("first terminal return failed: {:?}", failure.reason())
        });

        let second = prepare(
            &mut sender,
            &mut second_buffer,
            receiver.destination_hash(),
            b"second tombstone",
            3,
            &mut rng,
        );
        sender
            .ingest(
                &proof_for(45, second.attempt()),
                time(4),
                PacketInterfaceId::new(2),
                &mut rng,
            )
            .unwrap();
        let _second_buffer = sender.rollback_queued(second).unwrap_or_else(|failure| {
            panic!("second terminal return failed: {:?}", failure.reason())
        });
        assert_eq!(sender.capacities().attempts_terminal, 2);

        let entropy_before = rng.0;
        let failure = match sender.prepare_data_into_slot(
            first_buffer,
            &receiver.destination_hash(),
            b"blocked by tombstones",
            time(5),
            deadline(6_000),
            &mut rng,
        ) {
            Ok(_) => panic!("terminal tombstones did not backpressure preparation"),
            Err(failure) => failure,
        };
        assert_eq!(
            failure.reason(),
            SubmitError::AttemptLedgerFull { limit: 2 }
        );
        assert_eq!(rng.0, entropy_before);
        let first_buffer = failure.into_buffer();

        let first_terminal = sender
            .terminal_attempts()
            .find(|terminal| terminal.handle() == first_scalar.handle())
            .unwrap();
        assert_eq!(
            sender.acknowledge_terminal(first_scalar.handle()),
            Ok(first_terminal)
        );
        assert_eq!(
            sender.acknowledge_terminal(first_scalar.handle()),
            Err(AcknowledgeError::StaleOrUnknown)
        );

        let replacement = prepare(
            &mut sender,
            first_buffer,
            receiver.destination_hash(),
            b"after exact acknowledgement",
            6,
            &mut rng,
        );
        assert_ne!(replacement.attempt_handle(), first_scalar.handle());
        assert_eq!(sender.capacities().attempts_active, 1);
        assert_eq!(sender.capacities().attempts_terminal, 1);
    }

    #[test]
    fn invalid_proof_can_retry_without_releasing_job_or_receipt() {
        let mut sender = node::<2, 1>(25, "sender");
        let receiver = node::<2, 1>(26, "receiver");
        register_receiver(&mut sender, 26, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"proof retry",
            10,
            &mut rng,
        );
        let proof = proof_for(26, job.attempt());
        let mut invalid = proof.clone();
        *invalid.last_mut().unwrap() ^= 0xff;

        sender
            .ingest(&invalid, time(11), PacketInterfaceId::new(2), &mut rng)
            .unwrap();
        assert_eq!(sender.capacities().attempts_active, 1);
        assert_eq!(sender.capacities().receipts_used, 1);
        assert_eq!(sender.capacities().dispatches_queued, 1);

        sender
            .ingest(&proof, time(12), PacketInterfaceId::new(2), &mut rng)
            .unwrap();
        assert_eq!(
            sender.terminal_attempts().next().unwrap().outcome(),
            AttemptOutcome::Delivered
        );
        assert!(sender.rollback_queued(job).is_ok());
    }

    #[test]
    fn repeated_full_hash_selects_new_active_attempt_over_old_tombstone() {
        let mut sender = node::<2, 1>(27, "sender");
        let receiver = node::<2, 1>(28, "receiver");
        register_receiver(&mut sender, 28, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let mut first_rng = CounterRng::default();
        let first = prepare(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"repeat after proof",
            1,
            &mut first_rng,
        );
        let first_scalar = first.prepared();
        sender
            .ingest(
                &proof_for(28, first.attempt()),
                time(2),
                PacketInterfaceId::new(2),
                &mut first_rng,
            )
            .unwrap();
        let buffer = sender.rollback_queued(first).unwrap_or_else(|failure| {
            panic!("first terminal return failed: {:?}", failure.reason())
        });

        let mut repeated_rng = CounterRng::default();
        let repeated = prepare(
            &mut sender,
            buffer,
            receiver.destination_hash(),
            b"repeat after proof",
            1,
            &mut repeated_rng,
        );
        assert_eq!(repeated.attempt(), first_scalar.attempt());
        assert_ne!(repeated.attempt_handle(), first_scalar.handle());
        sender
            .ingest(
                &implicit_proof_for(28, repeated.attempt()),
                time(3),
                PacketInterfaceId::new(2),
                &mut repeated_rng,
            )
            .unwrap();

        let terminals = sender.terminal_attempts().collect::<std::vec::Vec<_>>();
        assert_eq!(terminals.len(), 2);
        assert!(
            terminals
                .iter()
                .any(|item| item.handle() == first_scalar.handle())
        );
        assert!(
            terminals
                .iter()
                .any(|item| item.handle() == repeated.attempt_handle())
        );
        assert!(
            terminals
                .iter()
                .all(|item| item.outcome() == AttemptOutcome::Delivered)
        );
    }

    #[test]
    fn unknown_and_already_terminal_receipts_are_explicit_faults() {
        let receiver = node::<2, 1>(30, "receiver");

        let mut unknown = node::<2, 1>(29, "unknown");
        register_receiver(&mut unknown, 30, "receiver");
        let mut unknown_buffer = registered_buffer(&mut unknown);
        let mut rng = CounterRng::default();
        let unknown_job = prepare(
            &mut unknown,
            &mut unknown_buffer,
            receiver.destination_hash(),
            b"orphan",
            1,
            &mut rng,
        );
        unknown.attempts[unknown_job.attempt_handle().slot].state = AttemptState::Free;
        assert_eq!(
            unknown
                .ingest(
                    &proof_for(30, unknown_job.attempt()),
                    time(2),
                    PacketInterfaceId::new(2),
                    &mut rng,
                )
                .unwrap_err(),
            ReceiptCorrelationError::UnknownData {
                attempt: unknown_job.attempt(),
            }
        );
        assert_eq!(unknown.capacities().receipts_used, 1);

        let mut terminal = node::<2, 1>(31, "terminal");
        register_receiver(&mut terminal, 30, "receiver");
        let mut terminal_buffer = registered_buffer(&mut terminal);
        let terminal_job = prepare(
            &mut terminal,
            &mut terminal_buffer,
            receiver.destination_hash(),
            b"premature",
            1,
            &mut rng,
        );
        let handle = terminal_job.attempt_handle();
        terminal.attempts[handle.slot].state = AttemptState::Terminal {
            generation: handle.generation,
            token: terminal_job.attempt(),
            outcome: AttemptOutcome::Delivered,
        };
        assert_eq!(
            terminal
                .ingest(
                    &proof_for(30, terminal_job.attempt()),
                    time(2),
                    PacketInterfaceId::new(2),
                    &mut rng,
                )
                .unwrap_err(),
            ReceiptCorrelationError::AlreadyTerminal {
                attempt: terminal_job.attempt(),
            }
        );
        assert_eq!(terminal.capacities().receipts_used, 1);
    }

    #[test]
    fn attempt_generation_exhaustion_rejects_before_any_transaction_mutation() {
        let mut sender = node::<2, 1>(34, "sender");
        let receiver = node::<2, 1>(35, "receiver");
        register_receiver(&mut sender, 35, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        sender.attempt_generation = u64::MAX;
        let mut rng = CounterRng::default();

        let failure = match sender.prepare_data_into_slot(
            &mut buffer,
            &receiver.destination_hash(),
            b"no attempt identifier",
            time(1),
            deadline(2_000),
            &mut rng,
        ) {
            Ok(_) => panic!("exhausted attempt generation was accepted"),
            Err(failure) => failure,
        };
        assert_eq!(failure.reason(), SubmitError::AttemptIdentifierExhausted);
        assert_eq!(failure.into_buffer().slot_id().unwrap().get(), 0);
        assert_eq!(rng.0, 0);
        assert_eq!(sender.dispatch_generation, 0);
        assert_eq!(sender.capacities().dispatches_used, 0);
        assert_eq!(sender.capacities().attempts_used, 0);
        assert_eq!(sender.capacities().receipts_used, 0);
    }

    #[test]
    fn unknown_timeout_preserves_native_receipt_and_returns_tick_actions() {
        let mut sender = node::<2, 1>(36, "sender");
        let receiver = node::<2, 1>(37, "receiver");
        register_receiver(&mut sender, 37, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"orphan timeout",
            100,
            &mut rng,
        );
        sender.attempts[job.attempt_handle().slot].state = AttemptState::Free;

        let report = sender.tick(time(132), &mut rng);
        assert_eq!(report.timed_out_attempts, 0);
        assert_eq!(
            report.correlation_fault,
            Some(ReceiptCorrelationError::UnknownData {
                attempt: job.attempt(),
            })
        );
        assert_eq!(report.actions.events.len(), 1);
        assert!(report.actions.packets.is_empty());
        assert_eq!(sender.capacities().receipts_used, 1);
        assert_eq!(sender.capacities().dispatches_queued, 1);
    }

    #[test]
    fn channel_receipt_candidate_is_an_explicit_unsupported_fault() {
        let mut initiator = node::<4, 2>(38, "initiator");
        let mut responder = node::<4, 2>(39, "responder");
        register_receiver(&mut initiator, 39, "responder");
        let responder_destination = responder.rns.destination_hash();
        assert!(
            responder
                .rns
                .set_accepts_links(&responder_destination, true)
        );
        let mut rng = CounterRng::default();
        let (request, link_id) = initiator
            .rns
            .initiate_link(responder_destination, 100, &mut rng)
            .unwrap();
        let response = responder
            .rns
            .ingest(request.bytes(), 100, RnsInterfaceId(1), &mut rng);
        let established = initiator.rns.ingest(
            response.actions.packets[0].bytes(),
            101,
            RnsInterfaceId(2),
            &mut rng,
        );
        responder.rns.ingest(
            established.actions.packets[0].bytes(),
            102,
            RnsInterfaceId(1),
            &mut rng,
        );
        let message = initiator
            .rns
            .send_channel_message(&link_id, 7, b"channel proof", 110, &mut rng)
            .unwrap();
        let proof_actions = responder
            .rns
            .ingest(message.bytes(), 110, RnsInterfaceId(1), &mut rng);
        let proof = proof_actions
            .actions
            .packets
            .iter()
            .find(|packet| {
                packet
                    .bytes()
                    .first()
                    .is_some_and(|header| header & 0x03 == 0x03)
            })
            .unwrap();

        assert_eq!(
            initiator
                .ingest(
                    proof.bytes(),
                    time(111),
                    PacketInterfaceId::new(2),
                    &mut rng,
                )
                .unwrap_err(),
            ReceiptCorrelationError::UnsupportedChannel
        );
        assert_eq!(initiator.rns.metrics().capacity.channel_receipts.used, 1);
    }

    #[test]
    fn full_hash_candidate_selects_the_exact_active_attempt() {
        let mut sender = node::<2, 1>(32, "sender");
        let receiver = node::<2, 1>(33, "receiver");
        register_receiver(&mut sender, 33, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"full hash selection",
            1,
            &mut rng,
        );
        let original = job.prepared();
        let generation = original.handle().generation;
        let mut colliding = original.attempt();
        colliding.0[31] ^= 0xff;
        sender.attempts[0].state = AttemptState::Active {
            generation: generation + 1,
            token: colliding,
        };
        sender.attempts[1].state = AttemptState::Active {
            generation,
            token: original.attempt(),
        };
        let DispatchState::Queued {
            prepared,
            generation: dispatch_generation,
            deadline,
            ..
        } = sender.dispatches[0].state
        else {
            panic!("expected queued dispatch")
        };
        let exact = AttemptRef {
            slot: 1,
            generation,
        };
        sender.dispatches[0].state = DispatchState::Queued {
            prepared,
            attempt: exact,
            generation: dispatch_generation,
            deadline,
        };

        sender
            .ingest(
                &proof_for(33, original.attempt()),
                time(2),
                PacketInterfaceId::new(2),
                &mut rng,
            )
            .unwrap();
        assert!(matches!(
            sender.attempts[0].state,
            AttemptState::Active { token, .. } if token == colliding
        ));
        assert!(matches!(
            sender.attempts[1].state,
            AttemptState::Terminal {
                token,
                outcome: AttemptOutcome::Delivered,
                ..
            } if token == original.attempt()
        ));
        let _ = job;
    }
}
