//! Canonical live-pairing record vocabulary and fixed layouts.

use core::num::NonZeroU64;

use reticulum_device_api_credentials::{CredentialGeneration, CredentialId};
use reticulum_device_api_framing::{
    AUTH_TAG_LENGTH, PAYLOAD_CAPACITY, PayloadLength, Record, SESSION_ID_LENGTH,
};
use zeroize::Zeroizing;

use crate::crypto::{
    ActivationConfirmation, ClientProof, PairingPsk, PairingTranscript, ProofRejected,
    VerifiedClientProof,
};

/// Live-pairing Begin request record kind.
pub const RECORD_KIND_BEGIN_REQUEST: u8 = 0x24;
/// Live-pairing Begin response record kind.
pub const RECORD_KIND_BEGIN_RESPONSE: u8 = 0x25;
/// Live-pairing ProofStart request record kind.
pub const RECORD_KIND_PROOF_START_REQUEST: u8 = 0x26;
/// Live-pairing proof-challenge response record kind.
pub const RECORD_KIND_PROOF_START_RESPONSE: u8 = 0x27;
/// Live-pairing Activate continuation request record kind.
pub const RECORD_KIND_ACTIVATE_REQUEST: u8 = 0x28;
/// Live-pairing Activate response record kind.
pub const RECORD_KIND_ACTIVATE_RESPONSE: u8 = 0x29;
/// Live-pairing AbortCurrent request record kind.
pub const RECORD_KIND_ABORT_CURRENT_REQUEST: u8 = 0x2a;
/// Live-pairing AbortCurrent response record kind.
pub const RECORD_KIND_ABORT_CURRENT_RESPONSE: u8 = 0x2b;

/// Pairing protocol major version.
pub const PROTOCOL_MAJOR: u16 = 1;
/// Pairing protocol minor version.
pub const PROTOCOL_MINOR: u16 = 0;
/// HMAC-SHA256 possession-proof suite identifier with Active-generation binding.
pub const PROOF_SUITE: u16 = 2;

/// Exact Begin request payload length.
pub const BEGIN_REQUEST_LENGTH: usize = 0;
/// Exact successful Begin offer payload length.
pub const BEGIN_OFFER_LENGTH: usize = 88;
/// Exact ProofStart request payload length.
pub const PROOF_START_REQUEST_LENGTH: usize = 64;
/// Exact successful proof-challenge payload length.
pub const PROOF_CHALLENGE_LENGTH: usize = 104;
/// Exact Activate continuation request payload length.
pub const ACTIVATE_REQUEST_LENGTH: usize = 56;
/// Exact successful Activate response payload length.
pub const ACTIVATE_SUCCESS_LENGTH: usize = 64;
/// Exact AbortCurrent request payload length.
pub const ABORT_CURRENT_REQUEST_LENGTH: usize = 0;
/// Exact AbortCurrent response payload length.
pub const ABORT_CURRENT_RESPONSE_LENGTH: usize = 1;

const ZERO_SESSION_ID: [u8; SESSION_ID_LENGTH] = [0; SESSION_ID_LENGTH];
const ZERO_AUTHENTICATION_TAG: [u8; AUTH_TAG_LENGTH] = [0; AUTH_TAG_LENGTH];
const RESULT_PREFIX_LENGTH: usize = 8;
const PROFILE_LENGTH: usize = 8;

/// Bearer cryptographically bound by this pairing profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum BearerBinding {
    /// ESP32-S3 fixed USB Serial/JTAG byte stream.
    UsbSerialJtag = 1,
    /// Authenticated Bluetooth Low Energy GATT appliance connection.
    BleGatt = 2,
}

impl BearerBinding {
    /// Return the stable one-byte bearer code.
    pub const fn code(self) -> u8 {
        self as u8
    }

    /// Decode one stable bearer assigned by pairing protocol version 1.
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::UsbSerialJtag),
            2 => Some(Self::BleGatt),
            _ => None,
        }
    }
}

/// Stable public identifier for one device API endpoint.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DeviceId([u8; 16]);

impl DeviceId {
    /// Validate a nonzero stable device API identifier.
    pub fn new(bytes: [u8; 16]) -> Result<Self, InvalidField> {
        require_nonzero(&bytes, InvalidField::ZeroDeviceId)?;
        Ok(Self(bytes))
    }

    /// Borrow the canonical identifier bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Boot-lifetime nonzero accepted-connection epoch.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ConnectionId(NonZeroU64);

impl ConnectionId {
    /// Construct a nonzero connection epoch.
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Return the raw connection epoch.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Boot-lifetime nonzero pairing-window identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WindowId(NonZeroU64);

impl WindowId {
    /// Construct a nonzero pairing-window identifier.
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Return the raw pairing-window identifier.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Fresh nonzero device challenge retained for one ProofStart continuation.
///
/// This owner deliberately implements neither `Copy`, `Clone`, nor `Debug`.
pub struct DeviceChallenge(Zeroizing<[u8; 32]>);

impl DeviceChallenge {
    /// Validate and own a fresh nonzero challenge.
    pub fn new(bytes: [u8; 32]) -> Result<Self, InvalidField> {
        Self::from_zeroizing(Zeroizing::new(bytes))
    }

    /// Validate and take an existing zeroizing challenge owner without copying.
    pub fn from_zeroizing(bytes: Zeroizing<[u8; 32]>) -> Result<Self, InvalidField> {
        require_nonzero(&bytes[..], InvalidField::ZeroDeviceChallenge)?;
        Ok(Self(bytes))
    }

    /// Borrow the challenge bytes without copying them.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Consume the challenge into its zeroizing fixed buffer.
    pub fn into_bytes(self) -> Zeroizing<[u8; 32]> {
        self.0
    }
}

/// A required field was not valid for the fixed pairing profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidField {
    /// Stable device API identifier was all zero.
    ZeroDeviceId,
    /// Credential identifier was all zero.
    ZeroCredentialId,
    /// Credential generation was zero.
    ZeroCredentialGeneration,
    /// Client nonce was all zero.
    ZeroClientNonce,
    /// Connection epoch was zero.
    ZeroConnectionId,
    /// Pairing-window identifier was zero.
    ZeroWindowId,
    /// Device challenge was all zero.
    ZeroDeviceChallenge,
    /// Pairing PSK was all zero.
    ZeroPairingPsk,
}

/// Coarse non-success result shared by Begin and ProofStart.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum PairingFailure {
    /// A fresh qualifying physical-presence window is required.
    PhysicalPresenceRequired = 1,
    /// Request was refused without exposing the reason.
    Refused = 2,
    /// Pairing is fail-closed and cannot currently proceed.
    Blocked = 3,
    /// Pairing is not available in this product or state.
    Unavailable = 4,
}

impl PairingFailure {
    /// Return the stable one-byte wire code.
    pub const fn code(self) -> u8 {
        self as u8
    }

