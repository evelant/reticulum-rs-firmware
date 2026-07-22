extern crate std;

use rand_core::{CryptoRng, RngCore};
use reticulum_node_core::{
    AcknowledgeError, AuthorizedTx, DestinationHash as NodeDestinationHash, InterfaceSet,
    MonotonicMillis, MonotonicSeconds, NodeConfig, NodeCore, NodeIdentity, NodeInstanceId,
    PacketInterfaceId, PermitResolution, PrepareDataRequest, RoutedTxJob, TxAuthorizationCandidate,
    TxAuthorizationPolicy, TxCompletionCode, TxCompletionDisposition, TxLeaseDeadline,
    TxPacketBuffer, TxPermitRequirements, TxPermitReservation, TxPermitResourceId,
    TxPolicyDecision,
};
use reticulum_storage_model::{
    AUTHORIZATION_PERMISSION_EXPERIMENTAL_SUBMIT_RNS_DATA,
    AUTHORIZATION_PERMISSION_READ_SUBMISSION_STATUS, AcceptOutcome, AcceptanceCandidate,
    ApplyOutcome, AuthorizationSnapshot, DestinationHash as StoredDestinationHash,
    ExperimentalRnsDataIntent, IdempotencyKey, PrincipalId, SubmissionReplay,
};
use std::boxed::Box;

use super::*;

type TestNode = NodeCore<4, 2, 8, 2, 1>;

const TEST_PERMIT_RESOURCE: TxPermitResourceId = TxPermitResourceId::new([0x50; 16]);

#[test]
fn in_place_projector_is_explicitly_no_drop() {
    assert!(!core::mem::needs_drop::<SubmissionProjector<128>>());
}

fn test_permit_requirements() -> TxPermitRequirements {
    TxPermitRequirements::try_new(TEST_PERMIT_RESOURCE, 1)
        .expect("test permit units must be nonzero")
}

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
        &["projector-sender"],
        NodeInstanceId::new([tag.wrapping_add(0x80); 16]),
        NodeConfig::endpoint(),
    )
    .unwrap();
    let receiver = identity(tag.wrapping_add(1));
    sender
        .register_peer(
            &receiver,
            "reticulum",
            &["projector-receiver"],
            MonotonicSeconds::new(0),
        )
        .unwrap();
    let receiver_node = TestNode::new(
        receiver,
        "reticulum",
        &["projector-receiver"],
        NodeInstanceId::new([tag.wrapping_add(0x81); 16]),
        NodeConfig::endpoint(),
    )
    .unwrap();
    let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
    sender.register_packet_buffer(buffer).unwrap();
    let mut rng = CounterRng::default();
    let job = sender
        .prepare_data_into_slot(
            buffer,
            PrepareDataRequest {
                destination: NodeDestinationHash::new(*receiver_node.destination_hash().as_bytes()),
                plaintext: b"durable projector test",
                rns_now: MonotonicSeconds::new(100),
                owner_now: MonotonicMillis::new(100_000),
                deadline: TxLeaseDeadline::new(MonotonicMillis::new(owner_deadline_ms)),
                enabled_interfaces: InterfaceSet::from_bits(1 << 1),
            },
            &mut rng,
        )
        .unwrap_or_else(|failure| panic!("preparation failed: {:?}", failure.reason()));
    (sender, job)
}

struct AllowPolicy;

impl TxAuthorizationPolicy for AllowPolicy {
    fn authorize(&mut self, candidate: TxAuthorizationCandidate) -> TxPolicyDecision {
        TxPolicyDecision::Authorize(
            TxPermitReservation::try_new(
                candidate.requirements.resource(),
                candidate.requirements.required_units(),
            )
            .expect("test policy must mirror valid requirements"),
        )
    }
}

