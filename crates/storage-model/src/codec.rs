//! Strict canonical indexed-CBOR journal codec.

use minicbor::{Decoder, Encoder, encode::write::Cursor};

use crate::model::{
    Accepted, AuditEntry, AuditEvent, AuthorizationSnapshot, BootRecoveryMarker,
    BootRecoveryPolicy, DestinationHash, EncodedPacketSha256, ExperimentalRnsDataIntent,
    FinalDisposition, IdempotencyKey, InternalFailure, InterruptedState, JournalEntry,
    LifecycleState, PreparedPacketDetails, PrincipalId, RnsAttemptToken, StateTransition,
    SubmissionFailure, SubmissionId, TransportRecoveryReason,
};
use crate::{JOURNAL_SCHEMA_VERSION, MAX_JOURNAL_RECORD_BYTES};

const ENTRY_ACCEPTED: u8 = 0;
const ENTRY_STATE_TRANSITION: u8 = 1;
const ENTRY_AUDIT: u8 = 2;

const STATE_PREPARING: u8 = 1;
const STATE_AWAITING_DELIVERY: u8 = 2;
const STATE_DELIVERED: u8 = 3;
const STATE_FAILED: u8 = 4;
const STATE_CANCELLED: u8 = 5;

const FAILURE_NO_PATH: u8 = 0;
const FAILURE_DELIVERY_TIMEOUT: u8 = 1;
const FAILURE_REJECTED: u8 = 2;
const FAILURE_INTERNAL: u8 = 3;

const INTERNAL_UNSPECIFIED: u8 = 0;
const INTERNAL_INTERRUPTED_BY_RESET: u8 = 1;

const AUDIT_TRANSPORT_RECOVERED: u8 = 1;
const AUDIT_TRANSPORT_QUARANTINED: u8 = 2;

const RECOVERY_DEADLINE_EXPIRED: u8 = 0;
const RECOVERY_COMPLETION_FAULT: u8 = 1;
const RECOVERY_RECEIPT_CANCELLATION_FAILED: u8 = 2;
const RECOVERY_HOP_IDENTIFIER_EXHAUSTED: u8 = 3;
const RECOVERY_INVARIANT: u8 = 4;

/// Failure to encode one canonical durable record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EncodeError {
    /// Caller-provided storage cannot hold the record.
    OutputTooSmall,
    /// A modeled record unexpectedly exceeds the frozen record maximum.
    RecordTooLarge,
}

/// Failure to decode exactly one canonical durable record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeError {
    /// Input exceeds the frozen durable-record maximum.
    RecordTooLarge,
    /// Input is not the required definite-map CBOR shape.
    Malformed,
    /// Durable record selected an unsupported schema version.
    UnsupportedSchema,
    /// Durable record selected an unsupported entry kind.
    UnsupportedEntryKind,
    /// A fixed or bounded byte string had an invalid length.
    InvalidByteStringLength,
    /// A numeric enum, revision, or modeled combination was invalid.
    InvalidValue,
    /// A complete CBOR value was followed by additional bytes.
    TrailingData,
    /// Input was semantically decodable but not the one canonical encoding.
    NonCanonical,
}

macro_rules! put {
    ($expression:expr) => {{
        $expression.map_err(|_| ())?;
    }};
}

/// Encode one durable record as canonical, definite-map indexed CBOR.
pub fn encode_journal_entry(entry: &JournalEntry, output: &mut [u8]) -> Result<usize, EncodeError> {
    let capacity = output.len().min(MAX_JOURNAL_RECORD_BYTES);
    match encode_inner(entry, &mut output[..capacity]) {
        Ok(written) => Ok(written),
        Err(()) if output.len() < MAX_JOURNAL_RECORD_BYTES => Err(EncodeError::OutputTooSmall),
        Err(()) => Err(EncodeError::RecordTooLarge),
    }
}

fn encode_inner(entry: &JournalEntry, output: &mut [u8]) -> Result<usize, ()> {
    let mut encoder = Encoder::new(Cursor::new(output));
    put!(encoder.map(3));
    put!(encoder.u8(0));
    put!(encoder.u16(JOURNAL_SCHEMA_VERSION));
    put!(encoder.u8(1));
    match entry {
        JournalEntry::Accepted(_) => put!(encoder.u8(ENTRY_ACCEPTED)),
        JournalEntry::StateTransition(_) => put!(encoder.u8(ENTRY_STATE_TRANSITION)),
        JournalEntry::Audit(_) => put!(encoder.u8(ENTRY_AUDIT)),
    };
    put!(encoder.u8(2));
    match entry {
        JournalEntry::Accepted(accepted) => encode_accepted(&mut encoder, *accepted)?,
        JournalEntry::StateTransition(transition) => encode_transition(&mut encoder, *transition)?,
        JournalEntry::Audit(audit) => encode_audit(&mut encoder, *audit)?,
    }
    Ok(encoder.writer().position())
}

