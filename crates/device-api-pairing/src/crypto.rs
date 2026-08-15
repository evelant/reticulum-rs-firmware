//! Canonical pairing transcript and full HMAC-SHA256 proofs.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use reticulum_device_api_credentials::{CredentialGeneration, CredentialId};

use crate::protocol::{
    BearerBinding, InvalidField, PROOF_CHALLENGE_LENGTH, PROOF_START_REQUEST_LENGTH,
    ProofChallenge, ProofStartRequest, RECORD_KIND_PROOF_START_REQUEST,
    RECORD_KIND_PROOF_START_RESPONSE,
};

type HmacSha256 = Hmac<Sha256>;

const TRANSCRIPT_DOMAIN: &[u8] = b"reticulum-rs-firmware/device-api/pairing/transcript/v1\0";
const CLIENT_PROOF_DOMAIN: &[u8] = b"reticulum-rs-firmware/device-api/pairing/client-proof/v2\0";
const ACTIVATION_PROOF_DOMAIN: &[u8] =
    b"reticulum-rs-firmware/device-api/pairing/activation-proof/v2\0";

/// Zeroizing 256-bit pairing pre-shared key.
///
/// This owner deliberately implements neither `Copy`, `Clone`, nor `Debug`.
pub struct PairingPsk(Zeroizing<[u8; 32]>);

impl PairingPsk {
    /// Validate and own a nonzero pairing PSK.
    pub fn new(bytes: [u8; 32]) -> Result<Self, InvalidField> {
        Self::from_zeroizing(Zeroizing::new(bytes))
    }

    /// Validate and take an existing zeroizing PSK owner without copying it.
    pub fn from_zeroizing(bytes: Zeroizing<[u8; 32]>) -> Result<Self, InvalidField> {
        if bytes.iter().all(|byte| *byte == 0) {
            Err(InvalidField::ZeroPairingPsk)
        } else {
            Ok(Self(bytes))
        }
    }

    /// Borrow the PSK bytes without copying them.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Consume the PSK into its zeroizing fixed buffer.
    pub fn into_bytes(self) -> Zeroizing<[u8; 32]> {
        self.0
    }
}

/// SHA-256 hash of one exact successful ProofStart exchange.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct PairingTranscript {
    hash: [u8; 32],
    bearer: BearerBinding,
    credential_id: CredentialId,
    generation: CredentialGeneration,
}

impl PairingTranscript {
    /// Hash one canonical matching ProofStart request and successful challenge.
    ///
    /// Framing sequences are intentionally excluded. The caller still owns
    /// exact-next ordering and must retain the challenge's connection/window
    /// binding until the Activate continuation is consumed.
    pub fn new(
        request: &ProofStartRequest,
        challenge: &ProofChallenge,
    ) -> Result<Self, TranscriptMismatch> {
        if request.bearer() != challenge.bearer()
            || request.credential_id() != challenge.credential_id()
            || request.generation() != challenge.generation()
        {
            return Err(TranscriptMismatch);
        }
        let request_payload = request.encode_payload();
        let challenge_payload: Zeroizing<[u8; PROOF_CHALLENGE_LENGTH]> = challenge.encode_payload();
        Ok(Self {
            hash: transcript_hash(
                RECORD_KIND_PROOF_START_REQUEST,
                PROOF_START_REQUEST_LENGTH as u16,
                &request_payload,
                RECORD_KIND_PROOF_START_RESPONSE,
                PROOF_CHALLENGE_LENGTH as u16,
                &challenge_payload[..],
            ),
            bearer: request.bearer(),
            credential_id: request.credential_id(),
            generation: request.generation(),
        })
    }

    /// Pairing bearer cryptographically bound by this transcript.
    pub const fn bearer(&self) -> BearerBinding {
        self.bearer
    }

    /// Borrow the exact transcript hash.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.hash
    }

    /// Credential identifier bound by this transcript.
    pub const fn credential_id(&self) -> CredentialId {
        self.credential_id
    }

    /// Credential generation bound by this transcript.
    pub const fn generation(&self) -> CredentialGeneration {
        self.generation
    }
}

/// ProofStart request and challenge named different credential generations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TranscriptMismatch;

/// Full zeroizing client possession proof.
///
/// This owner deliberately implements neither `Copy`, `Clone`, nor `Debug`.
///
/// ```compile_fail
/// use reticulum_device_api_pairing::ClientProof;
/// fn require_copy<T: Copy>() {}
/// require_copy::<ClientProof>();
/// ```
pub struct ClientProof(Zeroizing<[u8; 32]>);

