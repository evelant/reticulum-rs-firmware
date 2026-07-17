extern crate std;

use rand_core::{CryptoRng, RngCore};
use reticulum_node_core::{
    AttemptOutcome, DestinationHash as NodeDestinationHash, InterfaceSet, MonotonicMillis,
    MonotonicSeconds, NodeConfig, NodeCore, NodeIdentity, NodeInstanceId, PrepareDataRequest,
    RoutedTxJob, TxCompletionDisposition, TxLeaseDeadline, TxPacketBuffer,
};
use std::{boxed::Box, vec, vec::Vec};

use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
    check_erase, check_read, check_write,
};
use reticulum_storage_journal::{
    BANK_SLOT_COUNT, ERASE_SIZE, MAX_ACCEPTED_SUBMISSIONS, PARTITION_SIZE, PHYSICAL_FORMAT_VERSION,
    SLOT_SIZE, format_erased,
};
use reticulum_storage_model::{
    DestinationHash, ExperimentalRnsDataIntent, FinalDisposition, IdempotencyKey, InternalFailure,
    InterruptedState, LifecycleState, PrincipalId, SubmissionFailure, SubmissionReplay,
};
use reticulum_submission_projector::{
    AcknowledgementKind, AcknowledgementReply, PersistenceReply, PreparedFrameObservation,
    ProjectionProgress, ProjectorError, ProjectorFault, SubmissionPreparationObservation,
    SubmissionProjector,
};

use super::*;

type TestNode = NodeCore<4, 2, 8, 2, 1>;

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

fn identity(tag: u8) -> NodeIdentity {
    NodeIdentity::from_private_key(&[tag; 64]).unwrap()
}

fn prepared_job(tag: u8, owner_deadline_ms: u64) -> (TestNode, RoutedTxJob<'static>) {
    let mut sender = TestNode::new(
        identity(tag),
        "reticulum",
        &["storage-actor-sender"],
        NodeInstanceId::new([tag.wrapping_add(0x80); 16]),
        NodeConfig::endpoint(),
    )
    .unwrap();
    let receiver_identity = identity(tag.wrapping_add(1));
    sender
        .register_peer(
            &receiver_identity,
            "reticulum",
            &["storage-actor-receiver"],
            MonotonicSeconds::new(0),
        )
        .unwrap();
    let receiver = TestNode::new(
        receiver_identity,
        "reticulum",
        &["storage-actor-receiver"],
        NodeInstanceId::new([tag.wrapping_add(0x81); 16]),
        NodeConfig::endpoint(),
    )
    .unwrap();
    let destination = NodeDestinationHash::new(*receiver.destination_hash().as_bytes());

    let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
    sender.register_packet_buffer(buffer).unwrap();
    let job = sender
        .prepare_data_into_slot(
            buffer,
            PrepareDataRequest {
                destination,
                plaintext: b"storage actor projector runtime test",
                rns_now: MonotonicSeconds::new(100),
                owner_now: MonotonicMillis::new(100_000),
                deadline: TxLeaseDeadline::new(MonotonicMillis::new(owner_deadline_ms)),
                enabled_interfaces: InterfaceSet::from_bits(1 << 1),
            },
            &mut CounterRng::default(),
        )
        .unwrap_or_else(|failure| panic!("preparation failed: {:?}", failure.reason()));
    (sender, job)
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

#[derive(Clone)]
struct FakeNor {
    bytes: Vec<u8>,
    reads: usize,
    writes: usize,
    erases: usize,
    lost_write_reply_after: Option<usize>,
    lost_erase_reply_after: Option<usize>,
}

impl FakeNor {
    fn erased() -> Self {
        Self {
            bytes: vec![0xff; PARTITION_SIZE],
            reads: 0,
            writes: 0,
            erases: 0,
            lost_write_reply_after: None,
            lost_erase_reply_after: None,
        }
    }

    fn formatted() -> Self {
        let mut flash = Self::erased();
        format_erased(&mut flash).unwrap();
        flash
    }

    fn lose_write_reply_after(&mut self, successful_writes: usize) {
        self.lost_write_reply_after = Some(successful_writes);
    }

    fn lose_erase_reply_after(&mut self, successful_erases: usize) {
        self.lost_erase_reply_after = Some(successful_erases);
    }

    fn program(&mut self, offset: usize, bytes: &[u8]) {
        for (stored, supplied) in self.bytes[offset..offset + bytes.len()]
            .iter_mut()
            .zip(bytes)
        {
            *stored &= *supplied;
        }
    }
}

impl ErrorType for FakeNor {
    type Error = FakeError;
}

impl ReadNorFlash for FakeNor {
    const READ_SIZE: usize = 4;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        check_read(self, offset, bytes.len()).map_err(map_check_error)?;
        self.reads += 1;
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
    const ERASE_SIZE: usize = ERASE_SIZE;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        check_erase(self, from, to).map_err(map_check_error)?;
        self.erases += 1;
        let should_lose_reply = self.lost_erase_reply_after == Some(0);
        if let Some(remaining) = &mut self.lost_erase_reply_after
            && *remaining != 0
        {
            *remaining -= 1;
        }
        self.bytes[from as usize..to as usize].fill(0xff);
        if should_lose_reply {
            self.lost_erase_reply_after = None;
            Err(FakeError::Injected)
        } else {
            Ok(())
        }
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        check_write(self, offset, bytes.len()).map_err(map_check_error)?;
        self.writes += 1;
        let should_lose_reply = self.lost_write_reply_after == Some(0);
        if let Some(remaining) = &mut self.lost_write_reply_after
            && *remaining != 0
        {
            *remaining -= 1;
        }
        self.program(offset as usize, bytes);
        if should_lose_reply {
            self.lost_write_reply_after = None;
            Err(FakeError::Injected)
        } else {
            Ok(())
        }
    }
}

impl MultiwriteNorFlash for FakeNor {}

type TestJournal = BoundJournal<FakeNor>;