    /// Decode one stable non-success wire code.
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::PhysicalPresenceRequired),
            2 => Some(Self::Refused),
            3 => Some(Self::Blocked),
            4 => Some(Self::Unavailable),
            _ => None,
        }
    }
}

/// Public result of a Begin response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum BeginResult {
    /// A durable Pending credential is offered in the full success payload.
    Offered = 0,
    /// A fresh qualifying physical-presence window is required.
    PhysicalPresenceRequired = 1,
    /// Request was refused without exposing the reason.
    Refused = 2,
    /// Pairing is fail-closed and cannot currently proceed.
    Blocked = 3,
    /// Pairing is not available in this product or state.
    Unavailable = 4,
}

impl BeginResult {
    /// Return the stable one-byte result code.
    pub const fn code(self) -> u8 {
        self as u8
    }
}

impl From<PairingFailure> for BeginResult {
    fn from(value: PairingFailure) -> Self {
        match value {
            PairingFailure::PhysicalPresenceRequired => Self::PhysicalPresenceRequired,
            PairingFailure::Refused => Self::Refused,
            PairingFailure::Blocked => Self::Blocked,
            PairingFailure::Unavailable => Self::Unavailable,
        }
    }
}

/// Public result of a ProofStart response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ProofStartResult {
    /// The full response contains a fresh bound device challenge.
    Challenge = 0,
    /// A fresh qualifying physical-presence window is required.
    PhysicalPresenceRequired = 1,
    /// Request was refused without exposing the reason.
    Refused = 2,
    /// Pairing is fail-closed and cannot currently proceed.
    Blocked = 3,
    /// Pairing is not available in this product or state.
    Unavailable = 4,
}

impl ProofStartResult {
    /// Return the stable one-byte result code.
    pub const fn code(self) -> u8 {
        self as u8
    }
}

impl From<PairingFailure> for ProofStartResult {
    fn from(value: PairingFailure) -> Self {
        match value {
            PairingFailure::PhysicalPresenceRequired => Self::PhysicalPresenceRequired,
            PairingFailure::Refused => Self::Refused,
            PairingFailure::Blocked => Self::Blocked,
            PairingFailure::Unavailable => Self::Unavailable,
        }
    }
}

/// Coarse non-success Activate result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ActivateFailure {
    /// Credential reference or possession proof was rejected generically.
    ProofRejected = 1,
    /// Continuation was refused without exposing the reason.
    Refused = 2,
    /// Activation is fail-closed and cannot currently proceed.
    Blocked = 3,
    /// Activation is not available in this product or state.
    Unavailable = 4,
}

impl ActivateFailure {
    /// Return the stable one-byte wire code.
    pub const fn code(self) -> u8 {
        self as u8
    }

    /// Decode one stable non-success wire code.
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::ProofRejected),
            2 => Some(Self::Refused),
            3 => Some(Self::Blocked),
            4 => Some(Self::Unavailable),
            _ => None,
        }
    }
}

/// Public result of an Activate response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ActivateResult {
    /// Active state is durable and the full response carries its confirmation.
    Activated = 0,
    /// Credential reference or possession proof was rejected generically.
    ProofRejected = 1,
    /// Continuation was refused without exposing the reason.
    Refused = 2,
    /// Activation is fail-closed and cannot currently proceed.
    Blocked = 3,
    /// Activation is not available in this product or state.
    Unavailable = 4,
}

impl ActivateResult {
    /// Return the stable one-byte result code.
    pub const fn code(self) -> u8 {
        self as u8
    }
}

impl From<ActivateFailure> for ActivateResult {
    fn from(value: ActivateFailure) -> Self {
        match value {
            ActivateFailure::ProofRejected => Self::ProofRejected,
            ActivateFailure::Refused => Self::Refused,
            ActivateFailure::Blocked => Self::Blocked,
            ActivateFailure::Unavailable => Self::Unavailable,
        }
    }
}

/// Complete one-byte AbortCurrent result vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum AbortResult {
    /// The current Pending credential is durably replaced by a tombstone.
    Aborted = 0,
    /// A fresh qualifying physical-presence window is required.
    PhysicalPresenceRequired = 1,
    /// Request was refused without exposing the reason.
    Refused = 2,
    /// Abort is fail-closed and cannot currently proceed.
    Blocked = 3,
    /// Abort is not available in this product or state.
    Unavailable = 4,
}

impl AbortResult {
    /// Return the stable one-byte result code.
    pub const fn code(self) -> u8 {
        self as u8
    }

    /// Decode one stable wire code.
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Aborted),
            1 => Some(Self::PhysicalPresenceRequired),
            2 => Some(Self::Refused),
            3 => Some(Self::Blocked),
            4 => Some(Self::Unavailable),
            _ => None,
        }
    }
}

/// Empty Begin request carrying an opaque framing sequence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BeginRequest {
    sequence: u64,
}

impl BeginRequest {
    /// Construct an empty Begin request.
    pub const fn new(sequence: u64) -> Self {
        Self { sequence }
    }

    /// Opaque framing sequence.
    pub const fn sequence(self) -> u64 {
        self.sequence
    }

    /// Encode as one canonical zero-session, zero-tag record.
    pub fn into_record(self) -> Record {
        encode_record(RECORD_KIND_BEGIN_REQUEST, self.sequence, &[])
    }
}

/// Successful Begin material for one durably committed Pending credential.
///
/// This owner deliberately implements neither `Copy`, `Clone`, nor `Debug`.
///
/// ```compile_fail
/// use reticulum_device_api_pairing::BeginOffer;
/// fn require_copy<T: Copy>() {}
/// require_copy::<BeginOffer>();
/// ```
///
/// ```compile_fail
/// use reticulum_device_api_pairing::BeginOffer;
/// fn require_debug<T: core::fmt::Debug>() {}
/// require_debug::<BeginOffer>();
/// ```
pub struct BeginOffer {
    bearer: BearerBinding,
    device_id: DeviceId,
    credential_id: CredentialId,
    generation: CredentialGeneration,
    psk: PairingPsk,
}

