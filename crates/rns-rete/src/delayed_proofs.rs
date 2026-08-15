//! Fixed caller-backed ownership for proofs released after durable receipt.
//!
//! A producer first reserves one empty slot for an exact application-event
//! identity. That reservation can be held across fallible persistence work:
//! dropping it restores capacity, while committing it moves one proof into the
//! already-selected slot without allocation, copying, or another failure
//! point. Consumers lease committed proofs in reservation order. Dropping an
//! unresolved lease restores it to the ready queue.

use core::fmt;

use crate::application_events::ApplicationEventId;
use crate::embedded::{InboundProofEvidence, NodeActions, RetainedInboundProof};

/// Stable index of one caller-provided delayed-proof slot.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DelayedProofSlotId(usize);

impl DelayedProofSlotId {
    /// Zero-based index in the slot slice supplied to the owner.
    pub const fn get(self) -> usize {
        self.0
    }
}

/// Incarnation of a proof occupying one stable slot.
///
/// A slot receives a strictly newer generation on every reservation. Slots
/// whose generation reaches `u64::MAX` retire after their current reservation
/// or proof resolves instead of wrapping to a stale identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DelayedProofGeneration(u64);

impl DelayedProofGeneration {
    /// Nonzero scalar generation.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// FIFO sequence assigned when proof capacity is reserved.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DelayedProofSequence(u64);

impl DelayedProofSequence {
    /// Nonzero scalar sequence.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Complete generation-safe identity of one delayed proof.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DelayedProofId {
    slot: DelayedProofSlotId,
    generation: DelayedProofGeneration,
}

impl DelayedProofId {
    /// Stable caller-provided slot holding this proof.
    pub const fn slot(self) -> DelayedProofSlotId {
        self.slot
    }

    /// Incarnation assigned when this slot was reserved.
    pub const fn generation(self) -> DelayedProofGeneration {
        self.generation
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DelayedProofSlotState {
    Vacant,
    Reserved,
    Ready,
    Leased,
    Retired,
}

/// Caller-owned storage for one delayed proof.
///
/// Construct slots in board or task-owned storage, then pass their mutable
/// slice to [`DelayedProofOwner::new`]. Ready contents survive dropping and
/// reconstructing the owner over the same slice.
#[must_use = "a delayed-proof slot must outlive every owner bound to it"]
pub struct DelayedProofSlot {
    generation: u64,
    sequence: u64,
    state: DelayedProofSlotState,
    event_id: Option<ApplicationEventId>,
    proof: Option<RetainedInboundProof>,
}

impl DelayedProofSlot {
    /// Construct one empty reusable slot.
    pub const fn new() -> Self {
        Self {
            generation: 0,
            sequence: 0,
            state: DelayedProofSlotState::Vacant,
            event_id: None,
            proof: None,
        }
    }
}

impl Default for DelayedProofSlot {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for DelayedProofSlot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DelayedProofSlot")
            .field("generation", &self.generation)
            .field("sequence", &self.sequence)
            .field("state", &self.state)
            .field("event_id", &self.event_id)
            .field("proof_present", &self.proof.is_some())
            .finish()
    }
}

/// Scalar occupancy of one delayed-proof owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DelayedProofCapacitySnapshot {
    /// Total caller-provided slots, including permanently retired slots.
    pub capacity: usize,
    /// Slots available for a new reservation.
    pub vacant: usize,
    /// Slots reserved across fallible producer work.
    pub reserved: usize,
    /// Proofs waiting in normal FIFO order.
    pub ready: usize,
    /// Proofs currently borrowed by a consumer lease.
    pub leased: usize,
    /// Slots retired to prevent generation wraparound.
    pub retired: usize,
}

impl DelayedProofCapacitySnapshot {
    /// Slots that have not permanently retired.
    pub const fn reusable_capacity(self) -> usize {
        self.capacity - self.retired
    }

