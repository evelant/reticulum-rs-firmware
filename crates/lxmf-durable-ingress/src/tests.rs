use super::*;

extern crate std;

use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
    check_erase, check_read, check_write,
};
use reticulum_lxmf_ingress::{LocalDeliveryDestination, StampPolicy, WireLimits};
use reticulum_lxmf_store::{
    BoundLxmfStore, LxmfProgramStage, LxmfStoreBinding, LxmfStoreDeviceId, PHYSICAL_FORMAT_VERSION,
    RECORD_FOOTER_SIZE, mount,
};
use reticulum_node_core::{
    ApplicationEvent, ApplicationEventOwner, ApplicationEventQuarantineReason,
    ApplicationEventSlot, NodeActions,
};
use serde::Deserialize;
use std::{string::String, vec, vec::Vec};

const CORPUS_JSON: &str = include_str!("../../../interop/vectors/lxmf-1.0.1-v1.json");
const PARTITION_SIZE: usize = 4 * reticulum_lxmf_store::EXTENT_SIZE;
const STORE_OFFSET: usize = 0x73_0000;

#[derive(Deserialize)]
struct Corpus {
    messages: Vec<MessageFixture>,
}

#[derive(Deserialize)]
struct MessageFixture {
    name: String,
    destination_hash_hex: String,
    source_hash_hex: String,
    source_public_key_hex: String,
    ingress: IngressFixture,
}

#[derive(Deserialize)]
struct IngressFixture {
    carrier_event: String,
    payload_hex: String,
}

fn fixture() -> MessageFixture {
    serde_json::from_str::<Corpus>(CORPUS_JSON)
        .expect("checked-in Python LXMF corpus")
        .messages
        .into_iter()
        .find(|fixture| fixture.name == "opportunistic_limit_295")
        .expect("maximum opportunistic fixture")
}

fn decode(value: &str) -> Vec<u8> {
    hex::decode(value).expect("fixture hex")
}

fn array<const N: usize>(value: &str) -> [u8; N] {
    decode(value).try_into().expect("fixed fixture width")
}

fn event(fixture: &MessageFixture) -> ApplicationEvent {
    assert_eq!(fixture.ingress.carrier_event, "destination_data");
    ApplicationEvent::DataReceived {
        destination: array(&fixture.destination_hash_hex),
        payload: decode(&fixture.ingress.payload_hex),
    }
}

fn limits() -> WireLimits {
    WireLimits::new(4_096, 2_048, 256, 2_048, 65_536, 16)
}

fn binding() -> LxmfStoreBinding {
    LxmfStoreBinding::new(
        LxmfStoreDeviceId::new([0x5a; 16]),
        STORE_OFFSET,
        PARTITION_SIZE,
        PHYSICAL_FORMAT_VERSION,
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FakeError {
    Bounds,
    Alignment,
    Injected,
}

impl NorFlashError for FakeError {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::Bounds => NorFlashErrorKind::OutOfBounds,
            Self::Alignment => NorFlashErrorKind::NotAligned,
            Self::Injected => NorFlashErrorKind::Other,
        }
    }
}

struct FakeNor {
    bytes: Vec<u8>,
    fail_next_write: bool,
    fail_next_read: bool,
    lose_commit_reply: bool,
    reads: usize,
    writes: usize,
    erases: usize,
}

impl FakeNor {
    fn erased() -> Self {
        Self {
            bytes: vec![0xff; PARTITION_SIZE],
            fail_next_write: false,
            fail_next_read: false,
            lose_commit_reply: false,
            reads: 0,
            writes: 0,
            erases: 0,
        }
    }

    fn fail_next_write(&mut self) {
        self.fail_next_write = true;
    }

    fn lose_commit_reply(&mut self) {
        self.lose_commit_reply = true;
    }
}

impl ErrorType for FakeNor {
    type Error = FakeError;
}

impl ReadNorFlash for FakeNor {
    const READ_SIZE: usize = 4;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.reads += 1;
        if core::mem::take(&mut self.fail_next_read) {
            return Err(FakeError::Injected);
        }
        check_read(self, offset, bytes.len()).map_err(map_check_error)?;
        let offset = offset as usize;
        bytes.copy_from_slice(&self.bytes[offset..offset + bytes.len()]);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.bytes.len()
    }
}