impl BeginOffer {
    /// Construct an offer after its Pending record is durable and publishable.
    pub fn after_pending_commit(
        bearer: BearerBinding,
        device_id: DeviceId,
        credential_id: CredentialId,
        generation: CredentialGeneration,
        psk: PairingPsk,
    ) -> Result<Self, InvalidField> {
        validate_credential(credential_id, generation)?;
        Ok(Self {
            bearer,
            device_id,
            credential_id,
            generation,
            psk,
        })
    }

    /// Pairing bearer cryptographically bound by this offer.
    pub const fn bearer(&self) -> BearerBinding {
        self.bearer
    }

    /// Stable public device API identifier.
    pub const fn device_id(&self) -> DeviceId {
        self.device_id
    }

    /// New opaque credential identifier.
    pub const fn credential_id(&self) -> CredentialId {
        self.credential_id
    }

    /// New nonzero credential generation.
    pub const fn generation(&self) -> CredentialGeneration {
        self.generation
    }

    /// Borrow the new PSK without copying it.
    pub const fn psk(&self) -> &PairingPsk {
        &self.psk
    }

    /// Consume the offer into its exact public facts and zeroizing PSK owner.
    pub fn into_parts(
        self,
    ) -> (
        BearerBinding,
        DeviceId,
        CredentialId,
        CredentialGeneration,
        PairingPsk,
    ) {
        (
            self.bearer,
            self.device_id,
            self.credential_id,
            self.generation,
            self.psk,
        )
    }

    pub(crate) fn encode_payload(&self) -> Zeroizing<[u8; BEGIN_OFFER_LENGTH]> {
        let mut payload = Zeroizing::new([0_u8; BEGIN_OFFER_LENGTH]);
        encode_success_prefix(&mut payload);
        encode_profile(&mut payload[RESULT_PREFIX_LENGTH..16], self.bearer);
        payload[16..32].copy_from_slice(self.device_id.as_bytes());
        payload[32..48].copy_from_slice(self.credential_id.as_bytes());
        payload[48..56].copy_from_slice(&self.generation.get().to_le_bytes());
        payload[56..88].copy_from_slice(self.psk.as_bytes());
        payload
    }
}

/// Begin response with either a full secret offer or a coarse one-byte failure.
pub enum BeginResponse {
    /// Durable Pending credential offer.
    Offered {
        /// Echoed opaque request sequence.
        sequence: u64,
        /// Non-copyable secret-bearing offer.
        offer: BeginOffer,
    },
    /// Intentionally coarse failure.
    Failure {
        /// Echoed opaque request sequence.
        sequence: u64,
        /// Stable public failure category.
        failure: PairingFailure,
    },
}

impl BeginResponse {
    /// Construct a successful response after obtaining a durable offer owner.
    pub const fn offered(sequence: u64, offer: BeginOffer) -> Self {
        Self::Offered { sequence, offer }
    }

    /// Construct a coarse one-byte failure response.
    pub const fn failure(sequence: u64, failure: PairingFailure) -> Self {
        Self::Failure { sequence, failure }
    }

    /// Echoed opaque request sequence.
    pub const fn sequence(&self) -> u64 {
        match self {
            Self::Offered { sequence, .. } | Self::Failure { sequence, .. } => *sequence,
        }
    }

    /// Public result category.
    pub const fn result(&self) -> BeginResult {
        match self {
            Self::Offered { .. } => BeginResult::Offered,
            Self::Failure { failure, .. } => match failure {
                PairingFailure::PhysicalPresenceRequired => BeginResult::PhysicalPresenceRequired,
                PairingFailure::Refused => BeginResult::Refused,
                PairingFailure::Blocked => BeginResult::Blocked,
                PairingFailure::Unavailable => BeginResult::Unavailable,
            },
        }
    }

    /// Borrow the full offer when this response succeeded.
    pub const fn offer(&self) -> Option<&BeginOffer> {
        match self {
            Self::Offered { offer, .. } => Some(offer),
            Self::Failure { .. } => None,
        }
    }

    /// Return the coarse failure when this response did not succeed.
    pub const fn failure_kind(&self) -> Option<PairingFailure> {
        match self {
            Self::Offered { .. } => None,
            Self::Failure { failure, .. } => Some(*failure),
        }
    }

    /// Encode as one canonical zero-session, zero-tag record.
    pub fn into_record(self) -> Record {
        match self {
            Self::Offered { sequence, offer } => {
                let payload = offer.encode_payload();
                encode_record(RECORD_KIND_BEGIN_RESPONSE, sequence, &payload[..])
            }
            Self::Failure { sequence, failure } => {
                encode_record(RECORD_KIND_BEGIN_RESPONSE, sequence, &[failure.code()])
            }
        }
    }
}

/// Canonical bearer-bound ProofStart request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProofStartRequest {
    sequence: u64,
    bearer: BearerBinding,
    credential_id: CredentialId,
    generation: CredentialGeneration,
    client_nonce: [u8; 32],
}

impl ProofStartRequest {
    /// Validate and construct a ProofStart request.
    pub fn new(
        bearer: BearerBinding,
        sequence: u64,
        credential_id: CredentialId,
        generation: CredentialGeneration,
        client_nonce: [u8; 32],
    ) -> Result<Self, InvalidField> {
        validate_credential(credential_id, generation)?;
        require_nonzero(&client_nonce, InvalidField::ZeroClientNonce)?;
        Ok(Self {
            sequence,
            bearer,
            credential_id,
            generation,
            client_nonce,
        })
    }

    /// Pairing bearer cryptographically bound by this request.
    pub const fn bearer(&self) -> BearerBinding {
        self.bearer
    }

    /// Opaque framing sequence.
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Exact Pending credential identifier asserted by the client.
    pub const fn credential_id(&self) -> CredentialId {
        self.credential_id
    }

    /// Exact Pending credential generation asserted by the client.
    pub const fn generation(&self) -> CredentialGeneration {
        self.generation
    }

    /// Fresh nonzero client nonce.
    pub const fn client_nonce(&self) -> &[u8; 32] {
        &self.client_nonce
    }

