//! Fixed-capacity ownership for transport-neutral application events.
//!
//! The owner moves complete application events and their optional proof
//! sidecars out of one [`NodeActions`] envelope only after every event has a
//! caller-provided slot and every sidecar binding is valid. Consumers lease one
//! event at a time in FIFO order. Resolving a lease transfers, discards, or
//! quarantines its exact event and sidecar; dropping an unresolved lease
//! quarantines both instead of silently releasing their slot.

use core::{fmt, mem};

use reticulum_rns_rete::{
    ApplicationEvent, ApplicationEventKind, NodeActions, RetainedInboundProof,
};

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

/// Caller-owned storage for one transport-neutral event and optional sidecar.
///
/// Construct slots in board or task-owned storage, then pass their mutable
/// slice to [`ApplicationEventOwner::new`]. Slot contents survive dropping and
/// reconstructing the owner over the same slice.
#[must_use = "an application-event slot must remain owned for as long as its owner may use it"]
pub struct ApplicationEventSlot {
    generation: u64,
    sequence: u64,
    state: ApplicationEventSlotState,
    event: Option<ApplicationEvent>,
    retained_proof: Option<RetainedInboundProof>,
}

impl ApplicationEventSlot {
    /// Construct one empty reusable slot.
    pub const fn new() -> Self {
        Self {
            generation: 0,
            sequence: 0,
            state: ApplicationEventSlotState::Vacant,
            event: None,
            retained_proof: None,
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
            .field("event", &self.event.as_ref().map(|_| "<redacted>"))
            .field("retained_proof", &self.retained_proof.is_some())
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
    /// Retained proof sidecars moved into the same slots as their events.
    pub accepted_proof_sidecars: u64,
    /// Events transferred to consumers by acknowledgement.
    pub acknowledged_events: u64,
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
    /// Unresolved leases converted to quarantine dispositions on drop.
    pub unresolved_lease_quarantines: u64,
    /// Offers rejected because current occupancy left too few slots.
    pub full_rejections: u64,
    /// Offers rejected because the atomic batch can never fit.
    pub oversized_batch_rejections: u64,
    /// Offers rejected because they unexpectedly retained packet owners.
    pub packet_owner_rejections: u64,
    /// Offers rejected because a proof sidecar named no event in its envelope.
    pub proof_sidecar_index_rejections: u64,
    /// Offers rejected because more than one proof sidecar named one event.
    pub duplicate_proof_sidecar_rejections: u64,
    /// Offers rejected because a proof sidecar named an incompatible event.
    pub proof_sidecar_kind_rejections: u64,
    /// Offers rejected because a proof sidecar's private DATA binding changed.
    pub proof_sidecar_binding_rejections: u64,
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
    /// A retained proof sidecar names no event in its action envelope.
    ProofSidecarIndexOutOfRange {
        /// Invalid zero-based event index.
        event_index: usize,
        /// Events present in the envelope.
        event_count: usize,
    },
    /// More than one retained proof sidecar names the same event.
    DuplicateProofSidecar {
        /// Duplicated zero-based event index.
        event_index: usize,
    },
    /// A retained DATA proof sidecar names an event of another kind.
    ProofSidecarEventKindMismatch {
        /// Zero-based index of the incompatible event.
        event_index: usize,
        /// Payload-free kind of the incompatible event.
        event_kind: ApplicationEventKind,
    },
    /// A retained proof sidecar no longer names the exact DATA event it was
    /// created beside.
    ProofSidecarEventBindingMismatch {
        /// Zero-based index whose private event binding did not match.
        event_index: usize,
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
            .field("proof_sidecar_count", &self.actions.proof_sidecars.len())
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
    accepted_proof_sidecars: usize,
    unroutable_packets: usize,
}

impl ApplicationEventOfferReport {
    /// Events moved into caller-owned slots.
    pub const fn accepted_events(self) -> usize {
        self.accepted_events
    }

    /// Retained proof sidecars moved with their exact events.
    pub const fn accepted_proof_sidecars(self) -> usize {
        self.accepted_proof_sidecars
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
                slot.event.is_some(),
                matches!(
                    slot.state,
                    ApplicationEventSlotState::Ready
                        | ApplicationEventSlotState::Leased
                        | ApplicationEventSlotState::Quarantined(_)
                )
            );
            debug_assert!(slot.event.is_some() || slot.retained_proof.is_none());
            debug_assert!(
                slot.retained_proof.is_none()
                    || slot
                        .event
                        .as_ref()
                        .is_some_and(|event| event.kind() == ApplicationEventKind::DataReceived)
            );
            if slot.event.is_some() {
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

        for (sidecar_position, sidecar) in actions.proof_sidecars.iter().enumerate() {
            let event_index = sidecar.event_index();
            let Some(event) = actions.events.get(event_index) else {
                self.counters.proof_sidecar_index_rejections = self
                    .counters
                    .proof_sidecar_index_rejections
                    .saturating_add(1);
                return Err(ApplicationEventOfferFailure {
                    reason: ApplicationEventOfferError::ProofSidecarIndexOutOfRange {
                        event_index,
                        event_count: actions.events.len(),
                    },
                    actions,
                });
            };
            if actions.proof_sidecars[..sidecar_position]
                .iter()
                .any(|prior| prior.event_index() == event_index)
            {
                self.counters.duplicate_proof_sidecar_rejections = self
                    .counters
                    .duplicate_proof_sidecar_rejections
                    .saturating_add(1);
                return Err(ApplicationEventOfferFailure {
                    reason: ApplicationEventOfferError::DuplicateProofSidecar { event_index },
                    actions,
                });
            }
            let event_kind = event.kind();
            if event_kind != ApplicationEventKind::DataReceived {
                self.counters.proof_sidecar_kind_rejections = self
                    .counters
                    .proof_sidecar_kind_rejections
                    .saturating_add(1);
                return Err(ApplicationEventOfferFailure {
                    reason: ApplicationEventOfferError::ProofSidecarEventKindMismatch {
                        event_index,
                        event_kind,
                    },
                    actions,
                });
            }
            if !sidecar.is_bound_to(event_index, event) {
                self.counters.proof_sidecar_binding_rejections = self
                    .counters
                    .proof_sidecar_binding_rejections
                    .saturating_add(1);
                return Err(ApplicationEventOfferFailure {
                    reason: ApplicationEventOfferError::ProofSidecarEventBindingMismatch {
                        event_index,
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
        let accepted_proof_sidecars = actions.proof_sidecars.len();
        let unroutable_packets = actions.unroutable_packets;
        let events = mem::take(&mut actions.events);
        let mut proof_sidecars = mem::take(&mut actions.proof_sidecars);
        let mut next_sequence = sequence_range.map(|(first, _)| first).unwrap_or(0);
        for (event_index, event) in events.into_iter().enumerate() {
            let retained_proof = proof_sidecars
                .iter()
                .position(|sidecar| sidecar.event_index() == event_index)
                .map(|sidecar_position| {
                    let sidecar = proof_sidecars.swap_remove(sidecar_position);
                    let (bound_event_index, proof) = sidecar.into_parts();
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
            slot.event = Some(event);
            slot.retained_proof = retained_proof;
            slot.state = ApplicationEventSlotState::Ready;
            next_sequence = next_sequence.saturating_add(1);
        }
        debug_assert!(proof_sidecars.is_empty());
        if let Some((_, last)) = sequence_range {
            self.next_sequence = last.checked_add(1);
        }

        self.counters.accepted_batches = self.counters.accepted_batches.saturating_add(1);
        self.counters.accepted_events = self
            .counters
            .accepted_events
            .saturating_add(u64::try_from(accepted_events).unwrap_or(u64::MAX));
        self.counters.accepted_proof_sidecars = self
            .counters
            .accepted_proof_sidecars
            .saturating_add(u64::try_from(accepted_proof_sidecars).unwrap_or(u64::MAX));
        self.counters.unroutable_packets_observed = self
            .counters
            .unroutable_packets_observed
            .saturating_add(u64::try_from(unroutable_packets).unwrap_or(u64::MAX));

        Ok(ApplicationEventOfferReport {
            accepted_events,
            accepted_proof_sidecars,
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

    fn resolve_slot(&mut self, id: ApplicationEventId) -> AcknowledgedApplicationEvent {
        let slot = &mut self.slots[id.slot.0];
        assert_eq!(slot.generation, id.generation.0);
        assert_eq!(slot.state, ApplicationEventSlotState::Leased);
        let event = slot
            .event
            .take()
            .expect("a valid application-event lease retains its exact event");
        let retained_proof = slot.retained_proof.take();
        slot.sequence = 0;
        slot.state = if slot.generation == u64::MAX {
            ApplicationEventSlotState::Retired
        } else {
            ApplicationEventSlotState::Vacant
        };
        AcknowledgedApplicationEvent {
            event,
            retained_proof,
        }
    }

    fn quarantine_slot(
        &mut self,
        id: ApplicationEventId,
        reason: ApplicationEventQuarantineReason,
    ) {
        let slot = &mut self.slots[id.slot.0];
        assert_eq!(slot.generation, id.generation.0);
        assert_eq!(slot.state, ApplicationEventSlotState::Leased);
        let event = slot
            .event
            .as_ref()
            .expect("a valid application-event lease retains its exact event");
        debug_assert!(
            slot.retained_proof.is_none() || event.kind() == ApplicationEventKind::DataReceived
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

/// Exact event and optional retained proof transferred by acknowledgement.
///
/// The semantic [`ApplicationEvent`] remains unchanged. A proof sidecar, when
/// present, stays move-only and must be released or explicitly discarded by
/// the acknowledging consumer.
#[must_use = "an acknowledged application event and retained proof must be handled"]
pub struct AcknowledgedApplicationEvent {
    event: ApplicationEvent,
    retained_proof: Option<RetainedInboundProof>,
}

impl AcknowledgedApplicationEvent {
    /// Borrow the exact semantic event without copying its payload.
    pub const fn event(&self) -> &ApplicationEvent {
        &self.event
    }

    /// Borrow the retained direct proof associated with this event, if any.
    pub const fn retained_proof(&self) -> Option<&RetainedInboundProof> {
        self.retained_proof.as_ref()
    }

    /// Consume this owner into its exact event and optional proof sidecar.
    pub fn into_parts(self) -> (ApplicationEvent, Option<RetainedInboundProof>) {
        (self.event, self.retained_proof)
    }
}

impl fmt::Debug for AcknowledgedApplicationEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcknowledgedApplicationEvent")
            .field("event_kind", &self.event.kind())
            .field("retained_proof", &self.retained_proof.is_some())
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

impl ApplicationEventLease<'_, '_> {
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
            .event
            .as_ref()
            .expect("a valid application-event lease retains its exact event")
    }

    /// Borrow the move-only retained proof bound to this event, if any.
    pub fn retained_proof(&self) -> Option<&RetainedInboundProof> {
        self.owner.slots[self.id.slot.0].retained_proof.as_ref()
    }

    /// Acknowledge delivery and transfer the exact event plus any retained
    /// proof sidecar to the consumer.
    pub fn acknowledge(mut self) -> AcknowledgedApplicationEvent {
        let acknowledged = self.owner.resolve_slot(self.id);
        self.owner.counters.acknowledged_events =
            self.owner.counters.acknowledged_events.saturating_add(1);
        self.resolved = true;
        acknowledged
    }

    /// Explicitly discard this event for one typed product reason and release
    /// its slot.
    pub fn discard(mut self, reason: ApplicationEventDiscardReason) {
        let acknowledged = self.owner.resolve_slot(self.id);
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
        drop(acknowledged);
    }

    /// Retain this event outside the normal ready queue.
    pub fn quarantine(mut self, reason: ApplicationEventQuarantineReason) {
        self.owner.quarantine_slot(self.id, reason);
        self.owner.counters.explicit_quarantines =
            self.owner.counters.explicit_quarantines.saturating_add(1);
        self.resolved = true;
    }
}

impl fmt::Debug for ApplicationEventLease<'_, '_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApplicationEventLease")
            .field("id", &self.id)
            .field("sequence", &self.sequence)
            .field("prior_quarantine", &self.prior_quarantine)
            .field("retained_proof", &self.retained_proof().is_some())
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

    use rand_core::{CryptoRng, RngCore};
    use reticulum_rns_rete::{
        ApplicationEvent, EmbeddedNode, EmbeddedNodeConfig, InboundProofPolicy, InterfaceId,
        RNS_MTU, TxTarget, identity_from_private_key, parse_packet,
    };
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

    type TestRnsNode = EmbeddedNode<4, 4, 8, 2>;

    fn data_event(tag: u8) -> ApplicationEvent {
        ApplicationEvent::DataReceived {
            destination: [tag; 16],
            payload: vec![tag, tag.wrapping_add(1), tag.wrapping_add(2)],
        }
    }

    fn actions(tags: &[u8]) -> NodeActions {
        NodeActions {
            events: tags.iter().copied().map(data_event).collect::<Vec<_>>(),
            ..NodeActions::default()
        }
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
        assert_eq!(actions.proof_sidecars.len(), 1);
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
        let first_event = quarantined.acknowledge();
        assert_eq!(data_tag(first_event.event()), 10);
        assert!(first_event.retained_proof().is_none());

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
    fn proof_sidecar_survives_pressure_retry_unresolved_drop_and_owner_reconstruction() {
        let source = InterfaceId(9);
        let mut slots = [ApplicationEventSlot::new()];
        let mut owner = ApplicationEventOwner::new(&mut slots);
        owner
            .try_offer_actions(actions(&[1]))
            .expect("first event occupies the sole slot");

        let offered = retained_data_actions(0x41, source);
        let event_pointer = offered.events.as_ptr();
        let sidecar_pointer = offered.proof_sidecars.as_ptr();
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
        assert_eq!(offered.proof_sidecars.as_ptr(), sidecar_pointer);

        owner
            .lease_next()
            .expect("occupying event remains ready")
            .discard(ApplicationEventDiscardReason::Superseded);
        let report = owner
            .try_offer_actions(offered)
            .expect("the exact rejected envelope is retryable");
        assert_eq!(report.accepted_events(), 1);
        assert_eq!(report.accepted_proof_sidecars(), 1);
        assert_eq!(owner.counters().accepted_proof_sidecars, 1);

        let lease = owner.lease_next().expect("retained DATA is ready");
        assert_eq!(lease.event().kind(), ApplicationEventKind::DataReceived);
        assert!(lease.retained_proof().is_some());
        assert!(format!("{lease:?}").contains("retained_proof: true"));
        lease.quarantine(ApplicationEventQuarantineReason::ConsumerDeferred);
        assert_eq!(owner.capacities().quarantined, 1);

        let lease = owner
            .lease_next_quarantined()
            .expect("explicitly quarantined event and proof remain recoverable");
        assert_eq!(
            lease.quarantine_reason(),
            Some(ApplicationEventQuarantineReason::ConsumerDeferred)
        );
        assert!(lease.retained_proof().is_some());
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
        assert!(lease.retained_proof().is_some());
        let acknowledged = lease.acknowledge();
        assert_eq!(
            acknowledged.event().kind(),
            ApplicationEventKind::DataReceived
        );
        let (_, proof) = acknowledged.into_parts();
        let packet = proof
            .expect("the exact proof sidecar survives")
            .into_packet();
        assert_eq!(packet.target(), TxTarget::Only(source));
        assert_eq!(
            parse_packet(packet.bytes())
                .expect("retained bytes remain a valid packet")
                .packet_type,
            reticulum_rns_rete::PacketType::Proof
        );
    }

    #[test]
    fn malformed_proof_sidecar_bindings_fail_before_owner_or_envelope_mutation() {
        let mut slots = [ApplicationEventSlot::new()];
        let mut owner = ApplicationEventOwner::new(&mut slots);

        let mut out_of_range = retained_data_actions(0x51, InterfaceId(2));
        out_of_range.events.clear();
        let sidecar_pointer = out_of_range.proof_sidecars.as_ptr();
        let failure = owner
            .try_offer_actions(out_of_range)
            .expect_err("a sidecar cannot name a missing event");
        assert_eq!(
            failure.reason(),
            ApplicationEventOfferError::ProofSidecarIndexOutOfRange {
                event_index: 0,
                event_count: 0,
            }
        );
        let out_of_range = failure.into_actions();
        assert_eq!(out_of_range.proof_sidecars.as_ptr(), sidecar_pointer);
        assert!(out_of_range.events.is_empty());
        assert_eq!(owner.capacities().occupied(), 0);

        let mut wrong_kind = retained_data_actions(0x61, InterfaceId(3));
        wrong_kind.events[0] = ApplicationEvent::Tick {
            expired_paths: 0,
            closed_links: 0,
        };
        let event_pointer = wrong_kind.events.as_ptr();
        let sidecar_pointer = wrong_kind.proof_sidecars.as_ptr();
        let failure = owner
            .try_offer_actions(wrong_kind)
            .expect_err("a retained DATA proof cannot bind another event kind");
        assert_eq!(
            failure.reason(),
            ApplicationEventOfferError::ProofSidecarEventKindMismatch {
                event_index: 0,
                event_kind: ApplicationEventKind::Tick,
            }
        );
        let wrong_kind = failure.into_actions();
        assert_eq!(wrong_kind.events.as_ptr(), event_pointer);
        assert_eq!(wrong_kind.proof_sidecars.as_ptr(), sidecar_pointer);
        assert_eq!(owner.capacities().occupied(), 0);

        let mut wrong_binding = retained_data_actions(0x69, InterfaceId(3));
        wrong_binding.events[0] = ApplicationEvent::DataReceived {
            destination: [0x69; 16],
            payload: b"replacement DATA event".to_vec(),
        };
        let event_pointer = wrong_binding.events.as_ptr();
        let sidecar_pointer = wrong_binding.proof_sidecars.as_ptr();
        let failure = owner
            .try_offer_actions(wrong_binding)
            .expect_err("a sidecar cannot authorize a replacement DATA event");
        assert_eq!(
            failure.reason(),
            ApplicationEventOfferError::ProofSidecarEventBindingMismatch { event_index: 0 }
        );
        let wrong_binding = failure.into_actions();
        assert_eq!(wrong_binding.events.as_ptr(), event_pointer);
        assert_eq!(wrong_binding.proof_sidecars.as_ptr(), sidecar_pointer);
        assert_eq!(owner.capacities().occupied(), 0);

        let mut duplicate = retained_data_actions(0x71, InterfaceId(4));
        let another = retained_data_actions(0x81, InterfaceId(5));
        duplicate.proof_sidecars.extend(another.proof_sidecars);
        let event_pointer = duplicate.events.as_ptr();
        let sidecar_pointer = duplicate.proof_sidecars.as_ptr();
        let failure = owner
            .try_offer_actions(duplicate)
            .expect_err("one event cannot own two retained proofs");
        assert_eq!(
            failure.reason(),
            ApplicationEventOfferError::DuplicateProofSidecar { event_index: 0 }
        );
        let duplicate = failure.into_actions();
        assert_eq!(duplicate.events.as_ptr(), event_pointer);
        assert_eq!(duplicate.proof_sidecars.as_ptr(), sidecar_pointer);
        assert_eq!(duplicate.proof_sidecars.len(), 2);
        assert_eq!(owner.capacities().occupied(), 0);
        assert_eq!(owner.counters().proof_sidecar_index_rejections, 1);
        assert_eq!(owner.counters().proof_sidecar_kind_rejections, 1);
        assert_eq!(owner.counters().proof_sidecar_binding_rejections, 1);
        assert_eq!(owner.counters().duplicate_proof_sidecar_rejections, 1);
    }

    #[test]
    fn debug_output_redacts_application_payloads() {
        let mut slots = [ApplicationEventSlot::new()];
        let mut owner = ApplicationEventOwner::new(&mut slots);
        owner
            .try_offer_actions(NodeActions {
                events: vec![ApplicationEvent::DataReceived {
                    destination: [0x31; 16],
                    payload: b"secret-application-payload".to_vec(),
                }],
                ..NodeActions::default()
            })
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
        slots[0].event = Some(data_event(30));

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
