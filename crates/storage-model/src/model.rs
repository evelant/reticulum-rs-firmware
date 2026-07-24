//! Project-owned durable submission and journal vocabulary.

use sha2::{Digest, Sha256};

use crate::{
    AUTHORIZATION_KNOWN_PERMISSION_BITS, AUTHORIZATION_PERMISSION_EXPERIMENTAL_SUBMIT_RNS_DATA,
    MAX_ENCODED_PACKET_BYTES, MAX_EXPERIMENTAL_RNS_DATA_BYTES, MAX_INLINE_LXMF_MESSAGE_WIRE_BYTES,
    MIN_LXMF_MESSAGE_WIRE_BYTES,
};

const EXPERIMENTAL_RNS_DATA_DIGEST_DOMAIN: &[u8] =
    b"reticulum-rs-firmware/storage-model/experimental-rns-data/v1\0";
// Retain the schema-3 digest domain even though the source vocabulary now
// makes delivery-method selection explicitly separate from message ownership.
const LXMF_MESSAGE_DIGEST_DOMAIN: &[u8] = b"reticulum-rs-firmware/storage-model/direct-lxmf/v1\0";

/// Authenticated local-client principal owning one submission.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PrincipalId([u8; 16]);

impl PrincipalId {
    /// Construct a principal identifier from all 128 bits.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Borrow all identifier bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Principal-scoped client key used to deduplicate an accepted operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct IdempotencyKey([u8; 16]);

impl IdempotencyKey {
    /// Construct an idempotency key from all 128 bits.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Borrow all key bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Complete Reticulum destination hash for the experimental DATA intent.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DestinationHash([u8; 16]);

impl DestinationHash {
    /// Construct a destination hash from all 128 bits.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Borrow all destination bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// SHA-256 of canonical semantic intent content.
///
/// This is neither a digest of API CBOR nor Reticulum's delivery-proof token.
/// Semantic schema 3 does not store this redundant value in an acceptance;
/// callers derive it from the immutable intent when needed.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ContentSha256([u8; 32]);

impl ContentSha256 {
    /// Construct a digest from all SHA-256 bytes.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow all digest bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    fn for_experimental_rns_data(intent: &ExperimentalRnsDataIntent) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(EXPERIMENTAL_RNS_DATA_DIGEST_DOMAIN);
        hasher.update(intent.destination.as_bytes());
        hasher.update(intent.payload_len.to_be_bytes());
        hasher.update(intent.payload());
        Self(hasher.finalize().into())
    }

    fn for_lxmf_message(intent: &LxmfMessageIntent) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(LXMF_MESSAGE_DIGEST_DOMAIN);
        hasher.update(intent.wire_len.to_be_bytes());
        hasher.update(intent.wire());
        Self(hasher.finalize().into())
    }
}

/// Failure to construct a fixed-capacity experimental RNS DATA intent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IntentTooLarge {
    actual: usize,
    maximum: usize,
}

impl IntentTooLarge {
    /// Supplied payload length.
    pub const fn actual(self) -> usize {
        self.actual
    }

    /// Maximum accepted payload length.
    pub const fn maximum(self) -> usize {
        self.maximum
    }
}

/// Fixed-capacity owned input for the host-only experimental RNS DATA intent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExperimentalRnsDataIntent {
    destination: DestinationHash,
    payload_len: u16,
    payload: [u8; MAX_EXPERIMENTAL_RNS_DATA_BYTES],
}

impl ExperimentalRnsDataIntent {
    /// Copy one borrowed payload into bounded durable intent storage.
    pub fn new(destination: DestinationHash, payload: &[u8]) -> Result<Self, IntentTooLarge> {
        if payload.len() > MAX_EXPERIMENTAL_RNS_DATA_BYTES {
            return Err(IntentTooLarge {
                actual: payload.len(),
                maximum: MAX_EXPERIMENTAL_RNS_DATA_BYTES,
            });
        }
        let mut owned = [0_u8; MAX_EXPERIMENTAL_RNS_DATA_BYTES];
        owned[..payload.len()].copy_from_slice(payload);
        Ok(Self {
            destination,
            payload_len: payload.len() as u16,
            payload: owned,
        })
    }