    /// Slots currently reserved or holding proofs.
    pub const fn occupied(self) -> usize {
        self.reserved + self.ready + self.leased
    }
}

/// Cumulative scalar observations for one owner binding.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DelayedProofOwnerCounters {
    /// Reservation attempts.
    pub reservation_attempts: u64,
    /// Reservations successfully created.
    pub reservations_created: u64,
    /// Reservations committed into ready proofs.
    pub reservations_committed: u64,
    /// Reservations dropped before a proof was committed.
    pub reservations_released: u64,
    /// Ready proofs leased to consumers.
    pub proof_leases_created: u64,
    /// Proofs transferred out by explicit release.
    pub proofs_released: u64,
    /// Unresolved leases restored to the ready queue on drop.
    pub unresolved_leases_restored: u64,
    /// Reservations rejected because no reusable slot was vacant.
    pub full_rejections: u64,
    /// Reservations rejected because FIFO sequence space was exhausted.
    pub sequence_exhaustion_rejections: u64,
}

/// Why delayed-proof capacity could not be reserved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DelayedProofReservationError {
    /// No reusable caller-provided slot is currently vacant.
    Full {
        /// Total slots that have not permanently retired.
        capacity: usize,
    },
    /// Existing ready or leased state prevents assigning a non-wrapping FIFO
    /// sequence. Resolving all existing proofs permits a deterministic restart.
    SequenceExhausted,
}

/// Fixed-capacity delayed-proof owner over caller-provided storage.
#[must_use = "dropping the owner leaves every ready proof in caller-owned storage"]
pub struct DelayedProofOwner<'slots> {
    slots: &'slots mut [DelayedProofSlot],
    next_sequence: Option<u64>,
    counters: DelayedProofOwnerCounters,
}

impl<'slots> DelayedProofOwner<'slots> {
    /// Bind one caller-owned slot slice.
    ///
    /// Ready proofs from an earlier binding remain available. A leaked
    /// consumer lease is conservatively restored to ready. An uncommitted
    /// reservation contains no proof and is restored to vacant (or retired at
    /// generation exhaustion).
    pub fn new(slots: &'slots mut [DelayedProofSlot]) -> Self {
        for slot in slots.iter_mut() {
            match slot.state {
                DelayedProofSlotState::Reserved => {
                    debug_assert!(slot.proof.is_none());
                    slot.sequence = 0;
                    slot.event_id = None;
                    slot.state = if slot.generation == u64::MAX {
                        DelayedProofSlotState::Retired
                    } else {
                        DelayedProofSlotState::Vacant
                    };
                }
                DelayedProofSlotState::Leased => {
                    debug_assert!(slot.proof.is_some());
                    debug_assert!(slot.event_id.is_some());
                    slot.state = DelayedProofSlotState::Ready;
                }
                DelayedProofSlotState::Vacant | DelayedProofSlotState::Retired => {
                    debug_assert!(slot.proof.is_none());
                    debug_assert!(slot.event_id.is_none());
                }
                DelayedProofSlotState::Ready => {
                    debug_assert!(slot.proof.is_some());
                    debug_assert!(slot.event_id.is_some());
                }
            }
        }
        let maximum_sequence = slots
            .iter()
            .filter(|slot| matches!(slot.state, DelayedProofSlotState::Ready))
            .map(|slot| slot.sequence)
            .max();
        Self {
            slots,
            next_sequence: maximum_sequence.map_or(Some(1), |maximum| maximum.checked_add(1)),
            counters: DelayedProofOwnerCounters::default(),
        }
    }

    /// Current scalar slot occupancy.
    pub fn capacities(&self) -> DelayedProofCapacitySnapshot {
        let mut snapshot = DelayedProofCapacitySnapshot {
            capacity: self.slots.len(),
            vacant: 0,
            reserved: 0,
            ready: 0,
            leased: 0,
            retired: 0,
        };
        for slot in self.slots.iter() {
            match slot.state {
                DelayedProofSlotState::Vacant => snapshot.vacant += 1,
                DelayedProofSlotState::Reserved => snapshot.reserved += 1,
                DelayedProofSlotState::Ready => snapshot.ready += 1,
                DelayedProofSlotState::Leased => snapshot.leased += 1,
                DelayedProofSlotState::Retired => snapshot.retired += 1,
            }
        }
        snapshot
    }