    /// Encode as one canonical zero-session, zero-tag record.
    pub fn into_record(self) -> Record {
        encode_record(
            RECORD_KIND_PROOF_START_REQUEST,
            self.sequence,
            &self.encode_payload(),
        )
    }

    pub(crate) fn encode_payload(&self) -> [u8; PROOF_START_REQUEST_LENGTH] {
        let mut payload = [0_u8; PROOF_START_REQUEST_LENGTH];
        encode_profile(&mut payload[..PROFILE_LENGTH], self.bearer);
        payload[8..24].copy_from_slice(self.credential_id.as_bytes());
        payload[24..32].copy_from_slice(&self.generation.get().to_le_bytes());
        payload[32..64].copy_from_slice(&self.client_nonce);
        payload
    }
}

/// Successful ProofStart response bound to one connection and pairing window.
///
/// This owner deliberately implements neither `Copy`, `Clone`, nor `Debug`.
pub struct ProofChallenge {
    bearer: BearerBinding,
    device_id: DeviceId,
    connection_id: ConnectionId,
    window_id: WindowId,
    credential_id: CredentialId,
    generation: CredentialGeneration,
    challenge: DeviceChallenge,
}

impl ProofChallenge {
    /// Construct a canonical challenge for the exact selected Pending record.
    pub fn new(
        bearer: BearerBinding,
        device_id: DeviceId,
        connection_id: ConnectionId,
        window_id: WindowId,
        credential_id: CredentialId,
        generation: CredentialGeneration,
        challenge: DeviceChallenge,
    ) -> Result<Self, InvalidField> {
        validate_credential(credential_id, generation)?;
        Ok(Self {
            bearer,
            device_id,
            connection_id,
            window_id,
            credential_id,
            generation,
            challenge,
        })
    }

    /// Pairing bearer cryptographically bound by this challenge.
    pub const fn bearer(&self) -> BearerBinding {
        self.bearer
    }

    /// Stable public device API identifier.
    pub const fn device_id(&self) -> DeviceId {
        self.device_id
    }

    /// Bound accepted-connection epoch.
    pub const fn connection_id(&self) -> ConnectionId {
        self.connection_id
    }

    /// Bound physical-presence pairing-window identifier.
    pub const fn window_id(&self) -> WindowId {
        self.window_id
    }

    /// Exact Pending credential identifier.
    pub const fn credential_id(&self) -> CredentialId {
        self.credential_id
    }

    /// Exact Pending credential generation.
    pub const fn generation(&self) -> CredentialGeneration {
        self.generation
    }

    /// Borrow the fresh single-use challenge without copying it.
    pub const fn challenge(&self) -> &DeviceChallenge {
        &self.challenge
    }

    /// Consume into exact public facts and the zeroizing challenge owner.
    pub fn into_parts(
        self,
    ) -> (
        BearerBinding,
        DeviceId,
        ConnectionId,
        WindowId,
        CredentialId,
        CredentialGeneration,
        DeviceChallenge,
    ) {
        (
            self.bearer,
            self.device_id,
            self.connection_id,
            self.window_id,
            self.credential_id,
            self.generation,
            self.challenge,
        )
    }

    pub(crate) fn encode_payload(&self) -> Zeroizing<[u8; PROOF_CHALLENGE_LENGTH]> {
        let mut payload = Zeroizing::new([0_u8; PROOF_CHALLENGE_LENGTH]);
        encode_success_prefix(&mut payload);
        encode_profile(&mut payload[RESULT_PREFIX_LENGTH..16], self.bearer);
        payload[16..32].copy_from_slice(self.device_id.as_bytes());
        payload[32..40].copy_from_slice(&self.connection_id.get().to_le_bytes());
        payload[40..48].copy_from_slice(&self.window_id.get().to_le_bytes());
        payload[48..64].copy_from_slice(self.credential_id.as_bytes());
        payload[64..72].copy_from_slice(&self.generation.get().to_le_bytes());
        payload[72..104].copy_from_slice(self.challenge.as_bytes());
        payload
    }
}

/// ProofStart response with either a full challenge or a coarse one-byte failure.
pub enum ProofStartResponse {
    /// Fresh challenge for one exact outstanding proof operation.
    Challenge {
        /// Echoed opaque request sequence.
        sequence: u64,
        /// Bound non-copyable challenge owner.
        challenge: ProofChallenge,
    },
    /// Intentionally coarse failure.
    Failure {
        /// Echoed opaque request sequence.
        sequence: u64,
        /// Stable public failure category.
        failure: PairingFailure,
    },
}

impl ProofStartResponse {
    /// Construct a successful challenge response.
    pub const fn challenge(sequence: u64, challenge: ProofChallenge) -> Self {
        Self::Challenge {
            sequence,
            challenge,
        }
    }

    /// Construct a coarse one-byte failure response.
    pub const fn failure(sequence: u64, failure: PairingFailure) -> Self {
        Self::Failure { sequence, failure }
    }

    /// Echoed opaque request sequence.
    pub const fn sequence(&self) -> u64 {
        match self {
            Self::Challenge { sequence, .. } | Self::Failure { sequence, .. } => *sequence,
        }
    }

    /// Public result category.
    pub const fn result(&self) -> ProofStartResult {
        match self {
            Self::Challenge { .. } => ProofStartResult::Challenge,
            Self::Failure { failure, .. } => match failure {
                PairingFailure::PhysicalPresenceRequired => {
                    ProofStartResult::PhysicalPresenceRequired
                }
                PairingFailure::Refused => ProofStartResult::Refused,
                PairingFailure::Blocked => ProofStartResult::Blocked,
                PairingFailure::Unavailable => ProofStartResult::Unavailable,
            },
        }
    }

    /// Borrow the bound challenge when this response succeeded.
    pub const fn proof_challenge(&self) -> Option<&ProofChallenge> {
        match self {
            Self::Challenge { challenge, .. } => Some(challenge),
            Self::Failure { .. } => None,
        }
    }

    /// Return the coarse failure when this response did not succeed.
    pub const fn failure_kind(&self) -> Option<PairingFailure> {
        match self {
            Self::Challenge { .. } => None,
            Self::Failure { failure, .. } => Some(*failure),
        }
    }

    /// Encode as one canonical zero-session, zero-tag record.
    pub fn into_record(self) -> Record {
        match self {
            Self::Challenge {
                sequence,
                challenge,
            } => {
                let payload = challenge.encode_payload();
                encode_record(RECORD_KIND_PROOF_START_RESPONSE, sequence, &payload[..])
            }
            Self::Failure { sequence, failure } => encode_record(
                RECORD_KIND_PROOF_START_RESPONSE,
                sequence,
                &[failure.code()],
            ),
        }
    }
}

