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
mod tests {
    extern crate std;

    use crate::{
        ApplicationEvent, DelayedProofOwner, DelayedProofSlot, EmbeddedNode, EmbeddedNodeConfig,
        InboundProofPolicy, InterfaceId, PacketType, RNS_MTU, TxTarget, identity_from_private_key,
        parse_packet,
    };
    use rand_core::{CryptoRng, RngCore};
    use std::{format, vec, vec::Vec};

    use super::*;

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

    fn assert_send<T: Send>() {}

    type TestRnsNode = EmbeddedNode<4, 4, 8, 2>;

    fn data_event(tag: u8) -> ApplicationEvent {
        ApplicationEvent::DataReceived {
            destination: [tag; 16],
            payload: vec![tag, tag.wrapping_add(1), tag.wrapping_add(2)],
            ingress: None,
        }
    }

    fn actions(tags: &[u8]) -> NodeActions {
        NodeActions::without_retained_proofs(
            tags.iter().copied().map(data_event).collect::<Vec<_>>(),
            Vec::new(),
            0,
        )
    }

    fn data_tag(event: &ApplicationEvent) -> u8 {
        let ApplicationEvent::DataReceived { destination, .. } = event else {
            panic!("test event changed variant")
        };
        destination[0]
    }

    fn rns_node(tag: u8) -> TestRnsNode {
        TestRnsNode::new(
            identity_from_private_key(&[tag; 64]).expect("test identity is valid"),
            "reticulum",
            &["application-event-owner"],
            EmbeddedNodeConfig::endpoint(),
        )
        .expect("test node is valid")
    }

    fn retained_data_actions(tag: u8, source: InterfaceId) -> NodeActions {
        let mut rng = CounterRng::default();
        let mut sender = rns_node(tag);
        let mut receiver = rns_node(tag.wrapping_add(1));
        receiver.set_inbound_proof_policy(InboundProofPolicy::Retain);
        receiver
            .queue_announce(None, 1, &mut rng)
            .expect("receiver announce queues");
        let announce = receiver.flush_announces(1, &mut rng);
        assert_eq!(announce.len(), 1);
        sender.ingest(announce[0].bytes(), 1, InterfaceId(1), &mut rng);

        let mut encoded = [0u8; RNS_MTU];
        let prepared = sender
            .prepare_data_into(
                &receiver.destination_hash(),
                &[tag, tag.wrapping_add(1)],
                2,
                &mut rng,
                &mut encoded,
            )
            .expect("encrypted DATA prepares");
        let actions = receiver
            .ingest(
                &encoded[..usize::from(prepared.packet_len())],
                2,
                source,
                &mut rng,
            )
            .actions;
        assert_eq!(actions.events.len(), 1);
        assert_eq!(actions.retained_proof_count(), 1);
        assert!(actions.packets.is_empty());
        actions
    }

    #[test]
    fn full_and_oversized_offers_return_exact_action_owner() {
        let mut slots = [ApplicationEventSlot::new()];
        let mut owner = ApplicationEventOwner::new(&mut slots);
        owner
            .try_offer_actions(actions(&[1]))
            .expect("first event fits");

        let offered = actions(&[2]);
        let event_pointer = offered.events.as_ptr();
        let failure = owner
            .try_offer_actions(offered)
            .expect_err("occupied owner reports transient pressure");
        assert_eq!(
            failure.reason(),
            ApplicationEventOfferError::Full {
                required: 1,
                available: 0,
            }
        );
        let offered = failure.into_actions();
        assert_eq!(offered.events.as_ptr(), event_pointer);
        assert_eq!(data_tag(&offered.events[0]), 2);

        let oversized = actions(&[3, 4]);
        let event_pointer = oversized.events.as_ptr();
        let failure = owner
            .try_offer_actions(oversized)
            .expect_err("batch larger than the owner is terminal");
        assert_eq!(
            failure.reason(),
            ApplicationEventOfferError::BatchTooLarge {
                required: 2,
                capacity: 1,
            }
        );
        let oversized = failure.into_actions();
        assert_eq!(oversized.events.as_ptr(), event_pointer);
        assert_eq!(owner.capacities().ready, 1);
        assert_eq!(owner.counters().full_rejections, 1);
        assert_eq!(owner.counters().oversized_batch_rejections, 1);
    }