    /// Cumulative counters for this owner binding.
    pub const fn counters(&self) -> DelayedProofOwnerCounters {
        self.counters
    }

    /// Reserve one exact slot before starting fallible producer work.
    ///
    /// Success selects the final slot, generation, diagnostic event identity,
    /// and FIFO sequence. This crate-private primitive is only exposed through
    /// [`ApplicationEventLease::try_reserve_delayed`](crate::ApplicationEventLease::try_reserve_delayed),
    /// which binds the reservation to the exact proof-bearing lease before it
    /// can cross durable-store I/O.
    pub(crate) fn try_reserve(
        &mut self,
        event_id: ApplicationEventId,
    ) -> Result<DelayedProofReservation<'_, 'slots>, DelayedProofReservationError> {
        self.counters.reservation_attempts = self.counters.reservation_attempts.saturating_add(1);
        let capacities = self.capacities();
        if capacities.vacant == 0 {
            self.counters.full_rejections = self.counters.full_rejections.saturating_add(1);
            return Err(DelayedProofReservationError::Full {
                capacity: capacities.reusable_capacity(),
            });
        }
        if self.next_sequence.is_none() && capacities.occupied() == 0 {
            self.next_sequence = Some(1);
        }
        let Some(sequence) = self.next_sequence else {
            self.counters.sequence_exhaustion_rejections = self
                .counters
                .sequence_exhaustion_rejections
                .saturating_add(1);
            return Err(DelayedProofReservationError::SequenceExhausted);
        };
        let index = self
            .slots
            .iter()
            .position(|slot| matches!(slot.state, DelayedProofSlotState::Vacant))
            .expect("vacant capacity was measured before delayed-proof reservation");
        let slot = &mut self.slots[index];
        slot.generation = slot
            .generation
            .checked_add(1)
            .expect("generation-exhausted delayed-proof slots are retired");
        slot.sequence = sequence;
        slot.state = DelayedProofSlotState::Reserved;
        slot.event_id = Some(event_id);
        debug_assert!(slot.proof.is_none());
        let generation = slot.generation;
        self.next_sequence = sequence.checked_add(1);
        self.counters.reservations_created = self.counters.reservations_created.saturating_add(1);
        Ok(DelayedProofReservation {
            owner: self,
            id: DelayedProofId {
                slot: DelayedProofSlotId(index),
                generation: DelayedProofGeneration(generation),
            },
            event_id,
            sequence: DelayedProofSequence(sequence),
            resolved: false,
        })
    }

