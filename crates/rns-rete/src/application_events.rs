//! Fixed-capacity ownership for transport-neutral application events.
//!
//! The owner moves complete application events and any privately attached proof
//! out of one [`NodeActions`] envelope only after every event has a
//! caller-provided slot and the proof's event index is valid. Consumers lease
//! one event at a time in FIFO order. Resolving a lease transfers, discards, or
//! quarantines its exact event and proof; dropping an unresolved lease
//! quarantines both instead of silently releasing their slot. A consumer may
//! also retain an opaque backing-slot retry token so backoff can reacquire the
//! exact quarantine without treating a scalar event ID as authority.

use core::{fmt, marker::PhantomData, mem, ptr};

use crate::delayed_proofs::{
    DelayedProofId, DelayedProofOwner, DelayedProofReservation, DelayedProofReservationError,
    DelayedProofSequence,
};
use crate::embedded::RetainedInboundProof;
use crate::{ApplicationEvent, ApplicationEventKind, NodeActions};

const fn event_kind_accepts_retained_proof(kind: ApplicationEventKind) -> bool {
    matches!(
        kind,
        ApplicationEventKind::DataReceived | ApplicationEventKind::LinkData
    )
}

/// Stable index of one caller-provided application-event slot.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ApplicationEventSlotId(usize);

impl ApplicationEventSlotId {
    /// Zero-based index in the slot slice supplied to the owner.
    pub const fn get(self) -> usize {
        self.0
    }
}

/// Incarnation of an event occupying one stable slot.
///
/// A slot receives a strictly newer generation each time it is reused. Slots
/// whose generation reaches `u64::MAX` retire instead of wrapping to a stale
/// identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ApplicationEventGeneration(u64);

impl ApplicationEventGeneration {
    /// Nonzero scalar generation.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// FIFO sequence assigned when an event enters the owner.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ApplicationEventSequence(u64);

impl ApplicationEventSequence {
    /// Nonzero scalar sequence.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Complete generation-safe identity of one owned application event.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ApplicationEventId {
    slot: ApplicationEventSlotId,
    generation: ApplicationEventGeneration,
}

impl ApplicationEventId {
    /// Stable caller-provided slot holding this event.
    pub const fn slot(self) -> ApplicationEventSlotId {
        self.slot
    }

    /// Incarnation assigned when the slot was most recently filled.
    pub const fn generation(self) -> ApplicationEventGeneration {
        self.generation
    }
}

/// Why an event remains owned outside the normal ready queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplicationEventQuarantineReason {
    /// A consumer explicitly deferred the event for later inspection.
    ConsumerDeferred,
    /// A consumer reported a fault while inspecting the event.
    ConsumerFault,
    /// A lease left scope without an explicit disposition.
    UnresolvedLeaseDropped,
}

/// Why an exact retry token did not reacquire its quarantined event.
///
/// These values are diagnostics only. The unchanged opaque
/// [`ApplicationEventRetryToken`] returned by [`ApplicationEventRetryFailure`]
/// remains the structural retry authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplicationEventRetryError {
    /// The token's backing slot is not at the same stable index in this
    /// owner, even if this owner happens to contain an equal scalar event ID.
    ForeignOwner,
    /// The backing slot has since been reused for a newer event incarnation.
    StaleGeneration {
        /// Generation named by the retry token.
        token: ApplicationEventGeneration,
        /// Generation currently occupying the backing slot.
        current: ApplicationEventGeneration,
    },
    /// The event incarnation no longer has the FIFO sequence captured by the
    /// retry token.
    StaleSequence {
        /// FIFO sequence named by the retry token.
        token: ApplicationEventSequence,
        /// FIFO sequence currently attached to the backing slot.
        current: ApplicationEventSequence,
    },
    /// The exact event is no longer quarantined.
    NotQuarantined,
    /// Another recovery path changed why the exact event is quarantined.
    QuarantineChanged {
        /// Reason captured when the retry token was created.
        token: ApplicationEventQuarantineReason,
        /// Current quarantine reason on the backing slot.
        current: ApplicationEventQuarantineReason,
    },
}

/// Opaque structural authority to retry one exact quarantined event.
///
/// A token is created only by
/// [`ApplicationEventLease::quarantine_for_retry`]. Its private backing-slot
/// provenance disambiguates independent owners whose scalar
/// [`ApplicationEventId`] values happen to be equal. The token is deliberately
/// neither `Copy` nor `Clone`; retry consumes it, and a rejected retry returns
/// the same token unchanged. It owns no allocation and never dereferences its
/// private provenance address.
///
/// The storage lifetime prevents the token from outliving and subsequently
/// authorizing unrelated storage that happens to reuse the same address.
/// Keeping a token does not borrow the [`ApplicationEventOwner`], so the owner
/// may process other slots while product policy waits for retry backoff.
///
/// ```compile_fail
/// use reticulum_rns_rete::ApplicationEventRetryToken;
///
/// fn duplicate(token: ApplicationEventRetryToken<'_>) {
///     let _second_authority = token.clone();
/// }
/// ```
#[must_use = "a retry token retains exact structural authority for its deferred retry"]
pub struct ApplicationEventRetryToken<'slots> {
    slot_address: usize,
    id: ApplicationEventId,
    sequence: ApplicationEventSequence,
    reason: ApplicationEventQuarantineReason,
    storage: PhantomData<&'slots ()>,
}

impl ApplicationEventRetryToken<'_> {
    /// Scalar event identity for diagnostics and backoff bookkeeping.
    ///
    /// This value is not retry authority; callers must return this complete
    /// token to [`ApplicationEventOwner::try_reacquire_quarantined`].
    pub const fn event_id(&self) -> ApplicationEventId {
        self.id
    }

    /// Original FIFO sequence for diagnostics.
    pub const fn sequence(&self) -> ApplicationEventSequence {
        self.sequence
    }

    /// Reason supplied when the exact event was deferred for retry.
    pub const fn reason(&self) -> ApplicationEventQuarantineReason {
        self.reason
    }
}