    #[test]
    fn leases_are_fifo_generation_safe_and_never_free_on_unresolved_drop() {
        let mut slots = [ApplicationEventSlot::new(), ApplicationEventSlot::new()];
        let mut owner = ApplicationEventOwner::new(&mut slots);
        owner
            .try_offer_actions(actions(&[10, 11]))
            .expect("two events fit atomically");

        let first = owner.lease_next().expect("first event is ready");
        let first_id = first.id();
        assert_eq!(data_tag(first.event()), 10);
        drop(first);
        assert_eq!(owner.capacities().quarantined, 1);
        assert_eq!(owner.counters().unresolved_lease_quarantines, 1);

        let second = owner
            .lease_next()
            .expect("second ready event remains available");
        assert_eq!(data_tag(second.event()), 11);
        second.discard(ApplicationEventDiscardReason::Superseded);

        let quarantined = owner
            .lease_next_quarantined()
            .expect("unresolved first event remains exactly owned");
        assert_eq!(quarantined.id(), first_id);
        assert_eq!(
            quarantined.quarantine_reason(),
            Some(ApplicationEventQuarantineReason::UnresolvedLeaseDropped)
        );
        let first_event = quarantined
            .acknowledge()
            .expect("proofless event acknowledges directly");
        assert_eq!(data_tag(&first_event), 10);

        owner
            .try_offer_actions(actions(&[12]))
            .expect("released slot is reusable");
        let reused = owner.lease_next().expect("new event is ready");
        assert_eq!(reused.id().slot(), first_id.slot());
        assert!(reused.id().generation() > first_id.generation());
        reused.quarantine(ApplicationEventQuarantineReason::ConsumerDeferred);
        assert_eq!(owner.capacities().quarantined, 1);
        assert_eq!(owner.counters().acknowledged_events, 1);
        assert_eq!(owner.counters().discarded_events, 1);
        assert_eq!(owner.counters().superseded_discards, 1);
        assert_eq!(owner.counters().explicit_quarantines, 1);
    }

    #[test]
    fn retry_token_survives_backoff_work_and_reacquires_the_exact_event() {
        assert_send::<ApplicationEventRetryToken<'static>>();
        assert_send::<ApplicationEventRetryFailure<'static>>();

        let source = InterfaceId(6);
        let retained = retained_data_actions(0x71, source);
        let proof_pointer = retained
            .events
            .retained_proof()
            .expect("retained proof is privately attached")
            .packet_bytes_ptr();
        let mut slots = [ApplicationEventSlot::new(), ApplicationEventSlot::new()];
        let mut owner = ApplicationEventOwner::new(&mut slots);
        owner
            .try_offer_actions(retained)
            .expect("proof-bearing event fits");
        owner
            .try_offer_actions(actions(&[0x72]))
            .expect("independent event fits");

        let retained = owner.lease_next().expect("retained event is oldest");
        let retained_id = retained.id();
        let retained_sequence = retained.sequence();
        let payload_pointer = match retained.event() {
            ApplicationEvent::DataReceived { payload, .. } => payload.as_ptr(),
            _ => panic!("retained fixture changed event kind"),
        };
        let token =
            retained.quarantine_for_retry(ApplicationEventQuarantineReason::ConsumerDeferred);
        assert_eq!(token.event_id(), retained_id);
        assert_eq!(token.sequence(), retained_sequence);
        assert_eq!(
            token.reason(),
            ApplicationEventQuarantineReason::ConsumerDeferred
        );
        assert_eq!(owner.capacities().quarantined, 1);

        owner
            .lease_next()
            .expect("token does not borrow the owner during backoff")
            .discard(ApplicationEventDiscardReason::MaintenanceCoalesced);
        let lease = owner
            .try_reacquire_quarantined(token)
            .expect("the structural token reacquires its exact event");
        assert_eq!(lease.id(), retained_id);
        assert_eq!(lease.sequence(), retained_sequence);
        assert_eq!(
            lease.quarantine_reason(),
            Some(ApplicationEventQuarantineReason::ConsumerDeferred)
        );
        assert!(lease.has_retained_proof());
        assert!(matches!(
            lease.event(),
            ApplicationEvent::DataReceived { payload, .. }
                if payload.as_ptr() == payload_pointer && payload.as_slice() == [0x71, 0x72]
        ));

        let mut proof_slots = [DelayedProofSlot::new()];
        let mut proof_owner = DelayedProofOwner::new(&mut proof_slots);
        let committed = lease
            .try_reserve_delayed(&mut proof_owner)
            .expect("reacquired proof-bearing event reserves proof capacity")
            .acknowledge_into_ready();
        assert_eq!(committed.event_id(), retained_id);
        let actions = proof_owner
            .lease_next()
            .expect("the same proof became ready")
            .release_actions();
        assert_eq!(actions.packets[0].bytes().as_ptr(), proof_pointer);
        assert_eq!(owner.counters().retry_quarantines, 1);
        assert_eq!(owner.counters().retry_reacquisitions, 1);
        assert_eq!(owner.counters().retry_reacquisition_rejections, 0);
    }

