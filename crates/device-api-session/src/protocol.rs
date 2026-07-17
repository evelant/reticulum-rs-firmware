//! Canonical qualification-handshake messages and record validation.

use reticulum_device_api_framing::{AUTH_TAG_LENGTH, PAYLOAD_CAPACITY, PayloadLength, Record};

/// Qualification-session protocol major version.
pub const PROTOCOL_MAJOR: u16 = 1;
/// Qualification-session protocol minor version.
pub const PROTOCOL_MINOR: u16 = 0;
/// HKDF-SHA256 plus HMAC-SHA256 authentication-only qualification suite.
pub const QUALIFICATION_SUITE: u16 = 1;

/// Client hello record kind.
pub const RECORD_KIND_CLIENT_HELLO: u8 = 0x01;
/// Server hello record kind.
pub const RECORD_KIND_SERVER_HELLO: u8 = 0x02;
/// Server proof record kind.
pub const RECORD_KIND_SERVER_PROOF: u8 = 0x03;
/// Client proof record kind.
pub const RECORD_KIND_CLIENT_PROOF: u8 = 0x04;
/// Authenticated logical request record kind.
pub const RECORD_KIND_REQUEST: u8 = 0x10;
/// Authenticated logical response record kind.
pub const RECORD_KIND_RESPONSE: u8 = 0x11;
/// Authenticated session-close record kind reserved for bearer integration.
pub const RECORD_KIND_CLOSE: u8 = 0x12;

/// Server-hello flag marking a lab qualification profile.
pub const SERVER_FLAG_QUALIFICATION_ONLY: u32 = 1 << 0;
/// Server-hello flag marking integrity/authentication without confidentiality.
pub const SERVER_FLAG_INTEGRITY_ONLY: u32 = 1 << 1;
/// Server-hello flag advertising the bounded local device API.
pub const SERVER_FLAG_DEVICE_API: u32 = 1 << 2;
pub(crate) const QUALIFICATION_SERVER_FLAGS: u32 =
    SERVER_FLAG_QUALIFICATION_ONLY | SERVER_FLAG_INTEGRITY_ONLY | SERVER_FLAG_DEVICE_API;

pub(crate) const CLIENT_HELLO_LENGTH: usize = 56;
pub(crate) const SERVER_HELLO_LENGTH: usize = 76;
pub(crate) const PROOF_LENGTH: usize = 32;

const ZERO_SESSION_ID: [u8; 16] = [0; 16];
const ZERO_TAG: [u8; AUTH_TAG_LENGTH] = [0; AUTH_TAG_LENGTH];

/// Physical local bearer cryptographically bound into a session transcript.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum BearerBinding {
    /// ESP32-S3 fixed USB Serial/JTAG byte stream.
    UsbSerialJtag = 1,
    /// Programmable USB OTG device profile.
    UsbOtg = 2,
    /// Bluetooth Low Energy GATT local API profile.
    BleGatt = 3,
    /// Wi-Fi local API profile.
    Wifi = 4,
}

impl BearerBinding {
    pub(crate) const fn from_wire(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::UsbSerialJtag),
            2 => Some(Self::UsbOtg),
            3 => Some(Self::BleGatt),
            4 => Some(Self::Wifi),
            _ => None,
        }
    }

    pub(crate) const fn wire(self) -> u8 {
        self as u8
    }
}

/// Opaque device-owned paired-client credential identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CredentialId([u8; 16]);

impl CredentialId {
    /// Construct an opaque 128-bit credential identifier.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Borrow the canonical credential identifier bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Non-repeating version of one device-owned credential authorization record.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CredentialGeneration(u64);

impl CredentialGeneration {
    /// Construct a credential generation.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Return the generation value.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Stable public identifier for the device API endpoint.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DeviceId([u8; 16]);

impl DeviceId {
    /// Construct a device API identifier.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Borrow the canonical device identifier bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Handshake-derived identifier for one authenticated session attempt.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SessionId(pub(crate) [u8; 16]);

impl SessionId {
    /// Borrow the derived session identifier bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Canonical client hello parsed from one pre-authentication record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientHello {
    bearer: BearerBinding,
    credential_id: CredentialId,
    nonce: [u8; 32],
}

impl ClientHello {
    /// Construct the only supported qualification-suite client hello.
    pub const fn new(bearer: BearerBinding, credential_id: CredentialId, nonce: [u8; 32]) -> Self {
        Self {
            bearer,
            credential_id,
            nonce,
        }
    }

