extern crate std;

use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
    check_erase, check_read, check_write,
};
use reticulum_node_core::{
    AcknowledgeError, AttemptOutcome, AuthorizedFrameObservation, InterfaceSet, NodeConfig,
    NodeCore, NodeIdentity, NodeInstanceId, PermitResolution, PrepareDataRequest, RoutedTxJob,
    TxAuthorizationCandidate, TxAuthorizationPolicy, TxCompletionCode, TxCompletionDisposition,
    TxPacketBuffer, TxPermitRequirements, TxPermitReservation, TxPermitResourceId,
    TxPolicyDecision,
};
use reticulum_storage_actor::{BoundJournal, DriveError, JournalBinding, StorageDeviceId};
use reticulum_storage_journal::{
    ERASE_SIZE, PARTITION_SIZE, PHYSICAL_FORMAT_VERSION, format_erased,
};
use reticulum_storage_model::{
    AUTHORIZATION_PERMISSION_EXPERIMENTAL_SUBMIT_RNS_DATA,
    AUTHORIZATION_PERMISSION_READ_SUBMISSION_STATUS, AuthorizationSnapshot,
    DestinationHash as StoredDestinationHash, ExperimentalRnsDataIntent, FinalDisposition,
    IdempotencyKey, PrincipalId, SubmissionFailure,
};
use std::{boxed::Box, vec, vec::Vec};

use super::*;

type TestNodeCore = NodeCore<4, 2, 8, 2, 1>;

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
    lost_write_reply_after: Option<usize>,
    reject_writes: bool,
}

impl FakeNor {
    fn formatted() -> Self {
        let mut flash = Self {
            bytes: vec![0xff; PARTITION_SIZE],
            lost_write_reply_after: None,
            reject_writes: false,
        };
        format_erased(&mut flash).unwrap();
        flash
    }

    fn lose_write_reply_after(&mut self, successful_writes: usize) {
        self.lost_write_reply_after = Some(successful_writes);
    }

    fn reject_writes_permanently(&mut self) {
        self.reject_writes = true;
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
        self.bytes[from as usize..to as usize].fill(0xff);
        Ok(())
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        check_write(self, offset, bytes.len()).map_err(map_check_error)?;
        if self.reject_writes {
            return Err(FakeError::Injected);
        }
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
        _ => FakeError::Alignment,
    }
}

fn identity(tag: u8) -> NodeIdentity {
    NodeIdentity::from_private_key(&[tag; 64]).unwrap()
}

const TEST_PERMIT_RESOURCE: TxPermitResourceId = TxPermitResourceId::new([0x51; 16]);

struct AllowPolicy;

impl TxAuthorizationPolicy for AllowPolicy {
    fn authorize(&mut self, candidate: TxAuthorizationCandidate) -> TxPolicyDecision {
        TxPolicyDecision::Authorize(
            TxPermitReservation::try_new(
                candidate.requirements.resource(),
                candidate.requirements.required_units(),
            )
            .unwrap(),
        )
    }
}

struct TestNode {
    core: TestNodeCore,
    buffer: Option<&'static mut TxPacketBuffer>,
    job: Option<RoutedTxJob<'static>>,
    recovered: Option<TxRecoveryObservation>,
    hide_terminal: bool,
    destination: DestinationHash,
    prepare_calls: usize,
    acknowledge_calls: usize,
}

impl TestNode {
    fn new() -> Self {
        let receiver_node = TestNodeCore::new(
            identity(0x42),
            "reticulum",
            &["submission-runtime-receiver"],
            NodeInstanceId::new([0x82; 16]),
            NodeConfig::endpoint(),
        )
        .unwrap();
        let destination = DestinationHash::new(*receiver_node.destination_hash().as_bytes());
        let mut core = TestNodeCore::new(
            identity(0x41),
            "reticulum",
            &["submission-runtime-sender"],
            NodeInstanceId::new([0x81; 16]),
            NodeConfig::endpoint(),
        )
        .unwrap();
        core.register_peer(
            &identity(0x42),
            "reticulum",
            &["submission-runtime-receiver"],
            MonotonicSeconds::new(0),
        )
        .unwrap();
        let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        core.register_packet_buffer(buffer).unwrap();
        Self {
            core,
            buffer: Some(buffer),
            job: None,
            recovered: None,
            hide_terminal: false,
            destination,
            prepare_calls: 0,
            acknowledge_calls: 0,
        }
    }

