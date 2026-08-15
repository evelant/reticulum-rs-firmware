//! Allocation-free live pairing records and possession proofs.
//!
//! This crate freezes the local bearer-bound pairing protocol above canonical
//! device-API framing. Every record is pre-authentication traffic: its framing
//! session ID and authentication tag are zero, while its opaque sequence is
//! preserved for the connection owner to order and correlate. The codec owns
//! no bearer driver, physical-presence policy, entropy source, credential-store
//! mutation, authorization policy, ordinary authenticated session, or logical
//! device API.
//!
//! Successful enrollment material, challenges, client proofs, and activation
//! confirmations remain non-copyable, non-debuggable, zeroizing owners. A
//! successful Begin offer must not be constructed until the Pending credential
//! is durable and publishable. Likewise, callers must construct an Activated
//! response only after the Active successor is durable and publishable.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod crypto;
mod protocol;

pub use crypto::{
    ActivationConfirmation, ActivationGenerationNotAdvanced, ClientProof, PairingPsk,
    PairingTranscript, ProofRejected, TranscriptMismatch, VerifiedClientProof,
};
pub use protocol::{
    ABORT_CURRENT_REQUEST_LENGTH, ABORT_CURRENT_RESPONSE_LENGTH, ACTIVATE_REQUEST_LENGTH,
    ACTIVATE_SUCCESS_LENGTH, AbortCurrentRequest, AbortCurrentResponse, AbortResult,
    ActivateFailure, ActivateRequest, ActivateResponse, ActivateResult, BEGIN_OFFER_LENGTH,
    BEGIN_REQUEST_LENGTH, BearerBinding, BeginOffer, BeginRequest, BeginResponse, BeginResult,
    ConnectionId, DecodeError, DeviceChallenge, DeviceId, InvalidField, PROOF_CHALLENGE_LENGTH,
    PROOF_START_REQUEST_LENGTH, PROOF_SUITE, PROTOCOL_MAJOR, PROTOCOL_MINOR, PairingFailure,
    PairingRequest, PairingResponse, ProofChallenge, ProofStartRequest, ProofStartResponse,
    ProofStartResult, RECORD_KIND_ABORT_CURRENT_REQUEST, RECORD_KIND_ABORT_CURRENT_RESPONSE,
    RECORD_KIND_ACTIVATE_REQUEST, RECORD_KIND_ACTIVATE_RESPONSE, RECORD_KIND_BEGIN_REQUEST,
    RECORD_KIND_BEGIN_RESPONSE, RECORD_KIND_PROOF_START_REQUEST, RECORD_KIND_PROOF_START_RESPONSE,
    WindowId,
};
pub use reticulum_device_api_credentials::{CredentialGeneration, CredentialId};

#[cfg(test)]
mod tests;