impl NorFlash for FakeNor {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = reticulum_lxmf_store::EXTENT_SIZE;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.writes += 1;
        check_write(self, offset, bytes.len()).map_err(map_check_error)?;
        if core::mem::take(&mut self.fail_next_write) {
            return Err(FakeError::Injected);
        }
        let offset = offset as usize;
        for (stored, supplied) in self.bytes[offset..offset + bytes.len()]
            .iter_mut()
            .zip(bytes)
        {
            *stored &= *supplied;
        }
        let commit_offset = RECORD_FOOTER_SIZE - 32;
        if self.lose_commit_reply
            && bytes.len() == RECORD_FOOTER_SIZE
            && bytes[..commit_offset].iter().all(|byte| *byte == 0xff)
            && bytes[commit_offset..].iter().any(|byte| *byte != 0xff)
        {
            self.lose_commit_reply = false;
            self.fail_next_read = true;
            return Err(FakeError::Injected);
        }
        Ok(())
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.erases += 1;
        check_erase(self, from, to).map_err(map_check_error)?;
        self.bytes[from as usize..to as usize].fill(0xff);
        Ok(())
    }
}

impl MultiwriteNorFlash for FakeNor {}

fn map_check_error(error: NorFlashErrorKind) -> FakeError {
    match error {
        NorFlashErrorKind::OutOfBounds => FakeError::Bounds,
        NorFlashErrorKind::NotAligned => FakeError::Alignment,
        _ => FakeError::Injected,
    }
}

fn offer<'owner, 'slots>(
    owner: &'owner mut ApplicationEventOwner<'slots>,
    event: ApplicationEvent,
) -> ApplicationEventLease<'owner, 'slots> {
    owner
        .try_offer_actions(NodeActions {
            events: vec![event],
            packets: vec![],
            unroutable_packets: 0,
        })
        .expect("event owner capacity");
    owner.lease_next().expect("offered event lease")
}

#[test]
fn python_maximum_opportunistic_event_commits_before_acknowledgement_and_replays() {
    let fixture = fixture();
    let first_event = event(&fixture);
    let ApplicationEvent::DataReceived { payload, .. } = &first_event else {
        unreachable!()
    };
    assert_eq!(payload.len(), 391);
    assert!(payload.len() > 383);

    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
    let source = array::<16>(&fixture.source_hash_hex);
    let public_key = array::<64>(&fixture.source_public_key_hex);
    let resolver = |candidate: &[u8; 16]| (candidate == &source).then_some(public_key);
    let mut access = BoundLxmfStore::new(FakeNor::erased(), binding());
    let mut store = mount::<_, 2>(&mut access).expect("empty store mounts");
    let mut slots = [ApplicationEventSlot::new()];
    let mut owner = ApplicationEventOwner::new(&mut slots);

    let first = match commit_application_event(
        offer(&mut owner, first_event),
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
        &mut store,
        &mut access,
    ) {
        DurableIngressOutcome::Durable(success) => success,
        DurableIngressOutcome::Retained(retained) => {
            retained
                .into_lease()
                .quarantine(ApplicationEventQuarantineReason::ConsumerFault);
            panic!("valid Python event must commit")
        }
    };
    assert_eq!(first.kind(), DurableIngressCommitKind::New);
    assert_eq!(store.message_count(), 1);
    assert_eq!(owner.counters().acknowledged_events, 1);
    assert_eq!(
        store.receipt(first.receipt().handle()),
        Some(first.receipt())
    );

    let replay = match commit_application_event(
        offer(&mut owner, event(&fixture)),
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
        &mut store,
        &mut access,
    ) {
        DurableIngressOutcome::Durable(success) => success,
        DurableIngressOutcome::Retained(retained) => {
            retained
                .into_lease()
                .quarantine(ApplicationEventQuarantineReason::ConsumerFault);
            panic!("duplicate event must resolve durably")
        }
    };
    assert_eq!(replay.kind(), DurableIngressCommitKind::Replay);
    assert_eq!(replay.receipt(), first.receipt());
    assert_eq!(store.message_count(), 1);
    assert_eq!(owner.counters().acknowledged_events, 2);
}