fn authorize_job(node: &mut TestNode, job: RoutedTxJob<'static>) -> AuthorizedTx<'static> {
    let (pending, request) = job.begin_permit(test_permit_requirements());
    let reply = match node.authorize_tx(request, MonotonicMillis::new(100_010), &mut AllowPolicy) {
        Ok(reply) => reply,
        Err(_) => panic!("fresh permit request was rejected"),
    };
    let resolution = match pending.resolve(reply, MonotonicMillis::new(100_011)) {
        Ok(resolution) => resolution,
        Err(_) => panic!("matching permit reply was rejected"),
    };
    match resolution {
        PermitResolution::Authorized(authorized) => authorized,
        PermitResolution::Expired(_) => panic!("fresh authorization expired"),
        PermitResolution::Unpermitted(_) => panic!("allow policy denied authorization"),
    }
}

fn proof_for(receiver_tag: u8, attempt: AttemptToken) -> std::vec::Vec<u8> {
    let private_key = [receiver_tag; 64];
    let identity = reticulum_rns_rete::identity_from_private_key(&private_key).unwrap();
    let signature = identity.sign(attempt.as_bytes()).unwrap();
    let mut proof = std::vec![0u8; 19 + 32 + 64];
    proof[0] = 0x03;
    proof[2..18].copy_from_slice(&attempt.as_bytes()[..16]);
    proof[19..51].copy_from_slice(attempt.as_bytes());
    proof[51..].copy_from_slice(&signature);
    proof
}

fn acceptance_candidate(tag: u8) -> AcceptanceCandidate {
    let mut credential_id = [0xA5; 16];
    credential_id[0] = tag;
    AcceptanceCandidate::new(
        PrincipalId::new([tag; 16]),
        IdempotencyKey::new([tag.wrapping_add(1); 16]),
        ExperimentalRnsDataIntent::new(
            StoredDestinationHash::new([tag.wrapping_add(2); 16]),
            b"projector intent",
        )
        .unwrap(),
        AuthorizationSnapshot::new(
            credential_id,
            7,
            9,
            1,
            AUTHORIZATION_PERMISSION_EXPERIMENTAL_SUBMIT_RNS_DATA
                | AUTHORIZATION_PERMISSION_READ_SUBMISSION_STATUS,
        )
        .unwrap(),
    )
}

fn accepted_index<const N: usize>(id: u64) -> (SubmissionIndex<N>, SubmissionId) {
    let mut live = SubmissionReplay::<N>::new(SubmissionId::new(id))
        .complete()
        .unwrap();
    let accepted = accept_into(&mut live, id as u8);
    (live, accepted)
}

fn accept_into<const N: usize>(live: &mut SubmissionIndex<N>, tag: u8) -> SubmissionId {
    let AcceptOutcome::Accepted(planned) = live.plan_accept(acceptance_candidate(tag)) else {
        panic!("acceptance must plan")
    };
    let JournalEntry::Accepted(accepted) = planned.entry() else {
        panic!("acceptance plan changed record kind")
    };
    assert_eq!(live.apply_planned(planned), Ok(ApplyOutcome::Applied));
    accepted.id()
}

fn persisted_barrier<const N: usize, const P: usize>(
    projector: &mut SubmissionProjector<P>,
    live: &mut SubmissionIndex<N>,
    id: SubmissionId,
) {
    let ProjectionProgress::Persist(handle) = projector.begin_preparation(live, id).unwrap() else {
        panic!("preparation barrier must require persistence")
    };
    let request = projector.persistence_request(handle).unwrap();
    assert!(!projector.preparation_allowed(live, id));
    assert_eq!(
        projector
            .report_persistence(live, request, PersistenceReply::Applied)
            .unwrap(),
        PersistenceProgress::Committed
    );
    assert!(projector.preparation_allowed(live, id));
}

fn bind_job<const N: usize, const P: usize>(
    projector: &mut SubmissionProjector<P>,
    live: &SubmissionIndex<N>,
    id: SubmissionId,
    job: &RoutedTxJob<'_>,
) {
    assert_eq!(
        projector
            .observe_preparation(
                live,
                id,
                SubmissionPreparationObservation::Prepared(job.prepared()),
            )
            .unwrap(),
        ProjectionProgress::AttemptBound
    );
}

fn frame(job: &RoutedTxJob<'_>) -> PreparedFrameObservation {
    PreparedFrameObservation::new(
        job.attempt_handle(),
        job.attempt(),
        usize::from(job.packet_len()),
        *job.prepared().encoded_packet_sha256().as_bytes(),
    )
}

fn persist<const N: usize, const P: usize>(
    projector: &mut SubmissionProjector<P>,
    live: &mut SubmissionIndex<N>,
    progress: ProjectionProgress,
) -> PersistRequest {
    let ProjectionProgress::Persist(handle) = progress else {
        panic!("observation did not plan persistence")
    };
    let request = projector.persistence_request(handle).unwrap();
    assert_eq!(
        projector
            .report_persistence(live, request, PersistenceReply::Applied)
            .unwrap(),
        PersistenceProgress::Committed
    );
    request
}

fn recovered_observation(node: &mut TestNode, job: RoutedTxJob<'static>) -> TxRecoveryObservation {
    recovered_observation_at(node, job, 200_000, 200_001)
}

fn recovered_observation_at(
    node: &mut TestNode,
    job: RoutedTxJob<'static>,
    observed_at_ms: u64,
    rollback_at_ms: u64,
) -> TxRecoveryObservation {
    assert_eq!(
        node.maintain_tx(MonotonicMillis::new(observed_at_ms))
            .newly_recovery_required,
        1
    );
    match node
        .rollback_queued(job, MonotonicMillis::new(rollback_at_ms))
        .unwrap_or_else(|failure| panic!("recovery rollback failed: {:?}", failure.reason()))
    {
        TxCompletionDisposition::Recovered { observation, .. } => observation,
        TxCompletionDisposition::Available(_) => panic!("expired owner bypassed recovery"),
        TxCompletionDisposition::Next(_) => panic!("single interface unexpectedly fanned out"),
        TxCompletionDisposition::Quarantined(_) => panic!("ordinary expiry quarantined"),
    }
}

#[test]
fn recovery_reason_projection_preserves_categories_and_arbitrary_completion_codes() {
    for (source, durable) in [
        (
            TxRecoveryReason::DeadlineExpired,
            TransportRecoveryReason::DeadlineExpired,
        ),
        (
            TxRecoveryReason::ReceiptCancellationFailed,
            TransportRecoveryReason::ReceiptCancellationFailed,
        ),
        (
            TxRecoveryReason::HopIdentifierExhausted,
            TransportRecoveryReason::HopIdentifierExhausted,
        ),
        (
            TxRecoveryReason::Invariant,
            TransportRecoveryReason::Invariant,
        ),
    ] {
        assert_eq!(recovery_reason(source), durable);
    }

    for code in [0, 0xfff1, 0xfff2, 0xfff3, 0xffff] {
        assert_eq!(
            recovery_reason(TxRecoveryReason::CompletionFault(
                reticulum_node_core::TxCompletionCode::new(code),
            )),
            TransportRecoveryReason::CompletionFault(code)
        );
    }
}

#[test]
fn preparation_barrier_and_storage_retry_retain_the_exact_plan() {
    let (mut live, id) = accepted_index::<2>(10);
    let mut projector = SubmissionProjector::<2>::new();
    let ProjectionProgress::Persist(handle) = projector.begin_preparation(&live, id).unwrap()
    else {
        panic!("barrier did not plan")
    };
    let request = projector.persistence_request(handle).unwrap();
    assert_eq!(projector.pending_persistence().next(), Some(request));
    assert!(!projector.preparation_allowed(&live, id));
    assert_eq!(
        projector.observe_preparation(
            &live,
            id,
            SubmissionPreparationObservation::Rejected(SubmitError::AttemptLedgerFull { limit: 4 }),
        ),
        Err(ProjectorError::PreparationBarrierNotDurable)
    );
    assert_eq!(
        projector.observe_preparation(&live, id, SubmissionPreparationObservation::RetrySameBoot,),
        Err(ProjectorError::PreparationBarrierNotDurable)
    );
    let (_node, job) = prepared_job(11, 200_000);
    assert_eq!(
        projector.bind_attempt(
            &live,
            id,
            AttemptBinding {
                handle: job.attempt_handle(),
                token: job.attempt(),
                expected_packet_len: Some(job.packet_len()),
                expected_packet_sha256: Some(*job.prepared().encoded_packet_sha256().as_bytes(),),
            },
        ),
        Err(ProjectorError::PreparationBarrierNotDurable)
    );
    assert_eq!(
        projector
            .report_persistence(&mut live, request, PersistenceReply::Retryable)
            .unwrap(),
        PersistenceProgress::RetainedForRetry
    );
    assert_eq!(projector.pending_persistence().next(), Some(request));
    assert_eq!(projector.pending_acknowledgements().count(), 0);
    assert_eq!(
        projector
            .report_persistence(&mut live, request, PersistenceReply::Applied)
            .unwrap(),
        PersistenceProgress::Committed
    );
    assert!(projector.preparation_allowed(&live, id));
    assert_eq!(projector.durable_revision(&live, id), Some(1));
    assert_eq!(
        projector
            .bind_attempt(
                &live,
                id,
                AttemptBinding {
                    handle: job.attempt_handle(),
                    token: job.attempt(),
                    expected_packet_len: Some(job.packet_len()),
                    expected_packet_sha256: Some(
                        *job.prepared().encoded_packet_sha256().as_bytes(),
                    ),
                },
            )
            .unwrap(),
        ProjectionProgress::AttemptBound
    );
    assert!(!projector.preparation_allowed(&live, id));
    assert_eq!(
        projector.observe_preparation(&live, id, SubmissionPreparationObservation::RetrySameBoot,),
        Err(ProjectorError::PreparationAlreadyBound)
    );
    assert_eq!(
        projector.observe_preparation(
            &live,
            id,
            SubmissionPreparationObservation::Rejected(SubmitError::UnknownDestination),
        ),
        Err(ProjectorError::PreparationAlreadyBound)
    );
    assert_eq!(
        projector
            .observe_preparation(&live, id, SubmissionPreparationObservation::InternalFailure,),
        Err(ProjectorError::PreparationAlreadyBound)
    );
    assert_eq!(projector.pending_persistence().count(), 0);
    assert_eq!(live.get(id).unwrap().state(), LifecycleState::Preparing);
    assert_eq!(projector.fault(), None);
    let progress = projector.observe_frame(&live, frame(&job)).unwrap();
    persist(&mut projector, &mut live, progress);
    assert!(matches!(
        live.get(id).unwrap().state(),
        LifecycleState::AwaitingDelivery(_)
    ));
}

#[test]
fn transient_no_action_requires_a_known_durable_preparation_context() {
    let (live, id) = accepted_index::<2>(12);
    let mut projector = SubmissionProjector::<2>::new();
    assert_eq!(
        projector.observe_preparation(
            &live,
            id,
            SubmissionPreparationObservation::Rejected(SubmitError::ReceiptTableFull { limit: 4 }),
        ),
        Err(ProjectorError::UnknownSubmission)
    );
    assert_eq!(
        projector.observe_preparation(&live, id, SubmissionPreparationObservation::RetrySameBoot,),
        Err(ProjectorError::UnknownSubmission)
    );
    assert_eq!(projector.fault(), None);
}

#[test]
fn handles_are_unique_across_submissions_but_tokens_may_repeat() {
    let (mut live, first_id) = accepted_index::<4>(13);
    let second_id = accept_into(&mut live, 14);
    let mut projector = SubmissionProjector::<4>::new();
    persisted_barrier(&mut projector, &mut live, first_id);
    persisted_barrier(&mut projector, &mut live, second_id);
    let (_first_node, first) = prepared_job(13, 200_000);
    let (_second_node, second) = prepared_job(15, 200_000);
    bind_job(&mut projector, &live, first_id, &first);

    assert_eq!(
        projector
            .bind_attempt(
                &live,
                second_id,
                AttemptBinding {
                    handle: second.attempt_handle(),
                    token: first.attempt(),
                    expected_packet_len: Some(second.packet_len()),
                    expected_packet_sha256: Some(
                        *second.prepared().encoded_packet_sha256().as_bytes(),
                    ),
                },
            )
            .unwrap(),
        ProjectionProgress::AttemptBound
    );
    let repeated_token_frame = PreparedFrameObservation::new(
        second.attempt_handle(),
        first.attempt(),
        usize::from(second.packet_len()),
        *second.prepared().encoded_packet_sha256().as_bytes(),
    );
    let progress = projector
        .observe_frame(&live, repeated_token_frame)
        .unwrap();
    persist(&mut projector, &mut live, progress);
    assert_eq!(
        live.get(first_id).unwrap().state(),
        LifecycleState::Preparing
    );
    assert!(matches!(
        live.get(second_id).unwrap().state(),
        LifecycleState::AwaitingDelivery(_)
    ));

    let (mut live, first_id) = accepted_index::<4>(17);
    let second_id = accept_into(&mut live, 18);
    let mut projector = SubmissionProjector::<4>::new();
    persisted_barrier(&mut projector, &mut live, first_id);
    persisted_barrier(&mut projector, &mut live, second_id);
    bind_job(&mut projector, &live, first_id, &first);
    assert_eq!(
        projector.bind_attempt(
            &live,
            second_id,
            AttemptBinding {
                handle: first.attempt_handle(),
                token: second.attempt(),
                expected_packet_len: Some(second.packet_len()),
                expected_packet_sha256: Some(
                    *second.prepared().encoded_packet_sha256().as_bytes(),
                ),
            },
        ),
        Err(ProjectorError::Faulted(
            ProjectorFault::AttemptBindingConflict(second_id)
        ))
    );
    assert!(!projector.preparation_allowed(&live, second_id));
    assert_eq!(projector.pending_acknowledgements().count(), 0);
}

#[test]
fn a_fresh_projector_never_resumes_an_already_preparing_submission() {
    let (mut live, id) = accepted_index::<2>(15);
    let transition = StateTransition::new(id, 1, LifecycleState::Preparing).unwrap();
    let PlanOutcome::Append(planned) = live.plan_transition(transition).unwrap() else {
        panic!("preparation transition did not plan")
    };
    live.apply_planned(planned).unwrap();

    let mut projector = SubmissionProjector::<2>::new();
    assert_eq!(
        projector.begin_preparation(&live, id),
        Err(ProjectorError::Faulted(
            ProjectorFault::UnexpectedDurableState(id)
        ))
    );
    assert!(!projector.preparation_allowed(&live, id));
}

#[test]
fn lost_acceptance_reply_replays_the_same_principal_scoped_identifier() {
    let candidate = acceptance_candidate(7);
    let live = SubmissionReplay::<2>::new(SubmissionId::new(70))
        .complete()
        .unwrap();
    let AcceptOutcome::Accepted(planned) = live.plan_accept(candidate) else {
        panic!("acceptance did not plan")
    };
    let durable_entry = planned.entry();

    let mut replay = SubmissionReplay::<2>::new(SubmissionId::new(70));
    assert_eq!(replay.apply_entry(durable_entry), Ok(ApplyOutcome::Applied));
    let reconstructed = replay.complete().unwrap();
    assert_eq!(
        reconstructed
            .get(SubmissionId::new(70))
            .unwrap()
            .accepted()
            .authorization(),
        candidate.authorization()
    );
    assert_eq!(
        reconstructed.plan_accept(candidate),
        AcceptOutcome::Replay(SubmissionId::new(70))
    );
}

#[test]
fn lost_persistence_reply_applies_equivalent_plan_and_only_then_unlocks_ack() {
    let (mut live, id) = accepted_index::<2>(20);
    let mut projector = SubmissionProjector::<2>::new();
    persisted_barrier(&mut projector, &mut live, id);
    let (mut node, job) = prepared_job(21, 200_000);
    bind_job(&mut projector, &live, id, &job);
    let _ = node
        .rollback_queued(job, MonotonicMillis::new(100_100))
        .unwrap_or_else(|failure| panic!("rollback failed: {:?}", failure.reason()));
    let terminal = node.terminal_attempts().next().unwrap();
    let ProjectionProgress::Persist(handle) = projector.observe_terminal(&live, terminal).unwrap()
    else {
        panic!("terminal did not plan final state")
    };
    let request = projector.persistence_request(handle).unwrap();

    assert_eq!(
        live.apply_planned(request.planned),
        Ok(ApplyOutcome::Applied)
    );
    assert_eq!(projector.pending_acknowledgements().count(), 0);
    assert_eq!(
        projector
            .report_persistence(&mut live, request, PersistenceReply::AlreadyEquivalent)
            .unwrap(),
        PersistenceProgress::Committed
    );
    let action = projector.pending_acknowledgements().next().unwrap();
    assert_eq!(action.kind(), AcknowledgementKind::Terminal(terminal));
}

#[test]
fn repeated_frames_are_idempotent_and_conflicting_fanout_metadata_faults() {
    let (mut live, id) = accepted_index::<2>(30);
    let mut projector = SubmissionProjector::<2>::new();
    persisted_barrier(&mut projector, &mut live, id);
    let (_node, job) = prepared_job(31, 200_000);
    bind_job(&mut projector, &live, id, &job);
    let observation = frame(&job);
    let ProjectionProgress::Persist(handle) = projector.observe_frame(&live, observation).unwrap()
    else {
        panic!("frame did not plan awaiting state")
    };
    let request = projector.persistence_request(handle).unwrap();
    assert_eq!(
        projector.observe_frame(&live, observation),
        Ok(ProjectionProgress::AlreadyObserved)
    );
    projector
        .report_persistence(&mut live, request, PersistenceReply::Applied)
        .unwrap();
    assert_eq!(
        projector.observe_frame(&live, observation),
        Ok(ProjectionProgress::AlreadyObserved)
    );

    let conflict = PreparedFrameObservation::new(
        observation.attempt_handle(),
        observation.attempt(),
        observation.packet_len(),
        [0x45; 32],
    );
    assert_eq!(
        projector.observe_frame(&live, conflict),
        Err(ProjectorError::Faulted(
            ProjectorFault::PacketDigestMismatch(id)
        ))
    );
    assert_eq!(projector.pending_acknowledgements().count(), 0);
}

#[test]
fn delivery_before_frame_persistence_uses_preparation_digest_and_withholds_ack() {
    let (mut live, id) = accepted_index::<2>(32);
    let mut projector = SubmissionProjector::<2>::new();
    persisted_barrier(&mut projector, &mut live, id);
    let (mut node, job) = prepared_job(33, 200_000);
    let prepared = job.prepared();
    bind_job(&mut projector, &live, id, &job);
    let mut rng = CounterRng::default();
    node.ingest(
        &proof_for(34, prepared.attempt()),
        MonotonicSeconds::new(102),
        PacketInterfaceId::new(1),
        &mut rng,
    )
    .unwrap();
    let terminal = node.terminal_attempts().next().unwrap();
    assert_eq!(terminal.outcome(), AttemptOutcome::Delivered);

    let progress = projector.observe_terminal(&live, terminal).unwrap();
    assert_eq!(projector.pending_acknowledgements().count(), 0);
    persist(&mut projector, &mut live, progress);
    let expected = PreparedPacketDetails::new(
        prepared.packet_len(),
        EncodedPacketSha256::new(*prepared.encoded_packet_sha256().as_bytes()),
        RnsAttemptToken::new(*prepared.attempt().as_bytes()),
    )
    .unwrap();
    assert_eq!(
        live.get(id).unwrap().state(),
        LifecycleState::Final(FinalDisposition::Delivered(expected))
    );
    assert!(live.get(id).unwrap().may_have_transmitted());
    let action = projector.pending_acknowledgements().next().unwrap();
    assert_eq!(action.kind(), AcknowledgementKind::Terminal(terminal));

    let disposition = node
        .rollback_queued(job, MonotonicMillis::new(100_020))
        .unwrap_or_else(|failure| panic!("terminal job did not return: {:?}", failure.reason()));
    assert!(matches!(disposition, TxCompletionDisposition::Available(_)));
    assert_eq!(node.acknowledge_terminal(terminal.handle()), Ok(terminal));
    projector
        .report_acknowledgement(action, AcknowledgementReply::Completed)
        .unwrap();
}

#[test]
fn timeout_after_authorization_before_frame_is_conservative_and_late_frame_is_idempotent() {
    let (mut live, id) = accepted_index::<2>(34);
    let mut projector = SubmissionProjector::<2>::new();
    persisted_barrier(&mut projector, &mut live, id);
    let (mut node, job) = prepared_job(35, 200_000);
    let expected_frame = frame(&job);
    bind_job(&mut projector, &live, id, &job);
    let mut authorized = authorize_job(&mut node, job);
    let mut rng = CounterRng::default();
    assert_eq!(
        node.tick(MonotonicSeconds::new(132), &mut rng)
            .timed_out_attempts,
        1
    );
    let terminal = node.terminal_attempts().next().unwrap();
    assert_eq!(terminal.outcome(), AttemptOutcome::DeliveryTimeout);

    let progress = projector.observe_terminal(&live, terminal).unwrap();
    persist(&mut projector, &mut live, progress);
    assert_eq!(
        live.get(id).unwrap().state(),
        LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::DeliveryTimeout))
    );
    assert!(live.get(id).unwrap().may_have_transmitted());

    let exposed = authorized.frame(MonotonicMillis::new(100_020)).unwrap();
    assert_eq!(exposed.bytes().len(), expected_frame.packet_len());
    assert_eq!(exposed.attempt(), expected_frame.attempt());
    assert_eq!(
        projector.observe_frame(&live, expected_frame),
        Ok(ProjectionProgress::AlreadyObserved)
    );
    assert_eq!(projector.fault(), None);
    let action = projector.pending_acknowledgements().next().unwrap();
    let completion = authorized.complete(TxCompletionCode::new(91));
    let disposition = match node.complete_tx(completion, MonotonicMillis::new(100_021)) {
        Ok(disposition) => disposition,
        Err(_) => panic!("timed-out authorized owner did not return"),
    };
    assert!(matches!(disposition, TxCompletionDisposition::Available(_)));
    assert_eq!(node.acknowledge_terminal(terminal.handle()), Ok(terminal));
    projector
        .report_acknowledgement(action, AcknowledgementReply::Completed)
        .unwrap();
}