/// Activate continuation naming the outstanding Pending credential and proof.
///
/// This owner deliberately implements neither `Copy`, `Clone`, nor `Debug`.
pub struct ActivateRequest {
    sequence: u64,
    credential_id: CredentialId,
    generation: CredentialGeneration,
    proof: ClientProof,
}

impl ActivateRequest {
    /// Validate and construct an Activate continuation.
    pub fn new(
        sequence: u64,
        credential_id: CredentialId,
        generation: CredentialGeneration,
        proof: ClientProof,
    ) -> Result<Self, InvalidField> {
        validate_credential(credential_id, generation)?;
        Ok(Self {
            sequence,
            credential_id,
            generation,
            proof,
        })
    }

    /// Opaque framing sequence.
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Exact Pending credential identifier.
    pub const fn credential_id(&self) -> CredentialId {
        self.credential_id
    }

    /// Exact Pending credential generation.
    pub const fn generation(&self) -> CredentialGeneration {
        self.generation
    }

    /// Borrow the full client proof without copying it.
    pub const fn proof(&self) -> &ClientProof {
        &self.proof
    }

    /// Consume into the exact reference and zeroizing proof owner.
    pub fn into_parts(self) -> (u64, CredentialId, CredentialGeneration, ClientProof) {
        (
            self.sequence,
            self.credential_id,
            self.generation,
            self.proof,
        )
    }

    /// Verify the exact outstanding continuation and consume its proof.
    ///
    /// A stale or substituted credential reference is deliberately collapsed
    /// into the same rejection as a bad HMAC.
    pub fn verify_continuation(
        self,
        psk: &PairingPsk,
        transcript: &PairingTranscript,
    ) -> Result<VerifiedClientProof, ProofRejected> {
        if self.credential_id != transcript.credential_id()
            || self.generation != transcript.generation()
        {
            return Err(ProofRejected);
        }
        self.proof.verify(psk, transcript)
    }

    /// Encode as one canonical zero-session, zero-tag record.
    pub fn into_record(self) -> Record {
        let mut payload = Zeroizing::new([0_u8; ACTIVATE_REQUEST_LENGTH]);
        payload[0..16].copy_from_slice(self.credential_id.as_bytes());
        payload[16..24].copy_from_slice(&self.generation.get().to_le_bytes());
        payload[24..56].copy_from_slice(self.proof.as_bytes());
        encode_record(RECORD_KIND_ACTIVATE_REQUEST, self.sequence, &payload[..])
    }
}

/// Activate response carrying either durable success confirmation or failure.
pub enum ActivateResponse {
    /// Durable Active state and device confirmation.
    Activated {
        /// Echoed opaque request sequence.
        sequence: u64,
        /// Full zeroizing device activation confirmation.
        confirmation: ActivationConfirmation,
    },
    /// Intentionally coarse failure.
    Failure {
        /// Echoed opaque request sequence.
        sequence: u64,
        /// Stable public failure category.
        failure: ActivateFailure,
    },
}

impl ActivateResponse {
    /// Construct success only after Active state is durable and publishable.
    ///
    /// The physical-store owner is responsible for satisfying that precondition;
    /// this portable codec deliberately does not depend on a concrete store.
    pub fn activated_after_durable_commit(
        sequence: u64,
        confirmation: ActivationConfirmation,
    ) -> Self {
        Self::Activated {
            sequence,
            confirmation,
        }
    }

    /// Construct a coarse one-byte failure response.
    pub const fn failure(sequence: u64, failure: ActivateFailure) -> Self {
        Self::Failure { sequence, failure }
    }

    /// Echoed opaque request sequence.
    pub const fn sequence(&self) -> u64 {
        match self {
            Self::Activated { sequence, .. } | Self::Failure { sequence, .. } => *sequence,
        }
    }

    /// Public result category.
    pub const fn result(&self) -> ActivateResult {
        match self {
            Self::Activated { .. } => ActivateResult::Activated,
            Self::Failure { failure, .. } => match failure {
                ActivateFailure::ProofRejected => ActivateResult::ProofRejected,
                ActivateFailure::Refused => ActivateResult::Refused,
                ActivateFailure::Blocked => ActivateResult::Blocked,
                ActivateFailure::Unavailable => ActivateResult::Unavailable,
            },
        }
    }

    /// Activated credential identifier on success.
    pub const fn credential_id(&self) -> Option<CredentialId> {
        match self {
            Self::Activated { confirmation, .. } => Some(confirmation.credential_id()),
            Self::Failure { .. } => None,
        }
    }

    /// Activated credential generation on success.
    pub const fn generation(&self) -> Option<CredentialGeneration> {
        match self {
            Self::Activated { confirmation, .. } => Some(confirmation.generation()),
            Self::Failure { .. } => None,
        }
    }

    /// Borrow the activation confirmation on success.
    pub const fn confirmation(&self) -> Option<&ActivationConfirmation> {
        match self {
            Self::Activated { confirmation, .. } => Some(confirmation),
            Self::Failure { .. } => None,
        }
    }

    /// Return the coarse failure when this response did not succeed.
    pub const fn failure_kind(&self) -> Option<ActivateFailure> {
        match self {
            Self::Activated { .. } => None,
            Self::Failure { failure, .. } => Some(*failure),
        }
    }

    /// Verify a successful response against the exact outstanding transcript.
    ///
    /// Failure responses and substituted credential references return `false`
    /// without exposing which check failed. Because sending an Activate request
    /// consumes its non-cloneable [`ClientProof`], this method re-derives that
    /// deterministic proof into a fresh zeroizing owner instead of requiring
    /// the host to retain a second copy across transport I/O.
    pub fn verify_confirmation(&self, psk: &PairingPsk, transcript: &PairingTranscript) -> bool {
        // The Activate request consumes its sole non-cloneable proof owner.
        // Re-derive the deterministic proof in a fresh zeroizing owner so the
        // host never needs to retain a second secret copy across transport I/O.
        let client_proof = ClientProof::calculate(psk, transcript);
        match self {
            Self::Activated { confirmation, .. } => {
                confirmation.verify(psk, transcript, &client_proof)
            }
            Self::Failure { .. } => false,
        }
    }