#[test]
fn unrelated_deferred_and_rejected_outcomes_return_the_exact_lease() {
    let fixture = fixture();
    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
    let source = array::<16>(&fixture.source_hash_hex);
    let public_key = array::<64>(&fixture.source_public_key_hex);
    let resolver = |candidate: &[u8; 16]| (candidate == &source).then_some(public_key);
    let missing = |_candidate: &[u8; 16]| None::<[u8; 64]>;
    let mut access = BoundLxmfStore::new(FakeNor::erased(), binding());
    let mut store = mount::<_, 2>(&mut access).expect("empty store mounts");
    let mut slots = [ApplicationEventSlot::new()];
    let mut owner = ApplicationEventOwner::new(&mut slots);

    let unrelated_event = event(&fixture);
    let ApplicationEvent::DataReceived { payload, .. } = &unrelated_event else {
        unreachable!()
    };
    let unrelated_pointer = payload.as_ptr();
    let unrelated_lease = offer(&mut owner, unrelated_event);
    let unrelated_id = unrelated_lease.id();
    let unrelated_sequence = unrelated_lease.sequence();
    let unrelated_quarantine = unrelated_lease.quarantine_reason();
    let DurableIngressOutcome::Retained(unrelated) = commit_application_event(
        unrelated_lease,
        LocalDeliveryDestination::new([0x99; 16]),
        limits(),
        &resolver,
        StampPolicy::NotRequired,
        &mut store,
        &mut access,
    ) else {
        panic!("other destination must be retained")
    };
    assert_eq!(unrelated.event_id(), unrelated_id);
    assert!(matches!(
        unrelated.reason(),
        DurableIngressRetentionReason::Unrelated(_)
    ));
    let unrelated = unrelated.into_lease();
    assert_eq!(unrelated.id(), unrelated_id);
    assert_eq!(unrelated.sequence(), unrelated_sequence);
    assert_eq!(unrelated.quarantine_reason(), unrelated_quarantine);
    let ApplicationEvent::DataReceived { payload, .. } = unrelated.event() else {
        unreachable!()
    };
    assert_eq!(payload.as_ptr(), unrelated_pointer);
    unrelated.quarantine(ApplicationEventQuarantineReason::ConsumerDeferred);

    let deferred_lease = owner.lease_next_quarantined().expect("same retained event");
    let deferred_id = deferred_lease.id();
    let deferred_sequence = deferred_lease.sequence();
    let deferred_quarantine = deferred_lease.quarantine_reason();
    assert_eq!(deferred_id, unrelated_id);
    assert_eq!(deferred_sequence, unrelated_sequence);
    assert_eq!(
        deferred_quarantine,
        Some(ApplicationEventQuarantineReason::ConsumerDeferred)
    );
    let DurableIngressOutcome::Retained(deferred) = commit_application_event(
        deferred_lease,
        local,
        limits(),
        &missing,
        StampPolicy::NotRequired,
        &mut store,
        &mut access,
    ) else {
        panic!("missing identity must be retained")
    };
    assert_eq!(deferred.event_id(), deferred_id);
    assert!(matches!(
        deferred.reason(),
        DurableIngressRetentionReason::Deferred(_)
    ));
    let deferred = deferred.into_lease();
    assert_eq!(deferred.id(), deferred_id);
    assert_eq!(deferred.sequence(), deferred_sequence);
    assert_eq!(deferred.quarantine_reason(), deferred_quarantine);
    deferred.quarantine(ApplicationEventQuarantineReason::ConsumerDeferred);

    let rejected_lease = owner.lease_next_quarantined().expect("same retained event");
    let rejected_id = rejected_lease.id();
    let rejected_sequence = rejected_lease.sequence();
    let rejected_quarantine = rejected_lease.quarantine_reason();
    assert_eq!(rejected_id, unrelated_id);
    assert_eq!(rejected_sequence, unrelated_sequence);
    assert_eq!(rejected_quarantine, deferred_quarantine);
    let DurableIngressOutcome::Retained(rejected) = commit_application_event(
        rejected_lease,
        local,
        WireLimits::new(100, 100, 10, 10, 100, 4),
        &resolver,
        StampPolicy::NotRequired,
        &mut store,
        &mut access,
    ) else {
        panic!("over-limit wire must be retained after rejection")
    };
    assert_eq!(rejected.event_id(), rejected_id);
    assert!(matches!(
        rejected.reason(),
        DurableIngressRetentionReason::Rejected(_)
    ));
    let rejected = rejected.into_lease();
    assert_eq!(rejected.id(), rejected_id);
    assert_eq!(rejected.sequence(), rejected_sequence);
    assert_eq!(rejected.quarantine_reason(), rejected_quarantine);
    rejected.quarantine(ApplicationEventQuarantineReason::ConsumerDeferred);

    assert_eq!(owner.counters().acknowledged_events, 0);
    assert_eq!(owner.capacities().quarantined, 1);
    assert_eq!(store.message_count(), 0);
}