    /// Complete destination hash.
    pub const fn destination(&self) -> DestinationHash {
        self.destination
    }

    /// Borrow the initialized payload prefix.
    pub fn payload(&self) -> &[u8] {
        &self.payload[..usize::from(self.payload_len)]
    }

    /// Canonical semantic content digest.
    pub fn content_sha256(&self) -> ContentSha256 {
        ContentSha256::for_experimental_rns_data(self)
    }
}

/// Invalid complete inline LXMF wire length.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidLxmfMessageWireLength {
    actual: usize,
}

impl InvalidLxmfMessageWireLength {
    /// Supplied complete wire length.
    pub const fn actual(self) -> usize {
        self.actual
    }

    /// Smallest accepted wire retaining the complete destination prefix.
    pub const fn minimum(self) -> usize {
        MIN_LXMF_MESSAGE_WIRE_BYTES
    }

    /// Largest complete wire retained by the current inline intent.
    pub const fn maximum(self) -> usize {
        MAX_INLINE_LXMF_MESSAGE_WIRE_BYTES
    }
}

/// Exact complete signed LXMF wire bytes accepted before delivery planning.
///
/// This storage-owned value preserves the producer's bytes verbatim. It does
/// not parse, normalize, recompose, or re-sign LXMF. The producer remains
/// responsible for canonical LXMF composition. This constructor enforces the
/// retained destination prefix and current inline ceiling without choosing
/// opportunistic, direct, propagated, or any future delivery method.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LxmfMessageIntent {
    wire_len: u16,
    wire: [u8; MAX_INLINE_LXMF_MESSAGE_WIRE_BYTES],
}

impl LxmfMessageIntent {
    /// Copy one complete signed wire message into bounded durable storage.
    pub fn new(wire: &[u8]) -> Result<Self, InvalidLxmfMessageWireLength> {
        if !(MIN_LXMF_MESSAGE_WIRE_BYTES..=MAX_INLINE_LXMF_MESSAGE_WIRE_BYTES).contains(&wire.len())
        {
            return Err(InvalidLxmfMessageWireLength { actual: wire.len() });
        }
        let mut owned = [0_u8; MAX_INLINE_LXMF_MESSAGE_WIRE_BYTES];
        owned[..wire.len()].copy_from_slice(wire);
        Ok(Self {
            wire_len: wire.len() as u16,
            wire: owned,
        })
    }

    /// Complete destination hash retained as the first 16 wire bytes.
    pub fn destination(&self) -> DestinationHash {
        let mut destination = [0_u8; 16];
        destination.copy_from_slice(&self.wire[..16]);
        DestinationHash::new(destination)
    }

    /// Borrow the exact initialized complete signed wire bytes.
    pub fn wire(&self) -> &[u8] {
        &self.wire[..usize::from(self.wire_len)]
    }

    /// Borrow source hash, signature, and payload for opportunistic delivery.
    pub fn opportunistic_carrier(&self) -> &[u8] {
        &self.wire[16..usize::from(self.wire_len)]
    }

    /// Canonical semantic content digest over the exact wire bytes.
    pub fn content_sha256(&self) -> ContentSha256 {
        ContentSha256::for_lxmf_message(self)
    }
}

/// Immutable durable submission content selected before transport planning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmissionIntent {
    /// Generic destination-encrypted RNS DATA constrained to its protocol MDU.
    ExperimentalRnsData(ExperimentalRnsDataIntent),
    /// Exact complete signed LXMF wire retained before delivery planning.
    LxmfMessage(LxmfMessageIntent),
}

