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
    IdempotencyKey, LxmfMessageIntent, PrincipalId, SubmissionFailure,
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
    opportunistic_lxmf_prepare_calls: usize,
    last_opportunistic_lxmf_wire_len: Option<usize>,
    forced_opportunistic_lxmf_preparation: Option<SubmissionPreparationObservation>,
    acknowledge_calls: usize,
    forced_unknown_preparations: usize,
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
            opportunistic_lxmf_prepare_calls: 0,
            last_opportunistic_lxmf_wire_len: None,
            forced_opportunistic_lxmf_preparation: None,
            acknowledge_calls: 0,
            forced_unknown_preparations: 0,
        }
    }

    fn force_unknown_preparations(&mut self, count: usize) {
        self.forced_unknown_preparations = count;
    }

    fn force_opportunistic_lxmf_preparation(
        &mut self,
        observation: SubmissionPreparationObservation,
    ) {
        self.forced_opportunistic_lxmf_preparation = Some(observation);
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
        if self.forced_unknown_preparations > 0 {
            self.forced_unknown_preparations -= 1;
            return SubmissionPreparationObservation::Rejected(SubmitError::UnknownDestination);
        }
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

    fn prepare_rehydrated_opportunistic_lxmf_submission<R>(
        &mut self,
        request: SubmissionPrepareRequest<'_>,
        _rng: &mut R,
    ) -> SubmissionPreparationObservation
    where
        R: RngCore + CryptoRng,
    {
        self.opportunistic_lxmf_prepare_calls += 1;
        assert_eq!(request.destination, self.destination);
        assert_eq!(
            request.plaintext.get(..16),
            Some(request.destination.as_bytes().as_slice())
        );
        self.last_opportunistic_lxmf_wire_len = Some(request.plaintext.len());
        self.forced_opportunistic_lxmf_preparation
            .take()
            .unwrap_or(SubmissionPreparationObservation::RetrySameBoot)
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

fn lxmf_message_candidate(destination: DestinationHash, carrier_len: usize) -> AcceptanceCandidate {
    let mut wire = vec![0x5d; 16 + carrier_len];
    wire[..16].copy_from_slice(destination.as_bytes());
    AcceptanceCandidate::new(
        PrincipalId::new([0x21; 16]),
        IdempotencyKey::new([carrier_len as u8; 16]),
        LxmfMessageIntent::new(&wire).unwrap(),
        AuthorizationSnapshot::new(
            [0x23; 16],
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

#[test]
fn mount_into_initializes_the_supplied_storage_only_after_successful_replay() {
    let mut destination = MaybeUninit::<SubmissionRuntime<4, 2>>::uninit();
    let destination_address = destination.as_mut_ptr();
    let mut access = formatted_access();

    let runtime = SubmissionRuntime::<4, 2>::mount_into(
        &mut destination,
        &mut access,
        SubmissionId::new(38),
        6,
    )
    .unwrap();

    assert_eq!(core::ptr::from_mut(runtime), destination_address);
    assert_eq!(runtime.phase(), RuntimePhase::Recovering);
    assert_eq!(runtime.index().next_id(), Some(SubmissionId::new(38)));
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    assert_eq!(runtime.phase(), RuntimePhase::Ready);
}

#[test]
fn mount_into_failure_leaves_the_destination_available_for_a_later_mount() {
    assert!(!core::mem::needs_drop::<SubmissionRuntime<4, 2>>());
    let mut destination = MaybeUninit::<SubmissionRuntime<4, 2>>::uninit();
    let mut unformatted = BoundJournal::new(
        FakeNor {
            bytes: vec![0xff; PARTITION_SIZE],
            lost_write_reply_after: None,
            reject_writes: false,
        },
        binding(),
    );

    assert!(
        SubmissionRuntime::<4, 2>::mount_into(
            &mut destination,
            &mut unformatted,
            SubmissionId::new(39),
            7,
        )
        .is_err()
    );

    let mut formatted = formatted_access();
    let runtime = SubmissionRuntime::<4, 2>::mount_into(
        &mut destination,
        &mut formatted,
        SubmissionId::new(39),
        7,
    )
    .unwrap();
    assert_eq!(runtime.index().next_id(), Some(SubmissionId::new(39)));
}

fn try_drive(
    runtime: &mut SubmissionRuntime<4, 2>,
    access: &mut BoundJournal<FakeNor>,
    node: &mut TestNode,
) -> Result<RuntimeStep, RuntimeError<FakeError>> {
    try_drive_at(runtime, access, node, 100_000)
}

fn try_drive_at(
    runtime: &mut SubmissionRuntime<4, 2>,
    access: &mut BoundJournal<FakeNor>,
    node: &mut TestNode,
    now_ms: u64,
) -> Result<RuntimeStep, RuntimeError<FakeError>> {
    runtime.drive_step(
        access,
        node,
        MonotonicSeconds::new(now_ms / 1_000),
        MonotonicMillis::new(now_ms),
        TxLeaseDeadline::new(MonotonicMillis::new(now_ms.saturating_add(100_000))),
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
fn lxmf_message_uses_the_dedicated_opportunistic_path_above_generic_data_mdu() {
    let mut node = TestNode::new();
    node.force_opportunistic_lxmf_preparation(SubmissionPreparationObservation::Rejected(
        SubmitError::PayloadTooLarge {
            actual: MAX_OPPORTUNISTIC_LXMF_CARRIER,
            maximum: reticulum_node_core::MAX_DATA_PAYLOAD,
        },
    ));
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(45), 7).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let id = match runtime
        .accept(
            &mut access,
            lxmf_message_candidate(node.destination, MAX_OPPORTUNISTIC_LXMF_CARRIER),
        )
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("fresh LXMF-message candidate did not accept: {other:?}"),
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
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Preparation {
            id: observed,
            progress: ProjectionProgress::NoAction,
        } if observed == id
    ));
    assert_eq!(node.prepare_calls, 0);
    assert_eq!(node.opportunistic_lxmf_prepare_calls, 1);
    assert_eq!(
        node.last_opportunistic_lxmf_wire_len,
        Some(16 + MAX_OPPORTUNISTIC_LXMF_CARRIER)
    );
    assert_eq!(
        runtime.index().get(id).unwrap().state(),
        LifecycleState::Preparing
    );
}

#[test]
fn lxmf_message_above_opportunistic_ceiling_waits_for_link_without_terminal_rejection() {
    let mut node = TestNode::new();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(46), 7).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let id = match runtime
        .accept(
            &mut access,
            lxmf_message_candidate(node.destination, MAX_OPPORTUNISTIC_LXMF_CARRIER + 1),
        )
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("fresh LXMF-message candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::PreparationBarrier { id: observed, .. } if observed == id
    ));
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Idle
    );
    assert_eq!(node.prepare_calls, 0);
    assert_eq!(node.opportunistic_lxmf_prepare_calls, 0);
    assert_eq!(
        runtime.index().get(id).unwrap().state(),
        LifecycleState::Preparing
    );
}

#[test]
fn unknown_destination_requests_a_path_twice_then_prepares_after_announce_learning() {
    let mut node = TestNode::new();
    node.force_unknown_preparations(3);
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(50), 8).unwrap();
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
        try_drive_at(&mut runtime, &mut access, &mut node, 100_000).unwrap(),
        RuntimeStep::PreparationBarrier { .. }
    ));
    assert_eq!(
        try_drive_at(&mut runtime, &mut access, &mut node, 100_000).unwrap(),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    let RuntimeStep::PathDiscoveryRequest {
        offer: first_offer,
        progress: ProjectionProgress::NoAction,
    } = try_drive_at(&mut runtime, &mut access, &mut node, 100_000).unwrap()
    else {
        panic!("unknown destination must produce its first exact path offer")
    };
    assert_eq!(first_offer.id(), id);
    assert_eq!(first_offer.destination(), node.destination);
    assert_eq!(first_offer.ordinal(), 1);
    assert_eq!(node.prepare_calls, 1);
    assert_eq!(
        try_drive_at(&mut runtime, &mut access, &mut node, 200_000).unwrap(),
        RuntimeStep::Idle,
        "an undispatched offer must not start or exhaust discovery clocks"
    );
    runtime
        .acknowledge_path_request_dispatched(first_offer, MonotonicMillis::new(100_000))
        .unwrap();
    assert_eq!(
        try_drive_at(&mut runtime, &mut access, &mut node, 106_999).unwrap(),
        RuntimeStep::Idle
    );
    assert!(matches!(
        try_drive_at(&mut runtime, &mut access, &mut node, 107_000).unwrap(),
        RuntimeStep::Preparation {
            id: observed,
            progress: ProjectionProgress::NoAction,
        } if observed == id
    ));
    assert_eq!(node.prepare_calls, 2);
    assert_eq!(
        try_drive_at(&mut runtime, &mut access, &mut node, 120_999).unwrap(),
        RuntimeStep::Idle
    );
    let RuntimeStep::PathDiscoveryRequest {
        offer: second_offer,
        progress: ProjectionProgress::NoAction,
    } = try_drive_at(&mut runtime, &mut access, &mut node, 121_000).unwrap()
    else {
        panic!("the shared discovery must produce its bounded retry offer")
    };
    assert_eq!(second_offer.id(), id);
    assert_eq!(second_offer.destination(), node.destination);
    assert_eq!(second_offer.ordinal(), 2);
    assert_eq!(node.prepare_calls, 3);
    runtime
        .acknowledge_path_request_dispatched(second_offer, MonotonicMillis::new(121_000))
        .unwrap();
    assert_eq!(
        try_drive_at(&mut runtime, &mut access, &mut node, 127_999).unwrap(),
        RuntimeStep::Idle
    );
    assert!(matches!(
        try_drive_at(&mut runtime, &mut access, &mut node, 128_000).unwrap(),
        RuntimeStep::Preparation {
            id: observed,
            progress: ProjectionProgress::AttemptBound,
        } if observed == id
    ));
    assert_eq!(node.prepare_calls, 4);
    assert_eq!(
        runtime.index().get(id).unwrap().state(),
        LifecycleState::Preparing
    );
}