#[test]
fn recovery_only_binding_cannot_accept_caller_supplied_frame_metadata() {
    let (mut live, id) = accepted_index::<2>(36);
    let mut projector = SubmissionProjector::<2>::new();
    persisted_barrier(&mut projector, &mut live, id);
    let (mut node, job) = prepared_job(37, 200_000);
    let observation = recovered_observation(&mut node, job);
    assert_eq!(
        projector
            .bind_attempt(&live, id, AttemptBinding::from_recovery(observation),)
            .unwrap(),
        ProjectionProgress::AttemptBound
    );
    let supplied = PreparedFrameObservation::new(
        observation.attempt_handle(),
        observation.attempt(),
        100,
        [0x55; 32],
    );
    assert_eq!(
        projector.observe_frame(&live, supplied),
        Err(ProjectorError::Faulted(
            ProjectorFault::FrameWithoutQueuedMetadata(id)
        ))
    );
    assert_eq!(projector.pending_persistence().count(), 0);
}

#[test]
fn authorized_native_frame_adapts_to_backend_neutral_frame_contract() {
    let (mut node, job) = prepared_job(36, 200_000);
    let expected = job.prepared();
    let mut authorized = authorize_job(&mut node, job);
    let exposed = authorized.frame(MonotonicMillis::new(100_020)).unwrap();
    let neutral = PreparedFrameObservation::from(exposed.observation());

    assert_eq!(neutral.attempt_handle(), expected.handle());
    assert_eq!(neutral.attempt(), expected.attempt());
    assert_eq!(neutral.packet_len(), usize::from(expected.packet_len()));
    assert_eq!(
        neutral.encoded_packet_sha256(),
        *expected.encoded_packet_sha256().as_bytes()
    );
}