impl SubmissionIntent {
    /// Complete destination hash common to all currently modeled intents.
    pub fn destination(&self) -> DestinationHash {
        match self {
            Self::ExperimentalRnsData(intent) => intent.destination(),
            Self::LxmfMessage(intent) => intent.destination(),
        }
    }

    /// Canonical semantic content digest with an intent-specific domain.
    pub fn content_sha256(&self) -> ContentSha256 {
        match self {
            Self::ExperimentalRnsData(intent) => intent.content_sha256(),
            Self::LxmfMessage(intent) => intent.content_sha256(),
        }
    }

    /// Borrow generic destination-DATA content when this is that intent kind.
    pub const fn experimental_rns_data(&self) -> Option<&ExperimentalRnsDataIntent> {
        match self {
            Self::ExperimentalRnsData(intent) => Some(intent),
            Self::LxmfMessage(_) => None,
        }
    }

    /// Borrow exact LXMF content when this is that intent kind.
    pub const fn lxmf_message(&self) -> Option<&LxmfMessageIntent> {
        match self {
            Self::ExperimentalRnsData(_) => None,
            Self::LxmfMessage(intent) => Some(intent),
        }
    }
}

impl From<ExperimentalRnsDataIntent> for SubmissionIntent {
    fn from(intent: ExperimentalRnsDataIntent) -> Self {
        Self::ExperimentalRnsData(intent)
    }
}

impl From<LxmfMessageIntent> for SubmissionIntent {
    fn from(intent: LxmfMessageIntent) -> Self {
        Self::LxmfMessage(intent)
    }
}

/// Device-assigned durable identifier for one accepted submission.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SubmissionId(u64);

impl SubmissionId {
    /// Construct an identifier from its complete numeric representation.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Complete numeric representation.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Durable authentication and authorization facts used to admit a submission.
///
/// This storage-owned value intentionally contains no secret and no dependency
/// on the live device-API or credential-authority crates. It records the exact
/// authority facts applied at acceptance so later replay and audit do not
/// reinterpret historical work under a rotated credential.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthorizationSnapshot {
    credential_id: [u8; 16],
    credential_generation: u64,
    authority_revision: u64,
    policy_version: u32,
    granted_permission_bits: u32,
}

impl AuthorizationSnapshot {
    /// Validate and construct one durable authorization snapshot.
    pub fn new(
        credential_id: [u8; 16],
        credential_generation: u64,
        authority_revision: u64,
        policy_version: u32,
        granted_permission_bits: u32,
    ) -> Result<Self, AuthorizationSnapshotError> {
        if credential_id == [0; 16] {
            return Err(AuthorizationSnapshotError::ZeroCredentialId);
        }
        if credential_generation == 0 {
            return Err(AuthorizationSnapshotError::ZeroCredentialGeneration);
        }
        if authority_revision == 0 {
            return Err(AuthorizationSnapshotError::ZeroAuthorityRevision);
        }
        if policy_version == 0 {
            return Err(AuthorizationSnapshotError::ZeroPolicyVersion);
        }
        if credential_generation > authority_revision {
            return Err(
                AuthorizationSnapshotError::GenerationAfterAuthorityRevision {
                    generation: credential_generation,
                    authority_revision,
                },
            );
        }
        let unknown = granted_permission_bits & !AUTHORIZATION_KNOWN_PERMISSION_BITS;
        if unknown != 0 {
            return Err(AuthorizationSnapshotError::UnknownPermissionBits { unknown });
        }
        if granted_permission_bits & AUTHORIZATION_PERMISSION_EXPERIMENTAL_SUBMIT_RNS_DATA == 0 {
            return Err(AuthorizationSnapshotError::MissingSubmitPermission);
        }
        Ok(Self {
            credential_id,
            credential_generation,
            authority_revision,
            policy_version,
            granted_permission_bits,
        })
    }

    /// Opaque credential identifier authenticated for this submission.
    pub const fn credential_id(&self) -> &[u8; 16] {
        &self.credential_id
    }

