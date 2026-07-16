extern crate std;

use std::{vec, vec::Vec};

use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
    check_erase, check_read, check_write,
};
use reticulum_storage_journal::{
    BANK_SLOT_COUNT, ERASE_SIZE, MAX_ACCEPTED_SUBMISSIONS, PARTITION_SIZE, SLOT_SIZE, format_erased,
};
use reticulum_storage_model::{
    DestinationHash, ExperimentalRnsDataIntent, IdempotencyKey, LifecycleState, PrincipalId,
    SubmissionReplay,
};
use reticulum_submission_projector::{
    PersistenceReply, ProjectionProgress, ProjectorError, ProjectorFault, SubmissionProjector,
};

use super::*;

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
    actor: &mut StorageActor<FakeNor, S, P>,
    id: SubmissionId,
) -> PersistRequest {
    let ProjectionProgress::Persist(handle) = actor.begin_preparation(id).unwrap() else {
        panic!("preparation must produce a persistence request")
    };
    actor.projector().persistence_request(handle).unwrap()
}

#[test]
fn mount_is_the_only_service_entry_and_completes_replay() {
    let measured_pending_bytes = std::hint::black_box(PENDING_MUTATION_BYTES);
    assert_eq!(
        measured_pending_bytes,
        core::mem::size_of::<Option<PendingMutation>>()
    );
    assert!(matches!(
        StorageActor::<_, 2, 1>::mount(FakeNor::erased(), SubmissionId::new(1)),
        Err(MountError::Fault(StorageFault::UnformattedErased))
    ));

    let actor = StorageActor::<_, 2, 1>::mount(FakeNor::formatted(), SubmissionId::new(7)).unwrap();
    assert_eq!(actor.state().committed_records(), 0);
    assert_eq!(actor.index().next_id(), Some(SubmissionId::new(7)));
    assert_eq!(actor.pending_kind(), None);
    assert_eq!(actor.fault(), None);
}

#[test]
fn accepted_append_replays_after_remount() {
    let mut actor =
        StorageActor::<_, 2, 1>::mount(FakeNor::formatted(), SubmissionId::new(10)).unwrap();
    let exact = candidate(1, b"first");
    assert_eq!(
        actor.accept(exact),
        Ok(AcceptanceProgress::Accepted(SubmissionId::new(10)))
    );
    assert_eq!(actor.state().committed_records(), 1);
    assert_eq!(
        actor.index().get(SubmissionId::new(10)).unwrap().revision(),
        0
    );

    let flash = actor.into_flash();
    let replayed = StorageActor::<_, 2, 1>::mount(flash, SubmissionId::new(10)).unwrap();
    let accepted = replayed.index().get(SubmissionId::new(10)).unwrap();
    assert_eq!(accepted.accepted().principal(), exact.principal());
    assert_eq!(replayed.state().committed_records(), 1);
}

#[test]
fn acceptance_replay_conflict_and_index_capacity_are_typed_outcomes() {
    let mut actor =
        StorageActor::<_, 1, 1>::mount(FakeNor::formatted(), SubmissionId::new(20)).unwrap();
    let exact = candidate(2, b"same");
    assert_eq!(
        actor.accept(exact),
        Ok(AcceptanceProgress::Accepted(SubmissionId::new(20)))
    );
    assert_eq!(
        actor.accept(exact),
        Ok(AcceptanceProgress::Replay(SubmissionId::new(20)))
    );
    assert_eq!(
        actor.accept(candidate(2, b"different")),
        Ok(AcceptanceProgress::IdempotencyConflict {
            existing: SubmissionId::new(20)
        })
    );
    assert_eq!(
        actor.accept(candidate(3, b"second")),
        Ok(AcceptanceProgress::IndexExhausted)
    );
    assert_eq!(actor.fault(), None);
}