#[test]
fn store_capacity_and_backend_failures_return_the_exact_unacknowledged_lease() {
    let fixture = fixture();
    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
    let source = array::<16>(&fixture.source_hash_hex);
    let public_key = array::<64>(&fixture.source_public_key_hex);
    let resolver = |candidate: &[u8; 16]| (candidate == &source).then_some(public_key);

    let mut zero_access = BoundLxmfStore::new(FakeNor::erased(), binding());
    let mut zero_store = mount::<_, 0>(&mut zero_access).expect("zero-entry store mounts empty");
    let mut zero_slots = [ApplicationEventSlot::new()];
    let mut zero_owner = ApplicationEventOwner::new(&mut zero_slots);
    let blocked_event = event(&fixture);
    let ApplicationEvent::DataReceived { payload, .. } = &blocked_event else {
        unreachable!()
    };
    let blocked_pointer = payload.as_ptr();
    let blocked_lease = offer(&mut zero_owner, blocked_event);
    let blocked_id = blocked_lease.id();
    let blocked_sequence = blocked_lease.sequence();
    let blocked_quarantine = blocked_lease.quarantine_reason();
    let DurableIngressOutcome::Retained(blocked) = commit_application_event(
        blocked_lease,
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
        &mut zero_store,
        &mut zero_access,
    ) else {
        panic!("zero semantic capacity must retain the event")
    };
    assert_eq!(blocked.event_id(), blocked_id);
    assert!(matches!(
        blocked.reason(),
        DurableIngressRetentionReason::Store(LxmfCommitError::IndexFull { capacity: 0 })
    ));
    let blocked = blocked.into_lease();
    assert_eq!(blocked.id(), blocked_id);
    assert_eq!(blocked.sequence(), blocked_sequence);
    assert_eq!(blocked.quarantine_reason(), blocked_quarantine);
    let ApplicationEvent::DataReceived { payload, .. } = blocked.event() else {
        unreachable!()
    };
    assert_eq!(payload.as_ptr(), blocked_pointer);
    blocked.quarantine(ApplicationEventQuarantineReason::ConsumerDeferred);
    assert_eq!(zero_owner.counters().acknowledged_events, 0);

    let mut failed_access = BoundLxmfStore::new(FakeNor::erased(), binding());
    let mut failed_store = mount::<_, 1>(&mut failed_access).expect("empty store mounts");
    failed_access.backend_mut().fail_next_write();
    let mut failed_slots = [ApplicationEventSlot::new()];
    let mut failed_owner = ApplicationEventOwner::new(&mut failed_slots);
    let failed_lease = offer(&mut failed_owner, event(&fixture));
    let failed_id = failed_lease.id();
    let failed_sequence = failed_lease.sequence();
    let failed_quarantine = failed_lease.quarantine_reason();
    let DurableIngressOutcome::Retained(failed) = commit_application_event(
        failed_lease,
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
        &mut failed_store,
        &mut failed_access,
    ) else {
        panic!("backend fault must retain the event")
    };
    assert_eq!(failed.event_id(), failed_id);
    assert!(matches!(
        failed.reason(),
        DurableIngressRetentionReason::Store(LxmfCommitError::Backend {
            error: FakeError::Injected,
            ..
        })
    ));
    let failed = failed.into_lease();
    assert_eq!(failed.id(), failed_id);
    assert_eq!(failed.sequence(), failed_sequence);
    assert_eq!(failed.quarantine_reason(), failed_quarantine);
    failed.quarantine(ApplicationEventQuarantineReason::ConsumerFault);
    assert_eq!(failed_owner.counters().acknowledged_events, 0);
}