    fn expose_frame_and_timeout(&mut self) -> AuthorizedFrameObservation {
        let job = self.job.take().expect("one prepared job must be retained");
        let requirements = TxPermitRequirements::try_new(TEST_PERMIT_RESOURCE, 1).unwrap();
        let (pending, request) = job.begin_permit(requirements);
        let reply = self
            .core
            .authorize_tx(request, MonotonicMillis::new(100_010), &mut AllowPolicy)
            .unwrap_or_else(|_| panic!("fresh permit request must authorize"));
        let resolution = pending
            .resolve(reply, MonotonicMillis::new(100_011))
            .unwrap_or_else(|_| panic!("matching permit reply must resolve"));
        let PermitResolution::Authorized(mut authorized) = resolution else {
            panic!("allow policy must authorize the fresh packet")
        };
        let observation = authorized
            .frame(MonotonicMillis::new(100_012))
            .unwrap()
            .observation();
        let completion = authorized.complete(TxCompletionCode::new(1));
        let disposition = self
            .core
            .complete_tx(completion, MonotonicMillis::new(100_013))
            .unwrap_or_else(|_| panic!("matching completion must return"));
        let TxCompletionDisposition::Available(buffer) = disposition else {
            panic!("one-hop successful completion must return the buffer")
        };
        self.buffer = Some(buffer);
        assert_eq!(
            self.core
                .tick(MonotonicSeconds::new(132), &mut CounterRng::default())
                .timed_out_attempts,
            1
        );
        observation
    }

    fn expose_frame_and_recover(&mut self) -> (AuthorizedFrameObservation, TxRecoveryObservation) {
        let job = self.job.take().expect("one prepared job must be retained");
        let requirements = TxPermitRequirements::try_new(TEST_PERMIT_RESOURCE, 1).unwrap();
        let (pending, request) = job.begin_permit(requirements);
        let reply = self
            .core
            .authorize_tx(request, MonotonicMillis::new(100_010), &mut AllowPolicy)
            .unwrap_or_else(|_| panic!("fresh permit request must authorize"));
        let resolution = pending
            .resolve(reply, MonotonicMillis::new(100_011))
            .unwrap_or_else(|_| panic!("matching permit reply must resolve"));
        let PermitResolution::Authorized(mut authorized) = resolution else {
            panic!("allow policy must authorize the fresh packet")
        };
        let frame = authorized
            .frame(MonotonicMillis::new(100_012))
            .unwrap()
            .observation();
        assert_eq!(
            self.core
                .maintain_tx(MonotonicMillis::new(200_000))
                .newly_recovery_required,
            1
        );
        let completion = authorized.complete(TxCompletionCode::new(1));
        let disposition = self
            .core
            .complete_tx(completion, MonotonicMillis::new(200_001))
            .unwrap_or_else(|_| panic!("expired matching completion must return for recovery"));
        let TxCompletionDisposition::Recovered {
            buffer,
            observation: recovered,
        } = disposition
        else {
            panic!("expired authorized owner must return a recovery observation")
        };
        self.buffer = Some(buffer);
        self.recovered = Some(recovered);
        self.hide_terminal = true;
        (frame, recovered)
    }
}

impl SubmissionNodePort for TestNode {
    fn prepare_submission<R>(
        &mut self,
        request: SubmissionPrepareRequest<'_>,
        rng: &mut R,
    ) -> SubmissionPreparationObservation
    where
        R: RngCore + CryptoRng,
    {
        self.prepare_calls += 1;
        assert_eq!(request.destination, self.destination);
        let buffer = self
            .buffer
            .take()
            .expect("test node has one available owner");
        match self.core.prepare_data_into_slot(
            buffer,
            PrepareDataRequest {
                destination: request.destination,
                plaintext: request.plaintext,
                rns_now: request.rns_now,
                owner_now: request.owner_now,
                deadline: request.deadline,
                enabled_interfaces: InterfaceSet::from_bits(1 << 1),
            },
            rng,
        ) {
            Ok(job) => {
                let prepared = job.prepared();
                self.job = Some(job);
                SubmissionPreparationObservation::Prepared(prepared)
            }
            Err(failure) => {
                let reason = failure.reason();
                self.buffer = Some(
                    failure
                        .into_buffer()
                        .unwrap_or_else(|_| panic!("ordinary preparation rejection must recycle")),
                );
                SubmissionPreparationObservation::Rejected(reason)
            }
        }
    }

