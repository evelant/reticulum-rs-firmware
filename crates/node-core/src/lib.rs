//! Bounded orchestration between the Reticulum protocol owner and interface
//! actors.
//!
//! This slice owns a concrete RNS node, a fixed packet outbox, and a fixed DATA
//! attempt ledger. A receipt is registered only after both final slots have
//! been reserved. Proofs and timeouts become acknowledged terminal tombstones
//! before RNS releases its receipt state. The crate deliberately has no
//! device-API, executor, radio, board, or persistence dependency.
//!
//! Construction and unrelated RNS paths may still allocate inside the current
//! Rete integration. The DATA-to-outbox transaction implemented here uses only
//! caller-owned arrays; it is not a claim that the entire node is allocation
//! free.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use rand_core::{CryptoRng, RngCore};
use reticulum_rns_rete::{
    DestHash as RnsDestinationHash, EmbeddedNode as RnsNode, EmbeddedNodeConfig as RnsNodeConfig,
    Identity as RnsIdentity, IngressReport, InterfaceId, NodeActions, NodeRole as RnsNodeRole,
    PrepareDataError as RnsPrepareDataError, PreparedData as RnsPreparedData, RNS_MTU,
    ReceiptCandidate, ReceiptKind, ReceiptReservationUnavailable, ReceiptTerminal,
    ReceiptTerminalReservation, ReceiptTerminalSink,
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
/// The packet outbox is independent and may have zero slots.
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

/// Scalar metadata for one packet committed to the private outbox.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedPacket {
    packet_len: u16,
    attempt: AttemptToken,
    handle: AttemptHandle,
}

impl PreparedPacket {
    /// Encoded packet length in the fixed outbox slot.
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

    fn from_rns(prepared: RnsPreparedData, handle: AttemptHandle) -> Self {
        Self {
            packet_len: prepared.packet_len(),
            attempt: AttemptToken(*prepared.receipt().as_bytes()),
            handle,
        }
    }
}

/// Failure to reserve and prepare one DATA packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmitError {
    /// Every fixed packet slot is ready or leased.
    OutboxFull {
        /// Configured packet-slot limit.
        limit: usize,
    },
    /// Every attempt slot is active or retained as an unacknowledged
    /// terminal tombstone.
    AttemptLedgerFull {
        /// Configured DATA-attempt limit.
        limit: usize,
    },
    /// The non-repeating attempt-generation space has been exhausted.
    AttemptIdentifierExhausted,
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

/// Opaque, generation-checked authorization to inspect and complete one
/// leased packet slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TxLease {
    owner_identity: [u8; 16],
    instance: NodeInstanceId,
    slot: usize,
    generation: u64,
}

/// Failure while operating on a packet lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeaseError {
    /// The slot is absent, no longer leased, or belongs to a newer lease.
    StaleOrUnknown,
    /// The non-repeating lease-generation space has been exhausted.
    IdentifierExhausted,
    /// Private slot metadata violates the fixed-packet invariant.
    Invariant,
}

/// Failure to borrow bytes from a currently leased packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeasedFrameError {
    /// The supplied packet lease is stale or violates packet-pool invariants.
    Lease(LeaseError),
    /// The attempt became terminal while the interface held its lease. The
    /// interface must release the lease without transmitting these bytes.
    AttemptTerminal(TerminalAttempt),
}

/// Failure to abort a packet that an interface proves was never transmitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AbortError {
    /// The supplied lease is stale or invalid.
    Lease(LeaseError),
    /// The corresponding RNS receipt was already absent; the slot remains
    /// leased so the inconsistency cannot be hidden by reuse.
    ReceiptMissing {
        /// Attempt whose receipt could not be cancelled.
        attempt: AttemptToken,
    },
    /// The packet lease references an absent or mismatched attempt record.
    AttemptInvariant,
}

/// Failure to acknowledge a terminal DATA attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcknowledgeError {
    /// The handle belongs to another node/incarnation or no longer identifies
    /// the same attempt generation.
    StaleOrUnknown,
    /// The attempt is still awaiting a proof or timeout.
    NotTerminal,
    /// An interface still holds the packet bytes for this terminal attempt.
    StillLeased,
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

/// Borrowed packet bytes available only while a matching lease is active.
///
/// A lease is packet-pool bookkeeping, not RF transmit authorization. This
/// slice intentionally does not expose the prepared packet's interface target.
#[derive(Debug, Eq, PartialEq)]
pub struct TxFrame<'a> {
    bytes: &'a [u8],
    attempt: AttemptToken,
}

impl TxFrame<'_> {
    /// Complete encoded Reticulum packet bytes.
    pub const fn bytes(&self) -> &[u8] {
        self.bytes
    }

    /// Proof-correlation token for this leased attempt.
    pub const fn attempt(&self) -> AttemptToken {
        self.attempt
    }
}

/// Scalar occupancy snapshot for the fixed packet pool and DATA receipts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapacitySnapshot {
    /// Ready packets awaiting an interface lease.
    pub outbox_ready: usize,
    /// Packets currently leased to an interface actor.
    pub outbox_leased: usize,
    /// Total non-free packet slots.
    pub outbox_used: usize,
    /// Configured packet-slot limit.
    pub outbox_limit: usize,
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
enum SlotState {
    Free,
    Reserved(AttemptRef),
    Ready {
        prepared: RnsPreparedData,
        attempt: AttemptRef,
    },
    Leased {
        prepared: RnsPreparedData,
        attempt: AttemptRef,
        generation: u64,
    },
}

#[derive(Clone, Copy)]
struct OutboxSlot {
    bytes: [u8; PACKET_CAPACITY],
    state: SlotState,
}