    #[test]
    fn retry_token_rejects_an_independent_owner_with_an_equal_scalar_id() {
        let mut first_slots = [ApplicationEventSlot::new()];
        let mut second_slots = [ApplicationEventSlot::new()];
        let mut first_owner = ApplicationEventOwner::new(&mut first_slots);
        let mut second_owner = ApplicationEventOwner::new(&mut second_slots);
        first_owner
            .try_offer_actions(actions(&[0x81]))
            .expect("first event fits");
        second_owner
            .try_offer_actions(actions(&[0x82]))
            .expect("second event fits");

        let first = first_owner.lease_next().expect("first event is ready");
        let first_payload_pointer = match first.event() {
            ApplicationEvent::DataReceived { payload, .. } => payload.as_ptr(),
            _ => panic!("first fixture changed event kind"),
        };
        let first_token =
            first.quarantine_for_retry(ApplicationEventQuarantineReason::ConsumerDeferred);
        let second = second_owner.lease_next().expect("second event is ready");
        assert_eq!(second.id(), first_token.event_id());
        second.quarantine(ApplicationEventQuarantineReason::ConsumerDeferred);

        let token_id = first_token.event_id();
        let token_sequence = first_token.sequence();
        let token_reason = first_token.reason();
        let failure = second_owner
            .try_reacquire_quarantined(first_token)
            .expect_err("equal scalar identity from another owner is not authority");
        assert_eq!(failure.reason(), ApplicationEventRetryError::ForeignOwner);
        assert_eq!(second_owner.capacities().quarantined, 1);
        assert_eq!(second_owner.counters().retry_reacquisition_rejections, 1);

        let first_token = failure.into_token();
        assert_eq!(first_token.event_id(), token_id);
        assert_eq!(first_token.sequence(), token_sequence);
        assert_eq!(first_token.reason(), token_reason);
        let first = first_owner
            .try_reacquire_quarantined(first_token)
            .expect("the unchanged token still authorizes its original owner");
        assert!(matches!(
            first.event(),
            ApplicationEvent::DataReceived {
                destination,
                payload,
                ..
            }
                if destination[0] == 0x81 && payload.as_ptr() == first_payload_pointer
        ));
        first.discard(ApplicationEventDiscardReason::Superseded);
        second_owner
            .lease_next_quarantined()
            .expect("foreign rejection left the second event untouched")
            .discard(ApplicationEventDiscardReason::Superseded);
    }

    #[test]
    fn same_event_retry_tokens_serialize_without_authorizing_another_incarnation() {
        let mut slots = [ApplicationEventSlot::new()];
        let mut owner = ApplicationEventOwner::new(&mut slots);
        owner
            .try_offer_actions(actions(&[0x89]))
            .expect("event fits");
        let first = owner.lease_next().expect("event is ready");
        let event_id = first.id();
        let first = first.quarantine_for_retry(ApplicationEventQuarantineReason::ConsumerDeferred);

        let same_event = owner
            .lease_next_quarantined()
            .expect("ordinary recovery may inspect the same exact event");
        assert_eq!(same_event.id(), event_id);
        let second =
            same_event.quarantine_for_retry(ApplicationEventQuarantineReason::ConsumerDeferred);
        assert_eq!(first.event_id(), second.event_id());
        assert_eq!(first.sequence(), second.sequence());

        let lease = owner
            .try_reacquire_quarantined(first)
            .expect("one token serially reacquires the event");
        assert_eq!(lease.id(), event_id);
        drop(lease);
        assert_eq!(owner.capacities().quarantined, 1);

        let lease = owner
            .try_reacquire_quarantined(second)
            .expect("a coexisting token still names only the unchanged event");
        assert_eq!(lease.id(), event_id);
        assert_eq!(data_tag(lease.event()), 0x89);
        lease.discard(ApplicationEventDiscardReason::Superseded);
        assert_eq!(owner.counters().retry_quarantines, 2);
        assert_eq!(owner.counters().retry_reacquisitions, 2);
    }