    /// Encode as one canonical zero-session, zero-tag record.
    pub fn into_record(self) -> Record {
        match self {
            Self::Activated {
                sequence,
                confirmation,
            } => {
                let mut payload = Zeroizing::new([0_u8; ACTIVATE_SUCCESS_LENGTH]);
                encode_success_prefix(&mut payload);
                payload[8..24].copy_from_slice(confirmation.credential_id().as_bytes());
                payload[24..32].copy_from_slice(&confirmation.generation().get().to_le_bytes());
                payload[32..64].copy_from_slice(confirmation.as_bytes());
                encode_record(RECORD_KIND_ACTIVATE_RESPONSE, sequence, &payload[..])
            }
            Self::Failure { sequence, failure } => {
                encode_record(RECORD_KIND_ACTIVATE_RESPONSE, sequence, &[failure.code()])
            }
        }
    }
}

/// Empty identifier-free request to abort the device's current Pending record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AbortCurrentRequest {
    sequence: u64,
}

impl AbortCurrentRequest {
    /// Construct an empty AbortCurrent request.
    pub const fn new(sequence: u64) -> Self {
        Self { sequence }
    }

    /// Opaque framing sequence.
    pub const fn sequence(self) -> u64 {
        self.sequence
    }

    /// Encode as one canonical zero-session, zero-tag record.
    pub fn into_record(self) -> Record {
        encode_record(RECORD_KIND_ABORT_CURRENT_REQUEST, self.sequence, &[])
    }
}

/// One-byte AbortCurrent response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AbortCurrentResponse {
    sequence: u64,
    result: AbortResult,
}

impl AbortCurrentResponse {
    /// Construct a coarse AbortCurrent response.
    pub const fn new(sequence: u64, result: AbortResult) -> Self {
        Self { sequence, result }
    }

    /// Echoed opaque request sequence.
    pub const fn sequence(self) -> u64 {
        self.sequence
    }

    /// Stable public result.
    pub const fn result(self) -> AbortResult {
        self.result
    }

    /// Encode as one canonical zero-session, zero-tag record.
    pub fn into_record(self) -> Record {
        encode_record(
            RECORD_KIND_ABORT_CURRENT_RESPONSE,
            self.sequence,
            &[self.result.code()],
        )
    }
}

/// Union of all canonical client-to-device live-pairing requests.
pub enum PairingRequest {
    /// Empty Begin request.
    Begin(BeginRequest),
    /// Fixed-profile ProofStart request.
    ProofStart(ProofStartRequest),
    /// Activate continuation carrying a full client proof.
    Activate(ActivateRequest),
    /// Empty identifier-free current-Pending abort request.
    AbortCurrent(AbortCurrentRequest),
}

impl PairingRequest {
    /// Decode and consume one canonical live-pairing request record.
    ///
    /// Sequence ordering, replay policy, connection binding, and operation
    /// admission remain the caller's responsibility.
    pub fn from_record(bearer: BearerBinding, record: Record) -> Result<Self, DecodeError> {
        validate_pre_authentication(&record)?;
        let sequence = record.sequence();
        let payload = record.payload();
        match record.kind() {
            RECORD_KIND_BEGIN_REQUEST => {
                require_length(payload, BEGIN_REQUEST_LENGTH)?;
                Ok(Self::Begin(BeginRequest::new(sequence)))
            }
            RECORD_KIND_PROOF_START_REQUEST => {
                require_length(payload, PROOF_START_REQUEST_LENGTH)?;
                validate_profile(&payload[..PROFILE_LENGTH], bearer)?;
                let credential_id = decode_credential_id(&payload[8..24])?;
                let generation = decode_generation(&payload[24..32])?;
                let mut nonce = [0_u8; 32];
                nonce.copy_from_slice(&payload[32..64]);
                let request =
                    ProofStartRequest::new(bearer, sequence, credential_id, generation, nonce)
                        .map_err(DecodeError::InvalidRequiredField)?;
                Ok(Self::ProofStart(request))
            }
            RECORD_KIND_ACTIVATE_REQUEST => {
                require_length(payload, ACTIVATE_REQUEST_LENGTH)?;
                let credential_id = decode_credential_id(&payload[..16])?;
                let generation = decode_generation(&payload[16..24])?;
                let mut proof = Zeroizing::new([0_u8; 32]);
                proof.copy_from_slice(&payload[24..56]);
                let request = ActivateRequest::new(
                    sequence,
                    credential_id,
                    generation,
                    ClientProof::from_zeroizing(proof),
                )
                .map_err(DecodeError::InvalidRequiredField)?;
                Ok(Self::Activate(request))
            }
            RECORD_KIND_ABORT_CURRENT_REQUEST => {
                require_length(payload, ABORT_CURRENT_REQUEST_LENGTH)?;
                Ok(Self::AbortCurrent(AbortCurrentRequest::new(sequence)))
            }
            _ => Err(DecodeError::UnknownKind),
        }
    }

    /// Opaque framing sequence.
    pub const fn sequence(&self) -> u64 {
        match self {
            Self::Begin(request) => request.sequence(),
            Self::ProofStart(request) => request.sequence(),
            Self::Activate(request) => request.sequence(),
            Self::AbortCurrent(request) => request.sequence(),
        }
    }

    /// Encode as one canonical zero-session, zero-tag record.
    pub fn into_record(self) -> Record {
        match self {
            Self::Begin(request) => request.into_record(),
            Self::ProofStart(request) => request.into_record(),
            Self::Activate(request) => request.into_record(),
            Self::AbortCurrent(request) => request.into_record(),
        }
    }
}

/// Union of all canonical device-to-client live-pairing responses.
pub enum PairingResponse {
    /// Begin offer or coarse failure.
    Begin(BeginResponse),
    /// Proof challenge or coarse failure.
    ProofStart(ProofStartResponse),
    /// Durable activation confirmation or coarse failure.
    Activate(ActivateResponse),
    /// One-byte AbortCurrent result.
    AbortCurrent(AbortCurrentResponse),
}