    fn terminal_attempts(&self) -> impl Iterator<Item = TerminalAttempt> + '_ {
        self.core
            .terminal_attempts()
            .filter(|_| !self.hide_terminal)
    }

    fn recovered_observations(&self) -> impl Iterator<Item = TxRecoveryObservation> + '_ {
        self.recovered.iter().copied()
    }

    fn quarantined_observations(&self) -> impl Iterator<Item = TxRecoveryObservation> + '_ {
        core::iter::empty()
    }

    fn acknowledge(&mut self, action: AcknowledgementAction) -> AcknowledgementReply {
        self.acknowledge_calls += 1;
        match action.kind() {
            AcknowledgementKind::Terminal(expected) => {
                match self.core.acknowledge_terminal(expected.handle()) {
                    Ok(actual) if actual == expected => AcknowledgementReply::Completed,
                    Err(AcknowledgeError::PacketStillBound) => AcknowledgementReply::Retryable,
                    _ => AcknowledgementReply::Error(1),
                }
            }
            AcknowledgementKind::Recovered(expected) => {
                if self.recovered == Some(expected) {
                    self.recovered = None;
                    AcknowledgementReply::Completed
                } else {
                    AcknowledgementReply::Error(2)
                }
            }
        }
    }
}

fn candidate(destination: DestinationHash) -> AcceptanceCandidate {
    AcceptanceCandidate::new(
        PrincipalId::new([0x11; 16]),
        IdempotencyKey::new([0x12; 16]),
        ExperimentalRnsDataIntent::new(
            StoredDestinationHash::new(*destination.as_bytes()),
            b"portable durable submission",
        )
        .unwrap(),
        AuthorizationSnapshot::new(
            [0x13; 16],
            7,
            9,
            1,
            AUTHORIZATION_PERMISSION_EXPERIMENTAL_SUBMIT_RNS_DATA
                | AUTHORIZATION_PERMISSION_READ_SUBMISSION_STATUS,
        )
        .unwrap(),
    )
}

const TEST_DEVICE: StorageDeviceId = StorageDeviceId::new([0x51; 16]);

fn binding() -> JournalBinding {
    JournalBinding::new(
        TEST_DEVICE,
        0x0063_0000,
        PARTITION_SIZE,
        PHYSICAL_FORMAT_VERSION,
    )
}

fn formatted_access() -> BoundJournal<FakeNor> {
    BoundJournal::new(FakeNor::formatted(), binding())
}

fn try_drive(
    runtime: &mut SubmissionRuntime<4, 2>,
    access: &mut BoundJournal<FakeNor>,
    node: &mut TestNode,
) -> Result<RuntimeStep, RuntimeError<FakeError>> {
    runtime.drive_step(
        access,
        node,
        MonotonicSeconds::new(100),
        MonotonicMillis::new(100_000),
        TxLeaseDeadline::new(MonotonicMillis::new(200_000)),
        &mut CounterRng::default(),
    )
}

fn drive(
    runtime: &mut SubmissionRuntime<4, 2>,
    access: &mut BoundJournal<FakeNor>,
    node: &mut TestNode,
) -> RuntimeStep {
    try_drive(runtime, access, node).unwrap()
}

#[test]
fn durable_runtime_enforces_barrier_frame_terminal_and_ack_ordering() {
    let mut node = TestNode::new();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(40), 7).unwrap();
    assert_eq!(runtime.phase(), RuntimePhase::Recovering);
    assert!(matches!(
        runtime.accept(&mut access, candidate(node.destination)),
        Err(RuntimeError::Recovering)
    ));
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );

    let id = match runtime
        .accept(&mut access, candidate(node.destination))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("fresh candidate did not accept: {other:?}"),
    };
    assert_eq!(node.prepare_calls, 0);

    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::PreparationBarrier {
            id: observed,
            progress: ProjectionProgress::Persist(_),
        } if observed == id
    ));
    assert_eq!(node.prepare_calls, 0);
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert_eq!(node.prepare_calls, 0);
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Preparation {
            id,
            progress: ProjectionProgress::AttemptBound,
        }
    );
    assert_eq!(node.prepare_calls, 1);

    let frame = node.expose_frame_and_timeout();
    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Retain)
    );
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Durable)
    );

    let RuntimeStep::Terminal { terminal, progress } = drive(&mut runtime, &mut access, &mut node)
    else {
        panic!("terminal must be observed after the frame is durable")
    };
    assert_eq!(terminal.outcome(), AttemptOutcome::DeliveryTimeout);
    assert!(matches!(progress, ProjectionProgress::Persist(_)));
    assert_eq!(runtime.storage().pending_acknowledgements().count(), 0);
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert_eq!(runtime.storage().pending_acknowledgements().count(), 1);
    let expected_action = runtime.storage().pending_acknowledgements().next().unwrap();
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Acknowledgement {
            action: expected_action,
            reply: AcknowledgementReply::Completed,
        }
    );

    assert_eq!(runtime.storage().pending_acknowledgements().count(), 0);
    assert!(matches!(
        runtime.index().get(id).unwrap().state(),
        LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::DeliveryTimeout))
    ));
}