impl fmt::Debug for ApplicationEventRetryToken<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApplicationEventRetryToken")
            .field("event_id", &self.id)
            .field("sequence", &self.sequence)
            .field("reason", &self.reason)
            .field("slot_provenance", &"<redacted>")
            .finish()
    }
}

/// Rejected exact retry preserving its opaque authority unchanged.
#[must_use = "a rejected retry still owns the exact retry authority"]
pub struct ApplicationEventRetryFailure<'slots> {
    reason: ApplicationEventRetryError,
    token: ApplicationEventRetryToken<'slots>,
}

impl<'slots> ApplicationEventRetryFailure<'slots> {
    /// Typed reason the event was not reacquired.
    pub const fn reason(&self) -> ApplicationEventRetryError {
        self.reason
    }

    /// Recover the unchanged structural authority for inspection or another
    /// retry against its original owner.
    pub fn into_token(self) -> ApplicationEventRetryToken<'slots> {
        self.token
    }
}

impl fmt::Debug for ApplicationEventRetryFailure<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApplicationEventRetryFailure")
            .field("reason", &self.reason)
            .field("token", &self.token)
            .finish()
    }
}

/// Product-owned reason an application event was intentionally destroyed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplicationEventDiscardReason {
    /// The intended application consumer is not enabled on this product.
    ConsumerUnavailable,
    /// Downstream capacity policy explicitly chose loss instead of deferral.
    DownstreamCapacity,
    /// The consumer determined that the event payload is invalid.
    InvalidPayload,
    /// Newer state made this event obsolete.
    Superseded,
    /// Periodic maintenance state was intentionally coalesced.
    MaintenanceCoalesced,
    /// Product policy rejected delivery.
    PolicyRejected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ApplicationEventSlotState {
    Vacant,
    Ready,
    Leased,
    Quarantined(ApplicationEventQuarantineReason),
    Retired,
}

struct OwnedApplicationEvent {
    event: ApplicationEvent,
    retained_proof: Option<RetainedInboundProof>,
}

impl OwnedApplicationEvent {
    fn event(&self) -> &ApplicationEvent {
        &self.event
    }

    fn has_retained_proof(&self) -> bool {
        self.retained_proof.is_some()
    }
}

/// Caller-owned storage for one transport-neutral event and optional retained
/// proof.
///
/// Construct slots in board or task-owned storage, then pass their mutable
/// slice to [`ApplicationEventOwner::new`]. Slot contents survive dropping and
/// reconstructing the owner over the same slice.
#[must_use = "an application-event slot must remain owned for as long as its owner may use it"]
pub struct ApplicationEventSlot {
    generation: u64,
    sequence: u64,
    state: ApplicationEventSlotState,
    owned: Option<OwnedApplicationEvent>,
}

impl ApplicationEventSlot {
    /// Construct one empty reusable slot.
    pub const fn new() -> Self {
        Self {
            generation: 0,
            sequence: 0,
            state: ApplicationEventSlotState::Vacant,
            owned: None,
        }
    }
}

impl Default for ApplicationEventSlot {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for ApplicationEventSlot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApplicationEventSlot")
            .field("generation", &self.generation)
            .field("sequence", &self.sequence)
            .field("state", &self.state)
            .field("event", &self.owned.as_ref().map(|_| "<redacted>"))
            .field(
                "retained_proof",
                &self
                    .owned
                    .as_ref()
                    .is_some_and(OwnedApplicationEvent::has_retained_proof),
            )
            .finish()
    }
}

/// Scalar occupancy of one application-event owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApplicationEventCapacitySnapshot {
    /// Total caller-provided slots, including permanently retired slots.
    pub capacity: usize,
    /// Slots available for a new event.
    pub vacant: usize,
    /// Events waiting in normal FIFO order.
    pub ready: usize,
    /// Event currently borrowed by a lease. This is observable only through a
    /// lease's owner borrow and is normally zero at external call boundaries.
    pub leased: usize,
    /// Events retained for explicit recovery or disposition.
    pub quarantined: usize,
    /// Slots retired to prevent generation wraparound.
    pub retired: usize,
}

impl ApplicationEventCapacitySnapshot {
    /// Maximum events that can ever be present without reusing a retired slot.
    pub const fn reusable_capacity(self) -> usize {
        self.capacity - self.retired
    }

    /// Events currently retained in ready, leased, or quarantined states.
    pub const fn occupied(self) -> usize {
        self.ready + self.leased + self.quarantined
    }
}