#[test]
fn synchronous_rejection_distinguishes_retry_no_path_rejected_and_internal() {
    assert_eq!(
        classify_preparation_rejection(SubmitError::AttemptLedgerFull { limit: 4 }),
        PreparationRejectionDecision::RetrySameBoot
    );
    assert_eq!(
        classify_preparation_rejection(SubmitError::ReceiptTableFull { limit: 4 }),
        PreparationRejectionDecision::RetrySameBoot
    );
    assert_eq!(
        classify_preparation_rejection(SubmitError::ReceiptHashAlreadyTracked),
        PreparationRejectionDecision::RetrySameBoot
    );
    assert_eq!(
        classify_preparation_rejection(SubmitError::UnknownDestination),
        PreparationRejectionDecision::Final(SubmissionFailure::NoPath)
    );
    assert_eq!(
        classify_preparation_rejection(SubmitError::PayloadTooLarge {
            actual: 400,
            maximum: 300,
        }),
        PreparationRejectionDecision::Final(SubmissionFailure::Rejected)
    );
    assert_eq!(
        classify_preparation_rejection(SubmitError::Cryptography),
        PreparationRejectionDecision::Final(SubmissionFailure::Internal(
            InternalFailure::Unspecified
        ))
    );

    let (mut live, id) = accepted_index::<2>(40);
    let mut projector = SubmissionProjector::<2>::new();
    persisted_barrier(&mut projector, &mut live, id);
    assert_eq!(
        projector
            .observe_preparation(
                &live,
                id,
                SubmissionPreparationObservation::Rejected(SubmitError::AttemptLedgerFull {
                    limit: 4,
                }),
            )
            .unwrap(),
        ProjectionProgress::NoAction
    );
    assert!(projector.preparation_allowed(&live, id));
    let progress = projector
        .observe_preparation(
            &live,
            id,
            SubmissionPreparationObservation::Rejected(SubmitError::UnknownDestination),
        )
        .unwrap();
    persist(&mut projector, &mut live, progress);
    assert_eq!(
        live.get(id).unwrap().state(),
        LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::NoPath))
    );
}