const TEST_DEVICE: StorageDeviceId = StorageDeviceId::new([0x51; 16]);
const TEST_ABSOLUTE_OFFSET: usize = 0x63_0000;

const fn test_binding() -> JournalBinding {
    JournalBinding::new(
        TEST_DEVICE,
        TEST_ABSOLUTE_OFFSET,
        PARTITION_SIZE,
        PHYSICAL_FORMAT_VERSION,
    )
}

fn bound(flash: FakeNor) -> TestJournal {
    BoundJournal::new(flash, test_binding())
}

fn mounted<const S: usize, const P: usize>(
    flash: FakeNor,
    first_id: u64,
) -> (StorageActor<S, P>, TestJournal) {
    let mut journal = bound(flash);
    let actor = StorageActor::mount(&mut journal, SubmissionId::new(first_id)).unwrap();
    (actor, journal)
}

fn io_counts(journal: &TestJournal) -> (usize, usize, usize) {
    let backend = journal.backend();
    (backend.reads, backend.writes, backend.erases)
}

fn map_check_error(error: NorFlashErrorKind) -> FakeError {
    match error {
        NorFlashErrorKind::OutOfBounds => FakeError::Bounds,
        NorFlashErrorKind::NotAligned => FakeError::Alignment,
        _ => FakeError::Injected,
    }
}

fn candidate(tag: u8, payload: &[u8]) -> AcceptanceCandidate {
    AcceptanceCandidate::new(
        PrincipalId::new([tag; 16]),
        IdempotencyKey::new([tag.wrapping_add(1); 16]),
        ExperimentalRnsDataIntent::new(DestinationHash::new([tag.wrapping_add(2); 16]), payload)
            .unwrap(),
    )
}

fn request_for<const S: usize, const P: usize>(
    actor: &mut StorageActor<S, P>,
    id: SubmissionId,
) -> PersistRequest {
    let ProjectionProgress::Persist(handle) = actor.begin_preparation(id).unwrap() else {
        panic!("preparation must produce a persistence request")
    };
    actor.projector().persistence_request(handle).unwrap()
}

fn persist_projection<const S: usize, const P: usize>(
    actor: &mut StorageActor<S, P>,
    journal: &mut TestJournal,
    progress: ProjectionProgress,
) -> PersistRequest {
    let ProjectionProgress::Persist(handle) = progress else {
        panic!("projector operation did not produce a persistence request")
    };
    let request = actor.projector().persistence_request(handle).unwrap();
    assert_eq!(
        actor.persist_projector(journal, request),
        Ok(PersistenceProgress::Committed)
    );
    request
}

fn actor_with_durable_preparation(
    first_id: u64,
    tag: u8,
) -> (StorageActor<4, 2>, TestJournal, SubmissionId) {
    let (mut actor, mut journal) = mounted::<4, 2>(FakeNor::formatted(), first_id);
    let id = match actor.accept(&mut journal, candidate(tag, b"projector runtime")) {
        Ok(AcceptanceProgress::Accepted(id)) => id,
        other => panic!("acceptance failed: {other:?}"),
    };
    let request = request_for(&mut actor, id);
    assert_eq!(
        actor.persist_projector(&mut journal, request),
        Ok(PersistenceProgress::Committed)
    );
    assert!(actor.projector().preparation_allowed(actor.index(), id));
    (actor, journal, id)
}

#[test]
fn mount_is_the_only_service_entry_and_completes_replay() {
    let measured_pending_bytes = std::hint::black_box(PENDING_MUTATION_BYTES);
    assert_eq!(
        measured_pending_bytes,
        core::mem::size_of::<Option<PendingMutation>>()
    );
    let mut erased = bound(FakeNor::erased());
    assert!(matches!(
        StorageActor::<2, 1>::mount(&mut erased, SubmissionId::new(1)),
        Err(MountError::Fault(StorageFault::UnformattedErased))
    ));

    let (actor, _journal) = mounted::<2, 1>(FakeNor::formatted(), 7);
    assert_eq!(actor.binding(), test_binding());
    assert_eq!(actor.state().committed_records(), 0);
    assert_eq!(actor.index().next_id(), Some(SubmissionId::new(7)));
    assert_eq!(actor.pending_kind(), None);
    assert_eq!(actor.fault(), None);
}

#[test]
fn mount_rejects_layout_length_alignment_and_capacity_before_io() {
    let cases = [
        (
            JournalBinding::new(
                TEST_DEVICE,
                TEST_ABSOLUTE_OFFSET,
                PARTITION_SIZE,
                PHYSICAL_FORMAT_VERSION + 1,
            ),
            JournalBindingError::LayoutVersionMismatch {
                expected: PHYSICAL_FORMAT_VERSION,
                actual: PHYSICAL_FORMAT_VERSION + 1,
            },
        ),
        (
            JournalBinding::new(
                TEST_DEVICE,
                TEST_ABSOLUTE_OFFSET,
                PARTITION_SIZE - ERASE_SIZE,
                PHYSICAL_FORMAT_VERSION,
            ),
            JournalBindingError::LengthMismatch {
                expected: PARTITION_SIZE,
                actual: PARTITION_SIZE - ERASE_SIZE,
            },
        ),
        (
            JournalBinding::new(
                TEST_DEVICE,
                TEST_ABSOLUTE_OFFSET + 1,
                PARTITION_SIZE,
                PHYSICAL_FORMAT_VERSION,
            ),
            JournalBindingError::AlignmentMismatch {
                absolute_offset: TEST_ABSOLUTE_OFFSET + 1,
                length: PARTITION_SIZE,
                read_size: FakeNor::READ_SIZE,
                write_size: FakeNor::WRITE_SIZE,
                erase_size: FakeNor::ERASE_SIZE,
            },
        ),
    ];

    for (binding, expected) in cases {
        let mut journal = BoundJournal::new(FakeNor::erased(), binding);
        assert_eq!(io_counts(&journal), (0, 0, 0));
        assert!(matches!(
            StorageActor::<2, 1>::mount(&mut journal, SubmissionId::new(1)),
            Err(MountError::Binding(actual)) if actual == expected
        ));
        assert_eq!(io_counts(&journal), (0, 0, 0));
    }

    let mut short = FakeNor::erased();
    short.bytes.truncate(PARTITION_SIZE - ERASE_SIZE);
    let mut journal = bound(short);
    assert!(matches!(
        StorageActor::<2, 1>::mount(&mut journal, SubmissionId::new(1)),
        Err(MountError::Binding(JournalBindingError::CapacityMismatch {
            expected,
            actual,
        })) if expected == PARTITION_SIZE && actual == PARTITION_SIZE - ERASE_SIZE
    ));
    assert_eq!(io_counts(&journal), (0, 0, 0));
}