    #[test]
    fn stale_retry_token_cannot_authorize_a_reused_slot_generation() {
        let mut slots = [ApplicationEventSlot::new()];
        let mut owner = ApplicationEventOwner::new(&mut slots);
        owner
            .try_offer_actions(actions(&[0x91]))
            .expect("first event fits");
        let first = owner.lease_next().expect("first event is ready");
        let first_id = first.id();
        let stale = first.quarantine_for_retry(ApplicationEventQuarantineReason::ConsumerDeferred);

        owner
            .lease_next_quarantined()
            .expect("ordinary recovery remains an explicit escape hatch")
            .discard(ApplicationEventDiscardReason::Superseded);
        owner
            .try_offer_actions(actions(&[0x92]))
            .expect("the same physical slot is reused");
        let current = owner.lease_next().expect("replacement event is ready");
        let current_id = current.id();
        assert_eq!(current_id.slot(), first_id.slot());
        assert!(current_id.generation() > first_id.generation());
        let current =
            current.quarantine_for_retry(ApplicationEventQuarantineReason::ConsumerDeferred);

        let failure = owner
            .try_reacquire_quarantined(stale)
            .expect_err("stale generation cannot authorize the replacement event");
        assert_eq!(
            failure.reason(),
            ApplicationEventRetryError::StaleGeneration {
                token: first_id.generation(),
                current: current_id.generation(),
            }
        );
        assert_eq!(owner.capacities().quarantined, 1);
        assert_eq!(owner.counters().retry_reacquisition_rejections, 1);
        drop(failure.into_token());

        let replacement = owner
            .try_reacquire_quarantined(current)
            .expect("the replacement event still requires its fresh token");
        assert_eq!(replacement.id(), current_id);
        assert_eq!(data_tag(replacement.event()), 0x92);
        replacement.discard(ApplicationEventDiscardReason::Superseded);
    }

    #[test]
    fn proofless_delayed_transaction_failure_returns_the_exact_lease_before_reservation() {
        let mut event_slots = [ApplicationEventSlot::new()];
        let mut event_owner = ApplicationEventOwner::new(&mut event_slots);
        event_owner
            .try_offer_actions(actions(&[0x21]))
            .expect("proofless event fits");
        let lease = event_owner.lease_next().expect("proofless event is ready");
        let event_id = lease.id();

        let mut proof_slots = [DelayedProofSlot::new()];
        let mut proof_owner = DelayedProofOwner::new(&mut proof_slots);
        let failure = lease
            .try_reserve_delayed(&mut proof_owner)
            .expect_err("proofless event cannot reserve delayed-proof authority");
        assert_eq!(
            failure.reason(),
            DelayedProofTransactionError::ProofNotPresent
        );
        assert_eq!(proof_owner.counters().reservation_attempts, 0);
        assert_eq!(proof_owner.capacities().vacant, 1);

        let lease = failure.into_lease();
        assert_eq!(lease.id(), event_id);
        assert_eq!(data_tag(lease.event()), 0x21);
        assert!(!lease.has_retained_proof());
        let acknowledged = lease
            .acknowledge()
            .expect("returned proofless lease remains directly acknowledgeable");
        assert_eq!(data_tag(&acknowledged), 0x21);
    }

    #[test]
    fn dropping_pre_io_transaction_quarantines_event_and_releases_reservation() {
        let source = InterfaceId(7);
        let mut event_slots = [ApplicationEventSlot::new()];
        let mut event_owner = ApplicationEventOwner::new(&mut event_slots);
        let actions = retained_data_actions(0x31, source);
        let proof_pointer = actions
            .events
            .retained_proof()
            .expect("proof is privately attached")
            .packet_bytes_ptr();
        event_owner
            .try_offer_actions(actions)
            .expect("retained event fits");
        let lease = event_owner.lease_next().expect("retained event is ready");

        let mut proof_slots = [DelayedProofSlot::new()];
        let mut proof_owner = DelayedProofOwner::new(&mut proof_slots);
        let transaction = lease
            .try_reserve_delayed(&mut proof_owner)
            .expect("transaction reserves proof capacity");
        drop(transaction);

        assert_eq!(event_owner.capacities().quarantined, 1);
        assert_eq!(event_owner.counters().unresolved_lease_quarantines, 1);
        assert_eq!(proof_owner.capacities().vacant, 1);
        assert_eq!(proof_owner.counters().reservations_released, 1);
        let lease = event_owner
            .lease_next_quarantined()
            .expect("dropped transaction retained the exact event and proof");
        assert!(lease.has_retained_proof());
        let transaction = lease
            .try_reserve_delayed(&mut proof_owner)
            .expect("recovered proof can reserve the restored capacity");
        let acknowledged = transaction.acknowledge_into_ready();
        assert_eq!(
            acknowledged.event().kind(),
            ApplicationEventKind::DataReceived
        );
        let actions = proof_owner
            .lease_next()
            .expect("recovered exact proof is ready")
            .release_actions();
        assert_eq!(actions.packets[0].bytes().as_ptr(), proof_pointer);
    }