/// Cumulative scalar observations for one owner incarnation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ApplicationEventOwnerCounters {
    /// Whole action envelopes offered to the owner.
    pub offered_batches: u64,
    /// Whole action envelopes accepted atomically.
    pub accepted_batches: u64,
    /// Events moved into caller-owned slots.
    pub accepted_events: u64,
    /// Retained proofs moved into the same slots as their events.
    pub accepted_retained_proofs: u64,
    /// Events transferred to consumers by acknowledgement.
    pub acknowledged_events: u64,
    /// Proof-bearing events rejected by the proofless acknowledgement path.
    pub retained_proof_acknowledgement_rejections: u64,
    /// Proof-bearing events atomically acknowledged into delayed-proof slots.
    pub retained_proofs_queued: u64,
    /// Events explicitly discarded by consumers.
    pub discarded_events: u64,
    /// Discards because the intended consumer was unavailable.
    pub consumer_unavailable_discards: u64,
    /// Discards chosen by explicit downstream-capacity policy.
    pub downstream_capacity_discards: u64,
    /// Discards of payloads classified as invalid.
    pub invalid_payload_discards: u64,
    /// Discards of events superseded by newer state.
    pub superseded_discards: u64,
    /// Discards caused by intentional maintenance-event coalescing.
    pub maintenance_coalesced_discards: u64,
    /// Discards caused by product delivery policy.
    pub policy_rejected_discards: u64,
    /// Explicit consumer quarantine dispositions.
    pub explicit_quarantines: u64,
    /// Explicit quarantine dispositions that returned exact retry authority.
    pub retry_quarantines: u64,
    /// Exact quarantined events reacquired through their structural token.
    pub retry_reacquisitions: u64,
    /// Exact retry tokens rejected without mutating any event slot or token.
    pub retry_reacquisition_rejections: u64,
    /// Unresolved leases converted to quarantine dispositions on drop.
    pub unresolved_lease_quarantines: u64,
    /// Offers rejected because current occupancy left too few slots.
    pub full_rejections: u64,
    /// Offers rejected because the atomic batch can never fit.
    pub oversized_batch_rejections: u64,
    /// Offers rejected because they unexpectedly retained packet owners.
    pub packet_owner_rejections: u64,
    /// Offers rejected because a retained proof named no event in its envelope.
    pub retained_proof_index_rejections: u64,
    /// Offers rejected because a retained proof named an incompatible event.
    pub retained_proof_kind_rejections: u64,
    /// Offers rejected because a FIFO sequence could not be assigned safely.
    pub sequence_exhaustion_rejections: u64,
    /// Source-dependent packets already classified as unroutable upstream.
    pub unroutable_packets_observed: u64,
}

/// Why a complete action envelope did not enter an application-event owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplicationEventOfferError {
    /// Current occupancy leaves insufficient slots, but the batch can fit
    /// after consumers resolve existing events.
    Full {
        /// Events that must enter atomically.
        required: usize,
        /// Slots currently vacant.
        available: usize,
    },
    /// The batch exceeds the owner's reusable slot capacity and cannot succeed
    /// merely by retrying.
    BatchTooLarge {
        /// Events that must enter atomically.
        required: usize,
        /// Maximum events this owner can retain at once.
        capacity: usize,
    },
    /// This event-only boundary received ordinary packet owners.
    PacketOwnersPresent {
        /// Unexpected packet-owner count.
        count: usize,
    },
    /// A privately retained proof names no event in its action envelope.
    RetainedProofIndexOutOfRange {
        /// Invalid zero-based event index.
        event_index: usize,
        /// Events present in the envelope.
        event_count: usize,
    },
    /// A retained application delivery proof names an event of another kind.
    RetainedProofEventKindMismatch {
        /// Zero-based index of the incompatible event.
        event_index: usize,
        /// Payload-free kind of the incompatible event.
        event_kind: ApplicationEventKind,
    },
    /// Existing FIFO state prevents assigning another non-wrapping sequence.
    SequenceExhausted,
}

impl ApplicationEventOfferError {
    /// Whether resolving existing event leases can make this same offer
    /// admissible without changing its contents or replacing owner storage.
    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::Full { .. } | Self::SequenceExhausted)
    }
}

/// Rejected atomic offer retaining the exact [`NodeActions`] owner unchanged.
#[must_use = "a rejected application-event envelope remains exactly owned"]
pub struct ApplicationEventOfferFailure {
    reason: ApplicationEventOfferError,
    actions: NodeActions,
}

impl ApplicationEventOfferFailure {
    /// Scalar rejection reason.
    pub const fn reason(&self) -> ApplicationEventOfferError {
        self.reason
    }

    /// Recover the exact unmodified action envelope.
    pub fn into_actions(self) -> NodeActions {
        self.actions
    }
}

impl fmt::Debug for ApplicationEventOfferFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApplicationEventOfferFailure")
            .field("reason", &self.reason)
            .field("event_count", &self.actions.events.len())
            .field("retained_proof_count", &self.actions.retained_proof_count())
            .field("packet_count", &self.actions.packets.len())
            .field("unroutable_packets", &self.actions.unroutable_packets)
            .field("actions", &"<redacted>")
            .finish()
    }
}

/// Scalar report for one atomically accepted action envelope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApplicationEventOfferReport {
    accepted_events: usize,
    accepted_retained_proofs: usize,
    unroutable_packets: usize,
}

impl ApplicationEventOfferReport {
    /// Events moved into caller-owned slots.
    pub const fn accepted_events(self) -> usize {
        self.accepted_events
    }

    /// Retained proofs moved with their exact events.
    pub const fn accepted_retained_proofs(self) -> usize {
        self.accepted_retained_proofs
    }

    /// Upstream source-dependent packet actions observed and consumed as a
    /// scalar diagnostic with this envelope.
    pub const fn unroutable_packets(self) -> usize {
        self.unroutable_packets
    }
}

/// Fixed-capacity owner over a caller-provided application-event slot slice.
#[must_use = "dropping the owner leaves every unresolved event in its caller-owned slot"]
pub struct ApplicationEventOwner<'slots> {
    slots: &'slots mut [ApplicationEventSlot],
    next_sequence: Option<u64>,
    counters: ApplicationEventOwnerCounters,
}

