//! Fixed-capacity ownership for transport-neutral application events.
//!
//! The owner moves complete application events out of one [`NodeActions`]
//! envelope only after every event has a caller-provided slot. Consumers lease
//! one event at a time in FIFO order. Resolving a lease transfers, discards, or
//! quarantines its exact event; dropping an unresolved lease quarantines the
//! event instead of silently releasing its slot.

use core::{fmt, mem};

use reticulum_rns_rete::{ApplicationEvent, NodeActions};

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

/// Caller-owned storage for one transport-neutral application event.
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
}

impl ApplicationEventSlot {
    /// Construct one empty reusable slot.
    pub const fn new() -> Self {
        Self {
            generation: 0,
            sequence: 0,
            state: ApplicationEventSlotState::Vacant,
            event: None,
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
    unroutable_packets: usize,
}

impl ApplicationEventOfferReport {
    /// Events moved into caller-owned slots.
    pub const fn accepted_events(self) -> usize {
        self.accepted_events
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
        let unroutable_packets = actions.unroutable_packets;
        let events = mem::take(&mut actions.events);
        let mut next_sequence = sequence_range.map(|(first, _)| first).unwrap_or(0);
        for event in events {
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
            slot.state = ApplicationEventSlotState::Ready;
            next_sequence = next_sequence.saturating_add(1);
        }
        if let Some((_, last)) = sequence_range {
            self.next_sequence = last.checked_add(1);
        }

        self.counters.accepted_batches = self.counters.accepted_batches.saturating_add(1);
        self.counters.accepted_events = self
            .counters
            .accepted_events
            .saturating_add(u64::try_from(accepted_events).unwrap_or(u64::MAX));
        self.counters.unroutable_packets_observed = self
            .counters
            .unroutable_packets_observed
            .saturating_add(u64::try_from(unroutable_packets).unwrap_or(u64::MAX));

        Ok(ApplicationEventOfferReport {
            accepted_events,
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

    fn resolve_slot(&mut self, id: ApplicationEventId) -> ApplicationEvent {
        let slot = &mut self.slots[id.slot.0];
        assert_eq!(slot.generation, id.generation.0);
        assert_eq!(slot.state, ApplicationEventSlotState::Leased);
        let event = slot
            .event
            .take()
            .expect("a valid application-event lease retains its exact event");
        slot.sequence = 0;
        slot.state = if slot.generation == u64::MAX {
            ApplicationEventSlotState::Retired
        } else {
            ApplicationEventSlotState::Vacant
        };
        event
    }

    fn quarantine_slot(
        &mut self,
        id: ApplicationEventId,
        reason: ApplicationEventQuarantineReason,
    ) {
        let slot = &mut self.slots[id.slot.0];
        assert_eq!(slot.generation, id.generation.0);
        assert_eq!(slot.state, ApplicationEventSlotState::Leased);
        debug_assert!(slot.event.is_some());
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

    /// Acknowledge delivery and transfer the exact event to the consumer.
    pub fn acknowledge(mut self) -> ApplicationEvent {
        let event = self.owner.resolve_slot(self.id);
        self.owner.counters.acknowledged_events =
            self.owner.counters.acknowledged_events.saturating_add(1);
        self.resolved = true;
        event
    }

    /// Explicitly discard this event for one typed product reason and release
    /// its slot.
    pub fn discard(mut self, reason: ApplicationEventDiscardReason) {
        let event = self.owner.resolve_slot(self.id);
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
        drop(event);
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

    use reticulum_rns_rete::ApplicationEvent;
    use std::{format, vec, vec::Vec};

    use super::*;

    fn data_event(tag: u8) -> ApplicationEvent {
        ApplicationEvent::DataReceived {
            destination: [tag; 16],
            payload: vec![tag, tag.wrapping_add(1), tag.wrapping_add(2)],
        }
    }

    fn actions(tags: &[u8]) -> NodeActions {
        NodeActions {
            events: tags.iter().copied().map(data_event).collect::<Vec<_>>(),
            packets: Vec::new(),
            unroutable_packets: 0,
        }
    }

    fn data_tag(event: &ApplicationEvent) -> u8 {
        let ApplicationEvent::DataReceived { destination, .. } = event else {
            panic!("test event changed variant")
        };
        destination[0]
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
    fn debug_output_redacts_application_payloads() {
        let mut slots = [ApplicationEventSlot::new()];
        let mut owner = ApplicationEventOwner::new(&mut slots);
        owner
            .try_offer_actions(NodeActions {
                events: vec![ApplicationEvent::DataReceived {
                    destination: [0x31; 16],
                    payload: b"secret-application-payload".to_vec(),
                }],
                packets: Vec::new(),
                unroutable_packets: 0,
            })
            .expect("event fits");
        let owner_debug = format!("{owner:?}");
        assert!(!owner_debug.contains("secret-application-payload"));
        let lease = owner.lease_next().expect("event is ready");
        let lease_debug = format!("{lease:?}");
        assert!(!lease_debug.contains("secret-application-payload"));
        lease.discard(ApplicationEventDiscardReason::PolicyRejected);
    }

    #[test]
    fn accepted_envelope_accounts_for_unroutable_scalar_without_a_slot() {
        let mut slots = [ApplicationEventSlot::new()];
        let mut owner = ApplicationEventOwner::new(&mut slots);
        let report = owner
            .try_offer_actions(NodeActions {
                events: Vec::new(),
                packets: Vec::new(),
                unroutable_packets: 3,
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