#[test]
fn authorized_frame_is_retained_while_the_actor_reconciles_a_lost_write_reply() {
    let mut node = TestNode::new();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(60), 8).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let _id = match runtime
        .accept(&mut access, candidate(node.destination))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("fresh candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::PreparationBarrier { .. }
    ));
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Preparation { .. }
    ));

    let frame = node.expose_frame_and_timeout();
    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Retain)
    );
    access.backend_mut().lose_write_reply_after(1);
    assert_eq!(
        try_drive(&mut runtime, &mut access, &mut node),
        Err(RuntimeError::Storage(DriveError::Backend(
            FakeError::Injected
        )))
    );
    assert_eq!(
        runtime.storage().pending_kind(),
        Some(reticulum_storage_actor::PendingKind::Projector)
    );

    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Retain)
    );
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Pending(PendingProgress::ProjectorCommitted)
    );
    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Durable)
    );
}

#[test]
fn permanent_frame_persistence_failure_never_unlocks_durability_or_acknowledgement() {
    let mut node = TestNode::new();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(65), 8).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let id = match runtime
        .accept(&mut access, candidate(node.destination))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("fresh candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::PreparationBarrier { .. }
    ));
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Preparation { .. }
    ));

    let frame = node.expose_frame_and_timeout();
    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Retain)
    );
    access.backend_mut().reject_writes_permanently();

    for _ in 0..3 {
        assert_eq!(
            try_drive(&mut runtime, &mut access, &mut node),
            Err(RuntimeError::Storage(DriveError::Backend(
                FakeError::Injected
            )))
        );
        assert_eq!(
            runtime.offer_authorized_frame(frame),
            Ok(FrameOfferProgress::Retain),
            "a permanently uncommitted frame must never become durable"
        );
        assert_eq!(node.acknowledge_calls, 0);
        assert_eq!(runtime.storage().pending_acknowledgements().count(), 0);
    }

    assert_eq!(
        runtime.storage().pending_kind(),
        Some(reticulum_storage_actor::PendingKind::Projector)
    );
    assert_eq!(
        runtime.index().get(id).unwrap().state(),
        LifecycleState::Preparing
    );
}

#[test]
fn authorized_frame_is_retained_behind_a_pre_frame_terminal_write() {
    let mut node = TestNode::new();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(70), 9).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let _id = match runtime
        .accept(&mut access, candidate(node.destination))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("fresh candidate did not accept: {other:?}"),
    };
    let _ = drive(&mut runtime, &mut access, &mut node);
    let _ = drive(&mut runtime, &mut access, &mut node);
    let _ = drive(&mut runtime, &mut access, &mut node);

    let frame = node.expose_frame_and_timeout();
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Terminal {
            progress: ProjectionProgress::Persist(_),
            ..
        }
    ));
    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Retain)
    );
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Durable)
    );
}

#[test]
fn authorized_frame_is_retained_until_a_recovery_acknowledgement_clears() {
    let mut node = TestNode::new();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(75), 10).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let id = match runtime
        .accept(&mut access, candidate(node.destination))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("fresh candidate did not accept: {other:?}"),
    };
    let _ = drive(&mut runtime, &mut access, &mut node);
    let _ = drive(&mut runtime, &mut access, &mut node);
    let _ = drive(&mut runtime, &mut access, &mut node);

    let (frame, recovered) = node.expose_frame_and_recover();
    let RuntimeStep::Recovered {
        observation,
        progress: ProjectionProgress::Persist(_),
    } = drive(&mut runtime, &mut access, &mut node)
    else {
        panic!("recovery observation must plan its durable audit")
    };
    assert_eq!(observation, recovered);
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert_eq!(
        runtime.index().get(id).unwrap().state(),
        LifecycleState::Preparing
    );
    assert_eq!(runtime.storage().pending_kind(), None);
    assert_eq!(
        runtime.storage().projector().pending_persistence().count(),
        0
    );
    let recovery_action = runtime
        .storage()
        .pending_acknowledgements()
        .next()
        .expect("durable recovery audit must unlock its exact acknowledgement");
    assert_eq!(
        recovery_action.kind(),
        AcknowledgementKind::Recovered(recovered)
    );

    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Retain)
    );
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Acknowledgement {
            action: recovery_action,
            reply: AcknowledgementReply::Completed,
        }
    );
    assert_eq!(runtime.storage().pending_acknowledgements().count(), 0);

    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Retain)
    );
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Durable)
    );
}