impl<'slots> ApplicationEventOwner<'slots> {
    /// Bind one caller-owned slot slice.
    ///
    /// Ready and quarantined contents from an earlier owner binding remain
    /// available. Fresh slots should be initialized with
    /// [`ApplicationEventSlot::new`].
    pub fn new(slots: &'slots mut [ApplicationEventSlot]) -> Self {
        let mut maximum_sequence = None::<u64>;
        for slot in slots.iter() {
            debug_assert_eq!(
                slot.owned.is_some(),
                matches!(
                    slot.state,
                    ApplicationEventSlotState::Ready
                        | ApplicationEventSlotState::Leased
                        | ApplicationEventSlotState::Quarantined(_)
                )
            );
            debug_assert!(
                slot.owned
                    .as_ref()
                    .is_none_or(|owned| !owned.has_retained_proof()
                        || event_kind_accepts_retained_proof(owned.event.kind()))
            );
            if slot.owned.is_some() {
                maximum_sequence = Some(
                    maximum_sequence
                        .map(|maximum| maximum.max(slot.sequence))
                        .unwrap_or(slot.sequence),
                );
            }
        }
        Self {
            slots,
            next_sequence: match maximum_sequence {
                None => Some(1),
                Some(maximum) => maximum.checked_add(1),
            },
            counters: ApplicationEventOwnerCounters::default(),
        }
    }

    /// Current scalar slot occupancy.
    pub fn capacities(&self) -> ApplicationEventCapacitySnapshot {
        let mut snapshot = ApplicationEventCapacitySnapshot {
            capacity: self.slots.len(),
            vacant: 0,
            ready: 0,
            leased: 0,
            quarantined: 0,
            retired: 0,
        };
        for slot in self.slots.iter() {
            match slot.state {
                ApplicationEventSlotState::Vacant => snapshot.vacant += 1,
                ApplicationEventSlotState::Ready => snapshot.ready += 1,
                ApplicationEventSlotState::Leased => snapshot.leased += 1,
                ApplicationEventSlotState::Quarantined(_) => snapshot.quarantined += 1,
                ApplicationEventSlotState::Retired => snapshot.retired += 1,
            }
        }
        snapshot
    }

    /// Cumulative counters for this owner binding.
    pub const fn counters(&self) -> ApplicationEventOwnerCounters {
        self.counters
    }

    /// Atomically move every event from one packet-free action envelope.
    ///
    /// Capacity and structural checks run before the envelope is mutated. On
    /// every error the returned failure therefore contains the exact original
    /// `NodeActions`, including its allocation identities.
    #[allow(clippy::result_large_err)]
    pub fn try_offer_actions(
        &mut self,
        mut actions: NodeActions,
    ) -> Result<ApplicationEventOfferReport, ApplicationEventOfferFailure> {
        self.counters.offered_batches = self.counters.offered_batches.saturating_add(1);

        if !actions.packets.is_empty() {
            self.counters.packet_owner_rejections =
                self.counters.packet_owner_rejections.saturating_add(1);
            return Err(ApplicationEventOfferFailure {
                reason: ApplicationEventOfferError::PacketOwnersPresent {
                    count: actions.packets.len(),
                },
                actions,
            });
        }

        if let Some(retained_proof) = actions.events.retained_proof() {
            let event_index = retained_proof.event_index();
            let Some(event) = actions.events.get(event_index) else {
                self.counters.retained_proof_index_rejections = self
                    .counters
                    .retained_proof_index_rejections
                    .saturating_add(1);
                return Err(ApplicationEventOfferFailure {
                    reason: ApplicationEventOfferError::RetainedProofIndexOutOfRange {
                        event_index,
                        event_count: actions.events.len(),
                    },
                    actions,
                });
            };
            let event_kind = event.kind();
            if !event_kind_accepts_retained_proof(event_kind) {
                self.counters.retained_proof_kind_rejections = self
                    .counters
                    .retained_proof_kind_rejections
                    .saturating_add(1);
                return Err(ApplicationEventOfferFailure {
                    reason: ApplicationEventOfferError::RetainedProofEventKindMismatch {
                        event_index,
                        event_kind,
                    },
                    actions,
                });
            }
        }

        let required = actions.events.len();
        let capacities = self.capacities();
        let reusable_capacity = capacities.reusable_capacity();
        if required > reusable_capacity {
            self.counters.oversized_batch_rejections =
                self.counters.oversized_batch_rejections.saturating_add(1);
            return Err(ApplicationEventOfferFailure {
                reason: ApplicationEventOfferError::BatchTooLarge {
                    required,
                    capacity: reusable_capacity,
                },
                actions,
            });
        }
        if required > capacities.vacant {
            self.counters.full_rejections = self.counters.full_rejections.saturating_add(1);
            return Err(ApplicationEventOfferFailure {
                reason: ApplicationEventOfferError::Full {
                    required,
                    available: capacities.vacant,
                },
                actions,
            });
        }

        let sequence_range = if required == 0 {
            None
        } else {
            if self.next_sequence.is_none() && capacities.occupied() == 0 {
                self.next_sequence = Some(1);
            }
            let Some(first) = self.next_sequence else {
                self.counters.sequence_exhaustion_rejections = self
                    .counters
                    .sequence_exhaustion_rejections
                    .saturating_add(1);
                return Err(ApplicationEventOfferFailure {
                    reason: ApplicationEventOfferError::SequenceExhausted,
                    actions,
                });
            };
            let additional = u64::try_from(required - 1).unwrap_or(u64::MAX);
            let Some(last) = first.checked_add(additional) else {
                self.counters.sequence_exhaustion_rejections = self
                    .counters
                    .sequence_exhaustion_rejections
                    .saturating_add(1);
                return Err(ApplicationEventOfferFailure {
                    reason: ApplicationEventOfferError::SequenceExhausted,
                    actions,
                });
            };
            Some((first, last))
        };

        let accepted_events = required;
        let accepted_retained_proofs = actions.retained_proof_count();
        let unroutable_packets = actions.unroutable_packets;
        let (events, mut retained_proof) = mem::take(&mut actions.events).into_parts();
        let mut next_sequence = sequence_range.map(|(first, _)| first).unwrap_or(0);
        for (event_index, event) in events.into_iter().enumerate() {
            let retained_proof_for_event = retained_proof
                .as_ref()
                .is_some_and(|proof| proof.event_index() == event_index)
                .then(|| {
                    let (bound_event_index, proof) = retained_proof
                        .take()
                        .expect("matching retained proof was observed")
                        .into_parts();
                    debug_assert_eq!(bound_event_index, event_index);
                    proof
                });
            let slot = self
                .slots
                .iter_mut()
                .find(|slot| matches!(slot.state, ApplicationEventSlotState::Vacant))
                .expect("capacity was reserved before moving application events");
            slot.generation = slot
                .generation
                .checked_add(1)
                .expect("retired generations are excluded from vacant capacity");
            slot.sequence = next_sequence;
            slot.owned = Some(OwnedApplicationEvent {
                event,
                retained_proof: retained_proof_for_event,
            });
            slot.state = ApplicationEventSlotState::Ready;
            next_sequence = next_sequence.saturating_add(1);
        }
        debug_assert!(retained_proof.is_none());
        if let Some((_, last)) = sequence_range {
            self.next_sequence = last.checked_add(1);
        }

        self.counters.accepted_batches = self.counters.accepted_batches.saturating_add(1);
        self.counters.accepted_events = self
            .counters
            .accepted_events
            .saturating_add(u64::try_from(accepted_events).unwrap_or(u64::MAX));
        self.counters.accepted_retained_proofs = self
            .counters
            .accepted_retained_proofs
            .saturating_add(u64::try_from(accepted_retained_proofs).unwrap_or(u64::MAX));
        self.counters.unroutable_packets_observed = self
            .counters
            .unroutable_packets_observed
            .saturating_add(u64::try_from(unroutable_packets).unwrap_or(u64::MAX));

        Ok(ApplicationEventOfferReport {
            accepted_events,
            accepted_retained_proofs,
            unroutable_packets,
        })
    }