    /// Credential generation authenticated by the session transcript.
    pub const fn credential_generation(self) -> u64 {
        self.credential_generation
    }

    /// Complete credential-authority revision used for revalidation.
    pub const fn authority_revision(self) -> u64 {
        self.authority_revision
    }

    /// Authorization-policy version applied at dispatch.
    pub const fn policy_version(self) -> u32 {
        self.policy_version
    }

    /// Complete stable permission bitset granted at dispatch.
    pub const fn granted_permission_bits(self) -> u32 {
        self.granted_permission_bits
    }
}

/// Invalid durable authorization facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorizationSnapshotError {
    /// Credential identifier is the reserved all-zero value.
    ZeroCredentialId,
    /// Credential generation is zero rather than globally allocated.
    ZeroCredentialGeneration,
    /// Complete authority revision is zero rather than committed.
    ZeroAuthorityRevision,
    /// Authorization-policy version is zero rather than assigned.
    ZeroPolicyVersion,
    /// Credential generation is newer than the complete authority snapshot.
    GenerationAfterAuthorityRevision {
        /// Invalid credential generation.
        generation: u64,
        /// Older complete authority revision.
        authority_revision: u64,
    },
    /// Permission bitset contains vocabulary unknown to semantic schema 3.
    UnknownPermissionBits {
        /// Bits outside [`crate::AUTHORIZATION_KNOWN_PERMISSION_BITS`].
        unknown: u32,
    },
    /// Admission provenance omits the permission required by this record kind.
    MissingSubmitPermission,
}

/// Immutable durable acceptance record at revision zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Accepted {
    id: SubmissionId,
    principal: PrincipalId,
    idempotency_key: IdempotencyKey,
    authorization: AuthorizationSnapshot,
    intent: SubmissionIntent,
}

impl Accepted {
    /// Construct a self-consistent immutable acceptance record.
    pub fn new<I>(
        id: SubmissionId,
        principal: PrincipalId,
        idempotency_key: IdempotencyKey,
        intent: I,
        authorization: AuthorizationSnapshot,
    ) -> Self
    where
        I: Into<SubmissionIntent>,
    {
        Self {
            id,
            principal,
            idempotency_key,
            authorization,
            intent: intent.into(),
        }
    }

    pub(crate) fn from_parts(
        id: SubmissionId,
        principal: PrincipalId,
        idempotency_key: IdempotencyKey,
        intent: SubmissionIntent,
        authorization: AuthorizationSnapshot,
    ) -> Self {
        Self::new(id, principal, idempotency_key, intent, authorization)
    }

    /// Assigned submission identifier.
    pub const fn id(self) -> SubmissionId {
        self.id
    }

    /// Authenticated principal owning this record.
    pub const fn principal(self) -> PrincipalId {
        self.principal
    }

    /// Principal-scoped deduplication key.
    pub const fn idempotency_key(self) -> IdempotencyKey {
        self.idempotency_key
    }

    /// Canonical semantic request digest.
    pub fn content_sha256(&self) -> ContentSha256 {
        self.intent.content_sha256()
    }

    /// Exact authorization facts durably bound to this acceptance.
    pub const fn authorization(self) -> AuthorizationSnapshot {
        self.authorization
    }

    /// Complete fixed-capacity intent.
    pub const fn intent(self) -> SubmissionIntent {
        self.intent
    }
}

/// SHA-256 of every byte in one complete encoded Reticulum packet.
///
/// This type is deliberately distinct from [`RnsAttemptToken`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EncodedPacketSha256([u8; 32]);