    /// Parse and consume one canonical client-hello framing record.
    pub fn from_record(record: Record) -> Result<Self, HandshakeRecordError> {
        let payload = take_handshake_payload::<CLIENT_HELLO_LENGTH>(
            record,
            RECORD_KIND_CLIENT_HELLO,
            &ZERO_SESSION_ID,
        )?;
        let major = u16::from_le_bytes([payload[0], payload[1]]);
        let minor = u16::from_le_bytes([payload[2], payload[3]]);
        let suite = u16::from_le_bytes([payload[4], payload[5]]);
        if major != PROTOCOL_MAJOR || minor != PROTOCOL_MINOR {
            return Err(HandshakeRecordError::UnsupportedVersion { major, minor });
        }
        if suite != QUALIFICATION_SUITE {
            return Err(HandshakeRecordError::UnsupportedSuite { suite });
        }
        let bearer =
            BearerBinding::from_wire(payload[6]).ok_or(HandshakeRecordError::UnknownBearer {
                observed: payload[6],
            })?;
        if payload[7] != 0 {
            return Err(HandshakeRecordError::ReservedBitsSet);
        }
        let mut credential_id = [0_u8; 16];
        credential_id.copy_from_slice(&payload[8..24]);
        let mut nonce = [0_u8; 32];
        nonce.copy_from_slice(&payload[24..56]);
        Ok(Self {
            bearer,
            credential_id: CredentialId(credential_id),
            nonce,
        })
    }

    /// Encode this hello as one canonical zero-tagged pre-authentication record.
    pub fn into_record(self) -> Record {
        let payload = self.encode();
        handshake_record(RECORD_KIND_CLIENT_HELLO, ZERO_SESSION_ID, &payload)
    }

    /// Bearer the client expects this handshake to authenticate.
    pub const fn bearer(&self) -> BearerBinding {
        self.bearer
    }

    /// Credential identifier selected for device-side lookup.
    pub const fn credential_id(&self) -> CredentialId {
        self.credential_id
    }

    /// Fresh client nonce.
    pub const fn nonce(&self) -> &[u8; 32] {
        &self.nonce
    }

    pub(crate) fn encode(&self) -> [u8; CLIENT_HELLO_LENGTH] {
        let mut output = [0_u8; CLIENT_HELLO_LENGTH];
        output[0..2].copy_from_slice(&PROTOCOL_MAJOR.to_le_bytes());
        output[2..4].copy_from_slice(&PROTOCOL_MINOR.to_le_bytes());
        output[4..6].copy_from_slice(&QUALIFICATION_SUITE.to_le_bytes());
        output[6] = self.bearer.wire();
        output[7] = 0;
        output[8..24].copy_from_slice(self.credential_id.as_bytes());
        output[24..56].copy_from_slice(&self.nonce);
        output
    }
}

/// Canonical server hello bound into the qualification transcript.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServerHello {
    bearer: BearerBinding,
    device_id: DeviceId,
    nonce: [u8; 32],
    credential_generation: CredentialGeneration,
    flags: u32,
}

impl ServerHello {
    pub(crate) const fn new(
        bearer: BearerBinding,
        device_id: DeviceId,
        nonce: [u8; 32],
        credential_generation: CredentialGeneration,
    ) -> Self {
        Self {
            bearer,
            device_id,
            nonce,
            credential_generation,
            flags: QUALIFICATION_SERVER_FLAGS,
        }
    }

