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
    let token = retained.quarantine_for_retry(ApplicationEventQuarantineReason::ConsumerDeferred);
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
    let current = current.quarantine_for_retry(ApplicationEventQuarantineReason::ConsumerDeferred);

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