#[test]
fn rejected_quarantine_persists_audit_then_internal_final() {
    let (mut live, id) = accepted_index::<2>(50);
    let mut projector = SubmissionProjector::<2>::new();
    persisted_barrier(&mut projector, &mut live, id);
    let (mut node, job) = prepared_job(51, 200_000);
    let observation = recovered_observation(&mut node, job);
    let progress = projector
        .observe_preparation(
            &live,
            id,
            SubmissionPreparationObservation::Quarantined(observation),
        )
        .unwrap();
    let ProjectionProgress::Persist(handle) = progress else {
        panic!("quarantine audit was skipped")
    };
    let audit_request = projector.persistence_request(handle).unwrap();
    assert!(matches!(audit_request.entry(), JournalEntry::Audit(_)));
    projector
        .report_persistence(&mut live, audit_request, PersistenceReply::Applied)
        .unwrap();
    let audited = live.get(id).unwrap();
    assert_eq!(
        audited.rns_attempt_token(),
        Some(RnsAttemptToken::new(*observation.attempt().as_bytes()))
    );
    assert_eq!(
        audited.may_have_transmitted(),
        observation.record().may_have_transmitted()
    );
    let final_request = projector.pending_persistence().next().unwrap();
    assert!(matches!(
        final_request.entry(),
        JournalEntry::StateTransition(transition)
            if transition.state()
                == LifecycleState::Final(FinalDisposition::Failed(
                    SubmissionFailure::Internal(InternalFailure::Unspecified)
                ))
    ));
    projector
        .report_persistence(&mut live, final_request, PersistenceReply::Applied)
        .unwrap();
    assert_eq!(
        live.get(id).unwrap().state(),
        LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::Internal(
            InternalFailure::Unspecified
        )))
    );
}