impl ClientProof {
    /// Calculate the canonical client proof for one transcript.
    pub fn calculate(psk: &PairingPsk, transcript: &PairingTranscript) -> Self {
        Self(full_mac(
            psk.as_bytes(),
            CLIENT_PROOF_DOMAIN,
            &[transcript.as_bytes()],
        ))
    }

    /// Own 32 proof bytes decoded from the wire.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// Take 32 decoded proof bytes from an existing zeroizing owner.
    pub fn from_zeroizing(bytes: Zeroizing<[u8; 32]>) -> Self {
        Self(bytes)
    }

    /// Borrow the full proof without copying it.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Consume the proof into its zeroizing fixed buffer.
    pub fn into_bytes(self) -> Zeroizing<[u8; 32]> {
        self.0
    }

    /// Verify this proof in constant time and consume it into verified state.
    pub(crate) fn verify(
        self,
        psk: &PairingPsk,
        transcript: &PairingTranscript,
    ) -> Result<VerifiedClientProof, ProofRejected> {
        if !verify_full_mac(
            psk.as_bytes(),
            CLIENT_PROOF_DOMAIN,
            &[transcript.as_bytes()],
            self.as_bytes(),
        ) {
            return Err(ProofRejected);
        }
        Ok(VerifiedClientProof {
            transcript_hash: transcript.hash,
            credential_id: transcript.credential_id,
            generation: transcript.generation,
            proof: self.0,
        })
    }
}

/// A client proof failed constant-time HMAC verification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProofRejected;

/// Verified client proof retained until durable activation completes.
///
/// This owner deliberately implements neither `Copy`, `Clone`, nor `Debug`.
#[must_use = "verified proof state must be activated or explicitly dropped"]
pub struct VerifiedClientProof {
    transcript_hash: [u8; 32],
    credential_id: CredentialId,
    generation: CredentialGeneration,
    proof: Zeroizing<[u8; 32]>,
}

impl VerifiedClientProof {
    /// Produce the device confirmation after the caller commits Active state.
    ///
    /// Consuming the verified owner makes one confirmation the terminal use of
    /// this proof state. The physical-store owner must call this only after the
    /// exact Active successor is durable and publishable.
    pub fn into_activation_confirmation(
        self,
        activated_generation: CredentialGeneration,
        psk: &PairingPsk,
    ) -> Result<ActivationConfirmation, ActivationGenerationNotAdvanced> {
        if activated_generation.get() <= self.generation.get() {
            return Err(ActivationGenerationNotAdvanced);
        }
        let generation = activated_generation.get().to_le_bytes();
        Ok(ActivationConfirmation {
            credential_id: self.credential_id,
            generation: activated_generation,
            mac: full_mac(
                psk.as_bytes(),
                ACTIVATION_PROOF_DOMAIN,
                &[&self.transcript_hash, &self.proof[..], &generation],
            ),
        })
    }
}

/// A durable Active credential did not advance beyond its Pending generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActivationGenerationNotAdvanced;

/// Full zeroizing device activation-confirmation HMAC.
///
/// This owner deliberately implements neither `Copy`, `Clone`, nor `Debug`.
///
/// ```compile_fail
/// use reticulum_device_api_pairing::ActivationConfirmation;
/// fn require_debug<T: core::fmt::Debug>() {}
/// require_debug::<ActivationConfirmation>();
/// ```
pub struct ActivationConfirmation {
    credential_id: CredentialId,
    generation: CredentialGeneration,
    mac: Zeroizing<[u8; 32]>,
}

impl ActivationConfirmation {
    /// Calculate an expected confirmation for host-side verification.
    pub fn calculate(
        psk: &PairingPsk,
        transcript: &PairingTranscript,
        client_proof: &ClientProof,
        activated_generation: CredentialGeneration,
    ) -> Result<Self, ActivationGenerationNotAdvanced> {
        if activated_generation.get() <= transcript.generation.get() {
            return Err(ActivationGenerationNotAdvanced);
        }
        let generation = activated_generation.get().to_le_bytes();
        Ok(Self {
            credential_id: transcript.credential_id,
            generation: activated_generation,
            mac: full_mac(
                psk.as_bytes(),
                ACTIVATION_PROOF_DOMAIN,
                &[transcript.as_bytes(), client_proof.as_bytes(), &generation],
            ),
        })
    }