#[test]
fn later_wrong_device_range_layout_and_capacity_are_non_latching_and_io_free() {
    let (mut actor, mut journal) = mounted::<2, 1>(FakeNor::formatted(), 8);
    assert_eq!(actor.binding().device().as_bytes(), &[0x51; 16]);
    assert_eq!(actor.binding().absolute_offset(), TEST_ABSOLUTE_OFFSET);
    assert_eq!(actor.binding().length(), PARTITION_SIZE);
    assert_eq!(actor.binding().layout_version(), PHYSICAL_FORMAT_VERSION);

    let wrong_bindings = [
        (
            JournalBinding::new(
                StorageDeviceId::new([0x52; 16]),
                TEST_ABSOLUTE_OFFSET,
                PARTITION_SIZE,
                PHYSICAL_FORMAT_VERSION,
            ),
            JournalBindingError::DeviceMismatch {
                expected: TEST_DEVICE,
                actual: StorageDeviceId::new([0x52; 16]),
            },
        ),
        (
            JournalBinding::new(
                TEST_DEVICE,
                TEST_ABSOLUTE_OFFSET + ERASE_SIZE,
                PARTITION_SIZE,
                PHYSICAL_FORMAT_VERSION,
            ),
            JournalBindingError::RangeMismatch {
                expected_absolute_offset: TEST_ABSOLUTE_OFFSET,
                expected_length: PARTITION_SIZE,
                actual_absolute_offset: TEST_ABSOLUTE_OFFSET + ERASE_SIZE,
                actual_length: PARTITION_SIZE,
            },
        ),
        (
            JournalBinding::new(
                TEST_DEVICE,
                TEST_ABSOLUTE_OFFSET,
                PARTITION_SIZE,
                PHYSICAL_FORMAT_VERSION + 1,
            ),
            JournalBindingError::LayoutVersionMismatch {
                expected: PHYSICAL_FORMAT_VERSION,
                actual: PHYSICAL_FORMAT_VERSION + 1,
            },
        ),
    ];

    for (tag, (binding, expected)) in wrong_bindings.into_iter().enumerate() {
        let mut wrong = BoundJournal::new(journal.backend().clone(), binding);
        let before = io_counts(&wrong);
        assert_eq!(actor.validate_access(&wrong), Err(expected));
        assert_eq!(
            actor.accept(&mut wrong, candidate(0x60 + tag as u8, b"wrong binding")),
            Err(DriveError::Binding(expected))
        );
        assert_eq!(io_counts(&wrong), before);
        assert_eq!(actor.pending_kind(), None);
        assert_eq!(actor.fault(), None);
    }

    let mut short_backend = journal.backend().clone();
    short_backend.bytes.truncate(PARTITION_SIZE - ERASE_SIZE);
    let mut short = bound(short_backend);
    let expected = JournalBindingError::CapacityMismatch {
        expected: PARTITION_SIZE,
        actual: PARTITION_SIZE - ERASE_SIZE,
    };
    let before = io_counts(&short);
    assert_eq!(actor.validate_access(&short), Err(expected));
    assert_eq!(
        actor.accept(&mut short, candidate(0x63, b"wrong capacity")),
        Err(DriveError::Binding(expected))
    );
    assert_eq!(io_counts(&short), before);
    assert_eq!(actor.pending_kind(), None);
    assert_eq!(actor.fault(), None);

    assert_eq!(
        actor.accept(&mut journal, candidate(0x64, b"correct binding")),
        Ok(AcceptanceProgress::Accepted(SubmissionId::new(8)))
    );
}

#[test]
fn wrong_backend_cannot_displace_pending_reconciliation() {
    let (mut actor, mut journal) = mounted::<2, 1>(FakeNor::formatted(), 9);
    journal.backend_mut().lose_write_reply_after(1);
    assert_eq!(
        actor.accept(&mut journal, candidate(0x70, b"pending binding")),
        Err(DriveError::Backend(FakeError::Injected))
    );
    assert_eq!(actor.pending_kind(), Some(PendingKind::Acceptance));

    let wrong_binding = JournalBinding::new(
        StorageDeviceId::new([0x71; 16]),
        TEST_ABSOLUTE_OFFSET,
        PARTITION_SIZE,
        PHYSICAL_FORMAT_VERSION,
    );
    let mut wrong = BoundJournal::new(journal.backend().clone(), wrong_binding);
    let before = io_counts(&wrong);
    assert_eq!(
        actor.drive_pending(&mut wrong),
        Err(DriveError::Binding(JournalBindingError::DeviceMismatch {
            expected: TEST_DEVICE,
            actual: StorageDeviceId::new([0x71; 16]),
        }))
    );
    assert_eq!(io_counts(&wrong), before);
    assert_eq!(actor.pending_kind(), Some(PendingKind::Acceptance));
    assert_eq!(actor.fault(), None);

    assert_eq!(
        actor.drive_pending(&mut journal),
        Ok(PendingProgress::AcceptanceCommitted(SubmissionId::new(9)))
    );
    assert_eq!(actor.pending_kind(), None);
}