    /// Lease the oldest event in the normal ready queue.
    pub fn lease_next(&mut self) -> Option<ApplicationEventLease<'_, 'slots>> {
        self.lease_oldest(false)
    }

    /// Lease the oldest quarantined event for recovery or final disposition.
    pub fn lease_next_quarantined(&mut self) -> Option<ApplicationEventLease<'_, 'slots>> {
        self.lease_oldest(true)
    }

    /// Reacquire the exact quarantined event authorized by one structural
    /// retry token.
    ///
    /// Physical slot provenance is checked before scalar identity. A token
    /// from another owner therefore cannot authorize an equal slot/generation
    /// pair. Every rejection returns the same opaque token and leaves every
    /// event slot unchanged; only rejection diagnostics advance.
    #[allow(
        clippy::result_large_err,
        reason = "the error must preserve the exact allocation-free retry token"
    )]
    pub fn try_reacquire_quarantined<'owner, 'token_slots>(
        &'owner mut self,
        token: ApplicationEventRetryToken<'token_slots>,
    ) -> Result<ApplicationEventLease<'owner, 'slots>, ApplicationEventRetryFailure<'token_slots>>
    {
        let Some(index) = self
            .slots
            .iter()
            .position(|slot| ptr::from_ref(slot).addr() == token.slot_address)
        else {
            self.counters.retry_reacquisition_rejections = self
                .counters
                .retry_reacquisition_rejections
                .saturating_add(1);
            return Err(ApplicationEventRetryFailure {
                reason: ApplicationEventRetryError::ForeignOwner,
                token,
            });
        };

        if index != token.id.slot.0 {
            self.counters.retry_reacquisition_rejections = self
                .counters
                .retry_reacquisition_rejections
                .saturating_add(1);
            return Err(ApplicationEventRetryFailure {
                reason: ApplicationEventRetryError::ForeignOwner,
                token,
            });
        }

        let slot = &self.slots[index];
        let current_generation = ApplicationEventGeneration(slot.generation);
        if current_generation != token.id.generation {
            self.counters.retry_reacquisition_rejections = self
                .counters
                .retry_reacquisition_rejections
                .saturating_add(1);
            return Err(ApplicationEventRetryFailure {
                reason: ApplicationEventRetryError::StaleGeneration {
                    token: token.id.generation,
                    current: current_generation,
                },
                token,
            });
        }

        let current_reason = match slot.state {
            ApplicationEventSlotState::Quarantined(reason) => reason,
            _ => {
                self.counters.retry_reacquisition_rejections = self
                    .counters
                    .retry_reacquisition_rejections
                    .saturating_add(1);
                return Err(ApplicationEventRetryFailure {
                    reason: ApplicationEventRetryError::NotQuarantined,
                    token,
                });
            }
        };
        let current_sequence = ApplicationEventSequence(slot.sequence);
        if current_sequence != token.sequence {
            self.counters.retry_reacquisition_rejections = self
                .counters
                .retry_reacquisition_rejections
                .saturating_add(1);
            return Err(ApplicationEventRetryFailure {
                reason: ApplicationEventRetryError::StaleSequence {
                    token: token.sequence,
                    current: current_sequence,
                },
                token,
            });
        }
        if current_reason != token.reason {
            self.counters.retry_reacquisition_rejections = self
                .counters
                .retry_reacquisition_rejections
                .saturating_add(1);
            return Err(ApplicationEventRetryFailure {
                reason: ApplicationEventRetryError::QuarantineChanged {
                    token: token.reason,
                    current: current_reason,
                },
                token,
            });
        }

        let slot = &mut self.slots[index];
        slot.state = ApplicationEventSlotState::Leased;
        self.counters.retry_reacquisitions = self.counters.retry_reacquisitions.saturating_add(1);
        Ok(ApplicationEventLease {
            owner: self,
            id: token.id,
            sequence: token.sequence,
            prior_quarantine: Some(current_reason),
            resolved: false,
        })
    }

    fn lease_oldest(&mut self, quarantined: bool) -> Option<ApplicationEventLease<'_, 'slots>> {
        let index = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| {
                if quarantined {
                    matches!(slot.state, ApplicationEventSlotState::Quarantined(_))
                } else {
                    matches!(slot.state, ApplicationEventSlotState::Ready)
                }
            })
            .min_by_key(|(_, slot)| slot.sequence)
            .map(|(index, _)| index)?;
        let slot = &mut self.slots[index];
        let prior_quarantine = match slot.state {
            ApplicationEventSlotState::Quarantined(reason) => Some(reason),
            ApplicationEventSlotState::Ready => None,
            _ => unreachable!("selected event must remain leasable"),
        };
        slot.state = ApplicationEventSlotState::Leased;
        let id = ApplicationEventId {
            slot: ApplicationEventSlotId(index),
            generation: ApplicationEventGeneration(slot.generation),
        };
        let sequence = ApplicationEventSequence(slot.sequence);
        Some(ApplicationEventLease {
            owner: self,
            id,
            sequence,
            prior_quarantine,
            resolved: false,
        })
    }

    fn resolve_slot(&mut self, id: ApplicationEventId) -> OwnedApplicationEvent {
        let slot = &mut self.slots[id.slot.0];
        assert_eq!(slot.generation, id.generation.0);
        assert_eq!(slot.state, ApplicationEventSlotState::Leased);
        let owned = slot
            .owned
            .take()
            .expect("a valid application-event lease retains its exact event");
        slot.sequence = 0;
        slot.state = if slot.generation == u64::MAX {
            ApplicationEventSlotState::Retired
        } else {
            ApplicationEventSlotState::Vacant
        };
        owned
    }

    fn quarantine_slot(
        &mut self,
        id: ApplicationEventId,
        reason: ApplicationEventQuarantineReason,
    ) {
        let slot = &mut self.slots[id.slot.0];
        assert_eq!(slot.generation, id.generation.0);
        assert_eq!(slot.state, ApplicationEventSlotState::Leased);
        let owned = slot
            .owned
            .as_ref()
            .expect("a valid application-event lease retains its exact event");
        debug_assert!(
            !owned.has_retained_proof() || event_kind_accepts_retained_proof(owned.event.kind())
        );
        slot.state = ApplicationEventSlotState::Quarantined(reason);
    }
}