    /// Bind 32 decoded confirmation bytes to their validated credential reference.
    pub fn from_bytes(
        credential_id: CredentialId,
        generation: CredentialGeneration,
        bytes: [u8; 32],
    ) -> Result<Self, InvalidField> {
        Self::from_zeroizing(credential_id, generation, Zeroizing::new(bytes))
    }

    /// Bind an existing zeroizing confirmation to its credential reference.
    pub fn from_zeroizing(
        credential_id: CredentialId,
        generation: CredentialGeneration,
        bytes: Zeroizing<[u8; 32]>,
    ) -> Result<Self, InvalidField> {
        if credential_id.as_bytes().iter().all(|byte| *byte == 0) {
            return Err(InvalidField::ZeroCredentialId);
        }
        if generation.get() == 0 {
            return Err(InvalidField::ZeroCredentialGeneration);
        }
        Ok(Self {
            credential_id,
            generation,
            mac: bytes,
        })
    }

    /// Borrow the full confirmation without copying it.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.mac
    }

    /// Credential identifier whose activation this confirmation proves.
    pub const fn credential_id(&self) -> CredentialId {
        self.credential_id
    }

    /// Credential generation whose activation this confirmation proves.
    pub const fn generation(&self) -> CredentialGeneration {
        self.generation
    }

    /// Consume the confirmation into its zeroizing fixed buffer.
    pub fn into_bytes(self) -> Zeroizing<[u8; 32]> {
        self.mac
    }

    /// Verify this confirmation in constant time.
    pub fn verify(
        &self,
        psk: &PairingPsk,
        transcript: &PairingTranscript,
        client_proof: &ClientProof,
    ) -> bool {
        let generation = self.generation.get().to_le_bytes();
        self.credential_id == transcript.credential_id
            && self.generation.get() > transcript.generation.get()
            && verify_full_mac(
                psk.as_bytes(),
                ACTIVATION_PROOF_DOMAIN,
                &[transcript.as_bytes(), client_proof.as_bytes(), &generation],
                self.as_bytes(),
            )
    }
}

fn transcript_hash(
    request_kind: u8,
    request_length: u16,
    request_payload: &[u8],
    response_kind: u8,
    response_length: u16,
    response_payload: &[u8],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(TRANSCRIPT_DOMAIN);
    hasher.update([request_kind]);
    hasher.update(request_length.to_le_bytes());
    hasher.update(request_payload);
    hasher.update([response_kind]);
    hasher.update(response_length.to_le_bytes());
    hasher.update(response_payload);
    hasher.finalize().into()
}

#[cfg(test)]
pub(crate) fn transcript_hash_for_test(
    request_kind: u8,
    request_length: u16,
    request_payload: &[u8],
    response_kind: u8,
    response_length: u16,
    response_payload: &[u8],
) -> PairingTranscript {
    let mut credential_id = [0_u8; 16];
    credential_id.copy_from_slice(&request_payload[8..24]);
    let generation = u64::from_le_bytes(
        request_payload[24..32]
            .try_into()
            .expect("pairing transcript test request has the canonical shape"),
    );
    PairingTranscript {
        hash: transcript_hash(
            request_kind,
            request_length,
            request_payload,
            response_kind,
            response_length,
            response_payload,
        ),
        // This test-only constructor intentionally hashes arbitrary mutated
        // profile bytes, including unsupported bearer codes.
        bearer: BearerBinding::BleGatt,
        credential_id: CredentialId::new(credential_id),
        generation: CredentialGeneration::new(generation),
    }
}

fn full_mac(key: &[u8; 32], domain: &[u8], parts: &[&[u8]]) -> Zeroizing<[u8; 32]> {
    let mut mac =
        HmacSha256::new_from_slice(key).expect("HMAC-SHA256 accepts the fixed pairing key length");
    mac.update(domain);
    for part in parts {
        mac.update(part);
    }
    Zeroizing::new(mac.finalize().into_bytes().into())
}

fn verify_full_mac(key: &[u8; 32], domain: &[u8], parts: &[&[u8]], observed: &[u8; 32]) -> bool {
    let mut mac =
        HmacSha256::new_from_slice(key).expect("HMAC-SHA256 accepts the fixed pairing key length");
    mac.update(domain);
    for part in parts {
        mac.update(part);
    }
    mac.verify_slice(observed).is_ok()
}