#[test]
fn accepted_append_replays_after_remount() {
    let (mut actor, mut journal) = mounted::<2, 1>(FakeNor::formatted(), 10);
    let exact = candidate(1, b"first");
    assert_eq!(
        actor.accept(&mut journal, exact),
        Ok(AcceptanceProgress::Accepted(SubmissionId::new(10)))
    );
    assert_eq!(actor.state().committed_records(), 1);
    assert_eq!(
        actor.index().get(SubmissionId::new(10)).unwrap().revision(),
        0
    );

    let replayed = StorageActor::<2, 1>::mount(&mut journal, SubmissionId::new(10)).unwrap();
    let accepted = replayed.index().get(SubmissionId::new(10)).unwrap();
    assert_eq!(accepted.accepted().principal(), exact.principal());
    assert_eq!(replayed.state().committed_records(), 1);
}

#[test]
fn acceptance_replay_conflict_and_index_capacity_are_typed_outcomes() {
    let (mut actor, mut journal) = mounted::<1, 1>(FakeNor::formatted(), 20);
    let exact = candidate(2, b"same");
    assert_eq!(
        actor.accept(&mut journal, exact),
        Ok(AcceptanceProgress::Accepted(SubmissionId::new(20)))
    );
    assert_eq!(
        actor.accept(&mut journal, exact),
        Ok(AcceptanceProgress::Replay(SubmissionId::new(20)))
    );
    assert_eq!(
        actor.accept(&mut journal, candidate(2, b"different")),
        Ok(AcceptanceProgress::IdempotencyConflict {
            existing: SubmissionId::new(20)
        })
    );
    assert_eq!(
        actor.accept(&mut journal, candidate(3, b"second")),
        Ok(AcceptanceProgress::IndexExhausted)
    );
    assert_eq!(actor.fault(), None);
}

#[test]
fn lost_acceptance_reply_reconciles_autonomously_and_blocks_different_work() {
    let (mut actor, mut journal) = mounted::<2, 1>(FakeNor::formatted(), 30);
    journal.backend_mut().lose_write_reply_after(1);
    let exact = candidate(4, b"ambiguous");
    assert_eq!(
        actor.accept(&mut journal, exact),
        Err(DriveError::Backend(FakeError::Injected))
    );
    assert_eq!(actor.pending_kind(), Some(PendingKind::Acceptance));
    assert_eq!(actor.index().get(SubmissionId::new(30)), None);
    assert_eq!(
        actor.accept(&mut journal, candidate(5, b"blocked")),
        Err(DriveError::Busy {
            pending: PendingKind::Acceptance
        })
    );

    assert_eq!(
        actor.drive_pending(&mut journal),
        Ok(PendingProgress::AcceptanceCommitted(SubmissionId::new(30)))
    );
    assert_eq!(actor.pending_kind(), None);
    assert_eq!(actor.state().committed_records(), 1);
    assert_eq!(actor.state().consumed_slots(), 1);
    assert_eq!(
        actor.accept(&mut journal, exact),
        Ok(AcceptanceProgress::Replay(SubmissionId::new(30)))
    );
}

#[test]
fn boot_recovery_returns_typed_outcomes_and_commits_before_final_visibility() {
    let (mut initial, mut journal) = mounted::<2, 1>(FakeNor::formatted(), 32);
    let id = match initial.accept(&mut journal, candidate(14, b"boot recovery outcomes")) {
        Ok(AcceptanceProgress::Accepted(id)) => id,
        other => panic!("acceptance failed: {other:?}"),
    };
    let state_before = initial.state();
    let writes_before = journal.backend().writes;
    assert_eq!(
        initial.finalize_boot_recovery(&mut journal, id, 70),
        Ok(BootRecoveryProgress::ReplayQueued)
    );
    assert_eq!(initial.state(), state_before);
    assert_eq!(journal.backend().writes, writes_before);

    let request = request_for(&mut initial, id);
    assert_eq!(
        initial.persist_projector(&mut journal, request),
        Ok(PersistenceProgress::Committed)
    );
    assert_eq!(
        initial.index().get(id).unwrap().state(),
        LifecycleState::Preparing
    );

    // A real boot starts with a completely replayed index and an empty
    // volatile projector before conservative finalization is admitted.
    let mut recovered = StorageActor::<2, 1>::mount(&mut journal, SubmissionId::new(32)).unwrap();
    assert_eq!(
        recovered.finalize_boot_recovery(&mut journal, id, 71),
        Ok(BootRecoveryProgress::Finalized)
    );
    let LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::Internal(
        InternalFailure::InterruptedByReset(marker),
    ))) = recovered.index().get(id).unwrap().state()
    else {
        panic!("boot recovery did not make the interrupted state final")
    };
    assert_eq!(marker.boot_sequence(), 71);
    assert_eq!(marker.interrupted_state(), InterruptedState::Preparing);

    let mut replayed = StorageActor::<2, 1>::mount(&mut journal, SubmissionId::new(32)).unwrap();
    let writes_before = journal.backend().writes;
    assert_eq!(
        replayed.finalize_boot_recovery(&mut journal, id, 72),
        Ok(BootRecoveryProgress::AlreadyFinal)
    );
    assert_eq!(journal.backend().writes, writes_before);
}