    /// Lease the oldest committed proof in FIFO reservation order.
    pub fn lease_next(&mut self) -> Option<DelayedProofLease<'_, 'slots>> {
        let index = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| matches!(slot.state, DelayedProofSlotState::Ready))
            .min_by_key(|(_, slot)| slot.sequence)
            .map(|(index, _)| index)?;
        let slot = &mut self.slots[index];
        slot.state = DelayedProofSlotState::Leased;
        let id = DelayedProofId {
            slot: DelayedProofSlotId(index),
            generation: DelayedProofGeneration(slot.generation),
        };
        let event_id = slot
            .event_id
            .expect("a ready delayed proof retains its application-event identity");
        let sequence = DelayedProofSequence(slot.sequence);
        self.counters.proof_leases_created = self.counters.proof_leases_created.saturating_add(1);
        Some(DelayedProofLease {
            owner: self,
            id,
            event_id,
            sequence,
            resolved: false,
        })
    }

    fn commit_reserved(
        &mut self,
        id: DelayedProofId,
        event_id: ApplicationEventId,
        proof: RetainedInboundProof,
    ) {
        let slot = &mut self.slots[id.slot.0];
        assert_eq!(slot.generation, id.generation.0);
        assert_eq!(slot.state, DelayedProofSlotState::Reserved);
        assert_eq!(slot.event_id, Some(event_id));
        assert!(slot.proof.is_none());
        slot.proof = Some(proof);
        slot.state = DelayedProofSlotState::Ready;
        self.counters.reservations_committed =
            self.counters.reservations_committed.saturating_add(1);
    }

    fn release_reservation(&mut self, id: DelayedProofId) {
        let slot = &mut self.slots[id.slot.0];
        assert_eq!(slot.generation, id.generation.0);
        assert_eq!(slot.state, DelayedProofSlotState::Reserved);
        debug_assert!(slot.proof.is_none());
        slot.sequence = 0;
        slot.event_id = None;
        slot.state = if slot.generation == u64::MAX {
            DelayedProofSlotState::Retired
        } else {
            DelayedProofSlotState::Vacant
        };
        self.counters.reservations_released = self.counters.reservations_released.saturating_add(1);
    }

    fn release_proof(&mut self, id: DelayedProofId) -> RetainedInboundProof {
        let slot = &mut self.slots[id.slot.0];
        assert_eq!(slot.generation, id.generation.0);
        assert_eq!(slot.state, DelayedProofSlotState::Leased);
        let proof = slot
            .proof
            .take()
            .expect("a valid delayed-proof lease retains its exact proof");
        slot.sequence = 0;
        slot.event_id = None;
        slot.state = if slot.generation == u64::MAX {
            DelayedProofSlotState::Retired
        } else {
            DelayedProofSlotState::Vacant
        };
        self.counters.proofs_released = self.counters.proofs_released.saturating_add(1);
        proof
    }

    fn restore_ready(&mut self, id: DelayedProofId) {
        let slot = &mut self.slots[id.slot.0];
        assert_eq!(slot.generation, id.generation.0);
        assert_eq!(slot.state, DelayedProofSlotState::Leased);
        debug_assert!(slot.proof.is_some());
        slot.state = DelayedProofSlotState::Ready;
        self.counters.unresolved_leases_restored =
            self.counters.unresolved_leases_restored.saturating_add(1);
    }
}

impl fmt::Debug for DelayedProofOwner<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DelayedProofOwner")
            .field("capacities", &self.capacities())
            .field("counters", &self.counters)
            .field("proofs", &"<redacted>")
            .finish()
    }
}

/// Capacity reserved for one delayed proof before fallible producer work.
///
/// Dropping this owner before a correlated application-event acknowledgement
/// restores capacity. A successful acknowledgement consumes the reservation
/// and moves the proof into its final ready slot without allocation, copying,
/// or failure.
#[must_use = "a delayed-proof reservation must be committed or released"]
pub(crate) struct DelayedProofReservation<'owner, 'slots> {
    owner: &'owner mut DelayedProofOwner<'slots>,
    id: DelayedProofId,
    event_id: ApplicationEventId,
    sequence: DelayedProofSequence,
    resolved: bool,
}

impl DelayedProofReservation<'_, '_> {
    /// Generation-safe identity selected for the eventual ready proof.
    pub(crate) const fn id(&self) -> DelayedProofId {
        self.id
    }

    /// FIFO sequence selected before fallible producer work begins.
    pub(crate) const fn sequence(&self) -> DelayedProofSequence {
        self.sequence
    }

    pub(crate) fn commit_retained(mut self, proof: RetainedInboundProof) -> DelayedProofId {
        self.owner.commit_reserved(self.id, self.event_id, proof);
        self.resolved = true;
        self.id
    }
}

impl fmt::Debug for DelayedProofReservation<'_, '_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DelayedProofReservation")
            .field("id", &self.id)
            .field("event_id", &self.event_id)
            .field("sequence", &self.sequence)
            .field("proof", &"<not committed>")
            .finish()
    }
}

impl Drop for DelayedProofReservation<'_, '_> {
    fn drop(&mut self) {
        if self.resolved {
            return;
        }
        self.owner.release_reservation(self.id);
        self.resolved = true;
    }
}

/// Exclusive FIFO lease of one exact ready delayed proof.
///
/// Call [`Self::release_actions`] to transfer the proof into a packet-only
/// action envelope. Dropping an unresolved lease restores the exact proof to
/// `Ready`.
#[must_use = "a delayed-proof lease must be released or returned to ready"]
pub struct DelayedProofLease<'owner, 'slots> {
    owner: &'owner mut DelayedProofOwner<'slots>,
    id: DelayedProofId,
    event_id: ApplicationEventId,
    sequence: DelayedProofSequence,
    resolved: bool,
}