#[test]
fn commit_marker_lost_success_retains_exact_lease_until_same_lease_retry_is_durable() {
    let fixture = fixture();
    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
    let source = array::<16>(&fixture.source_hash_hex);
    let public_key = array::<64>(&fixture.source_public_key_hex);
    let resolver = |candidate: &[u8; 16]| (candidate == &source).then_some(public_key);
    let mut access = BoundLxmfStore::new(FakeNor::erased(), binding());
    let mut store = mount::<_, 1>(&mut access).expect("empty store mounts");
    access.backend_mut().lose_commit_reply();
    let mut slots = [ApplicationEventSlot::new()];
    let mut owner = ApplicationEventOwner::new(&mut slots);
    let retained_event = event(&fixture);
    let ApplicationEvent::DataReceived { payload, .. } = &retained_event else {
        unreachable!()
    };
    let retained_pointer = payload.as_ptr();
    let retained_lease = offer(&mut owner, retained_event);
    let retained_id = retained_lease.id();
    let retained_sequence = retained_lease.sequence();
    let retained_quarantine = retained_lease.quarantine_reason();

    let DurableIngressOutcome::Retained(retained) = commit_application_event(
        retained_lease,
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
        &mut store,
        &mut access,
    ) else {
        panic!("lost commit reply must retain the event until reconciliation")
    };
    assert!(matches!(
        retained.reason(),
        DurableIngressRetentionReason::Store(LxmfCommitError::Backend {
            stage: LxmfProgramStage::Commit,
            error: FakeError::Injected,
        })
    ));
    assert_eq!(store.message_count(), 0);
    assert_eq!(
        mount::<_, 1>(&mut access)
            .expect("the lost reply followed physical commit")
            .message_count(),
        1
    );
    let retained = retained.into_lease();
    assert_eq!(retained.id(), retained_id);
    assert_eq!(retained.sequence(), retained_sequence);
    assert_eq!(retained.quarantine_reason(), retained_quarantine);
    let ApplicationEvent::DataReceived { payload, .. } = retained.event() else {
        unreachable!()
    };
    assert_eq!(payload.as_ptr(), retained_pointer);

    let DurableIngressOutcome::Durable(reconciled) = commit_application_event(
        retained,
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
        &mut store,
        &mut access,
    ) else {
        panic!("the same exact lease must reconcile its committed mutation")
    };
    assert_eq!(reconciled.event_id(), retained_id);
    assert_eq!(reconciled.kind(), DurableIngressCommitKind::Replay);
    assert_eq!(store.message_count(), 1);
    assert_eq!(owner.counters().acknowledged_events, 1);
}

#[test]
fn wrong_binding_returns_exact_lease_without_touching_the_supplied_backend() {
    let fixture = fixture();
    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
    let source = array::<16>(&fixture.source_hash_hex);
    let public_key = array::<64>(&fixture.source_public_key_hex);
    let resolver = |candidate: &[u8; 16]| (candidate == &source).then_some(public_key);
    let mut mounted_access = BoundLxmfStore::new(FakeNor::erased(), binding());
    let mut store = mount::<_, 1>(&mut mounted_access).expect("empty store mounts");
    let mut wrong_access = BoundLxmfStore::new(
        FakeNor::erased(),
        LxmfStoreBinding::new(
            LxmfStoreDeviceId::new([0xa5; 16]),
            STORE_OFFSET,
            PARTITION_SIZE,
            PHYSICAL_FORMAT_VERSION,
        ),
    );
    let mut slots = [ApplicationEventSlot::new()];
    let mut owner = ApplicationEventOwner::new(&mut slots);
    let retained_event = event(&fixture);
    let ApplicationEvent::DataReceived { payload, .. } = &retained_event else {
        unreachable!()
    };
    let retained_pointer = payload.as_ptr();
    let retained_lease = offer(&mut owner, retained_event);
    let retained_id = retained_lease.id();
    let retained_sequence = retained_lease.sequence();
    let retained_quarantine = retained_lease.quarantine_reason();

    let DurableIngressOutcome::Retained(retained) = commit_application_event(
        retained_lease,
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
        &mut store,
        &mut wrong_access,
    ) else {
        panic!("wrong operation binding must retain the event")
    };
    assert!(matches!(
        retained.reason(),
        DurableIngressRetentionReason::Store(LxmfCommitError::Binding(_))
    ));
    assert_eq!(wrong_access.backend().reads, 0);
    assert_eq!(wrong_access.backend().writes, 0);
    assert_eq!(wrong_access.backend().erases, 0);
    let retained = retained.into_lease();
    assert_eq!(retained.id(), retained_id);
    assert_eq!(retained.sequence(), retained_sequence);
    assert_eq!(retained.quarantine_reason(), retained_quarantine);
    let ApplicationEvent::DataReceived { payload, .. } = retained.event() else {
        unreachable!()
    };
    assert_eq!(payload.as_ptr(), retained_pointer);
    retained.quarantine(ApplicationEventQuarantineReason::ConsumerFault);
    assert_eq!(owner.counters().acknowledged_events, 0);
    assert_eq!(store.message_count(), 0);
}