    #[test]
    fn transaction_ownership_disambiguates_equal_ids_from_independent_event_stores() {
        let source = InterfaceId(8);
        let first_actions = retained_data_actions(0x41, source);
        let first_proof_pointer = first_actions
            .events
            .retained_proof()
            .expect("first proof is privately attached")
            .packet_bytes_ptr();
        let second_actions = retained_data_actions(0x42, source);
        let second_proof_pointer = second_actions
            .events
            .retained_proof()
            .expect("second proof is privately attached")
            .packet_bytes_ptr();

        let mut first_slots = [ApplicationEventSlot::new()];
        let mut second_slots = [ApplicationEventSlot::new()];
        let mut first_owner = ApplicationEventOwner::new(&mut first_slots);
        let mut second_owner = ApplicationEventOwner::new(&mut second_slots);
        first_owner
            .try_offer_actions(first_actions)
            .expect("first retained event fits");
        second_owner
            .try_offer_actions(second_actions)
            .expect("second retained event fits");
        let first_lease = first_owner.lease_next().expect("first event is ready");
        let second_lease = second_owner.lease_next().expect("second event is ready");
        assert_eq!(first_lease.id(), second_lease.id());
        assert_ne!(first_proof_pointer, second_proof_pointer);

        let mut proof_slots = [DelayedProofSlot::new()];
        let mut proof_owner = DelayedProofOwner::new(&mut proof_slots);
        let transaction = first_lease
            .try_reserve_delayed(&mut proof_owner)
            .expect("transaction is created only from the exact first lease");
        assert!(matches!(
            transaction.event(),
            ApplicationEvent::DataReceived { payload, .. }
                if payload.as_slice() == [0x41, 0x42]
        ));
        let acknowledged = transaction.acknowledge_into_ready();
        assert!(matches!(
            acknowledged.event(),
            ApplicationEvent::DataReceived { payload, .. }
                if payload.as_slice() == [0x41, 0x42]
        ));

        let released = proof_owner
            .lease_next()
            .expect("first proof alone became ready")
            .release_actions();
        assert_eq!(released.packets[0].bytes().as_ptr(), first_proof_pointer);
        assert_ne!(released.packets[0].bytes().as_ptr(), second_proof_pointer);
        assert!(second_lease.has_retained_proof());
        second_lease.discard(ApplicationEventDiscardReason::Superseded);
    }