impl fmt::Debug for ApplicationEventOwner<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApplicationEventOwner")
            .field("capacities", &self.capacities())
            .field("counters", &self.counters)
            .field("events", &"<redacted>")
            .finish()
    }
}

/// Proof-bearing event rejected by the proofless acknowledgement path.
#[must_use = "the exact retained application-event lease still requires disposition"]
pub struct ApplicationEventAcknowledgeFailure<'owner, 'slots> {
    lease: ApplicationEventLease<'owner, 'slots>,
}

impl<'owner, 'slots> ApplicationEventAcknowledgeFailure<'owner, 'slots> {
    /// Recover the exact unresolved proof-bearing lease.
    pub fn into_lease(self) -> ApplicationEventLease<'owner, 'slots> {
        self.lease
    }
}

impl fmt::Debug for ApplicationEventAcknowledgeFailure<'_, '_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApplicationEventAcknowledgeFailure")
            .field("event_id", &self.lease.id())
            .field("retained_proof", &true)
            .finish()
    }
}

/// Why a retained application event could not begin a delayed-proof
/// transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DelayedProofTransactionError {
    /// The event carried no privately retained proof.
    ProofNotPresent,
    /// Delayed-proof capacity could not be reserved.
    Reservation(DelayedProofReservationError),
}

/// Failed delayed-proof transaction creation preserving the exact event lease.
///
/// No delayed-proof reservation exists on this path, so callers can recover
/// the lease and retry or choose another explicit disposition before any
/// durable-store I/O begins.
#[must_use = "the exact application-event lease still requires disposition"]
pub struct DelayedProofTransactionFailure<'event_owner, 'event_slots> {
    reason: DelayedProofTransactionError,
    lease: ApplicationEventLease<'event_owner, 'event_slots>,
}

impl<'event_owner, 'event_slots> DelayedProofTransactionFailure<'event_owner, 'event_slots> {
    /// Typed reason no transaction was created.
    pub const fn reason(&self) -> DelayedProofTransactionError {
        self.reason
    }

    /// Recover the exact unresolved event lease.
    pub fn into_lease(self) -> ApplicationEventLease<'event_owner, 'event_slots> {
        self.lease
    }
}

impl fmt::Debug for DelayedProofTransactionFailure<'_, '_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DelayedProofTransactionFailure")
            .field("reason", &self.reason)
            .field("event_id", &self.lease.id())
            .finish()
    }
}

/// Pre-I/O ownership transaction for one exact proof-bearing event.
///
/// This capability can only be created from the event lease that owns the
/// hidden proof. It also owns the already-selected delayed-proof reservation,
/// so no independently correlatable scalar ID can authorize proof release.
/// Dropping the transaction quarantines the event and releases the empty
/// reservation. Call [`Self::into_lease`] after store failure, or
/// [`Self::acknowledge_into_ready`] only after durable success.
#[must_use = "the delayed-proof transaction must follow durable success or failure"]
pub struct DelayedProofTransaction<'event_owner, 'event_slots, 'proof_owner, 'proof_slots> {
    lease: ApplicationEventLease<'event_owner, 'event_slots>,
    reservation: DelayedProofReservation<'proof_owner, 'proof_slots>,
}