impl EncodedPacketSha256 {
    /// Construct a digest from all SHA-256 bytes.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow all digest bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Reticulum proof-correlation token covering protocol-defined hashable bytes.
///
/// This type is deliberately distinct from [`EncodedPacketSha256`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RnsAttemptToken([u8; 32]);

impl RnsAttemptToken {
    /// Construct a token from all proof-correlation bytes.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow all token bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Durable scalar metadata for one complete encoded Reticulum packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedPacketDetails {
    packet_len: u16,
    encoded_packet_sha256: EncodedPacketSha256,
    rns_attempt_token: RnsAttemptToken,
}

/// Invalid complete encoded Reticulum packet length.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidPacketLength {
    actual: u16,
}

impl InvalidPacketLength {
    /// Rejected encoded packet length.
    pub const fn actual(self) -> u16 {
        self.actual
    }

    /// Largest encoded packet accepted by the current packet-buffer contract.
    pub const fn maximum(self) -> u16 {
        MAX_ENCODED_PACKET_BYTES as u16
    }
}

impl PreparedPacketDetails {
    /// Construct validated metadata without retaining packet bytes.
    pub const fn new(
        packet_len: u16,
        encoded_packet_sha256: EncodedPacketSha256,
        rns_attempt_token: RnsAttemptToken,
    ) -> Result<Self, InvalidPacketLength> {
        if packet_len == 0 || packet_len > MAX_ENCODED_PACKET_BYTES as u16 {
            return Err(InvalidPacketLength { actual: packet_len });
        }
        Ok(Self {
            packet_len,
            encoded_packet_sha256,
            rns_attempt_token,
        })
    }

    /// Complete encoded packet length.
    pub const fn packet_len(self) -> u16 {
        self.packet_len
    }

    /// SHA-256 of every complete encoded packet byte.
    pub const fn encoded_packet_sha256(self) -> EncodedPacketSha256 {
        self.encoded_packet_sha256
    }

    /// Reticulum delivery-proof correlation token.
    pub const fn rns_attempt_token(self) -> RnsAttemptToken {
        self.rns_attempt_token
    }
}

/// Public failure category retained by a final submission disposition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmissionFailure {
    /// No currently usable path reaches the destination.
    NoPath,
    /// No required delivery proof or acknowledgement arrived before timeout.
    DeliveryTimeout,
    /// A downstream protocol or policy stage rejected accepted work.
    Rejected,
    /// A non-client fault terminated the submission.
    Internal(InternalFailure),
}

/// Internal durable failure detail not exposed as a new public API category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InternalFailure {
    /// No more specific safe durable category is available.
    Unspecified,
    /// Reboot interrupted work after the replay-safe queued state.
    InterruptedByReset(BootRecoveryMarker),
}

/// Final immutable submission disposition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinalDisposition {
    /// A required proof or application acknowledgement completed delivery.
    Delivered(PreparedPacketDetails),
    /// Submission terminated with a typed failure.
    Failed(SubmissionFailure),
    /// Submission was cancelled while still replay-safe and queued.
    Cancelled,
}

/// Durable lifecycle state reconstructed from accepted and transition entries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleState {
    /// Accepted and safe to replay after reboot.
    Queued,
    /// A conservative no-replay barrier was committed before node preparation.
    Preparing,
    /// Packet metadata is durable while proof or acknowledgement remains pending.
    AwaitingDelivery(PreparedPacketDetails),
    /// Immutable terminal disposition.
    Final(FinalDisposition),
}

impl LifecycleState {
    /// Whether no later lifecycle transition is permitted.
    pub const fn is_final(self) -> bool {
        matches!(self, Self::Final(_))
    }
}

/// Why a requested durable lifecycle transition is illegal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionError {
    /// A transition cannot target the implicit initial queued state.
    QueuedIsInitialOnly,
    /// The requested source/target pair violates lifecycle ordering.
    IllegalLifecycle,
    /// Delivered packet metadata differs from awaiting-delivery metadata.
    PreparedMetadataMismatch,
}