impl PairingResponse {
    /// Decode and consume one canonical live-pairing response record.
    ///
    /// Response correlation and durable-state interpretation remain the
    /// caller's responsibility.
    pub fn from_record(bearer: BearerBinding, record: Record) -> Result<Self, DecodeError> {
        validate_pre_authentication(&record)?;
        let sequence = record.sequence();
        let payload = record.payload();
        match record.kind() {
            RECORD_KIND_BEGIN_RESPONSE => {
                decode_begin_response(sequence, payload, bearer).map(Self::Begin)
            }
            RECORD_KIND_PROOF_START_RESPONSE => {
                decode_proof_response(sequence, payload, bearer).map(Self::ProofStart)
            }
            RECORD_KIND_ACTIVATE_RESPONSE => {
                decode_activate_response(sequence, payload).map(Self::Activate)
            }
            RECORD_KIND_ABORT_CURRENT_RESPONSE => {
                require_length(payload, ABORT_CURRENT_RESPONSE_LENGTH)?;
                let result =
                    AbortResult::from_code(payload[0]).ok_or(DecodeError::UnknownResult {
                        observed: payload[0],
                    })?;
                Ok(Self::AbortCurrent(AbortCurrentResponse::new(
                    sequence, result,
                )))
            }
            _ => Err(DecodeError::UnknownKind),
        }
    }

    /// Echoed opaque request sequence.
    pub const fn sequence(&self) -> u64 {
        match self {
            Self::Begin(response) => response.sequence(),
            Self::ProofStart(response) => response.sequence(),
            Self::Activate(response) => response.sequence(),
            Self::AbortCurrent(response) => response.sequence(),
        }
    }

    /// Encode as one canonical zero-session, zero-tag record.
    pub fn into_record(self) -> Record {
        match self {
            Self::Begin(response) => response.into_record(),
            Self::ProofStart(response) => response.into_record(),
            Self::Activate(response) => response.into_record(),
            Self::AbortCurrent(response) => response.into_record(),
        }
    }
}

/// Public category for a noncanonical live-pairing record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeError {
    /// Unauthenticated record carried a nonzero framing session ID.
    NonzeroSessionId,
    /// Unauthenticated record carried a nonzero framing authentication tag.
    NonzeroAuthenticationTag,
    /// Kind is not accepted by the selected request or response union.
    UnknownKind,
    /// Payload length did not match the one legal shape for this result.
    WrongPayloadLength {
        /// Expected fixed length.
        expected: usize,
        /// Observed payload length.
        observed: usize,
    },
    /// Success and failure result codes used the wrong full/one-byte shape.
    WrongResultShape,
    /// One-byte result code is not assigned by this protocol version.
    UnknownResult {
        /// Rejected result code.
        observed: u8,
    },
    /// A reserved byte was nonzero.
    ReservedBitsSet,
    /// Pairing protocol major/minor pair is unsupported.
    UnsupportedVersion {
        /// Observed major version.
        major: u16,
        /// Observed minor version.
        minor: u16,
    },
    /// Possession-proof suite is unsupported.
    UnsupportedSuite {
        /// Observed suite identifier.
        observed: u16,
    },
    /// Bearer binding is unsupported or differs from the selected bearer.
    UnsupportedBearer {
        /// Observed bearer code.
        observed: u8,
    },
    /// A field required to be nonzero was zero.
    InvalidRequiredField(InvalidField),
}

fn decode_begin_response(
    sequence: u64,
    payload: &[u8],
    bearer: BearerBinding,
) -> Result<BeginResponse, DecodeError> {
    let result = first_result(payload)?;
    if result != 0 {
        require_length(payload, 1)?;
        let failure = PairingFailure::from_code(result)
            .ok_or(DecodeError::UnknownResult { observed: result })?;
        return Ok(BeginResponse::failure(sequence, failure));
    }
    if payload.len() == 1 {
        return Err(DecodeError::WrongResultShape);
    }
    require_length(payload, BEGIN_OFFER_LENGTH)?;
    validate_success_prefix(payload)?;
    validate_profile(&payload[8..16], bearer)?;
    let device_id = decode_device_id(&payload[16..32])?;
    let credential_id = decode_credential_id(&payload[32..48])?;
    let generation = decode_generation(&payload[48..56])?;
    let mut psk = Zeroizing::new([0_u8; 32]);
    psk.copy_from_slice(&payload[56..88]);
    let psk = PairingPsk::from_zeroizing(psk).map_err(DecodeError::InvalidRequiredField)?;
    let offer = BeginOffer::after_pending_commit(bearer, device_id, credential_id, generation, psk)
        .map_err(DecodeError::InvalidRequiredField)?;
    Ok(BeginResponse::offered(sequence, offer))
}

fn decode_proof_response(
    sequence: u64,
    payload: &[u8],
    bearer: BearerBinding,
) -> Result<ProofStartResponse, DecodeError> {
    let result = first_result(payload)?;
    if result != 0 {
        require_length(payload, 1)?;
        let failure = PairingFailure::from_code(result)
            .ok_or(DecodeError::UnknownResult { observed: result })?;
        return Ok(ProofStartResponse::failure(sequence, failure));
    }
    if payload.len() == 1 {
        return Err(DecodeError::WrongResultShape);
    }
    require_length(payload, PROOF_CHALLENGE_LENGTH)?;
    validate_success_prefix(payload)?;
    validate_profile(&payload[8..16], bearer)?;
    let device_id = decode_device_id(&payload[16..32])?;
    let connection_id = decode_connection(&payload[32..40])?;
    let window_id = decode_window(&payload[40..48])?;
    let credential_id = decode_credential_id(&payload[48..64])?;
    let generation = decode_generation(&payload[64..72])?;
    let mut challenge = Zeroizing::new([0_u8; 32]);
    challenge.copy_from_slice(&payload[72..104]);
    let challenge =
        DeviceChallenge::from_zeroizing(challenge).map_err(DecodeError::InvalidRequiredField)?;
    let challenge = ProofChallenge::new(
        bearer,
        device_id,
        connection_id,
        window_id,
        credential_id,
        generation,
        challenge,
    )
    .map_err(DecodeError::InvalidRequiredField)?;
    Ok(ProofStartResponse::challenge(sequence, challenge))
}