impl DelayedProofLease<'_, '_> {
    /// Generation-safe proof identity.
    pub const fn id(&self) -> DelayedProofId {
        self.id
    }

    /// Exact application event that was durably received before this proof
    /// entered the ready queue.
    pub const fn event_id(&self) -> ApplicationEventId {
        self.event_id
    }

    /// FIFO sequence assigned at reservation time.
    pub const fn sequence(&self) -> DelayedProofSequence {
        self.sequence
    }

    /// Copy-only covered-DATA and encoded-proof correlation evidence.
    pub fn evidence(&self) -> InboundProofEvidence {
        self.owner.slots[self.id.slot.0]
            .proof
            .as_ref()
            .expect("a valid delayed-proof lease retains its exact proof")
            .evidence()
    }

    /// Release the exact proof as a packet-only ordinary-router envelope.
    pub fn release_actions(mut self) -> NodeActions {
        let proof = self.owner.release_proof(self.id);
        self.resolved = true;
        NodeActions::without_retained_proofs(
            alloc::vec::Vec::new(),
            alloc::vec![proof.into_packet()],
            0,
        )
    }
}

impl fmt::Debug for DelayedProofLease<'_, '_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DelayedProofLease")
            .field("id", &self.id)
            .field("event_id", &self.event_id)
            .field("sequence", &self.sequence)
            .field("proof", &"<redacted>")
            .finish()
    }
}