#[test]
fn ambiguous_boot_finalization_retains_exact_plan_and_blocks_mismatched_identity() {
    let (mut initial, mut journal) = mounted::<4, 1>(FakeNor::formatted(), 34);
    let interrupted = match initial.accept(&mut journal, candidate(15, b"interrupted")) {
        Ok(AcceptanceProgress::Accepted(id)) => id,
        other => panic!("interrupted acceptance failed: {other:?}"),
    };
    let request = request_for(&mut initial, interrupted);
    assert_eq!(
        initial.persist_projector(&mut journal, request),
        Ok(PersistenceProgress::Committed)
    );
    let queued = match initial.accept(&mut journal, candidate(16, b"still queued")) {
        Ok(AcceptanceProgress::Accepted(id)) => id,
        other => panic!("queued acceptance failed: {other:?}"),
    };

    let mut actor = StorageActor::<4, 1>::mount(&mut journal, SubmissionId::new(34)).unwrap();
    let state_before = actor.state();
    journal.backend_mut().lose_write_reply_after(1);
    assert_eq!(
        actor.finalize_boot_recovery(&mut journal, interrupted, 90),
        Err(DriveError::Backend(FakeError::Injected))
    );
    assert_eq!(actor.pending_kind(), Some(PendingKind::BootRecovery));
    assert_eq!(actor.state(), state_before);
    assert_eq!(
        actor.index().get(interrupted).unwrap().state(),
        LifecycleState::Preparing,
        "an ambiguous physical result must not become live before reconciliation"
    );

    for (id, boot_sequence) in [(interrupted, 91), (queued, 90)] {
        assert_eq!(
            actor.finalize_boot_recovery(&mut journal, id, boot_sequence),
            Err(DriveError::Busy {
                pending: PendingKind::BootRecovery
            })
        );
    }
    assert_eq!(
        actor.accept(
            &mut journal,
            candidate(17, b"blocked while boot finalization is pending"),
        ),
        Err(DriveError::Busy {
            pending: PendingKind::BootRecovery
        })
    );
    assert_eq!(
        actor.begin_preparation(queued),
        Err(ProjectorOperationError::Busy {
            pending: PendingKind::BootRecovery
        })
    );

    assert_eq!(
        actor.drive_pending(&mut journal),
        Ok(PendingProgress::BootRecoveryFinalized(interrupted))
    );
    assert_eq!(actor.pending_kind(), None);
    assert_eq!(
        actor.state().committed_records(),
        state_before.committed_records() + 1
    );
    let LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::Internal(
        InternalFailure::InterruptedByReset(marker),
    ))) = actor.index().get(interrupted).unwrap().state()
    else {
        panic!("reconciled boot recovery did not become final")
    };
    assert_eq!(
        marker.boot_sequence(),
        90,
        "mismatched retries must not replace the exact retained plan"
    );
    assert_eq!(marker.interrupted_state(), InterruptedState::Preparing);
    assert_eq!(
        actor.finalize_boot_recovery(&mut journal, queued, 90),
        Ok(BootRecoveryProgress::ReplayQueued)
    );
}

#[test]
fn projector_request_commits_through_actor_owned_live_index() {
    let (mut actor, mut journal) = mounted::<4, 2>(FakeNor::formatted(), 40);
    let id = match actor.accept(&mut journal, candidate(6, b"project")) {
        Ok(AcceptanceProgress::Accepted(id)) => id,
        other => panic!("acceptance failed: {other:?}"),
    };
    let request = request_for(&mut actor, id);

    assert_eq!(
        actor.persist_projector(&mut journal, request),
        Ok(PersistenceProgress::Committed)
    );
    assert_eq!(actor.pending_kind(), None);
    assert_eq!(
        actor.index().get(id).unwrap().state(),
        LifecycleState::Preparing
    );
    assert!(actor.projector().preparation_allowed(actor.index(), id));
}

#[test]
fn ready_intent_is_absent_before_the_preparation_barrier_commits() {
    let (mut actor, mut journal) = mounted::<2, 1>(FakeNor::formatted(), 41);
    let exact = candidate(7, b"not ready");
    let id = match actor.accept(&mut journal, exact) {
        Ok(AcceptanceProgress::Accepted(id)) => id,
        other => panic!("acceptance failed: {other:?}"),
    };

    assert_eq!(actor.ready_intent(id), None);
    let _request = request_for(&mut actor, id);
    assert_eq!(actor.ready_intent(id), None);
}

#[test]
fn ready_intent_returns_the_owned_durable_intent_after_barrier_persistence() {
    let (mut actor, mut journal) = mounted::<2, 1>(FakeNor::formatted(), 42);
    let exact = candidate(8, b"ready after durable barrier");
    let id = match actor.accept(&mut journal, exact) {
        Ok(AcceptanceProgress::Accepted(id)) => id,
        other => panic!("acceptance failed: {other:?}"),
    };
    let request = request_for(&mut actor, id);
    assert_eq!(
        actor.persist_projector(&mut journal, request),
        Ok(PersistenceProgress::Committed)
    );

    assert_eq!(actor.ready_intent(id), Some(exact.intent()));
}

#[test]
fn ready_intent_is_absent_after_an_attempt_is_bound() {
    let (mut actor, _journal, id) = actor_with_durable_preparation(43, 9);
    assert!(actor.ready_intent(id).is_some());
    let (_node, job) = prepared_job(22, 200_000);

    assert_eq!(
        actor.observe_preparation(
            id,
            SubmissionPreparationObservation::Prepared(job.prepared()),
        ),
        Ok(ProjectionProgress::AttemptBound)
    );
    assert_eq!(actor.ready_intent(id), None);
}