fn encode_accepted(encoder: &mut Encoder<Cursor<&mut [u8]>>, accepted: Accepted) -> Result<(), ()> {
    put!(encoder.map(7));
    put!(encoder.u8(0));
    put!(encoder.u64(accepted.id().get()));
    put!(encoder.u8(1));
    put!(encoder.bytes(accepted.principal().as_bytes()));
    put!(encoder.u8(2));
    put!(encoder.bytes(accepted.idempotency_key().as_bytes()));
    put!(encoder.u8(3));
    encode_authorization(encoder, accepted.authorization())?;
    put!(encoder.u8(4));
    put!(encoder.u8(0));
    put!(encoder.u8(5));
    put!(encoder.bytes(accepted.intent().destination().as_bytes()));
    put!(encoder.u8(6));
    put!(encoder.bytes(accepted.intent().payload()));
    Ok(())
}

fn encode_authorization(
    encoder: &mut Encoder<Cursor<&mut [u8]>>,
    authorization: AuthorizationSnapshot,
) -> Result<(), ()> {
    put!(encoder.map(5));
    put!(encoder.u8(0));
    put!(encoder.bytes(authorization.credential_id()));
    put!(encoder.u8(1));
    put!(encoder.u64(authorization.credential_generation()));
    put!(encoder.u8(2));
    put!(encoder.u64(authorization.authority_revision()));
    put!(encoder.u8(3));
    put!(encoder.u32(authorization.policy_version()));
    put!(encoder.u8(4));
    put!(encoder.u32(authorization.granted_permission_bits()));
    Ok(())
}

fn encode_transition(
    encoder: &mut Encoder<Cursor<&mut [u8]>>,
    transition: StateTransition,
) -> Result<(), ()> {
    let entries = transition_map_entries(transition.state());
    put!(encoder.map(entries));
    put!(encoder.u8(0));
    put!(encoder.u64(transition.id().get()));
    put!(encoder.u8(1));
    put!(encoder.u64(transition.revision()));
    put!(encoder.u8(2));
    match transition.state() {
        LifecycleState::Queued => return Err(()),
        LifecycleState::Preparing => put!(encoder.u8(STATE_PREPARING)),
        LifecycleState::AwaitingDelivery(details) => {
            put!(encoder.u8(STATE_AWAITING_DELIVERY));
            encode_prepared_details(encoder, details)?;
        }
        LifecycleState::Final(FinalDisposition::Delivered(details)) => {
            put!(encoder.u8(STATE_DELIVERED));
            encode_prepared_details(encoder, details)?;
        }
        LifecycleState::Final(FinalDisposition::Failed(failure)) => {
            put!(encoder.u8(STATE_FAILED));
            put!(encoder.u8(5));
            match failure {
                SubmissionFailure::NoPath => put!(encoder.u8(FAILURE_NO_PATH)),
                SubmissionFailure::DeliveryTimeout => {
                    put!(encoder.u8(FAILURE_DELIVERY_TIMEOUT))
                }
                SubmissionFailure::Rejected => put!(encoder.u8(FAILURE_REJECTED)),
                SubmissionFailure::Internal(internal) => {
                    put!(encoder.u8(FAILURE_INTERNAL));
                    put!(encoder.u8(6));
                    match internal {
                        InternalFailure::Unspecified => put!(encoder.u8(INTERNAL_UNSPECIFIED)),
                        InternalFailure::InterruptedByReset(marker) => {
                            put!(encoder.u8(INTERNAL_INTERRUPTED_BY_RESET));
                            encode_boot_marker_fields(encoder, marker)?;
                        }
                    }
                }
            }
        }
        LifecycleState::Final(FinalDisposition::Cancelled) => {
            put!(encoder.u8(STATE_CANCELLED));
        }
    }
    Ok(())
}