    #[test]
    fn retained_proof_survives_pressure_retry_unresolved_drop_and_owner_reconstruction() {
        let source = InterfaceId(9);
        let mut slots = [ApplicationEventSlot::new()];
        let mut owner = ApplicationEventOwner::new(&mut slots);
        owner
            .try_offer_actions(actions(&[1]))
            .expect("first event occupies the sole slot");

        let offered = retained_data_actions(0x41, source);
        let event_pointer = offered.events.as_ptr();
        let proof_pointer = offered
            .events
            .retained_proof()
            .expect("retained proof is privately attached")
            .packet_bytes_ptr();
        let failure = owner
            .try_offer_actions(offered)
            .expect_err("occupied owner reports retryable pressure");
        assert_eq!(
            failure.reason(),
            ApplicationEventOfferError::Full {
                required: 1,
                available: 0,
            }
        );
        let offered = failure.into_actions();
        assert_eq!(offered.events.as_ptr(), event_pointer);
        assert_eq!(
            offered
                .events
                .retained_proof()
                .expect("rejected envelope retains its exact proof")
                .packet_bytes_ptr(),
            proof_pointer
        );

        owner
            .lease_next()
            .expect("occupying event remains ready")
            .discard(ApplicationEventDiscardReason::Superseded);
        let report = owner
            .try_offer_actions(offered)
            .expect("the exact rejected envelope is retryable");
        assert_eq!(report.accepted_events(), 1);
        assert_eq!(report.accepted_retained_proofs(), 1);
        assert_eq!(owner.counters().accepted_retained_proofs, 1);

        let lease = owner.lease_next().expect("retained DATA is ready");
        assert_eq!(lease.event().kind(), ApplicationEventKind::DataReceived);
        assert!(lease.has_retained_proof());
        assert!(format!("{lease:?}").contains("retained_proof: true"));
        let lease_id = lease.id();
        let failure = lease
            .acknowledge()
            .expect_err("proof-bearing events reject proofless acknowledgement");
        let lease = failure.into_lease();
        assert_eq!(lease.id(), lease_id);
        assert!(lease.has_retained_proof());

        let mut no_proof_slots: [DelayedProofSlot; 0] = [];
        let mut no_proof_owner = DelayedProofOwner::new(&mut no_proof_slots);
        let failure = lease
            .try_reserve_delayed(&mut no_proof_owner)
            .expect_err("capacity failure returns the exact proof-bearing lease");
        assert_eq!(
            failure.reason(),
            DelayedProofTransactionError::Reservation(DelayedProofReservationError::Full {
                capacity: 0,
            })
        );
        let lease = failure.into_lease();
        assert_eq!(lease.id(), lease_id);
        assert!(lease.has_retained_proof());

        let mut retry_proof_slots = [DelayedProofSlot::new()];
        let mut retry_proof_owner = DelayedProofOwner::new(&mut retry_proof_slots);
        let transaction = lease
            .try_reserve_delayed(&mut retry_proof_owner)
            .expect("exact lease and proof capacity form one pre-I/O transaction");
        assert_eq!(transaction.event_id(), lease_id);
        let lease = transaction.into_lease();
        assert_eq!(retry_proof_owner.capacities().vacant, 1);
        assert_eq!(lease.id(), lease_id);
        assert!(lease.has_retained_proof());
        lease.quarantine(ApplicationEventQuarantineReason::ConsumerDeferred);
        assert_eq!(owner.capacities().quarantined, 1);
        assert_eq!(
            owner.counters().retained_proof_acknowledgement_rejections,
            1
        );

        let lease = owner
            .lease_next_quarantined()
            .expect("explicitly quarantined event and proof remain recoverable");
        assert_eq!(
            lease.quarantine_reason(),
            Some(ApplicationEventQuarantineReason::ConsumerDeferred)
        );
        assert!(lease.has_retained_proof());
        drop(lease);
        assert_eq!(owner.capacities().quarantined, 1);
        assert_eq!(owner.counters().explicit_quarantines, 1);
        assert_eq!(owner.counters().unresolved_lease_quarantines, 1);
        drop(owner);

        let mut owner = ApplicationEventOwner::new(&mut slots);
        let lease = owner
            .lease_next_quarantined()
            .expect("reconstructed owner retains the unresolved event and proof");
        assert_eq!(
            lease.quarantine_reason(),
            Some(ApplicationEventQuarantineReason::ConsumerDeferred)
        );
        assert!(lease.has_retained_proof());
        let event_id = lease.id();
        let mut proof_slots = [DelayedProofSlot::new()];
        let mut proof_owner = DelayedProofOwner::new(&mut proof_slots);
        let transaction = lease
            .try_reserve_delayed(&mut proof_owner)
            .expect("the exact retained lease pre-reserves proof capacity");
        let acknowledged = transaction.acknowledge_into_ready();
        assert_eq!(
            acknowledged.event().kind(),
            ApplicationEventKind::DataReceived
        );
        assert_eq!(acknowledged.event_id(), event_id);
        let proof_id = acknowledged.proof_id();
        let actions = proof_owner
            .lease_next()
            .expect("committed proof is ready")
            .release_actions();
        assert_eq!(actions.packets.len(), 1);
        assert_eq!(actions.retained_proof_count(), 0);
        let packet = &actions.packets[0];
        assert_eq!(proof_id.generation().get(), 1);
        assert_eq!(packet.bytes().as_ptr(), proof_pointer);
        assert_eq!(packet.target(), TxTarget::Only(source));
        assert_eq!(
            parse_packet(packet.bytes())
                .expect("retained bytes remain a valid packet")
                .packet_type,
            PacketType::Proof
        );
    }