#[test]
fn actor_projects_preparation_frame_terminal_and_exact_acknowledgement() {
    let (mut actor, mut journal, id) = actor_with_durable_preparation(42, 20);
    let (mut node, job) = prepared_job(21, 200_000);
    let prepared = job.prepared();

    assert_eq!(
        actor.observe_preparation(id, SubmissionPreparationObservation::Prepared(prepared),),
        Ok(ProjectionProgress::AttemptBound)
    );
    let frame = PreparedFrameObservation::new(
        prepared.handle(),
        prepared.attempt(),
        usize::from(prepared.packet_len()),
        *prepared.encoded_packet_sha256().as_bytes(),
    );
    let progress = actor.observe_frame(frame).unwrap();
    persist_projection(&mut actor, &mut journal, progress);
    assert!(matches!(
        actor.index().get(id).unwrap().state(),
        LifecycleState::AwaitingDelivery(_)
    ));

    assert_eq!(
        node.tick(MonotonicSeconds::new(132), &mut CounterRng::default())
            .timed_out_attempts,
        1
    );
    let terminal = node.terminal_attempts().next().unwrap();
    assert_eq!(terminal.outcome(), AttemptOutcome::DeliveryTimeout);
    let progress = actor.observe_terminal(terminal).unwrap();
    assert_eq!(actor.pending_acknowledgements().count(), 0);
    persist_projection(&mut actor, &mut journal, progress);

    let action = actor.pending_acknowledgements().next().unwrap();
    assert_eq!(action.submission(), id);
    assert_eq!(action.kind(), AcknowledgementKind::Terminal(terminal));
    assert_eq!(
        actor.report_acknowledgement(action, AcknowledgementReply::Retryable),
        Ok(())
    );
    assert_eq!(actor.pending_acknowledgements().next(), Some(action));

    assert!(matches!(
        node.rollback_queued(job, MonotonicMillis::new(100_100))
            .unwrap_or_else(|failure| panic!(
                "queued owner did not return: {:?}",
                failure.reason()
            )),
        TxCompletionDisposition::Available(_)
    ));
    assert_eq!(node.acknowledge_terminal(terminal.handle()), Ok(terminal));
    assert_eq!(
        actor.report_acknowledgement(action, AcknowledgementReply::Completed),
        Ok(())
    );
    assert_eq!(actor.pending_acknowledgements().count(), 0);
}

#[test]
fn actor_projects_recovery_and_quarantine_without_exposing_mutable_projector() {
    let (mut recovered_actor, mut recovered_journal, recovered_id) =
        actor_with_durable_preparation(44, 22);
    let (mut recovered_node, recovered_job) = prepared_job(23, 200_000);
    assert_eq!(
        recovered_actor.observe_preparation(
            recovered_id,
            SubmissionPreparationObservation::Prepared(recovered_job.prepared()),
        ),
        Ok(ProjectionProgress::AttemptBound)
    );
    assert_eq!(
        recovered_node
            .maintain_tx(MonotonicMillis::new(200_000))
            .newly_recovery_required,
        1
    );
    let recovered = match recovered_node
        .rollback_queued(recovered_job, MonotonicMillis::new(200_001))
        .unwrap_or_else(|failure| panic!("recovery rollback failed: {:?}", failure.reason()))
    {
        TxCompletionDisposition::Recovered { observation, .. } => observation,
        TxCompletionDisposition::Available(_) => panic!("expired owner bypassed recovery"),
        TxCompletionDisposition::Next(_) => panic!("single-interface job unexpectedly fanned out"),
        TxCompletionDisposition::Quarantined(_) => panic!("ordinary expiry quarantined"),
    };
    let progress = recovered_actor.observe_recovered(recovered).unwrap();
    assert_eq!(recovered_actor.pending_acknowledgements().count(), 0);
    persist_projection(&mut recovered_actor, &mut recovered_journal, progress);
    let recovery_action = recovered_actor.pending_acknowledgements().next().unwrap();
    assert_eq!(recovery_action.submission(), recovered_id);
    assert_eq!(
        recovery_action.kind(),
        AcknowledgementKind::Recovered(recovered)
    );
    assert_eq!(
        recovered_actor.report_acknowledgement(recovery_action, AcknowledgementReply::Completed),
        Ok(())
    );
    assert_eq!(recovered_actor.pending_acknowledgements().count(), 0);

    let (mut quarantined_actor, mut quarantined_journal, quarantined_id) =
        actor_with_durable_preparation(46, 24);
    let (mut quarantined_node, quarantined_job) = prepared_job(25, 200_000);
    assert_eq!(
        quarantined_actor.observe_preparation(
            quarantined_id,
            SubmissionPreparationObservation::Prepared(quarantined_job.prepared()),
        ),
        Ok(ProjectionProgress::AttemptBound)
    );
    assert_eq!(
        quarantined_node
            .maintain_tx(MonotonicMillis::new(200_000))
            .newly_recovery_required,
        1
    );
    let quarantined = match quarantined_node
        .rollback_queued(quarantined_job, MonotonicMillis::new(200_001))
        .unwrap_or_else(|failure| panic!("quarantine fixture failed: {:?}", failure.reason()))
    {
        TxCompletionDisposition::Recovered { observation, .. } => observation,
        TxCompletionDisposition::Available(_) => panic!("expired owner bypassed recovery"),
        TxCompletionDisposition::Next(_) => panic!("single-interface job unexpectedly fanned out"),
        TxCompletionDisposition::Quarantined(_) => panic!("ordinary expiry quarantined"),
    };
    let progress = quarantined_actor.observe_quarantined(quarantined).unwrap();
    persist_projection(&mut quarantined_actor, &mut quarantined_journal, progress);
    let deferred_final = quarantined_actor
        .projector()
        .pending_persistence()
        .next()
        .expect("durable quarantine audit must stage a conservative final record");
    assert_eq!(
        quarantined_actor.persist_projector(&mut quarantined_journal, deferred_final),
        Ok(PersistenceProgress::Committed)
    );
    assert!(
        quarantined_actor
            .index()
            .get(quarantined_id)
            .unwrap()
            .state()
            .is_final()
    );
    assert_eq!(quarantined_actor.pending_acknowledgements().count(), 0);
}

#[test]
fn projector_fault_from_runtime_observation_latches_at_actor_boundary() {
    let (mut actor, _journal, id) = actor_with_durable_preparation(48, 26);
    let (_node, job) = prepared_job(27, 200_000);
    let prepared = job.prepared();
    assert_eq!(
        actor.observe_preparation(id, SubmissionPreparationObservation::Prepared(prepared),),
        Ok(ProjectionProgress::AttemptBound)
    );
    let mismatched = PreparedFrameObservation::new(
        prepared.handle(),
        prepared.attempt(),
        usize::from(prepared.packet_len()),
        [0x55; 32],
    );
    let projector_fault = ProjectorFault::PacketDigestMismatch(id);
    let actor_fault = StorageFault::ProjectorRejected(ProjectorError::Faulted(projector_fault));
    assert_eq!(
        actor.observe_frame(mismatched),
        Err(ProjectorOperationError::Faulted(actor_fault))
    );
    assert_eq!(actor.fault(), Some(actor_fault));
    assert_eq!(
        actor.observe_preparation(id, SubmissionPreparationObservation::RetrySameBoot),
        Err(ProjectorOperationError::Faulted(actor_fault))
    );
}