const fn transition_map_entries(state: LifecycleState) -> u64 {
    match state {
        LifecycleState::Queued | LifecycleState::Preparing => 3,
        LifecycleState::AwaitingDelivery(_)
        | LifecycleState::Final(FinalDisposition::Delivered(_)) => 6,
        LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::Internal(
            InternalFailure::InterruptedByReset(_),
        ))) => 8,
        LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::Internal(
            InternalFailure::Unspecified,
        ))) => 5,
        LifecycleState::Final(FinalDisposition::Failed(_)) => 4,
        LifecycleState::Final(FinalDisposition::Cancelled) => 3,
    }
}

fn encode_prepared_details(
    encoder: &mut Encoder<Cursor<&mut [u8]>>,
    details: PreparedPacketDetails,
) -> Result<(), ()> {
    put!(encoder.u8(3));
    put!(encoder.u16(details.packet_len()));
    put!(encoder.u8(4));
    put!(encoder.bytes(details.encoded_packet_sha256().as_bytes()));
    put!(encoder.u8(5));
    put!(encoder.bytes(details.rns_attempt_token().as_bytes()));
    Ok(())
}

fn encode_boot_marker_fields(
    encoder: &mut Encoder<Cursor<&mut [u8]>>,
    marker: BootRecoveryMarker,
) -> Result<(), ()> {
    put!(encoder.u8(7));
    put!(encoder.u64(marker.boot_sequence()));
    put!(encoder.u8(8));
    put!(encoder.u8(interrupted_state_code(marker.interrupted_state())));
    put!(encoder.u8(9));
    put!(encoder.u8(boot_policy_code(marker.policy())));
    Ok(())
}

fn encode_audit(encoder: &mut Encoder<Cursor<&mut [u8]>>, audit: AuditEntry) -> Result<(), ()> {
    let entries = match audit.event() {
        AuditEvent::TransportRecovered { reason, .. }
        | AuditEvent::TransportQuarantined { reason, .. } => transport_audit_entries(reason),
    };
    put!(encoder.map(entries));
    put!(encoder.u8(0));
    put!(encoder.u64(audit.id().get()));
    put!(encoder.u8(1));
    put!(encoder.u64(audit.revision()));
    put!(encoder.u8(2));
    match audit.event() {
        AuditEvent::TransportRecovered {
            rns_attempt_token,
            may_have_transmitted,
            reason,
        } => {
            put!(encoder.u8(AUDIT_TRANSPORT_RECOVERED));
            put!(encoder.u8(3));
            put!(encoder.bytes(rns_attempt_token.as_bytes()));
            put!(encoder.u8(4));
            put!(encoder.bool(may_have_transmitted));
            encode_transport_recovery_reason(encoder, reason)?;
        }
        AuditEvent::TransportQuarantined {
            rns_attempt_token,
            may_have_transmitted,
            reason,
        } => {
            put!(encoder.u8(AUDIT_TRANSPORT_QUARANTINED));
            put!(encoder.u8(3));
            put!(encoder.bytes(rns_attempt_token.as_bytes()));
            put!(encoder.u8(4));
            put!(encoder.bool(may_have_transmitted));
            encode_transport_recovery_reason(encoder, reason)?;
        }
    }
    Ok(())
}

const fn transport_audit_entries(reason: TransportRecoveryReason) -> u64 {
    match reason {
        TransportRecoveryReason::CompletionFault(_) => 7,
        TransportRecoveryReason::DeadlineExpired
        | TransportRecoveryReason::ReceiptCancellationFailed
        | TransportRecoveryReason::HopIdentifierExhausted
        | TransportRecoveryReason::Invariant => 6,
    }
}

fn encode_transport_recovery_reason(
    encoder: &mut Encoder<Cursor<&mut [u8]>>,
    reason: TransportRecoveryReason,
) -> Result<(), ()> {
    put!(encoder.u8(5));
    match reason {
        TransportRecoveryReason::DeadlineExpired => put!(encoder.u8(RECOVERY_DEADLINE_EXPIRED)),
        TransportRecoveryReason::CompletionFault(code) => {
            put!(encoder.u8(RECOVERY_COMPLETION_FAULT));
            put!(encoder.u8(6));
            put!(encoder.u16(code));
        }
        TransportRecoveryReason::ReceiptCancellationFailed => {
            put!(encoder.u8(RECOVERY_RECEIPT_CANCELLATION_FAILED));
        }
        TransportRecoveryReason::HopIdentifierExhausted => {
            put!(encoder.u8(RECOVERY_HOP_IDENTIFIER_EXHAUSTED));
        }
        TransportRecoveryReason::Invariant => put!(encoder.u8(RECOVERY_INVARIANT)),
    }
    Ok(())
}

