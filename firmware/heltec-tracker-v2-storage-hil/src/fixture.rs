//! Deterministic full-budget semantic fixture for the physical HIL.

use reticulum_storage_model::{
    Accepted, AuditEntry, AuditEvent, DestinationHash, EncodedPacketSha256,
    ExperimentalRnsDataIntent, FinalDisposition, IdempotencyKey, JournalEntry, LifecycleState,
    PreparedPacketDetails, PrincipalId, RnsAttemptToken, StateTransition, SubmissionFailure,
    SubmissionId, TransportRecoveryReason,
};

pub const RECORD_COUNT: usize = reticulum_storage_model::MAX_DURABLE_RECORDS_PER_SUBMISSION;

const SUBMISSION: SubmissionId = SubmissionId::new(0x4849_4c00_0000_0001);
const ATTEMPT: RnsAttemptToken = RnsAttemptToken::new([0xa5; 32]);

pub const fn submission_id() -> SubmissionId {
    SUBMISSION
}

pub fn records() -> [JournalEntry; RECORD_COUNT] {
    assert_eq!(
        RECORD_COUNT, 5,
        "storage HIL fixture must exercise the full schema-1 budget"
    );

    let intent = ExperimentalRnsDataIntent::new(
        DestinationHash::new([0xd3; 16]),
        b"reticulum-storage-journal physical HIL fixture v1",
    )
    .expect("fixed storage HIL payload must fit");
    let accepted = Accepted::new(
        SUBMISSION,
        PrincipalId::new([0x51; 16]),
        IdempotencyKey::new([0x71; 16]),
        intent,
    );
    let prepared = PreparedPacketDetails::new(97, EncodedPacketSha256::new([0x6e; 32]), ATTEMPT)
        .expect("fixed storage HIL packet metadata must be valid");
    let preparing = StateTransition::new(SUBMISSION, 1, LifecycleState::Preparing)
        .expect("fixed storage HIL preparing transition must be valid");
    let awaiting = StateTransition::new(SUBMISSION, 2, LifecycleState::AwaitingDelivery(prepared))
        .expect("fixed storage HIL awaiting transition must be valid");
    let audit = AuditEntry::new(
        SUBMISSION,
        3,
        AuditEvent::TransportRecovered {
            rns_attempt_token: ATTEMPT,
            may_have_transmitted: true,
            reason: TransportRecoveryReason::CompletionFault(0x4849),
        },
    )
    .expect("fixed storage HIL audit revision must be nonzero");
    let delivered = StateTransition::new(
        SUBMISSION,
        4,
        LifecycleState::Final(FinalDisposition::Delivered(prepared)),
    )
    .expect("fixed storage HIL delivered transition must be valid");

    [
        JournalEntry::Accepted(accepted),
        JournalEntry::StateTransition(preparing),
        JournalEntry::StateTransition(awaiting),
        JournalEntry::Audit(audit),
        JournalEntry::StateTransition(delivered),
    ]
}

/// A different valid terminal for the fixture's already committed revision 4.
pub fn same_key_conflict() -> JournalEntry {
    let timeout = StateTransition::new(
        SUBMISSION,
        4,
        LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::DeliveryTimeout)),
    )
    .expect("fixed storage HIL conflicting transition must be structurally valid");
    JournalEntry::StateTransition(timeout)
}