    /// Parse and consume one canonical server-hello record.
    pub fn from_record(record: Record) -> Result<Self, HandshakeRecordError> {
        let payload = take_handshake_payload::<SERVER_HELLO_LENGTH>(
            record,
            RECORD_KIND_SERVER_HELLO,
            &ZERO_SESSION_ID,
        )?;
        let major = u16::from_le_bytes([payload[0], payload[1]]);
        let minor = u16::from_le_bytes([payload[2], payload[3]]);
        let suite = u16::from_le_bytes([payload[4], payload[5]]);
        if major != PROTOCOL_MAJOR || minor != PROTOCOL_MINOR {
            return Err(HandshakeRecordError::UnsupportedVersion { major, minor });
        }
        if suite != QUALIFICATION_SUITE {
            return Err(HandshakeRecordError::UnsupportedSuite { suite });
        }
        let bearer =
            BearerBinding::from_wire(payload[6]).ok_or(HandshakeRecordError::UnknownBearer {
                observed: payload[6],
            })?;
        if payload[7] != 0 || payload[69..72] != [0, 0, 0] {
            return Err(HandshakeRecordError::ReservedBitsSet);
        }
        let mut device_id = [0_u8; 16];
        device_id.copy_from_slice(&payload[8..24]);
        let mut nonce = [0_u8; 32];
        nonce.copy_from_slice(&payload[24..56]);
        let credential_generation =
            CredentialGeneration(u64::from_le_bytes(payload[56..64].try_into().map_err(
                |_| HandshakeRecordError::WrongPayloadLength {
                    expected: SERVER_HELLO_LENGTH,
                    observed: payload.len(),
                },
            )?));
        let max_record_payload = u16::from_le_bytes([payload[64], payload[65]]);
        let max_message = u16::from_le_bytes([payload[66], payload[67]]);
        let max_in_flight = payload[68];
        if usize::from(max_record_payload) != PAYLOAD_CAPACITY
            || usize::from(max_message) != reticulum_device_api::MAX_MESSAGE_BYTES
            || max_in_flight != 1
        {
            return Err(HandshakeRecordError::UnsupportedLimits {
                max_record_payload,
                max_message,
                max_in_flight,
            });
        }
        let flags = u32::from_le_bytes(payload[72..76].try_into().map_err(|_| {
            HandshakeRecordError::WrongPayloadLength {
                expected: SERVER_HELLO_LENGTH,
                observed: payload.len(),
            }
        })?);
        if flags != QUALIFICATION_SERVER_FLAGS {
            return Err(HandshakeRecordError::UnsupportedFlags { observed: flags });
        }
        Ok(Self {
            bearer,
            device_id: DeviceId(device_id),
            nonce,
            credential_generation,
            flags,
        })
    }

    /// Actual bearer selected by the server.
    pub const fn bearer(&self) -> BearerBinding {
        self.bearer
    }

    /// Stable device API identifier.
    pub const fn device_id(&self) -> DeviceId {
        self.device_id
    }

    /// Fresh device nonce.
    pub const fn nonce(&self) -> &[u8; 32] {
        &self.nonce
    }

    /// Device-owned credential generation authenticated by this handshake.
    pub const fn credential_generation(&self) -> CredentialGeneration {
        self.credential_generation
    }

    /// Exact qualification-profile capability flags.
    pub const fn flags(&self) -> u32 {
        self.flags
    }

    pub(crate) fn into_record(self) -> Record {
        let payload = self.encode();
        handshake_record(RECORD_KIND_SERVER_HELLO, ZERO_SESSION_ID, &payload)
    }