#[test]
fn equal_external_projector_request_cannot_replace_actor_owned_projector() {
    let (mut actor, mut journal) = mounted::<4, 1>(FakeNor::formatted(), 45);
    let id = match actor.accept(&mut journal, candidate(11, b"common-origin")) {
        Ok(AcceptanceProgress::Accepted(id)) => id,
        other => panic!("acceptance failed: {other:?}"),
    };
    let owned_request = request_for(&mut actor, id);

    let mut external = SubmissionProjector::<1>::new();
    let ProjectionProgress::Persist(external_handle) =
        external.begin_preparation(actor.index(), id).unwrap()
    else {
        panic!("external preparation must produce a persistence request")
    };
    let external_request = external.persistence_request(external_handle).unwrap();
    assert_eq!(external_request, owned_request);

    assert_eq!(
        actor.persist_projector(&mut journal, external_request),
        Ok(PersistenceProgress::Committed)
    );
    assert_eq!(
        external.persistence_request(external_handle),
        Some(external_request),
        "the actor cannot mutate or substitute an external projector"
    );
    assert_eq!(
        actor
            .projector()
            .persistence_request(owned_request.handle()),
        None
    );
    assert!(actor.projector().preparation_allowed(actor.index(), id));
}

#[test]
fn projector_backend_retry_reconciles_autonomously_from_owned_request() {
    let (mut actor, mut journal) = mounted::<4, 2>(FakeNor::formatted(), 50);
    let first = match actor.accept(&mut journal, candidate(7, b"one")) {
        Ok(AcceptanceProgress::Accepted(id)) => id,
        other => panic!("first acceptance failed: {other:?}"),
    };
    let second = match actor.accept(&mut journal, candidate(8, b"two")) {
        Ok(AcceptanceProgress::Accepted(id)) => id,
        other => panic!("second acceptance failed: {other:?}"),
    };
    let first_request = request_for(&mut actor, first);
    let second_request = request_for(&mut actor, second);
    journal.backend_mut().lose_write_reply_after(1);

    assert_eq!(
        actor.persist_projector(&mut journal, first_request),
        Err(DriveError::Backend(FakeError::Injected))
    );
    assert_eq!(actor.pending_kind(), Some(PendingKind::Projector));
    assert_eq!(
        actor
            .projector()
            .persistence_request(first_request.handle()),
        Some(first_request)
    );
    assert_eq!(
        actor.persist_projector(&mut journal, second_request),
        Err(DriveError::Busy {
            pending: PendingKind::Projector
        })
    );
    assert_eq!(
        actor.begin_preparation(second),
        Err(ProjectorOperationError::Busy {
            pending: PendingKind::Projector
        })
    );
    assert_eq!(
        actor.observe_preparation(second, SubmissionPreparationObservation::RetrySameBoot),
        Err(ProjectorOperationError::Busy {
            pending: PendingKind::Projector
        })
    );

    assert_eq!(
        actor.drive_pending(&mut journal),
        Ok(PendingProgress::ProjectorCommitted)
    );
    assert_eq!(actor.pending_kind(), None);
    assert_eq!(actor.index().get(first).unwrap().revision(), 1);
    assert_eq!(actor.state().committed_records(), 3);
}

#[test]
fn projector_compaction_lost_handoff_reply_reconciles_owned_request() {
    const BANK_A_OFFSET: usize = 0x2000;

    let (mut initial, mut journal) = mounted::<2, 1>(FakeNor::formatted(), 52);
    let id = match initial.accept(&mut journal, candidate(12, b"project-through-compaction")) {
        Ok(AcceptanceProgress::Accepted(id)) => id,
        other => panic!("acceptance failed: {other:?}"),
    };
    for slot in 1..BANK_SLOT_COUNT {
        journal
            .backend_mut()
            .program(BANK_A_OFFSET + slot * SLOT_SIZE, &[0]);
    }

    let mut actor = StorageActor::<2, 1>::mount(&mut journal, SubmissionId::new(52)).unwrap();
    assert_eq!(actor.state().generation(), 1);
    assert_eq!(actor.state().committed_records(), 1);
    assert_eq!(actor.state().consumed_slots(), BANK_SLOT_COUNT);
    let request = request_for(&mut actor, id);
    let state_before = actor.state();

    // The first compaction write programs the handoff prefix. Losing the reply
    // from the second write leaves a fully committed handoff to rediscover.
    journal.backend_mut().lose_write_reply_after(1);
    assert_eq!(
        actor.persist_projector(&mut journal, request),
        Err(DriveError::Backend(FakeError::Injected))
    );
    assert_eq!(actor.pending_kind(), Some(PendingKind::Projector));
    assert_eq!(actor.state(), state_before);
    assert_eq!(actor.index().get(id).unwrap().revision(), 0);
    assert_eq!(
        actor.projector().persistence_request(request.handle()),
        Some(request)
    );

    assert_eq!(
        actor.drive_pending(&mut journal),
        Ok(PendingProgress::ProjectorCommitted)
    );
    assert_eq!(actor.pending_kind(), None);
    assert_eq!(actor.fault(), None);
    assert_eq!(actor.state().generation(), 2);
    assert_eq!(actor.state().committed_records(), 2);
    assert_eq!(actor.state().consumed_slots(), 2);
    assert_eq!(actor.index().get(id).unwrap().revision(), 1);
    assert_eq!(
        actor.index().get(id).unwrap().state(),
        LifecycleState::Preparing
    );
    assert_eq!(
        actor.projector().persistence_request(request.handle()),
        None
    );
    assert!(actor.projector().preparation_allowed(actor.index(), id));
}