/// Validate a transition without consulting journal revision state.
pub fn validate_transition(
    from: LifecycleState,
    to: LifecycleState,
) -> Result<(), TransitionError> {
    if matches!(to, LifecycleState::Queued) {
        return Err(TransitionError::QueuedIsInitialOnly);
    }
    match (from, to) {
        (LifecycleState::Queued, LifecycleState::Preparing) => Ok(()),
        (
            LifecycleState::Queued,
            LifecycleState::Final(
                FinalDisposition::Failed(SubmissionFailure::NoPath | SubmissionFailure::Rejected)
                | FinalDisposition::Cancelled,
            ),
        ) => Ok(()),
        (LifecycleState::Preparing, LifecycleState::AwaitingDelivery(_)) => Ok(()),
        (LifecycleState::Preparing, LifecycleState::Final(FinalDisposition::Delivered(_))) => {
            Ok(())
        }
        (
            LifecycleState::Preparing,
            LifecycleState::Final(FinalDisposition::Failed(
                SubmissionFailure::NoPath
                | SubmissionFailure::DeliveryTimeout
                | SubmissionFailure::Rejected
                | SubmissionFailure::Internal(InternalFailure::Unspecified),
            )),
        ) => Ok(()),
        (
            LifecycleState::Preparing,
            LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::Internal(
                InternalFailure::InterruptedByReset(marker),
            ))),
        ) if marker.interrupted_state() == InterruptedState::Preparing => Ok(()),
        (
            LifecycleState::AwaitingDelivery(awaiting),
            LifecycleState::Final(FinalDisposition::Delivered(delivered)),
        ) if awaiting.packet_len == delivered.packet_len
            && awaiting.encoded_packet_sha256 == delivered.encoded_packet_sha256
            && awaiting.rns_attempt_token == delivered.rns_attempt_token =>
        {
            Ok(())
        }
        (
            LifecycleState::AwaitingDelivery(_),
            LifecycleState::Final(FinalDisposition::Failed(
                SubmissionFailure::DeliveryTimeout
                | SubmissionFailure::Internal(InternalFailure::Unspecified),
            )),
        ) => Ok(()),
        (
            LifecycleState::AwaitingDelivery(_),
            LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::Internal(
                InternalFailure::InterruptedByReset(marker),
            ))),
        ) if marker.interrupted_state() == InterruptedState::AwaitingDelivery => Ok(()),
        (
            LifecycleState::AwaitingDelivery(_),
            LifecycleState::Final(FinalDisposition::Delivered(_)),
        ) => Err(TransitionError::PreparedMetadataMismatch),
        (
            LifecycleState::Queued
            | LifecycleState::Preparing
            | LifecycleState::AwaitingDelivery(_),
            _,
        ) => Err(TransitionError::IllegalLifecycle),
        (LifecycleState::Final(_), _) => Err(TransitionError::IllegalLifecycle),
    }
}

/// Interrupted nonterminal phase observed during boot reconstruction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterruptedState {
    /// Reset occurred after the no-replay preparation barrier.
    Preparing,
    /// Reset occurred while delivery outcome was unresolved.
    AwaitingDelivery,
}

/// Conservative policy selected for a reboot-interrupted submission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootRecoveryPolicy {
    /// Terminate with an internal failure instead of risking duplicate RF.
    FailInternal,
}

/// Durable reboot marker attached to an interrupted final disposition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BootRecoveryMarker {
    boot_sequence: u64,
    interrupted_state: InterruptedState,
    policy: BootRecoveryPolicy,
}

impl BootRecoveryMarker {
    /// Construct an explicit reboot-recovery marker.
    pub const fn new(boot_sequence: u64, interrupted_state: InterruptedState) -> Self {
        Self {
            boot_sequence,
            interrupted_state,
            policy: BootRecoveryPolicy::FailInternal,
        }
    }

    /// Caller-supplied durable boot sequence or boot marker.
    pub const fn boot_sequence(self) -> u64 {
        self.boot_sequence
    }

