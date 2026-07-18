//! Allocation-free pre-authentication credential-store initialization control.
//!
//! This crate freezes the small record codec used to discover whether a local
//! device needs credential-store initialization and to request that explicit
//! initialization. These records are deliberately unauthenticated: their
//! framing session ID and authentication tag must both be all zero. The
//! physical-presence policy, connection-bound sequence ordering, durable store
//! mutation, response delivery, and any authenticated pairing flow remain the
//! caller's responsibility.
//!
//! The framing sequence is carried through every typed value without being
//! interpreted. A bearer owner must enforce its own connection-local ordering,
//! replay, and response-correlation policy.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use reticulum_device_api_framing::{
    AUTH_TAG_LENGTH, PAYLOAD_CAPACITY, PayloadLength, Record, SESSION_ID_LENGTH,
};

/// Pre-authentication initialization-status request record kind.
pub const RECORD_KIND_STATUS_REQUEST: u8 = 0x20;
/// Pre-authentication initialization-status response record kind.
pub const RECORD_KIND_STATUS_RESPONSE: u8 = 0x21;
/// Pre-authentication explicit-initialization request record kind.
pub const RECORD_KIND_INITIALIZE_REQUEST: u8 = 0x22;
/// Pre-authentication explicit-initialization response record kind.
pub const RECORD_KIND_INITIALIZE_RESPONSE: u8 = 0x23;

const ZERO_SESSION_ID: [u8; SESSION_ID_LENGTH] = [0; SESSION_ID_LENGTH];
const ZERO_AUTHENTICATION_TAG: [u8; AUTH_TAG_LENGTH] = [0; AUTH_TAG_LENGTH];
const EMPTY_PAYLOAD_LENGTH: PayloadLength = match PayloadLength::new(0) {
    Ok(length) => length,
    Err(_) => panic!("empty framing payload must fit"),
};
const ONE_BYTE_PAYLOAD_LENGTH: PayloadLength = match PayloadLength::new(1) {
    Ok(length) => length,
    Err(_) => panic!("one-byte framing payload must fit"),
};

/// Stable, intentionally coarse credential-initialization status.
///
/// The numeric values are part of the wire contract. No variant reveals media,
/// policy, identity, or storage diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum InitializationStatus {
    /// Initialization is not available in the current product or state.
    Unavailable = 0,
    /// Explicit initialization is required before authenticated pairing.
    InitializationRequired = 1,
    /// An initialization attempt is currently owned by the device.
    InFlight = 2,
    /// Credential-store initialization has completed.
    Completed = 3,
    /// Initialization is fail-closed and cannot currently proceed.
    Blocked = 4,
}

impl InitializationStatus {
    /// Return the stable one-byte wire code.
    pub const fn code(self) -> u8 {
        self as u8
    }

    /// Decode one stable wire code.
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Unavailable),
            1 => Some(Self::InitializationRequired),
            2 => Some(Self::InFlight),
            3 => Some(Self::Completed),
            4 => Some(Self::Blocked),
            _ => None,
        }
    }
}

/// Stable, intentionally coarse result of one explicit initialization request.
///
/// The numeric values are part of the wire contract. Transient storage and
/// policy details are deliberately collapsed into these public categories.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum InitializeResult {
    /// Initialization completed durably.
    Completed = 0,
    /// The same request may be retried according to bearer policy.
    Retrying = 1,
    /// A qualifying physical-presence window is required.
    PhysicalPresenceRequired = 2,
    /// The request was refused without exposing the refusal reason.
    Refused = 3,
    /// Initialization is fail-closed and cannot currently proceed.
    Blocked = 4,
    /// Initialization is not available in the current product or state.
    Unavailable = 5,
}

impl InitializeResult {
    /// Return the stable one-byte wire code.
    pub const fn code(self) -> u8 {
        self as u8
    }

    /// Decode one stable wire code.
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Completed),
            1 => Some(Self::Retrying),
            2 => Some(Self::PhysicalPresenceRequired),
            3 => Some(Self::Refused),
            4 => Some(Self::Blocked),
            5 => Some(Self::Unavailable),
            _ => None,
        }
    }
}

/// One typed pre-authentication initialization-control request.
///
/// The sequence is opaque to this codec. The connection owner must enforce
/// ordering and replay rules before acting on a decoded request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "a decoded initialization-control request must be handled or explicitly dropped"]
pub enum ControlRequest {
    /// Query the current coarse initialization status.
    Status {
        /// Framing sequence selected by the connection owner.
        sequence: u64,
    },
    /// Request explicit credential-store initialization.
    Initialize {
        /// Framing sequence selected by the connection owner.
        sequence: u64,
    },
}

impl ControlRequest {
    /// Construct a status request carrying the supplied framing sequence.
    pub const fn status(sequence: u64) -> Self {
        Self::Status { sequence }
    }

    /// Construct an explicit-initialization request carrying the supplied
    /// framing sequence.
    pub const fn initialize(sequence: u64) -> Self {
        Self::Initialize { sequence }
    }

    /// Return the opaque framing sequence without interpreting it.
    pub const fn sequence(self) -> u64 {
        match self {
            Self::Status { sequence } | Self::Initialize { sequence } => sequence,
        }
    }