const fn interrupted_state_code(state: InterruptedState) -> u8 {
    match state {
        InterruptedState::Preparing => 0,
        InterruptedState::AwaitingDelivery => 1,
    }
}

const fn boot_policy_code(policy: BootRecoveryPolicy) -> u8 {
    match policy {
        BootRecoveryPolicy::FailInternal => 0,
    }
}

/// Decode exactly one canonical durable journal record.
pub fn decode_journal_entry(input: &[u8]) -> Result<JournalEntry, DecodeError> {
    if input.len() > MAX_JOURNAL_RECORD_BYTES {
        return Err(DecodeError::RecordTooLarge);
    }
    let mut decoder = Decoder::new(input);
    expect_map(&mut decoder, 3)?;
    expect_key(&mut decoder, 0)?;
    let schema = decoder.u16().map_err(|_| DecodeError::Malformed)?;
    if schema != JOURNAL_SCHEMA_VERSION {
        return Err(DecodeError::UnsupportedSchema);
    }
    expect_key(&mut decoder, 1)?;
    let kind = decoder.u8().map_err(|_| DecodeError::Malformed)?;
    expect_key(&mut decoder, 2)?;
    let entry = match kind {
        ENTRY_ACCEPTED => JournalEntry::Accepted(decode_accepted(&mut decoder)?),
        ENTRY_STATE_TRANSITION => JournalEntry::StateTransition(decode_transition(&mut decoder)?),
        ENTRY_AUDIT => JournalEntry::Audit(decode_audit(&mut decoder)?),
        _ => return Err(DecodeError::UnsupportedEntryKind),
    };
    if decoder.position() != input.len() {
        return Err(DecodeError::TrailingData);
    }
    let mut canonical = [0_u8; MAX_JOURNAL_RECORD_BYTES];
    let written =
        encode_journal_entry(&entry, &mut canonical).map_err(|_| DecodeError::InvalidValue)?;
    if &canonical[..written] != input {
        return Err(DecodeError::NonCanonical);
    }
    Ok(entry)
}

fn decode_accepted(decoder: &mut Decoder<'_>) -> Result<Accepted, DecodeError> {
    expect_map(decoder, 7)?;
    expect_key(decoder, 0)?;
    let id = SubmissionId::new(decoder.u64().map_err(|_| DecodeError::Malformed)?);
    expect_key(decoder, 1)?;
    let principal = PrincipalId::new(decode_fixed_bytes(decoder)?);
    expect_key(decoder, 2)?;
    let idempotency_key = IdempotencyKey::new(decode_fixed_bytes(decoder)?);
    expect_key(decoder, 3)?;
    let authorization = decode_authorization(decoder)?;
    expect_key(decoder, 4)?;
    if decoder.u8().map_err(|_| DecodeError::Malformed)? != 0 {
        return Err(DecodeError::InvalidValue);
    }
    expect_key(decoder, 5)?;
    let destination = DestinationHash::new(decode_fixed_bytes(decoder)?);
    expect_key(decoder, 6)?;
    let payload = decoder.bytes().map_err(|_| DecodeError::Malformed)?;
    let intent = ExperimentalRnsDataIntent::new(destination, payload)
        .map_err(|_| DecodeError::InvalidByteStringLength)?;
    Ok(Accepted::from_parts(
        id,
        principal,
        idempotency_key,
        intent,
        authorization,
    ))
}

fn decode_authorization(decoder: &mut Decoder<'_>) -> Result<AuthorizationSnapshot, DecodeError> {
    expect_map(decoder, 5)?;
    expect_key(decoder, 0)?;
    let credential_id = decode_fixed_bytes(decoder)?;
    expect_key(decoder, 1)?;
    let credential_generation = decoder.u64().map_err(|_| DecodeError::Malformed)?;
    expect_key(decoder, 2)?;
    let authority_revision = decoder.u64().map_err(|_| DecodeError::Malformed)?;
    expect_key(decoder, 3)?;
    let policy_version = decoder.u32().map_err(|_| DecodeError::Malformed)?;
    expect_key(decoder, 4)?;
    let granted_permission_bits = decoder.u32().map_err(|_| DecodeError::Malformed)?;
    AuthorizationSnapshot::new(
        credential_id,
        credential_generation,
        authority_revision,
        policy_version,
        granted_permission_bits,
    )
    .map_err(|_| DecodeError::InvalidValue)
}