#[test]
fn a_durable_recovery_audit_rejects_a_later_quarantine_kind_switch() {
    let (mut live, id) = accepted_index::<2>(55);
    let mut projector = SubmissionProjector::<2>::new();
    persisted_barrier(&mut projector, &mut live, id);
    let (mut node, job) = prepared_job(56, 200_000);
    bind_job(&mut projector, &live, id, &job);
    let observation = recovered_observation(&mut node, job);

    let ProjectionProgress::Persist(recovery_handle) =
        projector.observe_recovered(&live, observation).unwrap()
    else {
        panic!("recovery audit did not plan")
    };
    let recovery_request = projector.persistence_request(recovery_handle).unwrap();
    assert_eq!(
        projector.observe_quarantined(&live, observation),
        Err(ProjectorError::WritePending)
    );
    assert_eq!(
        projector
            .report_persistence(&mut live, recovery_request, PersistenceReply::Applied)
            .unwrap(),
        PersistenceProgress::Committed
    );
    assert_eq!(projector.pending_persistence().count(), 0);

    let recovery_action = projector.pending_acknowledgements().next().unwrap();
    assert_eq!(
        recovery_action.kind(),
        AcknowledgementKind::Recovered(observation)
    );
    projector
        .report_acknowledgement(recovery_action, AcknowledgementReply::Completed)
        .unwrap();

    assert_eq!(
        projector.observe_quarantined(&live, observation),
        Err(ProjectorError::Faulted(
            ProjectorFault::TransportObservationConflict(id)
        ))
    );
    assert_eq!(live.get(id).unwrap().state(), LifecycleState::Preparing);
    assert_eq!(projector.pending_persistence().count(), 0);
}

#[test]
fn a_durable_transport_audit_retains_exact_same_boot_recovery_identity() {
    let (mut live, id) = accepted_index::<2>(56);
    let mut projector = SubmissionProjector::<2>::new();
    persisted_barrier(&mut projector, &mut live, id);
    let (mut first_node, first_job) = prepared_job(57, 200_000);
    let (mut second_node, second_job) = prepared_job(57, 200_000);
    assert_eq!(first_job.attempt_handle(), second_job.attempt_handle());
    assert_eq!(first_job.attempt(), second_job.attempt());
    bind_job(&mut projector, &live, id, &first_job);

    let first = recovered_observation_at(&mut first_node, first_job, 200_000, 200_001);
    let conflicting = recovered_observation_at(&mut second_node, second_job, 200_010, 200_011);
    assert_ne!(first.record(), conflicting.record());
    assert_eq!(
        transport_event(TransportObservation {
            kind: TransportKind::Recovered,
            observation: first,
        }),
        transport_event(TransportObservation {
            kind: TransportKind::Recovered,
            observation: conflicting,
        })
    );

    let progress = projector.observe_recovered(&live, first).unwrap();
    persist(&mut projector, &mut live, progress);
    assert_eq!(
        projector.observe_recovered(&live, conflicting),
        Err(ProjectorError::Faulted(
            ProjectorFault::TransportObservationConflict(id)
        ))
    );
}