#[test]
fn same_id_different_authenticated_material_collision_returns_exact_lease() {
    let fixture = fixture();
    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
    let source = array::<16>(&fixture.source_hash_hex);
    let public_key = array::<64>(&fixture.source_public_key_hex);
    let resolver = |candidate: &[u8; 16]| (candidate == &source).then_some(public_key);
    let mut access = BoundLxmfStore::new(FakeNor::erased(), binding());
    let mut store = mount::<_, 2>(&mut access).expect("empty store mounts");

    let seed_event = event(&fixture);
    let IngressOutcome::Validated(validated) = validate_application_event(
        &seed_event,
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
    ) else {
        panic!("seed fixture must validate")
    };
    let evidence = validated.evidence();
    let authentic_metadata = metadata_from_evidence(evidence).expect("validated metadata");
    let conflicting_material = AuthenticatedMaterialFingerprint::new([0xa5; 32]);
    assert_ne!(
        conflicting_material,
        authentic_metadata.authenticated_material()
    );
    let conflicting_metadata = InboundMessageMetadata::new(
        authentic_metadata.message_id(),
        conflicting_material,
        authentic_metadata.destination(),
        authentic_metadata.source(),
        authentic_metadata.timestamp_bits(),
        authentic_metadata.carrier(),
        authentic_metadata.stamp_admission(),
        authentic_metadata.lengths(),
    )
    .expect("only authenticated material differs");
    let conflicting_candidate = InboundMessageCandidate::new(
        conflicting_metadata,
        NormalizedWire::Opportunistic {
            implied_destination: evidence.destination(),
            carrier_payload: validated.carrier_payload(),
        },
    )
    .expect("borrowed fixture matches the conflicting semantic candidate");
    assert!(matches!(
        store.commit(&mut access, conflicting_candidate),
        Ok(LxmfCommitOutcome::Committed(_))
    ));
    drop(validated);
    let writes_before_collision = access.backend().writes;

    let mut slots = [ApplicationEventSlot::new()];
    let mut owner = ApplicationEventOwner::new(&mut slots);
    let retained_event = event(&fixture);
    let ApplicationEvent::DataReceived { payload, .. } = &retained_event else {
        unreachable!()
    };
    let retained_pointer = payload.as_ptr();
    let retained_lease = offer(&mut owner, retained_event);
    let retained_id = retained_lease.id();
    let retained_sequence = retained_lease.sequence();
    let retained_quarantine = retained_lease.quarantine_reason();

    let DurableIngressOutcome::Retained(retained) = commit_application_event(
        retained_lease,
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
        &mut store,
        &mut access,
    ) else {
        panic!("same ID with different authenticated material must fail closed")
    };
    assert!(matches!(
        retained.reason(),
        DurableIngressRetentionReason::Store(LxmfCommitError::HashCollision { message_id })
            if *message_id == authentic_metadata.message_id()
    ));
    assert_eq!(access.backend().writes, writes_before_collision);
    let retained = retained.into_lease();
    assert_eq!(retained.id(), retained_id);
    assert_eq!(retained.sequence(), retained_sequence);
    assert_eq!(retained.quarantine_reason(), retained_quarantine);
    let ApplicationEvent::DataReceived { payload, .. } = retained.event() else {
        unreachable!()
    };
    assert_eq!(payload.as_ptr(), retained_pointer);
    retained.quarantine(ApplicationEventQuarantineReason::ConsumerFault);
    assert_eq!(owner.counters().acknowledged_events, 0);
    assert_eq!(store.message_count(), 1);
}