    /// Encode this request as one canonical zero-session, zero-tag record.
    pub fn into_record(self) -> Record {
        let kind = match self {
            Self::Status { .. } => RECORD_KIND_STATUS_REQUEST,
            Self::Initialize { .. } => RECORD_KIND_INITIALIZE_REQUEST,
        };
        empty_record(kind, self.sequence())
    }

    /// Decode and consume one canonical pre-authentication request record.
    ///
    /// This validates only the initialization-control wire contract. It does
    /// not enforce sequence ordering, replay protection, or connection state.
    pub fn from_record(record: Record) -> Result<Self, DecodeError> {
        validate_pre_authentication(&record)?;
        let is_status = match record.kind() {
            RECORD_KIND_STATUS_REQUEST => true,
            RECORD_KIND_INITIALIZE_REQUEST => false,
            _ => return Err(DecodeError::UnknownKind),
        };
        if record.payload_length().get() != 0 {
            return Err(DecodeError::WrongPayloadShape);
        }
        let sequence = record.sequence();
        if is_status {
            Ok(Self::Status { sequence })
        } else {
            Ok(Self::Initialize { sequence })
        }
    }
}

/// One typed pre-authentication initialization-control response.
///
/// The sequence is copied from framing without being interpreted. The bearer
/// owner remains responsible for correlating it with a request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "a decoded initialization-control response must be handled or explicitly dropped"]
pub enum ControlResponse {
    /// Coarse initialization status response.
    Status {
        /// Framing sequence selected by the connection owner.
        sequence: u64,
        /// Stable coarse status.
        status: InitializationStatus,
    },
    /// Result of an explicit initialization request.
    Initialize {
        /// Framing sequence selected by the connection owner.
        sequence: u64,
        /// Stable coarse initialization result.
        result: InitializeResult,
    },
}

impl ControlResponse {
    /// Construct a status response carrying the supplied framing sequence.
    pub const fn status(sequence: u64, status: InitializationStatus) -> Self {
        Self::Status { sequence, status }
    }

    /// Construct an initialization response carrying the supplied framing
    /// sequence.
    pub const fn initialize(sequence: u64, result: InitializeResult) -> Self {
        Self::Initialize { sequence, result }
    }

    /// Return the opaque framing sequence without interpreting it.
    pub const fn sequence(self) -> u64 {
        match self {
            Self::Status { sequence, .. } | Self::Initialize { sequence, .. } => sequence,
        }
    }

    /// Encode this response as one canonical zero-session, zero-tag record.
    pub fn into_record(self) -> Record {
        match self {
            Self::Status { sequence, status } => {
                one_byte_record(RECORD_KIND_STATUS_RESPONSE, sequence, status.code())
            }
            Self::Initialize { sequence, result } => {
                one_byte_record(RECORD_KIND_INITIALIZE_RESPONSE, sequence, result.code())
            }
        }
    }

    /// Decode and consume one canonical pre-authentication response record.
    ///
    /// This validates only the initialization-control wire contract. It does
    /// not enforce sequence ordering, replay protection, or correlation.
    pub fn from_record(record: Record) -> Result<Self, DecodeError> {
        validate_pre_authentication(&record)?;
        let is_status = match record.kind() {
            RECORD_KIND_STATUS_RESPONSE => true,
            RECORD_KIND_INITIALIZE_RESPONSE => false,
            _ => return Err(DecodeError::UnknownKind),
        };
        if record.payload_length().get() != 1 {
            return Err(DecodeError::WrongPayloadShape);
        }
        let sequence = record.sequence();
        let code = record.payload()[0];
        if is_status {
            InitializationStatus::from_code(code)
                .map(|status| Self::Status { sequence, status })
                .ok_or(DecodeError::UnknownCode)
        } else {
            InitializeResult::from_code(code)
                .map(|result| Self::Initialize { sequence, result })
                .ok_or(DecodeError::UnknownCode)
        }
    }
}

/// Public, non-diagnostic category for a rejected control record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeError {
    /// The unauthenticated record carried a nonzero framing session ID.
    NonzeroSessionId,
    /// The unauthenticated record carried a nonzero authentication tag.
    NonzeroAuthenticationTag,
    /// The record kind is not accepted by the selected request/response API.
    UnknownKind,
    /// The payload was not the exact empty or one-byte shape for its API.
    WrongPayloadShape,
    /// The one-byte response code is not assigned by this protocol version.
    UnknownCode,
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

fn empty_record(kind: u8, sequence: u64) -> Record {
    Record::new(
        kind,
        ZERO_SESSION_ID,
        sequence,
        EMPTY_PAYLOAD_LENGTH,
        [0; PAYLOAD_CAPACITY],
        ZERO_AUTHENTICATION_TAG,
    )
}

fn one_byte_record(kind: u8, sequence: u64, value: u8) -> Record {
    let mut payload = [0; PAYLOAD_CAPACITY];
    payload[0] = value;
    Record::new(
        kind,
        ZERO_SESSION_ID,
        sequence,
        ONE_BYTE_PAYLOAD_LENGTH,
        payload,
        ZERO_AUTHENTICATION_TAG,
    )
}

#[cfg(test)]
mod tests;