#[test]
fn path_discovery_exhaustion_commits_terminal_no_path() {
    let mut node = TestNode::new();
    node.force_unknown_preparations(4);
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(55), 9).unwrap();
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
    let _ = try_drive_at(&mut runtime, &mut access, &mut node, 100_000).unwrap();
    let _ = try_drive_at(&mut runtime, &mut access, &mut node, 100_000).unwrap();
    let RuntimeStep::PathDiscoveryRequest {
        offer: first_offer, ..
    } = try_drive_at(&mut runtime, &mut access, &mut node, 100_000).unwrap()
    else {
        panic!("first path offer must be produced")
    };
    assert_eq!(first_offer.ordinal(), 1);
    runtime
        .acknowledge_path_request_dispatched(first_offer, MonotonicMillis::new(100_000))
        .unwrap();
    let _ = try_drive_at(&mut runtime, &mut access, &mut node, 107_000).unwrap();
    let RuntimeStep::PathDiscoveryRequest {
        offer: second_offer,
        ..
    } = try_drive_at(&mut runtime, &mut access, &mut node, 121_000).unwrap()
    else {
        panic!("second path offer must be produced")
    };
    assert_eq!(second_offer.ordinal(), 2);
    runtime
        .acknowledge_path_request_dispatched(second_offer, MonotonicMillis::new(121_000))
        .unwrap();
    assert!(matches!(
        try_drive_at(&mut runtime, &mut access, &mut node, 128_000).unwrap(),
        RuntimeStep::Preparation {
            id: observed,
            progress: ProjectionProgress::Persist(_),
        } if observed == id
    ));
    assert_eq!(
        try_drive_at(&mut runtime, &mut access, &mut node, 128_000).unwrap(),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert_eq!(
        runtime.index().get(id).unwrap().state(),
        LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::NoPath))
    );
}