#[test]
fn frame_offer_retry_classification_excludes_permanent_projector_errors() {
    assert!(frame_offer_must_retry(ProjectorOperationError::Busy {
        pending: reticulum_storage_actor::PendingKind::Acceptance,
    }));
    assert!(frame_offer_must_retry(ProjectorOperationError::Rejected(
        ProjectorError::WritePending,
    )));
    assert!(frame_offer_must_retry(ProjectorOperationError::Rejected(
        ProjectorError::AcknowledgementPending,
    )));
    assert!(!frame_offer_must_retry(ProjectorOperationError::Rejected(
        ProjectorError::UnknownSubmission
    )));
    assert!(!frame_offer_must_retry(ProjectorOperationError::Rejected(
        ProjectorError::PreparationBarrierNotDurable
    )));
    assert!(!frame_offer_must_retry(ProjectorOperationError::Faulted(
        reticulum_storage_actor::StorageFault::ReservationInvariant,
    )));
}

#[test]
fn remount_boot_recovery_preserves_a_durable_final_submission() {
    let mut node = TestNode::new();
    let mut access = formatted_access();
    let expected_authorization = candidate(node.destination).authorization();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(80), 11).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let id = match runtime
        .accept(&mut access, candidate(node.destination))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("fresh candidate did not accept: {other:?}"),
    };
    let _ = drive(&mut runtime, &mut access, &mut node);
    let _ = drive(&mut runtime, &mut access, &mut node);
    let _ = drive(&mut runtime, &mut access, &mut node);
    let frame = node.expose_frame_and_timeout();
    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Retain)
    );
    let _ = drive(&mut runtime, &mut access, &mut node);
    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Durable)
    );
    let _ = drive(&mut runtime, &mut access, &mut node);
    let _ = drive(&mut runtime, &mut access, &mut node);
    let _ = drive(&mut runtime, &mut access, &mut node);

    {
        let storage = runtime.into_storage();
        assert_eq!(storage.binding(), binding());
    }
    let mut remounted =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(80), 12).unwrap();
    assert_eq!(
        remounted.recover_boot_step(&mut access),
        Ok(RecoveryStep::Submission {
            id,
            progress: BootRecoveryProgress::AlreadyFinal,
        })
    );
    assert_eq!(
        remounted.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    assert!(matches!(
        remounted.index().get(id).unwrap().state(),
        LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::DeliveryTimeout))
    ));
    assert_eq!(
        remounted
            .index()
            .get(id)
            .unwrap()
            .accepted()
            .authorization(),
        expected_authorization
    );
}

#[test]
fn wrong_binding_fails_before_node_preparation_or_acknowledgement() {
    let mut node = TestNode::new();
    let mut access = formatted_access();
    let mut wrong_access = BoundJournal::new(
        FakeNor::formatted(),
        JournalBinding::new(
            StorageDeviceId::new([0x52; 16]),
            binding().absolute_offset(),
            binding().length(),
            binding().layout_version(),
        ),
    );
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(120), 17).unwrap();
    assert!(matches!(
        runtime.recover_boot_step(&mut wrong_access),
        Err(RuntimeError::Storage(DriveError::Binding(_)))
    ));
    assert_eq!(runtime.phase(), RuntimePhase::Recovering);
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let id = match runtime
        .accept(&mut access, candidate(node.destination))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("fresh candidate did not accept: {other:?}"),
    };

    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::PreparationBarrier { id: observed, .. } if observed == id
    ));
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );

    assert!(matches!(
        try_drive(&mut runtime, &mut wrong_access, &mut node),
        Err(RuntimeError::Storage(DriveError::Binding(_)))
    ));
    assert_eq!(node.prepare_calls, 0);
    assert_eq!(node.acknowledge_calls, 0);

    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Preparation { id: observed, .. } if observed == id
    ));
    assert_eq!(node.prepare_calls, 1);
    let frame = node.expose_frame_and_timeout();
    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Retain)
    );
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Terminal { .. }
    ));
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert_eq!(runtime.storage().pending_acknowledgements().count(), 1);
    assert_eq!(node.acknowledge_calls, 0);

    assert!(matches!(
        try_drive(&mut runtime, &mut wrong_access, &mut node),
        Err(RuntimeError::Storage(DriveError::Binding(_)))
    ));
    assert_eq!(runtime.storage().pending_acknowledgements().count(), 1);
    assert_eq!(node.acknowledge_calls, 0);

    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Acknowledgement {
            reply: AcknowledgementReply::Completed,
            ..
        }
    ));
    assert_eq!(node.acknowledge_calls, 1);
}