    #[test]
    fn malformed_private_proof_indices_fail_before_owner_or_envelope_mutation() {
        let mut slots = [ApplicationEventSlot::new()];
        let mut owner = ApplicationEventOwner::new(&mut slots);

        let mut out_of_range = retained_data_actions(0x51, InterfaceId(2));
        out_of_range.events.clear_semantic_events_for_test();
        assert!(out_of_range.events.is_empty());
        assert!(out_of_range.packets.is_empty());
        assert_eq!(out_of_range.unroutable_packets, 0);
        assert_eq!(out_of_range.retained_proof_count(), 1);
        assert!(!out_of_range.is_empty());
        let proof_pointer = out_of_range
            .events
            .retained_proof()
            .unwrap()
            .packet_bytes_ptr();
        let failure = owner
            .try_offer_actions(out_of_range)
            .expect_err("a retained proof cannot name a missing event");
        assert_eq!(
            failure.reason(),
            ApplicationEventOfferError::RetainedProofIndexOutOfRange {
                event_index: 0,
                event_count: 0,
            }
        );
        let out_of_range = failure.into_actions();
        assert_eq!(
            out_of_range
                .events
                .retained_proof()
                .unwrap()
                .packet_bytes_ptr(),
            proof_pointer
        );
        assert!(out_of_range.events.is_empty());
        assert_eq!(owner.capacities().occupied(), 0);

        let mut wrong_kind = retained_data_actions(0x61, InterfaceId(3));
        wrong_kind.events.replace_semantic_event_for_test(
            0,
            ApplicationEvent::Tick {
                expired_paths: 0,
                closed_links: 0,
            },
        );
        let event_pointer = wrong_kind.events.as_ptr();
        let proof_pointer = wrong_kind
            .events
            .retained_proof()
            .unwrap()
            .packet_bytes_ptr();
        let failure = owner
            .try_offer_actions(wrong_kind)
            .expect_err("a retained DATA proof cannot bind another event kind");
        assert_eq!(
            failure.reason(),
            ApplicationEventOfferError::RetainedProofEventKindMismatch {
                event_index: 0,
                event_kind: ApplicationEventKind::Tick,
            }
        );
        let wrong_kind = failure.into_actions();
        assert_eq!(wrong_kind.events.as_ptr(), event_pointer);
        assert_eq!(
            wrong_kind
                .events
                .retained_proof()
                .unwrap()
                .packet_bytes_ptr(),
            proof_pointer
        );
        assert_eq!(owner.capacities().occupied(), 0);
        assert_eq!(owner.counters().retained_proof_index_rejections, 1);
        assert_eq!(owner.counters().retained_proof_kind_rejections, 1);
    }

    #[test]
    fn opaque_event_wrapper_moves_identical_event_and_proof_together() {
        let mut first = retained_data_actions(0x69, InterfaceId(3));
        let mut second = retained_data_actions(0x69, InterfaceId(3));
        let first_event_pointer = first.events.as_ptr();
        let first_proof_pointer = first.events.retained_proof().unwrap().packet_bytes_ptr();
        let second_event_pointer = second.events.as_ptr();
        let second_proof_pointer = second.events.retained_proof().unwrap().packet_bytes_ptr();
        assert_eq!(data_tag(&first.events[0]), data_tag(&second.events[0]));
        let (
            ApplicationEvent::DataReceived {
                payload: first_payload,
                ..
            },
            ApplicationEvent::DataReceived {
                payload: second_payload,
                ..
            },
        ) = (&first.events[0], &second.events[0])
        else {
            panic!("retained test envelopes must contain DATA events")
        };
        assert_eq!(first_payload, second_payload);
        assert_ne!(first_event_pointer, second_event_pointer);
        assert_ne!(first_proof_pointer, second_proof_pointer);

        core::mem::swap(&mut first.events, &mut second.events);
        assert_eq!(first.events.as_ptr(), second_event_pointer);
        assert_eq!(
            first.events.retained_proof().unwrap().packet_bytes_ptr(),
            second_proof_pointer
        );
        assert_eq!(second.events.as_ptr(), first_event_pointer);
        assert_eq!(
            second.events.retained_proof().unwrap().packet_bytes_ptr(),
            first_proof_pointer
        );

        let mut slots = [ApplicationEventSlot::new(), ApplicationEventSlot::new()];
        let mut owner = ApplicationEventOwner::new(&mut slots);
        assert_eq!(
            owner
                .try_offer_actions(first)
                .expect("whole swapped wrapper remains structurally valid")
                .accepted_retained_proofs(),
            1
        );
        assert_eq!(
            owner
                .try_offer_actions(second)
                .expect("other whole wrapper remains structurally valid")
                .accepted_retained_proofs(),
            1
        );
        owner
            .lease_next()
            .expect("first retained event")
            .discard(ApplicationEventDiscardReason::Superseded);
        owner
            .lease_next()
            .expect("second retained event")
            .discard(ApplicationEventDiscardReason::Superseded);
    }