impl<'event_owner, 'event_slots, 'proof_owner, 'proof_slots>
    DelayedProofTransaction<'event_owner, 'event_slots, 'proof_owner, 'proof_slots>
{
    /// Exact application-event incarnation owned by this transaction.
    pub const fn event_id(&self) -> ApplicationEventId {
        self.lease.id()
    }

    /// Borrow the exact transport-neutral event for durable-store work.
    pub fn event(&self) -> &ApplicationEvent {
        self.lease.event()
    }

    /// Generation-safe identity of the pre-reserved delayed-proof slot.
    pub const fn proof_id(&self) -> DelayedProofId {
        self.reservation.id()
    }

    /// FIFO sequence selected before durable-store work begins.
    pub const fn proof_sequence(&self) -> DelayedProofSequence {
        self.reservation.sequence()
    }

    /// Recover the event lease after failed durable-store work.
    ///
    /// The still-empty delayed-proof reservation is released before the lease
    /// is returned.
    pub fn into_lease(self) -> ApplicationEventLease<'event_owner, 'event_slots> {
        let Self { lease, reservation } = self;
        drop(reservation);
        lease
    }

    /// Complete the infallible post-durability ownership transition.
    ///
    /// The hidden proof moves directly from this transaction's exact event
    /// lease into its pre-reserved slot. The slot becomes `Ready`; no scalar ID
    /// comparison is involved in authorization.
    pub fn acknowledge_into_ready(self) -> RetainedProofCommitSuccess {
        let Self {
            mut lease,
            reservation,
        } = self;
        let event_id = lease.id;
        let owned = lease.owner.resolve_slot(event_id);
        let proof = owned
            .retained_proof
            .expect("a delayed-proof transaction owns one retained proof");
        let proof_id = reservation.commit_retained(proof);
        lease.owner.counters.acknowledged_events =
            lease.owner.counters.acknowledged_events.saturating_add(1);
        lease.owner.counters.retained_proofs_queued = lease
            .owner
            .counters
            .retained_proofs_queued
            .saturating_add(1);
        lease.resolved = true;
        RetainedProofCommitSuccess {
            event_id,
            proof_id,
            event: owned.event,
        }
    }
}

impl fmt::Debug for DelayedProofTransaction<'_, '_, '_, '_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DelayedProofTransaction")
            .field("event_id", &self.event_id())
            .field("proof_id", &self.proof_id())
            .field("proof_sequence", &self.proof_sequence())
            .field("event", &"<redacted>")
            .field("proof", &"<redacted>")
            .finish()
    }
}

/// Successful atomic transition from a retained event to a ready delayed
/// proof.
#[must_use = "the acknowledged semantic event remains caller-owned"]
pub struct RetainedProofCommitSuccess {
    event_id: ApplicationEventId,
    proof_id: DelayedProofId,
    event: ApplicationEvent,
}

impl RetainedProofCommitSuccess {
    /// Exact application-event incarnation acknowledged by the transition.
    pub const fn event_id(&self) -> ApplicationEventId {
        self.event_id
    }

    /// Ready delayed-proof identity created by the transition.
    pub const fn proof_id(&self) -> DelayedProofId {
        self.proof_id
    }

    /// Borrow the acknowledged transport-neutral event.
    pub const fn event(&self) -> &ApplicationEvent {
        &self.event
    }

    /// Recover the acknowledged transport-neutral event.
    pub fn into_event(self) -> ApplicationEvent {
        self.event
    }
}

impl fmt::Debug for RetainedProofCommitSuccess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RetainedProofCommitSuccess")
            .field("event_id", &self.event_id)
            .field("proof_id", &self.proof_id)
            .field("event_kind", &self.event.kind())
            .field("event", &"<redacted>")
            .finish()
    }
}

/// Exclusive FIFO lease of one exact application event.
///
/// [`Self::acknowledge`], [`Self::discard`], or [`Self::quarantine`] must be
/// called. Dropping the lease unresolved converts it to a quarantine entry.
#[must_use = "an application-event lease must be acknowledged, discarded, or quarantined"]
pub struct ApplicationEventLease<'owner, 'slots> {
    owner: &'owner mut ApplicationEventOwner<'slots>,
    id: ApplicationEventId,
    sequence: ApplicationEventSequence,
    prior_quarantine: Option<ApplicationEventQuarantineReason>,
    resolved: bool,
}

impl<'owner, 'slots> ApplicationEventLease<'owner, 'slots> {
    /// Generation-safe event identity.
    pub const fn id(&self) -> ApplicationEventId {
        self.id
    }

    /// FIFO sequence assigned when the event entered the owner.
    pub const fn sequence(&self) -> ApplicationEventSequence {
        self.sequence
    }

    /// Existing quarantine reason when leased from the quarantine queue.
    pub const fn quarantine_reason(&self) -> Option<ApplicationEventQuarantineReason> {
        self.prior_quarantine
    }

    /// Borrow the exact event without copying its payload.
    pub fn event(&self) -> &ApplicationEvent {
        self.owner.slots[self.id.slot.0]
            .owned
            .as_ref()
            .map(OwnedApplicationEvent::event)
            .expect("a valid application-event lease retains its exact event")
    }

    /// Whether this lease privately owns a retained proof requiring the delayed
    /// acknowledgement path.
    pub fn has_retained_proof(&self) -> bool {
        self.owner.slots[self.id.slot.0]
            .owned
            .as_ref()
            .is_some_and(OwnedApplicationEvent::has_retained_proof)
    }

