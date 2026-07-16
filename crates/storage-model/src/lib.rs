//! Allocation-free durable records for standalone Reticulum firmware.
//!
//! This crate owns portable submission identities, immutable journal entries,
//! lifecycle validation, canonical record encoding, and a bounded replay
//! index. It performs no I/O and deliberately has no device-API, node, TX,
//! executor, radio, board, or platform dependency.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod codec;
mod index;
mod model;

pub use codec::{DecodeError, EncodeError, decode_journal_entry, encode_journal_entry};
pub use index::{
    AcceptOutcome, AcceptanceCandidate, ApplyError, ApplyOutcome, BootRecoveryDecision,
    IndexedSubmission, PlanOutcome, PlannedMutation, SubmissionIndex, SubmissionReplay,
};
pub use model::{
    Accepted, AuditEntry, AuditEvent, BootRecoveryMarker, BootRecoveryPolicy, ContentSha256,
    DestinationHash, EncodedPacketSha256, ExperimentalRnsDataIntent, FinalDisposition,
    IdempotencyKey, IntentTooLarge, InternalFailure, InterruptedState, InvalidPacketLength,
    JournalEntry, LifecycleState, PreparedPacketDetails, PrincipalId, RnsAttemptToken,
    StateTransition, SubmissionFailure, SubmissionId, TransitionError, TransportRecoveryReason,
    validate_transition,
};

/// Maximum application bytes in the initial experimental RNS DATA intent.
pub const MAX_EXPERIMENTAL_RNS_DATA_BYTES: usize = 383;

/// Current complete encoded Reticulum packet-buffer capacity.
pub const MAX_ENCODED_PACKET_BYTES: usize = 500;

/// Maximum bytes in one canonical durable journal record.
pub const MAX_JOURNAL_RECORD_BYTES: usize = 512;

/// Maximum lifecycle-transition records for one accepted submission.
pub const MAX_STATE_TRANSITIONS_PER_SUBMISSION: usize = 3;

/// Maximum transport audit records for one accepted submission attempt.
pub const MAX_TRANSPORT_AUDITS_PER_SUBMISSION: usize = 1;

/// Maximum committed semantic records, including acceptance, for one
/// submission under schema 1.
pub const MAX_DURABLE_RECORDS_PER_SUBMISSION: usize =
    1 + MAX_STATE_TRANSITIONS_PER_SUBMISSION + MAX_TRANSPORT_AUDITS_PER_SUBMISSION;

/// Initial project-owned durable-record schema.
pub const JOURNAL_SCHEMA_VERSION: u16 = 1;