fn decode_transition(decoder: &mut Decoder<'_>) -> Result<StateTransition, DecodeError> {
    let entries = definite_map_len(decoder)?;
    expect_key(decoder, 0)?;
    let id = SubmissionId::new(decoder.u64().map_err(|_| DecodeError::Malformed)?);
    expect_key(decoder, 1)?;
    let revision = decoder.u64().map_err(|_| DecodeError::Malformed)?;
    expect_key(decoder, 2)?;
    let state_code = decoder.u8().map_err(|_| DecodeError::Malformed)?;
    let state = match state_code {
        STATE_PREPARING if entries == 3 => LifecycleState::Preparing,
        STATE_AWAITING_DELIVERY if entries == 6 => {
            LifecycleState::AwaitingDelivery(decode_prepared_details(decoder)?)
        }
        STATE_DELIVERED if entries == 6 => LifecycleState::Final(FinalDisposition::Delivered(
            decode_prepared_details(decoder)?,
        )),
        STATE_FAILED => {
            LifecycleState::Final(FinalDisposition::Failed(decode_failure(decoder, entries)?))
        }
        STATE_CANCELLED if entries == 3 => LifecycleState::Final(FinalDisposition::Cancelled),
        _ => return Err(DecodeError::InvalidValue),
    };
    StateTransition::new(id, revision, state).map_err(|_| DecodeError::InvalidValue)
}

fn decode_prepared_details(
    decoder: &mut Decoder<'_>,
) -> Result<PreparedPacketDetails, DecodeError> {
    expect_key(decoder, 3)?;
    let packet_len = decoder.u16().map_err(|_| DecodeError::Malformed)?;
    expect_key(decoder, 4)?;
    let digest = EncodedPacketSha256::new(decode_fixed_bytes(decoder)?);
    expect_key(decoder, 5)?;
    let attempt = RnsAttemptToken::new(decode_fixed_bytes(decoder)?);
    PreparedPacketDetails::new(packet_len, digest, attempt).map_err(|_| DecodeError::InvalidValue)
}

fn decode_failure(
    decoder: &mut Decoder<'_>,
    entries: u64,
) -> Result<SubmissionFailure, DecodeError> {
    expect_key(decoder, 5)?;
    let failure = decoder.u8().map_err(|_| DecodeError::Malformed)?;
    match failure {
        FAILURE_NO_PATH if entries == 4 => Ok(SubmissionFailure::NoPath),
        FAILURE_DELIVERY_TIMEOUT if entries == 4 => Ok(SubmissionFailure::DeliveryTimeout),
        FAILURE_REJECTED if entries == 4 => Ok(SubmissionFailure::Rejected),
        FAILURE_INTERNAL => {
            expect_key(decoder, 6)?;
            match decoder.u8().map_err(|_| DecodeError::Malformed)? {
                INTERNAL_UNSPECIFIED if entries == 5 => {
                    Ok(SubmissionFailure::Internal(InternalFailure::Unspecified))
                }
                INTERNAL_INTERRUPTED_BY_RESET if entries == 8 => {
                    let marker = decode_boot_marker_fields(decoder)?;
                    Ok(SubmissionFailure::Internal(
                        InternalFailure::InterruptedByReset(marker),
                    ))
                }
                _ => Err(DecodeError::InvalidValue),
            }
        }
        _ => Err(DecodeError::InvalidValue),
    }
}

fn decode_boot_marker_fields(decoder: &mut Decoder<'_>) -> Result<BootRecoveryMarker, DecodeError> {
    expect_key(decoder, 7)?;
    let boot_sequence = decoder.u64().map_err(|_| DecodeError::Malformed)?;
    expect_key(decoder, 8)?;
    let interrupted = decode_interrupted_state(decoder)?;
    expect_key(decoder, 9)?;
    decode_boot_policy(decoder)?;
    Ok(BootRecoveryMarker::new(boot_sequence, interrupted))
}