#[test]
fn faulted_projector_is_rejected_before_flash_mutation() {
    let (mut actor, mut journal) = mounted::<4, 1>(FakeNor::formatted(), 55);
    let id = match actor.accept(&mut journal, candidate(9, b"projector-fault")) {
        Ok(AcceptanceProgress::Accepted(id)) => id,
        other => panic!("acceptance failed: {other:?}"),
    };
    let request = request_for(&mut actor, id);
    let mut disconnected_index = SubmissionReplay::<1>::new(SubmissionId::new(1))
        .complete()
        .unwrap();
    let projector_fault = ProjectorFault::PersistenceConflict(id);
    assert_eq!(
        actor.projector.report_persistence(
            &mut disconnected_index,
            request,
            PersistenceReply::Conflict,
        ),
        Err(ProjectorError::Faulted(projector_fault))
    );
    assert_eq!(
        actor.projector().persistence_request(request.handle()),
        Some(request),
        "faulting must leave the exact request retained"
    );

    let state_before = actor.state();
    let revision_before = actor.index().get(id).unwrap().revision();
    let writes_before = journal.backend().writes;
    let erases_before = journal.backend().erases;
    let expected = StorageFault::ProjectorRejected(ProjectorError::Faulted(projector_fault));
    assert_eq!(
        actor.persist_projector(&mut journal, request),
        Err(DriveError::Faulted(expected))
    );
    assert_eq!(actor.fault(), Some(expected));
    assert_eq!(actor.pending_kind(), None);
    assert_eq!(actor.state(), state_before);
    assert_eq!(actor.index().get(id).unwrap().revision(), revision_before);
    assert_eq!(journal.backend().writes, writes_before);
    assert_eq!(journal.backend().erases, erases_before);
}

#[test]
fn compaction_erase_lost_reply_retains_acceptance_and_autonomous_retry_recovers() {
    const BANK_A_OFFSET: usize = 0x2000;

    let mut flash = FakeNor::formatted();
    for slot in 0..BANK_SLOT_COUNT {
        flash.program(BANK_A_OFFSET + slot * SLOT_SIZE, &[0]);
    }
    let (mut actor, mut journal) = mounted::<2, 1>(flash, 70);
    assert_eq!(actor.state().generation(), 1);
    assert_eq!(actor.state().committed_records(), 0);
    assert_eq!(actor.state().consumed_slots(), BANK_SLOT_COUNT);

    journal.backend_mut().lose_erase_reply_after(0);
    let exact = candidate(10, b"after-compaction");
    let state_before = actor.state();
    assert_eq!(
        actor.accept(&mut journal, exact),
        Err(DriveError::Backend(FakeError::Injected))
    );
    assert_eq!(actor.pending_kind(), Some(PendingKind::Acceptance));
    assert_eq!(actor.state(), state_before);
    assert_eq!(actor.index().get(SubmissionId::new(70)), None);

    assert_eq!(
        actor.drive_pending(&mut journal),
        Ok(PendingProgress::AcceptanceCommitted(SubmissionId::new(70)))
    );
    assert_eq!(actor.pending_kind(), None);
    assert_eq!(actor.fault(), None);
    assert_eq!(actor.state().generation(), 2);
    assert_eq!(actor.state().committed_records(), 1);
    assert_eq!(actor.state().consumed_slots(), 1);
    assert_eq!(
        actor.index().get(SubmissionId::new(70)).unwrap().revision(),
        0
    );
}

#[test]
fn journal_acceptance_capacity_is_definitive_and_clears_pending_plan() {
    const FIRST_ID: u64 = 1_000;

    let (mut actor, mut journal) =
        mounted::<{ MAX_ACCEPTED_SUBMISSIONS + 1 }, 1>(FakeNor::formatted(), FIRST_ID);
    for index in 0..MAX_ACCEPTED_SUBMISSIONS {
        assert_eq!(
            actor.accept(&mut journal, candidate(index as u8, b"reserved-lifetime"),),
            Ok(AcceptanceProgress::Accepted(SubmissionId::new(
                FIRST_ID + index as u64
            )))
        );
    }

    assert_eq!(
        actor.state().accepted_submissions(),
        MAX_ACCEPTED_SUBMISSIONS
    );
    let state_before = actor.state();
    let writes_before = journal.backend().writes;
    let erases_before = journal.backend().erases;
    let next_id = SubmissionId::new(FIRST_ID + MAX_ACCEPTED_SUBMISSIONS as u64);
    let over_capacity = candidate(MAX_ACCEPTED_SUBMISSIONS as u8, b"one-too-many");

    for _ in 0..2 {
        assert_eq!(
            actor.accept(&mut journal, over_capacity),
            Ok(AcceptanceProgress::JournalCapacityExhausted)
        );
        assert_eq!(actor.pending_kind(), None);
        assert_eq!(actor.fault(), None);
        assert_eq!(actor.state(), state_before);
        assert_eq!(actor.index().get(next_id), None);
        assert_eq!(journal.backend().writes, writes_before);
        assert_eq!(journal.backend().erases, erases_before);
    }
}

#[test]
fn fatal_journal_error_latches_and_future_work_fails_closed() {
    let (mut actor, mut journal) = mounted::<2, 1>(FakeNor::formatted(), 60);
    journal.backend_mut().bytes[0] = 0;
    let exact = candidate(9, b"fault");
    assert_eq!(
        actor.accept(&mut journal, exact),
        Err(DriveError::Faulted(StorageFault::ManifestCorrupt))
    );
    assert_eq!(actor.fault(), Some(StorageFault::ManifestCorrupt));
    assert_eq!(
        actor.accept(&mut journal, exact),
        Err(DriveError::Faulted(StorageFault::ManifestCorrupt))
    );
    assert_eq!(
        actor.begin_preparation(SubmissionId::new(60)),
        Err(ProjectorOperationError::Faulted(
            StorageFault::ManifestCorrupt
        ))
    );
}