#[test]
fn path_discovery_is_shared_per_destination_and_acknowledged_exactly() {
    let node = TestNode::new();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(58), 10).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let first_id = SubmissionId::new(58);
    let second_id = SubmissionId::new(59);
    let unknown = SubmissionPreparationObservation::Rejected(SubmitError::UnknownDestination);

    let (first_observation, first_offer) =
        runtime.classify_path_discovery(first_id, node.destination, unknown, 100_000);
    assert_eq!(
        first_observation,
        SubmissionPreparationObservation::RetrySameBoot
    );
    let first_offer = first_offer.expect("first destination miss must own one offer");
    assert_eq!(first_offer.ordinal(), 1);

    let (second_observation, duplicate_offer) =
        runtime.classify_path_discovery(second_id, node.destination, unknown, 100_000);
    assert_eq!(
        second_observation,
        SubmissionPreparationObservation::RetrySameBoot
    );
    assert_eq!(duplicate_offer, None);

    let mismatched = PathDiscoveryOffer {
        id: second_id,
        destination: node.destination,
        ordinal: 1,
    };
    assert_eq!(
        runtime.acknowledge_path_request_dispatched(mismatched, MonotonicMillis::new(100_000)),
        Err(PathDiscoveryAcknowledgeError::OfferMismatch)
    );
    runtime
        .acknowledge_path_request_dispatched(first_offer, MonotonicMillis::new(100_000))
        .unwrap();
    assert!(!runtime.path_discovery_due(node.destination, 106_999));
    assert!(runtime.path_discovery_due(node.destination, 107_000));

    let (_, early_retry) =
        runtime.classify_path_discovery(second_id, node.destination, unknown, 107_000);
    assert_eq!(early_retry, None);
    assert!(!runtime.path_discovery_due(node.destination, 120_999));
    assert!(runtime.path_discovery_due(node.destination, 121_000));
    let (_, shared_retry) =
        runtime.classify_path_discovery(second_id, node.destination, unknown, 121_000);
    let shared_retry = shared_retry.expect("one shared retry must become due");
    assert_eq!(shared_retry.id(), second_id);
    assert_eq!(shared_retry.ordinal(), 2);
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