#[test]
fn quarantine_after_a_pending_terminal_audits_without_a_second_final() {
    let (mut live, id) = accepted_index::<2>(57);
    let mut projector = SubmissionProjector::<2>::new();
    persisted_barrier(&mut projector, &mut live, id);
    let (mut node, job) = prepared_job(58, 200_000);
    bind_job(&mut projector, &live, id, &job);
    let observation = recovered_observation(&mut node, job);
    let terminal = node.terminal_attempts().next().unwrap();

    let ProjectionProgress::Persist(terminal_handle) =
        projector.observe_terminal(&live, terminal).unwrap()
    else {
        panic!("terminal final did not plan")
    };
    let terminal_request = projector.persistence_request(terminal_handle).unwrap();
    assert_eq!(
        projector.observe_quarantined(&live, observation),
        Err(ProjectorError::WritePending)
    );
    projector
        .report_persistence(&mut live, terminal_request, PersistenceReply::Applied)
        .unwrap();
    assert!(live.get(id).unwrap().state().is_final());

    let ProjectionProgress::Persist(quarantine_handle) =
        projector.observe_quarantined(&live, observation).unwrap()
    else {
        panic!("late quarantine audit did not plan")
    };
    let quarantine_request = projector.persistence_request(quarantine_handle).unwrap();
    assert!(matches!(quarantine_request.entry(), JournalEntry::Audit(_)));
    projector
        .report_persistence(&mut live, quarantine_request, PersistenceReply::Applied)
        .unwrap();
    assert_eq!(projector.pending_persistence().count(), 0);
    assert!(live.get(id).unwrap().state().is_final());
}

#[test]
fn terminal_first_keeps_correlation_for_late_recovery_and_two_exact_acks() {
    let (mut live, id) = accepted_index::<2>(60);
    let mut projector = SubmissionProjector::<2>::new();
    persisted_barrier(&mut projector, &mut live, id);
    let (mut node, job) = prepared_job(61, 200_000);
    bind_job(&mut projector, &live, id, &job);
    let observation = recovered_observation(&mut node, job);
    let terminal = node.terminal_attempts().next().unwrap();

    let progress = projector.observe_terminal(&live, terminal).unwrap();
    persist(&mut projector, &mut live, progress);
    let terminal_action = projector.pending_acknowledgements().next().unwrap();
    assert_eq!(
        terminal_action.kind(),
        AcknowledgementKind::Terminal(terminal)
    );
    assert_eq!(node.acknowledge_terminal(terminal.handle()), Ok(terminal));
    projector
        .report_acknowledgement(terminal_action, AcknowledgementReply::Completed)
        .unwrap();
    assert_eq!(projector.pending_acknowledgements().count(), 0);
    assert_eq!(projector.retained_submissions(), 1);

    let progress = projector.observe_recovered(&live, observation).unwrap();
    persist(&mut projector, &mut live, progress);
    let recovery_action = projector.pending_acknowledgements().next().unwrap();
    assert_eq!(
        recovery_action.kind(),
        AcknowledgementKind::Recovered(observation)
    );
    assert!(projector.durable_state(&live, id).unwrap().is_final());
}

#[test]
fn recovery_first_then_terminal_retains_both_acknowledgements_independently() {
    let (mut live, id) = accepted_index::<2>(70);
    let mut projector = SubmissionProjector::<2>::new();
    persisted_barrier(&mut projector, &mut live, id);
    let (mut node, job) = prepared_job(71, 200_000);
    bind_job(&mut projector, &live, id, &job);
    let observation = recovered_observation(&mut node, job);
    let terminal = node.terminal_attempts().next().unwrap();

    let progress = projector.observe_recovered(&live, observation).unwrap();
    persist(&mut projector, &mut live, progress);
    let progress = projector.observe_terminal(&live, terminal).unwrap();
    persist(&mut projector, &mut live, progress);

    let actions = projector
        .pending_acknowledgements()
        .collect::<std::vec::Vec<_>>();
    assert_eq!(actions.len(), 2);
    assert!(
        actions
            .iter()
            .any(|action| { action.kind() == AcknowledgementKind::Recovered(observation) })
    );
    assert!(
        actions
            .iter()
            .any(|action| action.kind() == AcknowledgementKind::Terminal(terminal))
    );
    let terminal_action = *actions
        .iter()
        .find(|action| matches!(action.kind(), AcknowledgementKind::Terminal(_)))
        .unwrap();
    projector
        .report_acknowledgement(terminal_action, AcknowledgementReply::Completed)
        .unwrap();
    assert_eq!(projector.pending_acknowledgements().count(), 1);
}

#[test]
fn packet_still_bound_keeps_exact_terminal_ack_for_retry() {
    let (mut live, id) = accepted_index::<2>(80);
    let mut projector = SubmissionProjector::<2>::new();
    persisted_barrier(&mut projector, &mut live, id);
    let (mut node, job) = prepared_job(81, 200_000);
    bind_job(&mut projector, &live, id, &job);
    let progress = projector.observe_frame(&live, frame(&job)).unwrap();
    persist(&mut projector, &mut live, progress);

    let mut rng = CounterRng::default();
    assert_eq!(
        node.tick(MonotonicSeconds::new(132), &mut rng)
            .timed_out_attempts,
        1
    );
    let terminal = node.terminal_attempts().next().unwrap();
    let progress = projector.observe_terminal(&live, terminal).unwrap();
    persist(&mut projector, &mut live, progress);
    let action = projector.pending_acknowledgements().next().unwrap();
    assert_eq!(
        node.acknowledge_terminal(terminal.handle()),
        Err(AcknowledgeError::PacketStillBound)
    );
    projector
        .report_acknowledgement(action, AcknowledgementReply::Retryable)
        .unwrap();
    assert_eq!(projector.pending_acknowledgements().next(), Some(action));

    let _ = node
        .rollback_queued(job, MonotonicMillis::new(100_100))
        .unwrap_or_else(|failure| panic!("rollback failed: {:?}", failure.reason()));
    assert_eq!(node.acknowledge_terminal(terminal.handle()), Ok(terminal));
    projector
        .report_acknowledgement(action, AcknowledgementReply::Completed)
        .unwrap();
    assert_eq!(projector.pending_acknowledgements().count(), 0);
    assert!(projector.durable_state(&live, id).unwrap().is_final());
}