fn decode_activate_response(
    sequence: u64,
    payload: &[u8],
) -> Result<ActivateResponse, DecodeError> {
    let result = first_result(payload)?;
    if result != 0 {
        require_length(payload, 1)?;
        let failure = ActivateFailure::from_code(result)
            .ok_or(DecodeError::UnknownResult { observed: result })?;
        return Ok(ActivateResponse::failure(sequence, failure));
    }
    if payload.len() == 1 {
        return Err(DecodeError::WrongResultShape);
    }
    require_length(payload, ACTIVATE_SUCCESS_LENGTH)?;
    validate_success_prefix(payload)?;
    let credential_id = decode_credential_id(&payload[8..24])?;
    let generation = decode_generation(&payload[24..32])?;
    let mut confirmation = Zeroizing::new([0_u8; 32]);
    confirmation.copy_from_slice(&payload[32..64]);
    let confirmation =
        ActivationConfirmation::from_zeroizing(credential_id, generation, confirmation)
            .map_err(DecodeError::InvalidRequiredField)?;
    Ok(ActivateResponse::activated_after_durable_commit(
        sequence,
        confirmation,
    ))
}

fn first_result(payload: &[u8]) -> Result<u8, DecodeError> {
    payload
        .first()
        .copied()
        .ok_or(DecodeError::WrongPayloadLength {
            expected: 1,
            observed: 0,
        })
}

fn encode_success_prefix<const N: usize>(payload: &mut [u8; N]) {
    payload[..RESULT_PREFIX_LENGTH].fill(0);
}

fn validate_success_prefix(payload: &[u8]) -> Result<(), DecodeError> {
    if payload[0] != 0 {
        return Err(DecodeError::WrongResultShape);
    }
    if payload[1..RESULT_PREFIX_LENGTH] != [0; RESULT_PREFIX_LENGTH - 1] {
        return Err(DecodeError::ReservedBitsSet);
    }
    Ok(())
}

fn encode_profile(output: &mut [u8], bearer: BearerBinding) {
    output[0..2].copy_from_slice(&PROTOCOL_MAJOR.to_le_bytes());
    output[2..4].copy_from_slice(&PROTOCOL_MINOR.to_le_bytes());
    output[4..6].copy_from_slice(&PROOF_SUITE.to_le_bytes());
    output[6] = bearer.code();
    output[7] = 0;
}

fn validate_profile(payload: &[u8], bearer: BearerBinding) -> Result<(), DecodeError> {
    let major = u16::from_le_bytes([payload[0], payload[1]]);
    let minor = u16::from_le_bytes([payload[2], payload[3]]);
    if major != PROTOCOL_MAJOR || minor != PROTOCOL_MINOR {
        return Err(DecodeError::UnsupportedVersion { major, minor });
    }
    let suite = u16::from_le_bytes([payload[4], payload[5]]);
    if suite != PROOF_SUITE {
        return Err(DecodeError::UnsupportedSuite { observed: suite });
    }
    if BearerBinding::from_code(payload[6]) != Some(bearer) {
        return Err(DecodeError::UnsupportedBearer {
            observed: payload[6],
        });
    }
    if payload[7] != 0 {
        return Err(DecodeError::ReservedBitsSet);
    }
    Ok(())
}

fn decode_device_id(payload: &[u8]) -> Result<DeviceId, DecodeError> {
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(payload);
    DeviceId::new(bytes).map_err(DecodeError::InvalidRequiredField)
}

fn decode_credential_id(payload: &[u8]) -> Result<CredentialId, DecodeError> {
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(payload);
    require_nonzero(&bytes, InvalidField::ZeroCredentialId)
        .map_err(DecodeError::InvalidRequiredField)?;
    Ok(CredentialId::new(bytes))
}

fn decode_generation(payload: &[u8]) -> Result<CredentialGeneration, DecodeError> {
    let value =
        u64::from_le_bytes(
            payload
                .try_into()
                .map_err(|_| DecodeError::WrongPayloadLength {
                    expected: 8,
                    observed: payload.len(),
                })?,
        );
    if value == 0 {
        return Err(DecodeError::InvalidRequiredField(
            InvalidField::ZeroCredentialGeneration,
        ));
    }
    Ok(CredentialGeneration::new(value))
}

fn decode_connection(payload: &[u8]) -> Result<ConnectionId, DecodeError> {
    let value =
        u64::from_le_bytes(
            payload
                .try_into()
                .map_err(|_| DecodeError::WrongPayloadLength {
                    expected: 8,
                    observed: payload.len(),
                })?,
        );
    ConnectionId::new(value).ok_or(DecodeError::InvalidRequiredField(
        InvalidField::ZeroConnectionId,
    ))
}

fn decode_window(payload: &[u8]) -> Result<WindowId, DecodeError> {
    let value =
        u64::from_le_bytes(
            payload
                .try_into()
                .map_err(|_| DecodeError::WrongPayloadLength {
                    expected: 8,
                    observed: payload.len(),
                })?,
        );
    WindowId::new(value).ok_or(DecodeError::InvalidRequiredField(
        InvalidField::ZeroWindowId,
    ))
}

fn validate_credential(
    credential_id: CredentialId,
    generation: CredentialGeneration,
) -> Result<(), InvalidField> {
    require_nonzero(credential_id.as_bytes(), InvalidField::ZeroCredentialId)?;
    if generation.get() == 0 {
        return Err(InvalidField::ZeroCredentialGeneration);
    }
    Ok(())
}

fn require_nonzero(bytes: &[u8], error: InvalidField) -> Result<(), InvalidField> {
    if bytes.iter().all(|byte| *byte == 0) {
        Err(error)
    } else {
        Ok(())
    }
}

fn require_length(payload: &[u8], expected: usize) -> Result<(), DecodeError> {
    if payload.len() == expected {
        Ok(())
    } else {
        Err(DecodeError::WrongPayloadLength {
            expected,
            observed: payload.len(),
        })
    }
}

fn validate_pre_authentication(record: &Record) -> Result<(), DecodeError> {
    if record.session_id() != &ZERO_SESSION_ID {
        return Err(DecodeError::NonzeroSessionId);
    }
    if record.authentication_tag() != &ZERO_AUTHENTICATION_TAG {
        return Err(DecodeError::NonzeroAuthenticationTag);
    }
    Ok(())
}

fn encode_record(kind: u8, sequence: u64, payload: &[u8]) -> Record {
    let mut owner = Zeroizing::new([0_u8; PAYLOAD_CAPACITY]);
    owner[..payload.len()].copy_from_slice(payload);
    Record::new(
        kind,
        ZERO_SESSION_ID,
        sequence,
        PayloadLength::new(payload.len()).expect("fixed pairing payload fits framing record"),
        *owner,
        ZERO_AUTHENTICATION_TAG,
    )
}