    #[test]
    fn debug_output_redacts_application_payloads() {
        let mut slots = [ApplicationEventSlot::new()];
        let mut owner = ApplicationEventOwner::new(&mut slots);
        owner
            .try_offer_actions(NodeActions::without_retained_proofs(
                vec![ApplicationEvent::DataReceived {
                    destination: [0x31; 16],
                    payload: b"secret-application-payload".to_vec(),
                    ingress: None,
                }],
                vec![],
                0,
            ))
            .expect("event fits");
        let owner_debug = format!("{owner:?}");
        assert!(!owner_debug.contains("secret-application-payload"));
        let lease = owner.lease_next().expect("event is ready");
        let lease_debug = format!("{lease:?}");
        assert!(!lease_debug.contains("secret-application-payload"));
        assert!(lease_debug.contains("retained_proof: false"));
        lease.discard(ApplicationEventDiscardReason::PolicyRejected);
    }

    #[test]
    fn accepted_envelope_accounts_for_unroutable_scalar_without_a_slot() {
        let mut slots = [ApplicationEventSlot::new()];
        let mut owner = ApplicationEventOwner::new(&mut slots);
        let report = owner
            .try_offer_actions(NodeActions {
                unroutable_packets: 3,
                ..NodeActions::default()
            })
            .expect("scalar-only envelope needs no event slot");
        assert_eq!(report.accepted_events(), 0);
        assert_eq!(report.unroutable_packets(), 3);
        assert_eq!(owner.counters().unroutable_packets_observed, 3);
    }

    #[test]
    fn reconstructed_owner_continues_after_the_largest_retained_sequence() {
        let mut slots = [
            ApplicationEventSlot::new(),
            ApplicationEventSlot::new(),
            ApplicationEventSlot::new(),
        ];
        {
            let mut owner = ApplicationEventOwner::new(&mut slots);
            owner
                .try_offer_actions(actions(&[20, 21]))
                .expect("initial events fit");
        }

        let mut owner = ApplicationEventOwner::new(&mut slots);
        owner
            .try_offer_actions(actions(&[22]))
            .expect("reconstructed owner assigns a later sequence");
        for expected in [20, 21, 22] {
            let lease = owner.lease_next().expect("FIFO event remains available");
            assert_eq!(data_tag(lease.event()), expected);
            let _ = lease.acknowledge();
        }
        assert!(owner.lease_next().is_none());
    }

    #[test]
    fn exhausted_sequence_resets_only_after_all_retained_events_resolve() {
        let mut slots = [ApplicationEventSlot::new(), ApplicationEventSlot::new()];
        slots[0].generation = 1;
        slots[0].sequence = u64::MAX;
        slots[0].state = ApplicationEventSlotState::Ready;
        slots[0].owned = Some(OwnedApplicationEvent {
            event: data_event(30),
            retained_proof: None,
        });

        let mut owner = ApplicationEventOwner::new(&mut slots);
        let failure = owner
            .try_offer_actions(actions(&[31]))
            .expect_err("a retained MAX sequence prevents safe assignment");
        assert_eq!(
            failure.reason(),
            ApplicationEventOfferError::SequenceExhausted
        );
        assert!(failure.reason().is_retryable());
        let returned = failure.into_actions();
        assert_eq!(data_tag(&returned.events[0]), 31);

        let retained = owner
            .lease_next()
            .expect("MAX-sequence event remains owned");
        assert_eq!(data_tag(retained.event()), 30);
        retained.discard(ApplicationEventDiscardReason::Superseded);
        owner
            .try_offer_actions(returned)
            .expect("an empty owner safely restarts FIFO sequence numbering");
        let restarted = owner.lease_next().expect("restarted event is ready");
        assert_eq!(restarted.sequence().get(), 1);
        restarted.discard(ApplicationEventDiscardReason::Superseded);
    }
}