    /// Acknowledge and transfer an event only when it has no retained proof.
    ///
    /// A proof-bearing event is returned unchanged in the error and can only be
    /// acknowledged through a [`DelayedProofTransaction`].
    pub fn acknowledge(
        mut self,
    ) -> Result<ApplicationEvent, ApplicationEventAcknowledgeFailure<'owner, 'slots>> {
        if self.has_retained_proof() {
            self.owner
                .counters
                .retained_proof_acknowledgement_rejections = self
                .owner
                .counters
                .retained_proof_acknowledgement_rejections
                .saturating_add(1);
            return Err(ApplicationEventAcknowledgeFailure { lease: self });
        }
        let owned = self.owner.resolve_slot(self.id);
        debug_assert!(owned.retained_proof.is_none());
        self.owner.counters.acknowledged_events =
            self.owner.counters.acknowledged_events.saturating_add(1);
        self.resolved = true;
        Ok(owned.event)
    }

    /// Reserve delayed-proof capacity and bind it structurally to this exact
    /// proof-bearing lease before durable-store I/O.
    ///
    /// Missing proof, full capacity, and sequence exhaustion return the exact
    /// lease unchanged. Success produces one combined ownership transaction;
    /// no independently created reservation or scalar event ID can later be
    /// substituted for it.
    #[allow(
        clippy::result_large_err,
        reason = "the error must return the exact allocation-free lease for retry"
    )]
    pub fn try_reserve_delayed<'proof_owner, 'proof_slots>(
        self,
        proof_owner: &'proof_owner mut DelayedProofOwner<'proof_slots>,
    ) -> Result<
        DelayedProofTransaction<'owner, 'slots, 'proof_owner, 'proof_slots>,
        DelayedProofTransactionFailure<'owner, 'slots>,
    > {
        if !self.has_retained_proof() {
            return Err(DelayedProofTransactionFailure {
                reason: DelayedProofTransactionError::ProofNotPresent,
                lease: self,
            });
        }
        let reservation = match proof_owner.try_reserve(self.id) {
            Ok(reservation) => reservation,
            Err(reason) => {
                return Err(DelayedProofTransactionFailure {
                    reason: DelayedProofTransactionError::Reservation(reason),
                    lease: self,
                });
            }
        };
        Ok(DelayedProofTransaction {
            lease: self,
            reservation,
        })
    }

    /// Explicitly discard this event for one typed product reason and release
    /// its slot.
    pub fn discard(mut self, reason: ApplicationEventDiscardReason) {
        let owned = self.owner.resolve_slot(self.id);
        self.owner.counters.discarded_events =
            self.owner.counters.discarded_events.saturating_add(1);
        let classified = match reason {
            ApplicationEventDiscardReason::ConsumerUnavailable => {
                &mut self.owner.counters.consumer_unavailable_discards
            }
            ApplicationEventDiscardReason::DownstreamCapacity => {
                &mut self.owner.counters.downstream_capacity_discards
            }
            ApplicationEventDiscardReason::InvalidPayload => {
                &mut self.owner.counters.invalid_payload_discards
            }
            ApplicationEventDiscardReason::Superseded => {
                &mut self.owner.counters.superseded_discards
            }
            ApplicationEventDiscardReason::MaintenanceCoalesced => {
                &mut self.owner.counters.maintenance_coalesced_discards
            }
            ApplicationEventDiscardReason::PolicyRejected => {
                &mut self.owner.counters.policy_rejected_discards
            }
        };
        *classified = classified.saturating_add(1);
        self.resolved = true;
        drop(owned);
    }

    /// Retain this event outside the normal ready queue.
    pub fn quarantine(mut self, reason: ApplicationEventQuarantineReason) {
        self.owner.quarantine_slot(self.id, reason);
        self.owner.counters.explicit_quarantines =
            self.owner.counters.explicit_quarantines.saturating_add(1);
        self.resolved = true;
    }

    /// Retain this exact event and return structural authority for a later
    /// backoff retry.
    ///
    /// Unlike the scalar [`Self::id`], the returned opaque token carries
    /// private backing-slot provenance. It can therefore be presented to
    /// [`ApplicationEventOwner::try_reacquire_quarantined`] without allowing a
    /// different owner whose event has an equal ID to be substituted.
    pub fn quarantine_for_retry(
        mut self,
        reason: ApplicationEventQuarantineReason,
    ) -> ApplicationEventRetryToken<'slots> {
        let slot_address = ptr::from_ref(&self.owner.slots[self.id.slot.0]).addr();
        self.owner.quarantine_slot(self.id, reason);
        self.owner.counters.explicit_quarantines =
            self.owner.counters.explicit_quarantines.saturating_add(1);
        self.owner.counters.retry_quarantines =
            self.owner.counters.retry_quarantines.saturating_add(1);
        self.resolved = true;
        ApplicationEventRetryToken {
            slot_address,
            id: self.id,
            sequence: self.sequence,
            reason,
            storage: PhantomData,
        }
    }
}

impl fmt::Debug for ApplicationEventLease<'_, '_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApplicationEventLease")
            .field("id", &self.id)
            .field("sequence", &self.sequence)
            .field("prior_quarantine", &self.prior_quarantine)
            .field("retained_proof", &self.has_retained_proof())
            .field("event", &"<redacted>")
            .finish()
    }
}

impl Drop for ApplicationEventLease<'_, '_> {
    fn drop(&mut self) {
        if self.resolved {
            return;
        }
        let reason = self
            .prior_quarantine
            .unwrap_or(ApplicationEventQuarantineReason::UnresolvedLeaseDropped);
        self.owner.quarantine_slot(self.id, reason);
        self.owner.counters.unresolved_lease_quarantines = self
            .owner
            .counters
            .unresolved_lease_quarantines
            .saturating_add(1);
        self.resolved = true;
    }
}

#[cfg(test)]
mod tests;