#[test]
fn lost_acceptance_reply_reconciles_autonomously_and_blocks_different_work() {
    let mut actor =
        StorageActor::<_, 2, 1>::mount(FakeNor::formatted(), SubmissionId::new(30)).unwrap();
    actor.flash.lose_write_reply_after(1);
    let exact = candidate(4, b"ambiguous");
    assert_eq!(
        actor.accept(exact),
        Err(DriveError::Backend(FakeError::Injected))
    );
    assert_eq!(actor.pending_kind(), Some(PendingKind::Acceptance));
    assert_eq!(actor.index().get(SubmissionId::new(30)), None);
    assert_eq!(
        actor.accept(candidate(5, b"blocked")),
        Err(DriveError::Busy {
            pending: PendingKind::Acceptance
        })
    );

    assert_eq!(
        actor.drive_pending(),
        Ok(PendingProgress::AcceptanceCommitted(SubmissionId::new(30)))
    );
    assert_eq!(actor.pending_kind(), None);
    assert_eq!(actor.state().committed_records(), 1);
    assert_eq!(actor.state().consumed_slots(), 1);
    assert_eq!(
        actor.accept(exact),
        Ok(AcceptanceProgress::Replay(SubmissionId::new(30)))
    );
}

#[test]
fn projector_request_commits_through_actor_owned_live_index() {
    let mut actor =
        StorageActor::<_, 4, 2>::mount(FakeNor::formatted(), SubmissionId::new(40)).unwrap();
    let id = match actor.accept(candidate(6, b"project")) {
        Ok(AcceptanceProgress::Accepted(id)) => id,
        other => panic!("acceptance failed: {other:?}"),
    };
    let request = request_for(&mut actor, id);

    assert_eq!(
        actor.persist_projector(request),
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
fn equal_external_projector_request_cannot_replace_actor_owned_projector() {
    let mut actor =
        StorageActor::<_, 4, 1>::mount(FakeNor::formatted(), SubmissionId::new(45)).unwrap();
    let id = match actor.accept(candidate(11, b"common-origin")) {
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
        actor.persist_projector(external_request),
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
    let mut actor =
        StorageActor::<_, 4, 2>::mount(FakeNor::formatted(), SubmissionId::new(50)).unwrap();
    let first = match actor.accept(candidate(7, b"one")) {
        Ok(AcceptanceProgress::Accepted(id)) => id,
        other => panic!("first acceptance failed: {other:?}"),
    };
    let second = match actor.accept(candidate(8, b"two")) {
        Ok(AcceptanceProgress::Accepted(id)) => id,
        other => panic!("second acceptance failed: {other:?}"),
    };
    let first_request = request_for(&mut actor, first);
    let second_request = request_for(&mut actor, second);
    actor.flash.lose_write_reply_after(1);

    assert_eq!(
        actor.persist_projector(first_request),
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
        actor.persist_projector(second_request),
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
        actor.drive_pending(),
        Ok(PendingProgress::ProjectorCommitted)
    );
    assert_eq!(actor.pending_kind(), None);
    assert_eq!(actor.index().get(first).unwrap().revision(), 1);
    assert_eq!(actor.state().committed_records(), 3);
}

#[test]
fn projector_compaction_lost_handoff_reply_reconciles_owned_request() {
    const BANK_A_OFFSET: usize = 0x2000;

    let mut initial =
        StorageActor::<_, 2, 1>::mount(FakeNor::formatted(), SubmissionId::new(52)).unwrap();
    let id = match initial.accept(candidate(12, b"project-through-compaction")) {
        Ok(AcceptanceProgress::Accepted(id)) => id,
        other => panic!("acceptance failed: {other:?}"),
    };
    let mut flash = initial.into_flash();
    for slot in 1..BANK_SLOT_COUNT {
        flash.program(BANK_A_OFFSET + slot * SLOT_SIZE, &[0]);
    }

    let mut actor = StorageActor::<_, 2, 1>::mount(flash, SubmissionId::new(52)).unwrap();
    assert_eq!(actor.state().generation(), 1);
    assert_eq!(actor.state().committed_records(), 1);
    assert_eq!(actor.state().consumed_slots(), BANK_SLOT_COUNT);
    let request = request_for(&mut actor, id);
    let state_before = actor.state();

    // The first compaction write programs the handoff prefix. Losing the reply
    // from the second write leaves a fully committed handoff to rediscover.
    actor.flash.lose_write_reply_after(1);
    assert_eq!(
        actor.persist_projector(request),
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
        actor.drive_pending(),
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
    let mut actor =
        StorageActor::<_, 4, 1>::mount(FakeNor::formatted(), SubmissionId::new(55)).unwrap();
    let id = match actor.accept(candidate(9, b"projector-fault")) {
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
    let writes_before = actor.flash.writes;
    let erases_before = actor.flash.erases;
    let expected = StorageFault::ProjectorRejected(ProjectorError::Faulted(projector_fault));
    assert_eq!(
        actor.persist_projector(request),
        Err(DriveError::Faulted(expected))
    );
    assert_eq!(actor.fault(), Some(expected));
    assert_eq!(actor.pending_kind(), None);
    assert_eq!(actor.state(), state_before);
    assert_eq!(actor.index().get(id).unwrap().revision(), revision_before);
    assert_eq!(actor.flash.writes, writes_before);
    assert_eq!(actor.flash.erases, erases_before);
}

#[test]
fn compaction_erase_lost_reply_retains_acceptance_and_autonomous_retry_recovers() {
    const BANK_A_OFFSET: usize = 0x2000;

    let mut flash = FakeNor::formatted();
    for slot in 0..BANK_SLOT_COUNT {
        flash.program(BANK_A_OFFSET + slot * SLOT_SIZE, &[0]);
    }
    let mut actor = StorageActor::<_, 2, 1>::mount(flash, SubmissionId::new(70)).unwrap();
    assert_eq!(actor.state().generation(), 1);
    assert_eq!(actor.state().committed_records(), 0);
    assert_eq!(actor.state().consumed_slots(), BANK_SLOT_COUNT);

    actor.flash.lose_erase_reply_after(0);
    let exact = candidate(10, b"after-compaction");
    let state_before = actor.state();
    assert_eq!(
        actor.accept(exact),
        Err(DriveError::Backend(FakeError::Injected))
    );
    assert_eq!(actor.pending_kind(), Some(PendingKind::Acceptance));
    assert_eq!(actor.state(), state_before);
    assert_eq!(actor.index().get(SubmissionId::new(70)), None);

    assert_eq!(
        actor.drive_pending(),
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

    let mut actor = StorageActor::<_, { MAX_ACCEPTED_SUBMISSIONS + 1 }, 1>::mount(
        FakeNor::formatted(),
        SubmissionId::new(FIRST_ID),
    )
    .unwrap();
    for index in 0..MAX_ACCEPTED_SUBMISSIONS {
        assert_eq!(
            actor.accept(candidate(index as u8, b"reserved-lifetime")),
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
    let writes_before = actor.flash.writes;
    let erases_before = actor.flash.erases;
    let next_id = SubmissionId::new(FIRST_ID + MAX_ACCEPTED_SUBMISSIONS as u64);
    let over_capacity = candidate(MAX_ACCEPTED_SUBMISSIONS as u8, b"one-too-many");

    for _ in 0..2 {
        assert_eq!(
            actor.accept(over_capacity),
            Ok(AcceptanceProgress::JournalCapacityExhausted)
        );
        assert_eq!(actor.pending_kind(), None);
        assert_eq!(actor.fault(), None);
        assert_eq!(actor.state(), state_before);
        assert_eq!(actor.index().get(next_id), None);
        assert_eq!(actor.flash.writes, writes_before);
        assert_eq!(actor.flash.erases, erases_before);
    }
}

#[test]
fn fatal_journal_error_latches_and_future_work_fails_closed() {
    let mut actor =
        StorageActor::<_, 2, 1>::mount(FakeNor::formatted(), SubmissionId::new(60)).unwrap();
    actor.flash.bytes[0] = 0;
    let exact = candidate(9, b"fault");
    assert_eq!(
        actor.accept(exact),
        Err(DriveError::Faulted(StorageFault::ManifestCorrupt))
    );
    assert_eq!(actor.fault(), Some(StorageFault::ManifestCorrupt));
    assert_eq!(
        actor.accept(exact),
        Err(DriveError::Faulted(StorageFault::ManifestCorrupt))
    );
    assert_eq!(
        actor.begin_preparation(SubmissionId::new(60)),
        Err(ProjectorOperationError::Faulted(
            StorageFault::ManifestCorrupt
        ))
    );
}
