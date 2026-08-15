//! Allocation-free durable records for standalone Reticulum firmware.
//!
//! This crate owns portable submission identities, immutable journal entries,
//! lifecycle validation, canonical record encoding, and a bounded replay
//! index. It performs no I/O and deliberately has no device-API, node, TX,
//! executor, radio, board, or platform dependency.

#![no_std]
#![deny(unsafe_code)]
#![deny(missing_docs)]

mod codec;
mod index;
mod model;

pub use codec::{DecodeError, EncodeError, decode_journal_entry, encode_journal_entry};
pub use index::{
    AcceptOutcome, AcceptanceCandidate, ApplyError, ApplyOutcome, BootRecoveryDecision,
    IndexedSubmission, PlanOutcome, PlannedMutation, SubmissionIndex, SubmissionReplay,
    SubmissionReplayInPlace,
};
pub use model::{
    Accepted, AuditEntry, AuditEvent, AuthorizationSnapshot, AuthorizationSnapshotError,
    BootRecoveryMarker, BootRecoveryPolicy, ContentSha256, DestinationHash, EncodedPacketSha256,
    ExperimentalRnsDataIntent, FinalDisposition, IdempotencyKey, IntentTooLarge, InternalFailure,
    InterruptedState, InvalidLxmfMessageWireLength, InvalidPacketLength, JournalEntry,
    LifecycleState, LxmfMessageIntent, PreparedPacketDetails, PrincipalId, RnsAttemptToken,
    StateTransition, SubmissionFailure, SubmissionId, SubmissionIntent, TransitionError,
    TransportRecoveryReason, validate_transition,
};

/// Maximum application bytes in the initial experimental RNS DATA intent.
pub const MAX_EXPERIMENTAL_RNS_DATA_BYTES: usize = 383;

/// Minimum complete LXMF wire bytes needed to retain its destination.
pub const MIN_LXMF_MESSAGE_WIRE_BYTES: usize = 16;

/// Maximum complete LXMF wire bytes retained by the current inline intent.
///
/// This equals the current plain Link DATA message boundary but does not select
/// a delivery method. Smaller messages may use opportunistic DATA, while a
/// future larger-message intent requires Resource-backed durable storage.
pub const MAX_INLINE_LXMF_MESSAGE_WIRE_BYTES: usize = 431;

/// Current complete encoded Reticulum packet-buffer capacity.
pub const MAX_ENCODED_PACKET_BYTES: usize = 500;

/// Maximum bytes in one canonical durable journal record.
pub const MAX_JOURNAL_RECORD_BYTES: usize = 544;

/// Maximum lifecycle-transition records for one accepted submission.
pub const MAX_STATE_TRANSITIONS_PER_SUBMISSION: usize = 3;

/// Maximum durable transport audit records for one accepted submission.
///
/// Coherent LXMF carrier recovery is covered by its durable logical delivery
/// loop and does not consume an attempt-specific audit record.
pub const MAX_TRANSPORT_AUDITS_PER_SUBMISSION: usize = 1;

/// Maximum committed semantic records, including acceptance, for one
/// submission under schema 3.
pub const MAX_DURABLE_RECORDS_PER_SUBMISSION: usize =
    1 + MAX_STATE_TRANSITIONS_PER_SUBMISSION + MAX_TRANSPORT_AUDITS_PER_SUBMISSION;

/// Current project-owned durable-record semantic schema.
pub const JOURNAL_SCHEMA_VERSION: u16 = 3;

/// Stable persisted bit granting owned-submission status reads.
pub const AUTHORIZATION_PERMISSION_READ_SUBMISSION_STATUS: u32 = 1 << 0;

/// Stable persisted bit granting experimental outbound RNS DATA submission.
pub const AUTHORIZATION_PERMISSION_EXPERIMENTAL_SUBMIT_RNS_DATA: u32 = 1 << 1;

/// Stable persisted bit granting board-owned network configuration mutation.
///
/// A submission authorization snapshot retains the authenticated credential's
/// complete permission vocabulary even though this bit does not itself
/// authorize a submission.
pub const AUTHORIZATION_PERMISSION_MANAGE_NETWORK_CONFIG: u32 = 1 << 2;

/// Complete permission mask understood by durable authorization schema 3.
pub const AUTHORIZATION_KNOWN_PERMISSION_BITS: u32 = AUTHORIZATION_PERMISSION_READ_SUBMISSION_STATUS
    | AUTHORIZATION_PERMISSION_EXPERIMENTAL_SUBMIT_RNS_DATA
    | AUTHORIZATION_PERMISSION_MANAGE_NETWORK_CONFIG;