#[test]
fn unknown_and_generation_mismatched_observations_fault_without_ack() {
    let (mut live, id) = accepted_index::<2>(90);
    let mut projector = SubmissionProjector::<2>::new();
    persisted_barrier(&mut projector, &mut live, id);
    let (_first_node, first) = prepared_job(91, 200_000);
    let (_second_node, second) = prepared_job(93, 200_000);
    bind_job(&mut projector, &live, id, &first);
    let mismatched = PreparedFrameObservation::new(
        second.attempt_handle(),
        first.attempt(),
        usize::from(first.packet_len()),
        [0x90; 32],
    );
    assert_eq!(
        projector.observe_frame(&live, mismatched),
        Err(ProjectorError::Faulted(
            ProjectorFault::AttemptGenerationMismatch(id)
        ))
    );
    assert_eq!(projector.pending_acknowledgements().count(), 0);

    let (mut live, id) = accepted_index::<2>(92);
    let mut unknown = SubmissionProjector::<2>::new();
    persisted_barrier(&mut unknown, &mut live, id);
    assert_eq!(
        unknown.observe_frame(&live, frame(&first)),
        Err(ProjectorError::Faulted(ProjectorFault::UnknownAttempt))
    );
    assert!(!unknown.preparation_allowed(&live, id));
    assert_eq!(unknown.pending_acknowledgements().count(), 0);
    assert_eq!(
        unknown.observe_preparation(
            &live,
            id,
            SubmissionPreparationObservation::Rejected(SubmitError::AttemptLedgerFull { limit: 4 }),
        ),
        Err(ProjectorError::Faulted(ProjectorFault::UnknownAttempt))
    );
    assert_eq!(
        unknown.observe_preparation(&live, id, SubmissionPreparationObservation::RetrySameBoot,),
        Err(ProjectorError::Faulted(ProjectorFault::UnknownAttempt))
    );
}

#[test]
fn persistence_conflict_and_error_fault_without_unlocking_ack() {
    for (reply, expected) in [
        (
            PersistenceReply::Conflict,
            ProjectorFault::PersistenceConflict(SubmissionId::new(100)),
        ),
        (
            PersistenceReply::Error(0x1234),
            ProjectorFault::PersistenceFailure {
                submission: SubmissionId::new(100),
                code: 0x1234,
            },
        ),
    ] {
        let (mut live, id) = accepted_index::<2>(100);
        let mut projector = SubmissionProjector::<2>::new();
        let ProjectionProgress::Persist(handle) = projector.begin_preparation(&live, id).unwrap()
        else {
            panic!("barrier did not plan")
        };
        let request = projector.persistence_request(handle).unwrap();
        assert_eq!(
            projector.report_persistence(&mut live, request, reply),
            Err(ProjectorError::Faulted(expected))
        );
        assert_eq!(projector.pending_acknowledgements().count(), 0);
    }
}

#[test]
fn acknowledgement_reply_must_match_the_exact_retained_action() {
    let (mut live, id) = accepted_index::<2>(110);
    let mut projector = SubmissionProjector::<2>::new();
    persisted_barrier(&mut projector, &mut live, id);
    let (mut node, job) = prepared_job(111, 200_000);
    bind_job(&mut projector, &live, id, &job);
    let _ = node
        .rollback_queued(job, MonotonicMillis::new(100_100))
        .unwrap_or_else(|failure| panic!("rollback failed: {:?}", failure.reason()));
    let terminal = node.terminal_attempts().next().unwrap();
    let progress = projector.observe_terminal(&live, terminal).unwrap();
    persist(&mut projector, &mut live, progress);
    let exact = projector.pending_acknowledgements().next().unwrap();
    let wrong = AcknowledgementAction {
        generation: exact.generation.wrapping_add(1),
        submission: exact.submission,
        kind: exact.kind,
    };
    assert_eq!(
        projector.report_acknowledgement(wrong, AcknowledgementReply::Completed),
        Err(ProjectorError::Faulted(
            ProjectorFault::AcknowledgementReplyMismatch
        ))
    );
    assert_eq!(projector.pending_acknowledgements().next(), Some(exact));
}

#[test]
fn terminal_mapping_is_conservative_for_delivery_timeout_rejection_and_recovery() {
    let details = PreparedPacketDetails::new(
        100,
        EncodedPacketSha256::new([1; 32]),
        RnsAttemptToken::new([2; 32]),
    )
    .unwrap();
    assert_eq!(
        terminal_disposition(
            LifecycleState::AwaitingDelivery(details),
            AttemptOutcome::Delivered,
            None,
        ),
        Some(FinalDisposition::Delivered(details))
    );
    assert_eq!(
        terminal_disposition(
            LifecycleState::AwaitingDelivery(details),
            AttemptOutcome::DeliveryTimeout,
            None,
        ),
        Some(FinalDisposition::Failed(SubmissionFailure::DeliveryTimeout))
    );
    assert_eq!(
        terminal_disposition(
            LifecycleState::Preparing,
            AttemptOutcome::Unsent(AttemptUnsentReason::PermitDeadlineExpired),
            None,
        ),
        Some(FinalDisposition::Failed(SubmissionFailure::Rejected))
    );
    assert_eq!(
        terminal_disposition(
            LifecycleState::Preparing,
            AttemptOutcome::Unsent(AttemptUnsentReason::RecoveryRequired),
            None,
        ),
        Some(FinalDisposition::Failed(SubmissionFailure::Internal(
            InternalFailure::Unspecified
        )))
    );
}