    pub(crate) fn encode(&self) -> [u8; SERVER_HELLO_LENGTH] {
        let mut output = [0_u8; SERVER_HELLO_LENGTH];
        output[0..2].copy_from_slice(&PROTOCOL_MAJOR.to_le_bytes());
        output[2..4].copy_from_slice(&PROTOCOL_MINOR.to_le_bytes());
        output[4..6].copy_from_slice(&QUALIFICATION_SUITE.to_le_bytes());
        output[6] = self.bearer.wire();
        output[7] = 0;
        output[8..24].copy_from_slice(self.device_id.as_bytes());
        output[24..56].copy_from_slice(&self.nonce);
        output[56..64].copy_from_slice(&self.credential_generation.get().to_le_bytes());
        output[64..66].copy_from_slice(&(PAYLOAD_CAPACITY as u16).to_le_bytes());
        output[66..68]
            .copy_from_slice(&(reticulum_device_api::MAX_MESSAGE_BYTES as u16).to_le_bytes());
        output[68] = 1;
        output[69..72].fill(0);
        output[72..76].copy_from_slice(&self.flags.to_le_bytes());
        output
    }
}

/// Canonical handshake-record validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HandshakeRecordError {
    /// Record kind was not the required handshake message.
    UnexpectedKind {
        /// Required record kind.
        expected: u8,
        /// Observed record kind.
        observed: u8,
    },
    /// Handshake record carried an unexpected session identifier.
    WrongSessionId,
    /// Handshake record sequence was not zero.
    NonzeroSequence,
    /// Handshake record used the framing tag slot instead of the proof payload.
    NonzeroTag,
    /// Handshake payload length was not canonical.
    WrongPayloadLength {
        /// Required fixed payload length.
        expected: usize,
        /// Observed payload length.
        observed: usize,
    },
    /// Protocol major/minor pair is unsupported.
    UnsupportedVersion {
        /// Observed major version.
        major: u16,
        /// Observed minor version.
        minor: u16,
    },
    /// Requested cryptographic suite is unsupported.
    UnsupportedSuite {
        /// Observed suite identifier.
        suite: u16,
    },
    /// Bearer binding code is unknown.
    UnknownBearer {
        /// Observed numeric bearer code.
        observed: u8,
    },
    /// A reserved byte was nonzero.
    ReservedBitsSet,
    /// Server limits do not match this fixed qualification profile.
    UnsupportedLimits {
        /// Observed framing payload maximum.
        max_record_payload: u16,
        /// Observed logical message maximum.
        max_message: u16,
        /// Observed outstanding-request maximum.
        max_in_flight: u8,
    },
    /// Server capability flags do not match this fixed profile.
    UnsupportedFlags {
        /// Observed server flags.
        observed: u32,
    },
}

pub(crate) fn proof_record(kind: u8, session_id: SessionId, proof: &[u8; 32]) -> Record {
    handshake_record(kind, session_id.0, proof)
}

pub(crate) fn take_proof(
    record: Record,
    kind: u8,
    session_id: SessionId,
) -> Result<[u8; 32], HandshakeRecordError> {
    take_handshake_payload::<PROOF_LENGTH>(record, kind, session_id.as_bytes())
}

fn handshake_record(kind: u8, session_id: [u8; 16], payload: &[u8]) -> Record {
    let mut owner = [0_u8; PAYLOAD_CAPACITY];
    owner[..payload.len()].copy_from_slice(payload);
    Record::new(
        kind,
        session_id,
        0,
        PayloadLength::new(payload.len()).expect("fixed handshake payload fits framing record"),
        owner,
        ZERO_TAG,
    )
}

fn take_handshake_payload<const N: usize>(
    record: Record,
    expected_kind: u8,
    expected_session_id: &[u8; 16],
) -> Result<[u8; N], HandshakeRecordError> {
    if record.kind() != expected_kind {
        return Err(HandshakeRecordError::UnexpectedKind {
            expected: expected_kind,
            observed: record.kind(),
        });
    }
    if record.session_id() != expected_session_id {
        return Err(HandshakeRecordError::WrongSessionId);
    }
    if record.sequence() != 0 {
        return Err(HandshakeRecordError::NonzeroSequence);
    }
    if record.authentication_tag() != &ZERO_TAG {
        return Err(HandshakeRecordError::NonzeroTag);
    }
    if record.payload().len() != N {
        return Err(HandshakeRecordError::WrongPayloadLength {
            expected: N,
            observed: record.payload().len(),
        });
    }
    let mut output = [0_u8; N];
    output.copy_from_slice(record.payload());
    Ok(output)
}