fn decode_audit(decoder: &mut Decoder<'_>) -> Result<AuditEntry, DecodeError> {
    let entries = definite_map_len(decoder)?;
    expect_key(decoder, 0)?;
    let id = SubmissionId::new(decoder.u64().map_err(|_| DecodeError::Malformed)?);
    expect_key(decoder, 1)?;
    let revision = decoder.u64().map_err(|_| DecodeError::Malformed)?;
    expect_key(decoder, 2)?;
    let kind = decoder.u8().map_err(|_| DecodeError::Malformed)?;
    let event = match kind {
        AUDIT_TRANSPORT_RECOVERED if entries == 6 || entries == 7 => {
            expect_key(decoder, 3)?;
            let rns_attempt_token = RnsAttemptToken::new(decode_fixed_bytes(decoder)?);
            expect_key(decoder, 4)?;
            let may_have_transmitted = decoder.bool().map_err(|_| DecodeError::Malformed)?;
            let reason = decode_transport_recovery_reason(decoder, entries)?;
            AuditEvent::TransportRecovered {
                rns_attempt_token,
                may_have_transmitted,
                reason,
            }
        }
        AUDIT_TRANSPORT_QUARANTINED if entries == 6 || entries == 7 => {
            expect_key(decoder, 3)?;
            let rns_attempt_token = RnsAttemptToken::new(decode_fixed_bytes(decoder)?);
            expect_key(decoder, 4)?;
            let may_have_transmitted = decoder.bool().map_err(|_| DecodeError::Malformed)?;
            let reason = decode_transport_recovery_reason(decoder, entries)?;
            AuditEvent::TransportQuarantined {
                rns_attempt_token,
                may_have_transmitted,
                reason,
            }
        }
        _ => return Err(DecodeError::InvalidValue),
    };
    AuditEntry::new(id, revision, event).ok_or(DecodeError::InvalidValue)
}

fn decode_transport_recovery_reason(
    decoder: &mut Decoder<'_>,
    entries: u64,
) -> Result<TransportRecoveryReason, DecodeError> {
    expect_key(decoder, 5)?;
    match decoder.u8().map_err(|_| DecodeError::Malformed)? {
        RECOVERY_DEADLINE_EXPIRED if entries == 6 => Ok(TransportRecoveryReason::DeadlineExpired),
        RECOVERY_COMPLETION_FAULT if entries == 7 => {
            expect_key(decoder, 6)?;
            Ok(TransportRecoveryReason::CompletionFault(
                decoder.u16().map_err(|_| DecodeError::Malformed)?,
            ))
        }
        RECOVERY_RECEIPT_CANCELLATION_FAILED if entries == 6 => {
            Ok(TransportRecoveryReason::ReceiptCancellationFailed)
        }
        RECOVERY_HOP_IDENTIFIER_EXHAUSTED if entries == 6 => {
            Ok(TransportRecoveryReason::HopIdentifierExhausted)
        }
        RECOVERY_INVARIANT if entries == 6 => Ok(TransportRecoveryReason::Invariant),
        _ => Err(DecodeError::InvalidValue),
    }
}

fn decode_interrupted_state(decoder: &mut Decoder<'_>) -> Result<InterruptedState, DecodeError> {
    match decoder.u8().map_err(|_| DecodeError::Malformed)? {
        0 => Ok(InterruptedState::Preparing),
        1 => Ok(InterruptedState::AwaitingDelivery),
        _ => Err(DecodeError::InvalidValue),
    }
}

fn decode_boot_policy(decoder: &mut Decoder<'_>) -> Result<BootRecoveryPolicy, DecodeError> {
    match decoder.u8().map_err(|_| DecodeError::Malformed)? {
        0 => Ok(BootRecoveryPolicy::FailInternal),
        _ => Err(DecodeError::InvalidValue),
    }
}

fn expect_map(decoder: &mut Decoder<'_>, expected: u64) -> Result<(), DecodeError> {
    if definite_map_len(decoder)? == expected {
        Ok(())
    } else {
        Err(DecodeError::Malformed)
    }
}

fn definite_map_len(decoder: &mut Decoder<'_>) -> Result<u64, DecodeError> {
    decoder
        .map()
        .map_err(|_| DecodeError::Malformed)?
        .ok_or(DecodeError::Malformed)
}

fn expect_key(decoder: &mut Decoder<'_>, expected: u8) -> Result<(), DecodeError> {
    if decoder.u8().map_err(|_| DecodeError::Malformed)? == expected {
        Ok(())
    } else {
        Err(DecodeError::Malformed)
    }
}

fn decode_fixed_bytes<const N: usize>(decoder: &mut Decoder<'_>) -> Result<[u8; N], DecodeError> {
    let bytes = decoder.bytes().map_err(|_| DecodeError::Malformed)?;
    bytes
        .try_into()
        .map_err(|_| DecodeError::InvalidByteStringLength)
}