    /// Interrupted nonterminal phase.
    pub const fn interrupted_state(self) -> InterruptedState {
        self.interrupted_state
    }

    /// Conservative recovery policy.
    pub const fn policy(self) -> BootRecoveryPolicy {
        self.policy
    }
}

/// Immutable revisioned lifecycle transition journal entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StateTransition {
    id: SubmissionId,
    revision: u64,
    state: LifecycleState,
}

impl StateTransition {
    /// Construct a transition target at a nonzero journal revision.
    pub const fn new(
        id: SubmissionId,
        revision: u64,
        state: LifecycleState,
    ) -> Result<Self, TransitionError> {
        if revision == 0 || matches!(state, LifecycleState::Queued) {
            return Err(TransitionError::QueuedIsInitialOnly);
        }
        Ok(Self {
            id,
            revision,
            state,
        })
    }

    /// Submission being updated.
    pub const fn id(self) -> SubmissionId {
        self.id
    }

    /// Monotonically increasing per-submission revision.
    pub const fn revision(self) -> u64 {
        self.revision
    }

    /// Complete target lifecycle state.
    pub const fn state(self) -> LifecycleState {
        self.state
    }
}

/// Durable semantic reason for a recovered or quarantined transport owner.
///
/// The completion-fault namespace is intentionally kept inside its own enum
/// variant. Every `u16` remains available to a driver or control plane without
/// colliding with the project-owned categorical reasons.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportRecoveryReason {
    /// Packet-owner deadline expired.
    DeadlineExpired,
    /// Interface or control plane returned a recovery fault.
    CompletionFault(u16),
    /// Exact receipt cancellation unexpectedly failed.
    ReceiptCancellationFailed,
    /// Per-hop generation space was exhausted during fan-out.
    HopIdentifierExhausted,
    /// Same-owner scalar and unique-owner metadata disagreed.
    Invariant,
}

/// Durable, non-lifecycle audit observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuditEvent {
    /// A packet owner returned through conservative recovery.
    TransportRecovered {
        /// Durable Reticulum proof-correlation token for the affected attempt.
        rns_attempt_token: RnsAttemptToken,
        /// Whether any hop may have transmitted.
        may_have_transmitted: bool,
        /// Exact semantic recovery reason, including any completion-fault code.
        reason: TransportRecoveryReason,
    },
    /// A packet owner remains fail-closed in quarantine.
    TransportQuarantined {
        /// Durable Reticulum proof-correlation token for the affected attempt.
        rns_attempt_token: RnsAttemptToken,
        /// Whether any hop may have transmitted before quarantine.
        may_have_transmitted: bool,
        /// Exact semantic recovery reason, including any completion-fault code.
        reason: TransportRecoveryReason,
    },
}

/// Immutable revisioned audit journal entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuditEntry {
    id: SubmissionId,
    revision: u64,
    event: AuditEvent,
}

impl AuditEntry {
    /// Construct an audit entry at a nonzero journal revision.
    pub const fn new(id: SubmissionId, revision: u64, event: AuditEvent) -> Option<Self> {
        if revision == 0 {
            None
        } else {
            Some(Self {
                id,
                revision,
                event,
            })
        }
    }

    /// Submission associated with this observation.
    pub const fn id(self) -> SubmissionId {
        self.id
    }

    /// Monotonically increasing per-submission revision.
    pub const fn revision(self) -> u64 {
        self.revision
    }

    /// Complete immutable audit event.
    pub const fn event(self) -> AuditEvent {
        self.event
    }
}

/// One complete durable journal record.
// Accepted records deliberately own their bounded payload; indirection would
// violate this no-alloc crate's durable handoff contract.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JournalEntry {
    /// Immutable acceptance at implicit revision zero.
    Accepted(Accepted),
    /// Revisioned lifecycle mutation.
    StateTransition(StateTransition),
    /// Revisioned non-lifecycle observation.
    Audit(AuditEntry),
}