impl OutboxSlot {
    const EMPTY: Self = Self {
        bytes: [0; PACKET_CAPACITY],
        state: SlotState::Free,
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

/// Single owner of a concrete bounded RNS node and fixed packet handoff pool.
///
/// `PATHS` also bounds destination-DATA receipts in the current Rete storage
/// backend and this owner's attempt ledger. `PATHS` and `LINKS` must be powers
/// of two greater than one; `ANNOUNCES` and `DEDUPLICATION` must be nonzero.
/// `OUTBOX` independently bounds packets waiting for or held by an interface
/// actor and may be zero.
pub struct NodeCore<
    const PATHS: usize,
    const ANNOUNCES: usize,
    const DEDUPLICATION: usize,
    const LINKS: usize,
    const OUTBOX: usize,
> {
    rns: RnsNode<PATHS, ANNOUNCES, DEDUPLICATION, LINKS>,
    owner_identity: [u8; 16],
    instance: NodeInstanceId,
    outbox: [OutboxSlot; OUTBOX],
    attempts: [AttemptSlot; PATHS],
    next_reserve: usize,
    next_lease_slot: usize,
    next_attempt: usize,
    lease_generation: u64,
    attempt_generation: u64,
}

impl<
    const PATHS: usize,
    const ANNOUNCES: usize,
    const DEDUPLICATION: usize,
    const LINKS: usize,
    const OUTBOX: usize,
> NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, OUTBOX>
{
    /// Construct the concrete RNS owner and an empty fixed packet pool.
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
            outbox: [OutboxSlot::EMPTY; OUTBOX],
            attempts: [AttemptSlot::EMPTY; PATHS],
            next_reserve: 0,
            next_lease_slot: 0,
            next_attempt: 0,
            lease_generation: 0,
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

    /// Reserve a final outbox slot and prepare encrypted destination DATA
    /// directly into it.
    ///
    /// Capacity rejection happens before entropy or RNS state mutation. Once
    /// RNS commits the receipt, changing `Reserved` to `Ready` cannot fail.
    pub fn enqueue_data<R: RngCore + CryptoRng>(
        &mut self,
        destination: &DestinationHash,
        plaintext: &[u8],
        now: MonotonicSeconds,
        rng: &mut R,
    ) -> Result<PreparedPacket, SubmitError> {
        let (slot_index, attempt_ref) = self.reserve_submission()?;
        let rns_destination = destination.into_rns();
        let result = {
            let (rns, outbox) = (&mut self.rns, &mut self.outbox);
            rns.prepare_data_into(
                &rns_destination,
                plaintext,
                now.get(),
                rng,
                &mut outbox[slot_index].bytes,
            )
        };

        match result {
            Ok(prepared) => {
                let token = AttemptToken(*prepared.receipt().as_bytes());
                self.attempts[attempt_ref.slot].state = AttemptState::Active {
                    generation: attempt_ref.generation,
                    token,
                };
                self.outbox[slot_index].state = SlotState::Ready {
                    prepared,
                    attempt: attempt_ref,
                };
                self.next_reserve = Self::next_outbox_index(slot_index);
                self.next_attempt = Self::next_attempt_index(attempt_ref.slot);
                self.attempt_generation = attempt_ref.generation;
                Ok(PreparedPacket::from_rns(
                    prepared,
                    self.handle_for(attempt_ref),
                ))
            }
            Err(error) => {
                self.outbox[slot_index].state = SlotState::Free;
                self.attempts[attempt_ref.slot].state = AttemptState::Free;
                Err(error.into())
            }
        }
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
        interface: InterfaceId,
        rng: &mut R,
    ) -> Result<IngressReport, ReceiptCorrelationError> {
        let (result, fault) = {
            let (rns, attempts) = (&mut self.rns, &mut self.attempts);
            let mut sink = AttemptReceiptSink::new(attempts);
            let result = rns.ingest_with_receipt_sink(raw, now.get(), interface, rng, &mut sink);
            (result, sink.fault)
        };

        match (result, fault) {
            (Ok(report), None) => {
                self.reconcile_terminal_packets();
                Ok(report)
            }
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
        self.reconcile_terminal_packets();
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

    /// Lease the next ready packet to an interface actor.
    ///
    /// Each lease receives a globally non-repeating generation. Returning a
    /// transiently unsent packet and leasing it again therefore cannot make an
    /// old copied lease valid.
    pub fn lease_next(&mut self) -> Result<Option<TxLease>, LeaseError> {
        self.reconcile_terminal_packets();
        let Some(slot_index) = self.find_ready_slot() else {
            return Ok(None);
        };
        let generation = self
            .lease_generation
            .checked_add(1)
            .ok_or(LeaseError::IdentifierExhausted)?;
        let SlotState::Ready { prepared, attempt } = self.outbox[slot_index].state else {
            return Err(LeaseError::Invariant);
        };
        if !self.attempt_is_active(attempt, prepared) {
            return Err(LeaseError::Invariant);
        }

        self.lease_generation = generation;
        self.outbox[slot_index].state = SlotState::Leased {
            prepared,
            attempt,
            generation,
        };
        self.next_lease_slot = Self::next_outbox_index(slot_index);
        Ok(Some(TxLease {
            owner_identity: self.owner_identity,
            instance: self.instance,
            slot: slot_index,
            generation,
        }))
    }

    /// Borrow packet bytes while a matching interface lease remains active.
    pub fn leased_frame(&self, lease: &TxLease) -> Result<TxFrame<'_>, LeasedFrameError> {
        let (prepared, attempt_ref) = self.leased_entry(lease).map_err(LeasedFrameError::Lease)?;
        if let Some(terminal) = self.terminal_for(attempt_ref) {
            return Err(LeasedFrameError::AttemptTerminal(terminal));
        }
        if !self.attempt_is_active(attempt_ref, prepared) {
            return Err(LeasedFrameError::Lease(LeaseError::Invariant));
        }
        let slot = self
            .outbox
            .get(lease.slot)
            .ok_or(LeasedFrameError::Lease(LeaseError::StaleOrUnknown))?;
        let bytes = slot
            .bytes
            .get(..usize::from(prepared.packet_len()))
            .ok_or(LeasedFrameError::Lease(LeaseError::Invariant))?;
        Ok(TxFrame {
            bytes,
            attempt: AttemptToken(*prepared.receipt().as_bytes()),
        })
    }

    /// Return a transiently rejected, definitely unsent packet to `Ready`.
    ///
    /// The same bytes and receipt are retained. RNS starts its timeout at
    /// preparation, so callers must not use this as an unbounded dwell queue.
    /// If a proof or timeout won the race while the packet was leased, the
    /// bytes are discarded instead and the terminal tombstone is preserved.
    pub fn return_unsent(&mut self, lease: TxLease) -> Result<(), LeaseError> {
        let (prepared, attempt) = self.leased_entry(&lease)?;
        if self.terminal_for(attempt).is_some() {
            self.outbox[lease.slot].state = SlotState::Free;
        } else if self.attempt_is_active(attempt, prepared) {
            self.outbox[lease.slot].state = SlotState::Ready { prepared, attempt };
        } else {
            return Err(LeaseError::Invariant);
        }
        Ok(())
    }

    /// Release packet storage after transmission or any outcome where
    /// transmission may have occurred.
    ///
    /// The receipt deliberately remains live so a later proof or timeout can
    /// resolve the attempt.
    pub fn complete_tx(&mut self, lease: TxLease) -> Result<PreparedPacket, LeaseError> {
        let (prepared, attempt) = self.leased_entry(&lease)?;
        if !self.attempt_matches(attempt, prepared) {
            return Err(LeaseError::Invariant);
        }
        let handle = self.handle_for(attempt);
        self.outbox[lease.slot].state = SlotState::Free;
        Ok(PreparedPacket::from_rns(prepared, handle))
    }

    /// Cancel and release a leased packet that the interface proves never
    /// reached a transmission path.
    ///
    /// Receipt cancellation happens before slot reuse. If the exact receipt is
    /// unexpectedly absent, the slot remains leased and the caller receives an
    /// invariant error. If the attempt already became terminal, the bytes are
    /// discarded without cancellation and the tombstone remains available for
    /// acknowledgement.
    pub fn abort_unsent(&mut self, lease: TxLease) -> Result<PreparedPacket, AbortError> {
        let (prepared, attempt_ref) = self.leased_entry(&lease).map_err(AbortError::Lease)?;
        let attempt = AttemptToken(*prepared.receipt().as_bytes());
        let handle = self.handle_for(attempt_ref);
        if self.terminal_for(attempt_ref).is_some() {
            self.outbox[lease.slot].state = SlotState::Free;
            return Ok(PreparedPacket::from_rns(prepared, handle));
        }
        if !self.attempt_is_active(attempt_ref, prepared) {
            return Err(AbortError::AttemptInvariant);
        }
        if !self.rns.cancel_data_receipt(prepared.receipt()) {
            return Err(AbortError::ReceiptMissing { attempt });
        }
        self.outbox[lease.slot].state = SlotState::Free;
        self.attempts[attempt_ref.slot].state = AttemptState::Free;
        Ok(PreparedPacket::from_rns(prepared, handle))
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

        for slot in &self.outbox {
            match slot.state {
                SlotState::Leased { attempt, .. } if attempt == attempt_ref => {
                    return Err(AcknowledgeError::StillLeased);
                }
                SlotState::Ready { attempt, .. } | SlotState::Reserved(attempt)
                    if attempt == attempt_ref =>
                {
                    return Err(AcknowledgeError::Invariant);
                }
                SlotState::Free
                | SlotState::Reserved(_)
                | SlotState::Ready { .. }
                | SlotState::Leased { .. } => {}
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

    /// Read scalar fixed-pool and receipt occupancy without exposing RNS
    /// internals.
    pub fn capacities(&self) -> CapacitySnapshot {
        let mut ready = 0;
        let mut leased = 0;
        let mut used = 0;
        for slot in &self.outbox {
            match slot.state {
                SlotState::Free => {}
                SlotState::Reserved(_) => used += 1,
                SlotState::Ready { .. } => {
                    ready += 1;
                    used += 1;
                }
                SlotState::Leased { .. } => {
                    leased += 1;
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
            outbox_ready: ready,
            outbox_leased: leased,
            outbox_used: used,
            outbox_limit: OUTBOX,
            receipts_used: rns.capacity.receipts.used,
            receipts_limit: rns.capacity.receipts.limit,
            attempts_active,
            attempts_terminal,
            attempts_used,
            attempts_limit: PATHS,
        }
    }

    fn reserve_submission(&mut self) -> Result<(usize, AttemptRef), SubmitError> {
        if OUTBOX == 0 {
            return Err(SubmitError::OutboxFull { limit: OUTBOX });
        }
        let mut outbox_slot = self.next_reserve;
        let mut free_outbox = None;
        for _ in 0..OUTBOX {
            if matches!(self.outbox[outbox_slot].state, SlotState::Free) {
                free_outbox = Some(outbox_slot);
                break;
            }
            outbox_slot = Self::next_outbox_index(outbox_slot);
        }
        let outbox_slot = free_outbox.ok_or(SubmitError::OutboxFull { limit: OUTBOX })?;

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
        let generation = self
            .attempt_generation
            .checked_add(1)
            .ok_or(SubmitError::AttemptIdentifierExhausted)?;
        let attempt = AttemptRef {
            slot: attempt_slot,
            generation,
        };
        self.attempts[attempt_slot].state = AttemptState::Reserved { generation };
        self.outbox[outbox_slot].state = SlotState::Reserved(attempt);
        Ok((outbox_slot, attempt))
    }

    fn find_ready_slot(&self) -> Option<usize> {
        if OUTBOX == 0 {
            return None;
        }
        let mut slot_index = self.next_lease_slot;
        for _ in 0..OUTBOX {
            if matches!(self.outbox[slot_index].state, SlotState::Ready { .. }) {
                return Some(slot_index);
            }
            slot_index = Self::next_outbox_index(slot_index);
        }
        None
    }

    fn leased_entry(&self, lease: &TxLease) -> Result<(RnsPreparedData, AttemptRef), LeaseError> {
        if lease.owner_identity != self.owner_identity || lease.instance != self.instance {
            return Err(LeaseError::StaleOrUnknown);
        }
        let slot = self
            .outbox
            .get(lease.slot)
            .ok_or(LeaseError::StaleOrUnknown)?;
        match slot.state {
            SlotState::Leased {
                prepared,
                attempt,
                generation,
            } if generation == lease.generation => Ok((prepared, attempt)),
            SlotState::Free
            | SlotState::Reserved(_)
            | SlotState::Ready { .. }
            | SlotState::Leased { .. } => Err(LeaseError::StaleOrUnknown),
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

    fn attempt_matches(&self, attempt: AttemptRef, prepared: RnsPreparedData) -> bool {
        matches!(
            self.attempts.get(attempt.slot).map(|slot| slot.state),
            Some(AttemptState::Active { generation, token }
                | AttemptState::Terminal { generation, token, .. })
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

    fn reconcile_terminal_packets(&mut self) {
        for slot in &mut self.outbox {
            if let SlotState::Ready { attempt, .. } = slot.state
                && matches!(
                    self.attempts.get(attempt.slot).map(|entry| entry.state),
                    Some(AttemptState::Terminal { generation, .. })
                        if generation == attempt.generation
                )
            {
                slot.state = SlotState::Free;
            }
        }
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

    fn next_outbox_index(index: usize) -> usize {
        if index + 1 == OUTBOX { 0 } else { index + 1 }
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

    type TestNode<const PATHS: usize, const OUTBOX: usize> = NodeCore<PATHS, 2, 8, 2, OUTBOX>;

    fn identity(tag: u8) -> NodeIdentity {
        NodeIdentity::from_private_key(&[tag; 64]).unwrap()
    }

    fn node<const PATHS: usize, const OUTBOX: usize>(
        tag: u8,
        aspect: &str,
    ) -> TestNode<PATHS, OUTBOX> {
        node_with_instance(tag, aspect, tag.wrapping_add(0x80))
    }

    fn node_with_instance<const PATHS: usize, const OUTBOX: usize>(
        tag: u8,
        aspect: &str,
        instance_tag: u8,
    ) -> TestNode<PATHS, OUTBOX> {
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

    fn register_receiver<const PATHS: usize, const OUTBOX: usize>(
        sender: &mut TestNode<PATHS, OUTBOX>,
        tag: u8,
        aspect: &str,
    ) {
        sender
            .register_peer(&identity(tag), "reticulum", &[aspect], time(0))
            .unwrap();
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
    fn capacity_profile_contract_matches_rete_heapless_requirements() {
        assert!(capacity_profile_is_supported(2, 1, 1, 2));
        assert!(capacity_profile_is_supported(16, 4, 32, 4));
        for (paths, announces, deduplication, links) in [
            (0, 1, 1, 2),
            (1, 1, 1, 2),
            (3, 1, 1, 2),
            (2, 0, 1, 2),
            (2, 1, 0, 2),
            (2, 1, 1, 0),
            (2, 1, 1, 1),
            (2, 1, 1, 3),
        ] {
            assert!(!capacity_profile_is_supported(
                paths,
                announces,
                deduplication,
                links
            ));
        }
    }

    #[test]
    fn successful_enqueue_writes_a_parseable_packet_with_matching_receipt_hash() {
        let mut sender = node::<2, 1>(1, "sender");
        let receiver = node::<2, 1>(2, "receiver");
        register_receiver(&mut sender, 2, "receiver");
        let mut rng = CounterRng::default();

        let prepared = sender
            .enqueue_data(
                &receiver.destination_hash(),
                b"bounded packet",
                time(1),
                &mut rng,
            )
            .unwrap();
        assert_eq!(sender.capacities().outbox_ready, 1);
        assert_eq!(sender.capacities().receipts_used, 1);

        let lease = sender.lease_next().unwrap().unwrap();
        let frame = sender.leased_frame(&lease).unwrap();
        let parsed = reticulum_rns_rete::parse_packet(frame.bytes()).unwrap();
        assert_eq!(&parsed.compute_hash(), frame.attempt().as_bytes());
        assert_eq!(frame.attempt(), prepared.attempt());
        assert_eq!(frame.bytes().len(), usize::from(prepared.packet_len()));
    }

    #[test]
    fn full_outbox_rejects_before_entropy_or_receipt_mutation() {
        let mut sender = node::<2, 1>(3, "sender");
        let receiver = node::<2, 1>(4, "receiver");
        register_receiver(&mut sender, 4, "receiver");
        let mut rng = CounterRng::default();
        let first = sender
            .enqueue_data(&receiver.destination_hash(), b"first", time(1), &mut rng)
            .unwrap();
        let rng_before = rng.0;

        assert_eq!(
            sender.enqueue_data(&receiver.destination_hash(), b"second", time(2), &mut rng,),
            Err(SubmitError::OutboxFull { limit: 1 })
        );
        assert_eq!(rng.0, rng_before);
        assert_eq!(sender.capacities().receipts_used, 1);
        let lease = sender.lease_next().unwrap().unwrap();
        assert_eq!(
            sender.leased_frame(&lease).unwrap().attempt(),
            first.attempt()
        );
    }

    #[test]
    fn preparation_failures_release_the_reserved_slot() {
        let mut sender = node::<2, 1>(5, "sender");
        let receiver = node::<2, 1>(6, "receiver");
        let mut rng = CounterRng::default();
        let rng_before_unknown = rng.0;

        assert_eq!(
            sender.enqueue_data(
                &DestinationHash::new([0xA5; 16]),
                b"unknown",
                time(1),
                &mut rng,
            ),
            Err(SubmitError::UnknownDestination)
        );
        assert_eq!(rng.0, rng_before_unknown);
        assert_eq!(sender.capacities().outbox_used, 0);

        register_receiver(&mut sender, 6, "receiver");
        let oversized = [0x5A; MAX_DATA_PAYLOAD + 1];
        let rng_before_oversized = rng.0;
        assert_eq!(
            sender.enqueue_data(&receiver.destination_hash(), &oversized, time(2), &mut rng,),
            Err(SubmitError::PayloadTooLarge {
                actual: MAX_DATA_PAYLOAD + 1,
                maximum: MAX_DATA_PAYLOAD,
            })
        );
        assert_eq!(rng.0, rng_before_oversized);
        assert_eq!(sender.capacities().outbox_used, 0);

        sender
            .enqueue_data(&receiver.destination_hash(), b"valid", time(3), &mut rng)
            .unwrap();
        assert_eq!(sender.capacities().outbox_ready, 1);
    }

    #[test]
    fn complete_tx_frees_packet_storage_but_keeps_the_receipt_live() {
        let mut sender = node::<2, 1>(7, "sender");
        let receiver = node::<2, 1>(8, "receiver");
        register_receiver(&mut sender, 8, "receiver");
        let mut rng = CounterRng::default();
        sender
            .enqueue_data(&receiver.destination_hash(), b"released", time(1), &mut rng)
            .unwrap();
        let lease = sender.lease_next().unwrap().unwrap();
        sender.complete_tx(lease).unwrap();

        sender
            .enqueue_data(
                &receiver.destination_hash(),
                b"released again",
                time(2),
                &mut rng,
            )
            .unwrap();
        let lease = sender.lease_next().unwrap().unwrap();
        sender.complete_tx(lease).unwrap();

        assert_eq!(sender.capacities().outbox_used, 0);
        assert_eq!(sender.capacities().receipts_used, 2);
        let rng_before = rng.0;
        assert_eq!(
            sender.enqueue_data(&receiver.destination_hash(), b"blocked", time(3), &mut rng,),
            Err(SubmitError::AttemptLedgerFull { limit: 2 })
        );
        assert_eq!(rng.0, rng_before);
        assert_eq!(sender.capacities().outbox_used, 0);
    }

    #[test]
    fn abort_unsent_cancels_the_receipt_before_reusing_the_slot() {
        let mut sender = node::<2, 1>(9, "sender");
        let receiver = node::<2, 1>(10, "receiver");
        register_receiver(&mut sender, 10, "receiver");
        let mut rng = CounterRng::default();
        let first = sender
            .enqueue_data(&receiver.destination_hash(), b"abort", time(1), &mut rng)
            .unwrap();
        let lease = sender.lease_next().unwrap().unwrap();
        assert_eq!(sender.abort_unsent(lease).unwrap(), first);
        assert_eq!(sender.capacities().outbox_used, 0);
        assert_eq!(sender.capacities().receipts_used, 0);

        sender
            .enqueue_data(&receiver.destination_hash(), b"reuse", time(2), &mut rng)
            .unwrap();
        assert_eq!(sender.capacities().outbox_ready, 1);
        assert_eq!(sender.capacities().receipts_used, 1);
    }

    #[test]
    fn return_unsent_preserves_bytes_and_invalidates_the_old_lease() {
        let mut sender = node::<2, 1>(11, "sender");
        let receiver = node::<2, 1>(12, "receiver");
        register_receiver(&mut sender, 12, "receiver");
        let mut rng = CounterRng::default();
        sender
            .enqueue_data(&receiver.destination_hash(), b"retry", time(1), &mut rng)
            .unwrap();
        let first_lease = sender.lease_next().unwrap().unwrap();
        let mut first_bytes = [0u8; PACKET_CAPACITY];
        let first_len;
        let first_attempt;
        {
            let frame = sender.leased_frame(&first_lease).unwrap();
            first_len = frame.bytes().len();
            first_attempt = frame.attempt();
            first_bytes[..first_len].copy_from_slice(frame.bytes());
        }

        sender.return_unsent(first_lease).unwrap();
        assert_eq!(
            sender.leased_frame(&first_lease),
            Err(LeasedFrameError::Lease(LeaseError::StaleOrUnknown))
        );
        let second_lease = sender.lease_next().unwrap().unwrap();
        assert_ne!(first_lease, second_lease);
        let frame = sender.leased_frame(&second_lease).unwrap();
        assert_eq!(frame.bytes(), &first_bytes[..first_len]);
        assert_eq!(frame.attempt(), first_attempt);
        assert_eq!(sender.capacities().receipts_used, 1);
    }

    #[test]
    fn copied_stale_lease_cannot_touch_a_reused_slot() {
        let mut sender = node::<2, 1>(13, "sender");
        let receiver = node::<2, 1>(14, "receiver");
        register_receiver(&mut sender, 14, "receiver");
        let mut rng = CounterRng::default();
        sender
            .enqueue_data(&receiver.destination_hash(), b"first", time(1), &mut rng)
            .unwrap();
        let stale = sender.lease_next().unwrap().unwrap();
        sender.complete_tx(stale).unwrap();

        sender
            .enqueue_data(&receiver.destination_hash(), b"second", time(2), &mut rng)
            .unwrap();
        let current = sender.lease_next().unwrap().unwrap();
        assert_ne!(stale, current);
        assert_eq!(
            sender.leased_frame(&stale),
            Err(LeasedFrameError::Lease(LeaseError::StaleOrUnknown))
        );
        assert_eq!(sender.return_unsent(stale), Err(LeaseError::StaleOrUnknown));
        assert_eq!(
            sender.abort_unsent(stale),
            Err(AbortError::Lease(LeaseError::StaleOrUnknown))
        );
        assert_eq!(sender.complete_tx(stale), Err(LeaseError::StaleOrUnknown));
        assert!(sender.leased_frame(&current).is_ok());
    }

    #[test]
    fn missing_abort_receipt_retains_the_lease_as_an_invariant_failure() {
        let mut sender = node::<2, 1>(15, "sender");
        let receiver = node::<2, 1>(16, "receiver");
        register_receiver(&mut sender, 16, "receiver");
        let mut rng = CounterRng::default();
        sender
            .enqueue_data(
                &receiver.destination_hash(),
                b"invariant",
                time(1),
                &mut rng,
            )
            .unwrap();
        let lease = sender.lease_next().unwrap().unwrap();
        let native = sender.leased_entry(&lease).unwrap().0;
        let attempt = AttemptToken(*native.receipt().as_bytes());
        assert!(sender.rns.cancel_data_receipt(native.receipt()));

        assert_eq!(
            sender.abort_unsent(lease),
            Err(AbortError::ReceiptMissing { attempt })
        );
        assert_eq!(sender.capacities().outbox_leased, 1);
        assert_eq!(sender.capacities().receipts_used, 0);
        assert!(sender.leased_frame(&lease).is_ok());
    }

    #[test]
    fn zero_capacity_outbox_fails_without_touching_rns_or_entropy() {
        let mut sender = node::<2, 0>(17, "sender");
        let receiver = node::<2, 0>(18, "receiver");
        register_receiver(&mut sender, 18, "receiver");
        let mut rng = CounterRng::default();

        assert_eq!(
            sender.enqueue_data(&receiver.destination_hash(), b"no slot", time(1), &mut rng,),
            Err(SubmitError::OutboxFull { limit: 0 })
        );
        assert_eq!(rng.0, 0);
        assert_eq!(sender.capacities().receipts_used, 0);
        assert_eq!(sender.lease_next(), Ok(None));
    }

    #[test]
    fn lease_generation_exhaustion_does_not_mutate_a_ready_slot() {
        let mut sender = node::<2, 1>(19, "sender");
        let receiver = node::<2, 1>(20, "receiver");
        register_receiver(&mut sender, 20, "receiver");
        let mut rng = CounterRng::default();
        sender
            .enqueue_data(&receiver.destination_hash(), b"ready", time(1), &mut rng)
            .unwrap();
        sender.lease_generation = u64::MAX;

        assert_eq!(sender.lease_next(), Err(LeaseError::IdentifierExhausted));
        assert_eq!(sender.capacities().outbox_ready, 1);
        assert_eq!(sender.capacities().outbox_leased, 0);
        assert_eq!(sender.capacities().receipts_used, 1);
    }

    #[test]
    fn rejected_preparation_does_not_reorder_later_committed_packets() {
        let mut sender = node::<4, 2>(21, "sender");
        let receiver = node::<4, 2>(22, "receiver");
        let mut rng = CounterRng::default();

        assert_eq!(
            sender.enqueue_data(
                &DestinationHash::new([0xA5; 16]),
                b"rejected",
                time(1),
                &mut rng,
            ),
            Err(SubmitError::UnknownDestination)
        );
        register_receiver(&mut sender, 22, "receiver");
        let first = sender
            .enqueue_data(&receiver.destination_hash(), b"first", time(2), &mut rng)
            .unwrap();
        let second = sender
            .enqueue_data(&receiver.destination_hash(), b"second", time(3), &mut rng)
            .unwrap();

        let first_lease = sender.lease_next().unwrap().unwrap();
        assert_eq!(
            sender.leased_frame(&first_lease).unwrap().attempt(),
            first.attempt()
        );
        sender.complete_tx(first_lease).unwrap();
        let second_lease = sender.lease_next().unwrap().unwrap();
        assert_eq!(
            sender.leased_frame(&second_lease).unwrap().attempt(),
            second.attempt()
        );
    }

    #[test]
    fn lease_from_another_owner_incarnation_is_always_rejected() {
        let receiver = node::<2, 1>(24, "receiver");
        let mut first = node_with_instance::<2, 1>(23, "sender", 1);
        let mut reconstructed = node_with_instance::<2, 1>(23, "sender", 2);
        register_receiver(&mut first, 24, "receiver");
        register_receiver(&mut reconstructed, 24, "receiver");
        let mut first_rng = CounterRng::default();
        let mut reconstructed_rng = CounterRng::default();
        first
            .enqueue_data(
                &receiver.destination_hash(),
                b"same packet",
                time(1),
                &mut first_rng,
            )
            .unwrap();
        reconstructed
            .enqueue_data(
                &receiver.destination_hash(),
                b"same packet",
                time(1),
                &mut reconstructed_rng,
            )
            .unwrap();
        let old_lease = first.lease_next().unwrap().unwrap();
        let current_lease = reconstructed.lease_next().unwrap().unwrap();

        assert_eq!(
            reconstructed.leased_frame(&old_lease),
            Err(LeasedFrameError::Lease(LeaseError::StaleOrUnknown))
        );
        assert_eq!(
            reconstructed.complete_tx(old_lease),
            Err(LeaseError::StaleOrUnknown)
        );
        assert_eq!(
            reconstructed.abort_unsent(old_lease),
            Err(AbortError::Lease(LeaseError::StaleOrUnknown))
        );
        assert!(reconstructed.leased_frame(&current_lease).is_ok());
        assert!(first.leased_frame(&old_lease).is_ok());
    }

    #[test]
    fn receipt_hash_collision_rolls_back_only_the_second_reserved_slot() {
        let mut sender = node::<2, 2>(25, "sender");
        let receiver = node::<2, 2>(26, "receiver");
        register_receiver(&mut sender, 26, "receiver");
        let mut first_rng = CounterRng::default();
        let first = sender
            .enqueue_data(
                &receiver.destination_hash(),
                b"repeat entropy",
                time(1),
                &mut first_rng,
            )
            .unwrap();
        let mut repeated_rng = CounterRng::default();

        assert_eq!(
            sender.enqueue_data(
                &receiver.destination_hash(),
                b"repeat entropy",
                time(1),
                &mut repeated_rng,
            ),
            Err(SubmitError::ReceiptHashAlreadyTracked)
        );
        assert!(repeated_rng.0 > 0);
        assert_eq!(sender.capacities().outbox_ready, 1);
        assert_eq!(sender.capacities().outbox_used, 1);
        assert_eq!(sender.capacities().receipts_used, 1);
        let lease = sender.lease_next().unwrap().unwrap();
        assert_eq!(
            sender.leased_frame(&lease).unwrap().attempt(),
            first.attempt()
        );
    }

    #[test]
    fn valid_proof_commits_exact_terminal_and_reconciles_a_ready_packet() {
        let mut sender = node::<2, 1>(31, "sender");
        let receiver = node::<2, 1>(32, "receiver");
        register_receiver(&mut sender, 32, "receiver");
        let mut rng = CounterRng::default();
        let prepared = sender
            .enqueue_data(
                &receiver.destination_hash(),
                b"proof terminal",
                time(1),
                &mut rng,
            )
            .unwrap();
        let proof = proof_for(32, prepared.attempt());

        sender
            .ingest(&proof, time(3), InterfaceId(2), &mut rng)
            .unwrap();

        let terminal = sender.terminal_attempts().next().unwrap();
        assert_eq!(terminal.handle(), prepared.handle());
        assert_eq!(terminal.token(), prepared.attempt());
        assert_eq!(terminal.outcome(), AttemptOutcome::Delivered);
        assert_eq!(sender.terminal_attempts().count(), 1);
        assert_eq!(sender.capacities().outbox_used, 0);
        assert_eq!(sender.capacities().receipts_used, 0);
        assert_eq!(sender.capacities().attempts_active, 0);
        assert_eq!(sender.capacities().attempts_terminal, 1);
        assert_eq!(sender.lease_next(), Ok(None));
        assert_eq!(sender.acknowledge_terminal(prepared.handle()), Ok(terminal));
        assert_eq!(sender.capacities().attempts_used, 0);
    }

    #[test]
    fn invalid_proof_releases_its_reservation_and_the_valid_proof_can_retry() {
        let mut sender = node::<2, 1>(33, "sender");
        let receiver = node::<2, 1>(34, "receiver");
        register_receiver(&mut sender, 34, "receiver");
        let mut rng = CounterRng::default();
        let prepared = sender
            .enqueue_data(
                &receiver.destination_hash(),
                b"proof retry",
                time(10),
                &mut rng,
            )
            .unwrap();
        let proof = proof_for(34, prepared.attempt());
        let mut invalid = proof.clone();
        *invalid.last_mut().unwrap() ^= 0xff;

        sender
            .ingest(&invalid, time(12), InterfaceId(2), &mut rng)
            .unwrap();
        assert_eq!(sender.capacities().attempts_active, 1);
        assert_eq!(sender.capacities().attempts_terminal, 0);
        assert_eq!(sender.capacities().receipts_used, 1);
        assert!(sender.terminal_attempts().next().is_none());
        assert_eq!(
            sender.acknowledge_terminal(prepared.handle()),
            Err(AcknowledgeError::NotTerminal)
        );

        sender
            .ingest(&proof, time(13), InterfaceId(2), &mut rng)
            .unwrap();
        assert_eq!(
            sender.terminal_attempts().next().unwrap().outcome(),
            AttemptOutcome::Delivered
        );
    }

    #[test]
    fn timeout_tombstone_backpressures_then_acknowledgement_allows_reuse() {
        let mut sender = node::<2, 1>(35, "sender");
        let receiver = node::<2, 1>(36, "receiver");
        register_receiver(&mut sender, 36, "receiver");
        let mut rng = CounterRng::default();
        let first = sender
            .enqueue_data(
                &receiver.destination_hash(),
                b"eventual timeout",
                time(100),
                &mut rng,
            )
            .unwrap();

        let report = sender.tick(time(132), &mut rng);
        assert_eq!(report.timed_out_attempts, 1);
        assert_eq!(report.correlation_fault, None);
        let terminal = sender.terminal_attempts().next().unwrap();
        assert_eq!(terminal.handle(), first.handle());
        assert_eq!(terminal.outcome(), AttemptOutcome::DeliveryTimeout);
        assert_eq!(sender.capacities().outbox_used, 0);
        assert_eq!(sender.capacities().attempts_terminal, 1);

        let second = sender
            .enqueue_data(
                &receiver.destination_hash(),
                b"second tombstone",
                time(133),
                &mut rng,
            )
            .unwrap();
        let second_tick = sender.tick(time(165), &mut rng);
        assert_eq!(second_tick.timed_out_attempts, 1);
        assert_eq!(sender.capacities().attempts_terminal, 2);

        let rng_before = rng.0;
        assert_eq!(
            sender.enqueue_data(
                &receiver.destination_hash(),
                b"blocked by tombstone",
                time(166),
                &mut rng,
            ),
            Err(SubmitError::AttemptLedgerFull { limit: 2 })
        );
        assert_eq!(rng.0, rng_before);

        sender.acknowledge_terminal(first.handle()).unwrap();
        let third = sender
            .enqueue_data(
                &receiver.destination_hash(),
                b"reused capacity",
                time(167),
                &mut rng,
            )
            .unwrap();
        assert_ne!(first.handle(), third.handle());
        assert_ne!(second.handle(), third.handle());
        assert_eq!(
            sender.acknowledge_terminal(first.handle()),
            Err(AcknowledgeError::StaleOrUnknown)
        );
    }

    #[test]
    fn timeout_while_leased_blocks_bytes_until_the_actor_returns() {
        let mut sender = node::<2, 1>(59, "sender");
        let receiver = node::<2, 1>(60, "receiver");
        register_receiver(&mut sender, 60, "receiver");
        let mut rng = CounterRng::default();
        let prepared = sender
            .enqueue_data(
                &receiver.destination_hash(),
                b"leased timeout",
                time(100),
                &mut rng,
            )
            .unwrap();
        let lease = sender.lease_next().unwrap().unwrap();

        let report = sender.tick(time(132), &mut rng);
        assert_eq!(report.timed_out_attempts, 1);
        assert_eq!(report.correlation_fault, None);
        let terminal = sender.terminal_attempts().next().unwrap();
        assert_eq!(terminal.outcome(), AttemptOutcome::DeliveryTimeout);
        assert_eq!(
            sender.leased_frame(&lease),
            Err(LeasedFrameError::AttemptTerminal(terminal))
        );
        assert_eq!(
            sender.acknowledge_terminal(prepared.handle()),
            Err(AcknowledgeError::StillLeased)
        );

        sender.return_unsent(lease).unwrap();
        assert_eq!(sender.capacities().outbox_used, 0);
        assert_eq!(sender.acknowledge_terminal(prepared.handle()), Ok(terminal));
    }

    #[test]
    fn repeated_full_hash_selects_the_new_active_attempt_over_an_old_tombstone() {
        let mut sender = node::<2, 1>(61, "sender");
        let receiver = node::<2, 1>(62, "receiver");
        register_receiver(&mut sender, 62, "receiver");
        let mut first_rng = CounterRng::default();
        let first = sender
            .enqueue_data(
                &receiver.destination_hash(),
                b"repeat after delivery",
                time(1),
                &mut first_rng,
            )
            .unwrap();
        let first_proof = proof_for(62, first.attempt());
        sender
            .ingest(&first_proof, time(2), InterfaceId(2), &mut first_rng)
            .unwrap();

        let mut repeated_rng = CounterRng::default();
        let repeated = sender
            .enqueue_data(
                &receiver.destination_hash(),
                b"repeat after delivery",
                time(1),
                &mut repeated_rng,
            )
            .unwrap();
        assert_eq!(repeated.attempt(), first.attempt());
        assert_ne!(repeated.handle(), first.handle());

        let repeated_proof = implicit_proof_for(62, repeated.attempt());
        sender
            .ingest(&repeated_proof, time(3), InterfaceId(2), &mut repeated_rng)
            .unwrap();
        let terminals = sender.terminal_attempts().collect::<std::vec::Vec<_>>();
        assert_eq!(terminals.len(), 2);
        assert!(
            terminals
                .iter()
                .any(|terminal| terminal.handle() == first.handle())
        );
        assert!(
            terminals
                .iter()
                .any(|terminal| terminal.handle() == repeated.handle())
        );
        assert!(
            terminals
                .iter()
                .all(|terminal| terminal.outcome() == AttemptOutcome::Delivered)
        );
    }

    #[test]
    fn proof_while_leased_blocks_bytes_and_return_discards_the_packet() {
        let mut sender = node::<2, 1>(37, "sender");
        let receiver = node::<2, 1>(38, "receiver");
        register_receiver(&mut sender, 38, "receiver");
        let mut rng = CounterRng::default();
        let prepared = sender
            .enqueue_data(
                &receiver.destination_hash(),
                b"leased proof",
                time(1),
                &mut rng,
            )
            .unwrap();
        let proof = proof_for(38, prepared.attempt());
        let lease = sender.lease_next().unwrap().unwrap();

        sender
            .ingest(&proof, time(3), InterfaceId(2), &mut rng)
            .unwrap();
        let terminal = sender.terminal_attempts().next().unwrap();
        assert_eq!(
            sender.leased_frame(&lease),
            Err(LeasedFrameError::AttemptTerminal(terminal))
        );
        assert_eq!(
            sender.acknowledge_terminal(prepared.handle()),
            Err(AcknowledgeError::StillLeased)
        );

        sender.return_unsent(lease).unwrap();
        assert_eq!(sender.capacities().outbox_used, 0);
        assert_eq!(sender.capacities().attempts_terminal, 1);
        sender.acknowledge_terminal(prepared.handle()).unwrap();
    }

    #[test]
    fn proof_while_leased_allows_complete_without_losing_the_tombstone() {
        let mut sender = node::<2, 1>(39, "sender");
        let receiver = node::<2, 1>(40, "receiver");
        register_receiver(&mut sender, 40, "receiver");
        let mut rng = CounterRng::default();
        let prepared = sender
            .enqueue_data(
                &receiver.destination_hash(),
                b"complete race",
                time(1),
                &mut rng,
            )
            .unwrap();
        let proof = proof_for(40, prepared.attempt());
        let lease = sender.lease_next().unwrap().unwrap();
        sender
            .ingest(&proof, time(3), InterfaceId(2), &mut rng)
            .unwrap();

        assert_eq!(sender.complete_tx(lease).unwrap(), prepared);
        assert_eq!(sender.capacities().outbox_used, 0);
        assert_eq!(sender.capacities().attempts_terminal, 1);
        sender.acknowledge_terminal(prepared.handle()).unwrap();
    }

    #[test]
    fn proof_while_leased_allows_abort_without_cancelling_terminal_state() {
        let mut sender = node::<2, 1>(41, "sender");
        let receiver = node::<2, 1>(42, "receiver");
        register_receiver(&mut sender, 42, "receiver");
        let mut rng = CounterRng::default();
        let prepared = sender
            .enqueue_data(
                &receiver.destination_hash(),
                b"abort race",
                time(1),
                &mut rng,
            )
            .unwrap();
        let proof = proof_for(42, prepared.attempt());
        let lease = sender.lease_next().unwrap().unwrap();
        sender
            .ingest(&proof, time(3), InterfaceId(2), &mut rng)
            .unwrap();

        assert_eq!(sender.abort_unsent(lease).unwrap(), prepared);
        assert_eq!(sender.capacities().receipts_used, 0);
        assert_eq!(sender.capacities().attempts_terminal, 1);
        sender.acknowledge_terminal(prepared.handle()).unwrap();
    }

    #[test]
    fn completion_before_proof_frees_bytes_but_leaves_the_attempt_active() {
        let mut sender = node::<2, 1>(43, "sender");
        let receiver = node::<2, 1>(44, "receiver");
        register_receiver(&mut sender, 44, "receiver");
        let mut rng = CounterRng::default();
        let prepared = sender
            .enqueue_data(
                &receiver.destination_hash(),
                b"complete first",
                time(1),
                &mut rng,
            )
            .unwrap();
        let lease = sender.lease_next().unwrap().unwrap();
        assert_eq!(sender.complete_tx(lease).unwrap(), prepared);
        assert_eq!(sender.capacities().outbox_used, 0);
        assert_eq!(sender.capacities().attempts_active, 1);

        let proof = proof_for(44, prepared.attempt());
        sender
            .ingest(&proof, time(3), InterfaceId(2), &mut rng)
            .unwrap();
        assert_eq!(sender.capacities().attempts_terminal, 1);
        assert_eq!(
            sender.terminal_attempts().next().unwrap().handle(),
            prepared.handle()
        );
    }

    #[test]
    fn attempt_generation_exhaustion_rejects_before_any_mutation() {
        let mut sender = node::<2, 1>(45, "sender");
        let receiver = node::<2, 1>(46, "receiver");
        register_receiver(&mut sender, 46, "receiver");
        sender.attempt_generation = u64::MAX;
        let mut rng = CounterRng::default();

        assert_eq!(
            sender.enqueue_data(
                &receiver.destination_hash(),
                b"no identifier",
                time(1),
                &mut rng,
            ),
            Err(SubmitError::AttemptIdentifierExhausted)
        );
        assert_eq!(rng.0, 0);
        assert_eq!(sender.capacities().outbox_used, 0);
        assert_eq!(sender.capacities().attempts_used, 0);
        assert_eq!(sender.capacities().receipts_used, 0);
        assert_eq!(sender.next_reserve, 0);
        assert_eq!(sender.next_attempt, 0);
    }

    #[test]
    fn attempt_handles_are_scoped_to_incarnation_and_invalidated_by_abort() {
        let receiver = node::<2, 1>(48, "receiver");
        let mut first = node_with_instance::<2, 1>(47, "sender", 1);
        let mut reconstructed = node_with_instance::<2, 1>(47, "sender", 2);
        register_receiver(&mut first, 48, "receiver");
        register_receiver(&mut reconstructed, 48, "receiver");
        let mut rng = CounterRng::default();
        let prepared = first
            .enqueue_data(
                &receiver.destination_hash(),
                b"scoped handle",
                time(1),
                &mut rng,
            )
            .unwrap();

        assert_eq!(
            first.acknowledge_terminal(prepared.handle()),
            Err(AcknowledgeError::NotTerminal)
        );
        assert_eq!(
            reconstructed.acknowledge_terminal(prepared.handle()),
            Err(AcknowledgeError::StaleOrUnknown)
        );
        let lease = first.lease_next().unwrap().unwrap();
        first.abort_unsent(lease).unwrap();
        assert_eq!(
            first.acknowledge_terminal(prepared.handle()),
            Err(AcknowledgeError::StaleOrUnknown)
        );
    }

    #[test]
    fn unknown_native_receipt_is_an_explicit_ingress_correlation_fault() {
        let mut sender = node::<2, 1>(49, "sender");
        let receiver = node::<2, 1>(50, "receiver");
        register_receiver(&mut sender, 50, "receiver");
        let mut rng = CounterRng::default();
        let prepared = sender
            .enqueue_data(
                &receiver.destination_hash(),
                b"orphan native receipt",
                time(1),
                &mut rng,
            )
            .unwrap();
        sender.attempts[prepared.handle().slot].state = AttemptState::Free;
        let proof = proof_for(50, prepared.attempt());

        assert_eq!(
            sender
                .ingest(&proof, time(2), InterfaceId(2), &mut rng)
                .unwrap_err(),
            ReceiptCorrelationError::UnknownData {
                attempt: prepared.attempt(),
            }
        );
        assert_eq!(sender.capacities().receipts_used, 1);
    }

    #[test]
    fn already_terminal_native_receipt_is_not_reported_as_capacity_backpressure() {
        let mut sender = node::<2, 1>(51, "sender");
        let receiver = node::<2, 1>(52, "receiver");
        register_receiver(&mut sender, 52, "receiver");
        let mut rng = CounterRng::default();
        let prepared = sender
            .enqueue_data(
                &receiver.destination_hash(),
                b"premature tombstone",
                time(1),
                &mut rng,
            )
            .unwrap();
        sender.attempts[prepared.handle().slot].state = AttemptState::Terminal {
            generation: prepared.handle().generation,
            token: prepared.attempt(),
            outcome: AttemptOutcome::Delivered,
        };
        let proof = proof_for(52, prepared.attempt());

        assert_eq!(
            sender
                .ingest(&proof, time(2), InterfaceId(2), &mut rng)
                .unwrap_err(),
            ReceiptCorrelationError::AlreadyTerminal {
                attempt: prepared.attempt(),
            }
        );
        assert_eq!(sender.capacities().receipts_used, 1);
    }

    #[test]
    fn unknown_timeout_is_reported_without_dropping_the_tick_report() {
        let mut sender = node::<2, 1>(53, "sender");
        let receiver = node::<2, 1>(54, "receiver");
        register_receiver(&mut sender, 54, "receiver");
        let mut rng = CounterRng::default();
        let prepared = sender
            .enqueue_data(
                &receiver.destination_hash(),
                b"orphan timeout",
                time(100),
                &mut rng,
            )
            .unwrap();
        sender.attempts[prepared.handle().slot].state = AttemptState::Free;

        let report = sender.tick(time(132), &mut rng);
        assert_eq!(report.timed_out_attempts, 0);
        assert_eq!(
            report.correlation_fault,
            Some(ReceiptCorrelationError::UnknownData {
                attempt: prepared.attempt(),
            })
        );
        assert_eq!(report.actions.events.len(), 1);
        assert!(report.actions.packets.is_empty());
        assert_eq!(sender.capacities().receipts_used, 1);
    }

    #[test]
    fn full_hash_candidate_selects_the_exact_active_attempt() {
        let mut sender = node::<2, 1>(55, "sender");
        let receiver = node::<2, 1>(56, "receiver");
        register_receiver(&mut sender, 56, "receiver");
        let mut rng = CounterRng::default();
        let prepared = sender
            .enqueue_data(
                &receiver.destination_hash(),
                b"full hash selection",
                time(1),
                &mut rng,
            )
            .unwrap();
        assert_eq!(prepared.handle().slot, 0);
        let generation = prepared.handle().generation;
        let mut colliding = prepared.attempt();
        colliding.0[31] ^= 0xff;
        sender.attempts[0].state = AttemptState::Active {
            generation: generation + 1,
            token: colliding,
        };
        sender.attempts[1].state = AttemptState::Active {
            generation,
            token: prepared.attempt(),
        };
        let exact_ref = AttemptRef {
            slot: 1,
            generation,
        };
        let SlotState::Ready {
            prepared: native, ..
        } = sender.outbox[0].state
        else {
            panic!("expected ready packet")
        };
        sender.outbox[0].state = SlotState::Ready {
            prepared: native,
            attempt: exact_ref,
        };

        let proof = proof_for(56, prepared.attempt());
        sender
            .ingest(&proof, time(2), InterfaceId(2), &mut rng)
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
            } if token == prepared.attempt()
        ));
        assert_eq!(sender.capacities().outbox_used, 0);
    }

    #[test]
    fn channel_receipt_candidate_is_an_explicit_unsupported_fault() {
        let mut initiator = node::<4, 2>(57, "initiator");
        let mut responder = node::<4, 2>(58, "responder");
        register_receiver(&mut initiator, 58, "responder");
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
            .ingest(request.bytes(), 100, InterfaceId(1), &mut rng);
        let established = initiator.rns.ingest(
            response.actions.packets[0].bytes(),
            101,
            InterfaceId(2),
            &mut rng,
        );
        responder.rns.ingest(
            established.actions.packets[0].bytes(),
            102,
            InterfaceId(1),
            &mut rng,
        );
        let message = initiator
            .rns
            .send_channel_message(&link_id, 7, b"unsupported channel", 110, &mut rng)
            .unwrap();
        let proof = responder
            .rns
            .ingest(message.bytes(), 110, InterfaceId(1), &mut rng)
            .actions
            .packets
            .into_iter()
            .next()
            .unwrap();

        assert_eq!(
            initiator
                .ingest(proof.bytes(), time(111), InterfaceId(2), &mut rng)
                .unwrap_err(),
            ReceiptCorrelationError::UnsupportedChannel
        );
        assert_eq!(initiator.rns.metrics().capacity.channel_receipts.used, 1);
    }
}