impl Drop for DelayedProofLease<'_, '_> {
    fn drop(&mut self) {
        if self.resolved {
            return;
        }
        self.owner.restore_ready(self.id);
        self.resolved = true;
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec;

    use super::*;
    use crate::embedded::retained_proof_for_owner_test;
    use crate::{
        ApplicationEvent, ApplicationEventDiscardReason, ApplicationEventOwner,
        ApplicationEventSlot, InterfaceId, NodeActions, TxTarget,
    };

    fn event_ids() -> (ApplicationEventId, ApplicationEventId) {
        let mut slots = [ApplicationEventSlot::new(), ApplicationEventSlot::new()];
        let mut owner = ApplicationEventOwner::new(&mut slots);
        owner
            .try_offer_actions(NodeActions::without_retained_proofs(
                vec![
                    ApplicationEvent::DataReceived {
                        destination: [1; 16],
                        payload: vec![1],
                        ingress: None,
                    },
                    ApplicationEvent::DataReceived {
                        destination: [2; 16],
                        payload: vec![2],
                        ingress: None,
                    },
                ],
                vec![],
                0,
            ))
            .expect("test events fit");
        let first = owner.lease_next().expect("first event");
        let first_id = first.id();
        first.discard(ApplicationEventDiscardReason::Superseded);
        let second = owner.lease_next().expect("second event");
        let second_id = second.id();
        second.discard(ApplicationEventDiscardReason::Superseded);
        (first_id, second_id)
    }

    fn proof(marker: u8) -> RetainedInboundProof {
        retained_proof_for_owner_test(marker).0
    }

    fn release_marker(lease: DelayedProofLease<'_, '_>) -> u8 {
        let actions = lease.release_actions();
        assert!(!actions.is_empty());
        assert!(actions.events.is_empty());
        assert_eq!(actions.retained_proof_count(), 0);
        assert_eq!(actions.packets.len(), 1);
        assert_eq!(actions.unroutable_packets, 0);
        let packet = &actions.packets[0];
        assert_eq!(
            packet.target(),
            TxTarget::Only(InterfaceId(packet.bytes()[0]))
        );
        packet.bytes()[0]
    }

    #[test]
    fn zero_capacity_and_full_capacity_are_deterministic() {
        let (event_id, _) = event_ids();
        let mut zero_slots: [DelayedProofSlot; 0] = [];
        let mut zero = DelayedProofOwner::new(&mut zero_slots);
        assert_eq!(
            zero.try_reserve(event_id).expect_err("zero owner is full"),
            DelayedProofReservationError::Full { capacity: 0 }
        );
        assert_eq!(zero.counters().full_rejections, 1);

        let mut slots = [DelayedProofSlot::new()];
        let mut owner = DelayedProofOwner::new(&mut slots);
        let reservation = owner.try_reserve(event_id).expect("one slot reserves");
        let first_id = reservation.id();
        reservation.commit_retained(proof(1));
        assert_eq!(owner.capacities().ready, 1);
        assert_eq!(
            owner
                .try_reserve(event_id)
                .expect_err("ready proof fills owner"),
            DelayedProofReservationError::Full { capacity: 1 }
        );
        assert_eq!(owner.counters().full_rejections, 1);
        assert_eq!(owner.lease_next().expect("proof is ready").id(), first_id);
    }

    #[test]
    fn reservation_drop_restores_capacity_and_advances_generation() {
        let (event_id, _) = event_ids();
        let mut slots = [DelayedProofSlot::new()];
        let mut owner = DelayedProofOwner::new(&mut slots);
        let first_id = {
            let reservation = owner.try_reserve(event_id).expect("reservation succeeds");
            let id = reservation.id();
            drop(reservation);
            id
        };
        assert_eq!(owner.capacities().vacant, 1);
        assert_eq!(owner.counters().reservations_released, 1);
        let second = owner.try_reserve(event_id).expect("capacity was restored");
        assert_eq!(second.id().slot(), first_id.slot());
        assert!(second.id().generation() > first_id.generation());
        drop(second);
    }

    #[test]
    fn commit_and_release_move_the_exact_proof_without_copying() {
        let (event_id, _) = event_ids();
        let mut slots = [DelayedProofSlot::new()];
        let mut owner = DelayedProofOwner::new(&mut slots);
        let (proof, bytes_pointer) = retained_proof_for_owner_test(9);
        let reservation = owner.try_reserve(event_id).expect("reservation succeeds");
        let proof_id = reservation.id();
        assert_eq!(reservation.event_id, event_id);
        reservation.commit_retained(proof);

        let lease = owner.lease_next().expect("proof is ready");
        assert_eq!(lease.id(), proof_id);
        assert_eq!(lease.event_id(), event_id);
        let actions = lease.release_actions();
        assert_eq!(actions.packets.len(), 1);
        assert_eq!(actions.packets[0].bytes().as_ptr(), bytes_pointer);
        assert_eq!(actions.packets[0].bytes()[0], 9);
        assert_eq!(actions.packets[0].target(), TxTarget::Only(InterfaceId(9)));
        assert_eq!(owner.capacities().vacant, 1);
        assert_eq!(owner.counters().proofs_released, 1);
    }

    #[test]
    fn ready_leases_are_fifo_and_unresolved_drop_restores_ready() {
        let (first_event, second_event) = event_ids();
        let mut slots = [DelayedProofSlot::new(), DelayedProofSlot::new()];
        let mut owner = DelayedProofOwner::new(&mut slots);
        owner
            .try_reserve(first_event)
            .expect("first reserves")
            .commit_retained(proof(1));
        owner
            .try_reserve(second_event)
            .expect("second reserves")
            .commit_retained(proof(2));

        let first = owner.lease_next().expect("first ready proof");
        assert_eq!(first.event_id(), first_event);
        drop(first);
        assert_eq!(owner.capacities().ready, 2);
        assert_eq!(owner.counters().unresolved_leases_restored, 1);
        let first = owner.lease_next().expect("restored first remains oldest");
        assert_eq!(release_marker(first), 1);
        let second = owner.lease_next().expect("second follows first");
        assert_eq!(second.event_id(), second_event);
        assert_eq!(release_marker(second), 2);
    }

    #[test]
    fn reconstructing_owner_preserves_ready_proof_and_fifo_sequence() {
        let (first_event, second_event) = event_ids();
        let mut slots = [DelayedProofSlot::new(), DelayedProofSlot::new()];
        {
            let mut owner = DelayedProofOwner::new(&mut slots);
            owner
                .try_reserve(first_event)
                .expect("first reserves")
                .commit_retained(proof(1));
        }
        let mut owner = DelayedProofOwner::new(&mut slots);
        owner
            .try_reserve(second_event)
            .expect("second reserves after reconstruction")
            .commit_retained(proof(2));
        assert_eq!(release_marker(owner.lease_next().expect("first proof")), 1);
        assert_eq!(release_marker(owner.lease_next().expect("second proof")), 2);
    }

    #[test]
    fn reconstruction_releases_forgotten_reservation_and_restores_forgotten_lease() {
        let (first_event, second_event) = event_ids();
        let mut slots = [DelayedProofSlot::new(), DelayedProofSlot::new()];
        {
            let mut owner = DelayedProofOwner::new(&mut slots);
            let reservation = owner
                .try_reserve(first_event)
                .expect("first reservation succeeds");
            core::mem::forget(reservation);
        }
        {
            let mut owner = DelayedProofOwner::new(&mut slots);
            assert_eq!(owner.capacities().vacant, 2);
            owner
                .try_reserve(second_event)
                .expect("second reservation succeeds")
                .commit_retained(proof(2));
            let lease = owner.lease_next().expect("committed proof leases");
            core::mem::forget(lease);
        }

        let mut owner = DelayedProofOwner::new(&mut slots);
        assert_eq!(owner.capacities().ready, 1);
        assert_eq!(owner.capacities().vacant, 1);
        let lease = owner
            .lease_next()
            .expect("forgotten lease is conservatively restored to ready");
        assert_eq!(lease.event_id(), second_event);
        assert_eq!(release_marker(lease), 2);
    }

    #[test]
    fn max_generation_slot_retires_after_reservation_drop() {
        let (event_id, _) = event_ids();
        let mut slots = [DelayedProofSlot::new()];
        slots[0].generation = u64::MAX - 1;
        let mut owner = DelayedProofOwner::new(&mut slots);
        let reservation = owner
            .try_reserve(event_id)
            .expect("final generation reserves");
        assert_eq!(reservation.id().generation().get(), u64::MAX);
        drop(reservation);
        assert_eq!(owner.capacities().retired, 1);
        assert_eq!(
            owner
                .try_reserve(event_id)
                .expect_err("retired owner stays full"),
            DelayedProofReservationError::Full { capacity: 0 }
        );
    }

    #[test]
    fn max_generation_slot_retires_after_committed_proof_release() {
        let (event_id, _) = event_ids();
        let mut slots = [DelayedProofSlot::new()];
        slots[0].generation = u64::MAX - 1;
        let mut owner = DelayedProofOwner::new(&mut slots);
        let reservation = owner
            .try_reserve(event_id)
            .expect("final generation reserves");
        assert_eq!(reservation.id().generation().get(), u64::MAX);
        reservation.commit_retained(proof(3));
        assert_eq!(
            release_marker(owner.lease_next().expect("proof is ready")),
            3
        );
        assert_eq!(owner.capacities().retired, 1);
        assert_eq!(owner.capacities().vacant, 0);
        assert_eq!(
            owner
                .try_reserve(event_id)
                .expect_err("released max-generation slot stays retired"),
            DelayedProofReservationError::Full { capacity: 0 }
        );
    }

    #[test]
    fn sequence_exhaustion_blocks_until_existing_ready_proof_resolves() {
        let (first_event, second_event) = event_ids();
        let mut slots = [DelayedProofSlot::new(), DelayedProofSlot::new()];
        slots[0].generation = 1;
        slots[0].sequence = u64::MAX;
        slots[0].state = DelayedProofSlotState::Ready;
        slots[0].event_id = Some(first_event);
        slots[0].proof = Some(proof(1));
        let mut owner = DelayedProofOwner::new(&mut slots);
        assert_eq!(
            owner
                .try_reserve(second_event)
                .expect_err("MAX ready sequence blocks a new reservation"),
            DelayedProofReservationError::SequenceExhausted
        );
        assert_eq!(owner.counters().sequence_exhaustion_rejections, 1);
        assert_eq!(
            release_marker(owner.lease_next().expect("existing proof")),
            1
        );
        let restarted = owner
            .try_reserve(second_event)
            .expect("empty owner restarts sequence numbering");
        assert_eq!(restarted.sequence().get(), 1);
        drop(restarted);
    }
}
