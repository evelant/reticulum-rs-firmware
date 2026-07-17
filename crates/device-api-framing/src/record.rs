//! Decoded record owner and exact wire-body validation.

/// Maximum opaque session or application payload in one record.
pub const PAYLOAD_CAPACITY: usize = 512;
/// 128-bit authentication-tag slot carried by every decoded record.
pub const AUTH_TAG_LENGTH: usize = 16;
/// Session identifier derived by the authenticated session protocol.
pub const SESSION_ID_LENGTH: usize = 16;
/// Fixed record magic.
pub const MAGIC: [u8; 4] = *b"RDA1";
/// Current record framing version.
pub const PROTOCOL_VERSION: u8 = 1;
/// Fixed decoded header length before the payload.
pub const HEADER_LENGTH: usize = 34;
/// Maximum canonical header and payload bytes authenticated by the session.
pub const AUTHENTICATED_DATA_CAPACITY: usize = HEADER_LENGTH + PAYLOAD_CAPACITY;
/// Maximum decoded header, payload, and authentication-tag length.
pub const DECODED_RECORD_CAPACITY: usize = HEADER_LENGTH + PAYLOAD_CAPACITY + AUTH_TAG_LENGTH;

const MAGIC_RANGE: core::ops::Range<usize> = 0..4;
const VERSION_INDEX: usize = 4;
const KIND_INDEX: usize = 5;
const RESERVED_RANGE: core::ops::Range<usize> = 6..8;
const SESSION_ID_RANGE: core::ops::Range<usize> = 8..24;
const SEQUENCE_RANGE: core::ops::Range<usize> = 24..32;
const LENGTH_RANGE: core::ops::Range<usize> = 32..34;

/// Valid payload length bounded by [`PAYLOAD_CAPACITY`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PayloadLength(u16);

impl PayloadLength {
    /// Validate a payload length.
    pub const fn new(length: usize) -> Result<Self, PayloadTooLong> {
        if length <= PAYLOAD_CAPACITY {
            Ok(Self(length as u16))
        } else {
            Err(PayloadTooLong { length })
        }
    }

    /// Return the validated length as `usize`.
    pub const fn get(self) -> usize {
        self.0 as usize
    }
}

/// A payload length exceeded [`PAYLOAD_CAPACITY`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PayloadTooLong {
    length: usize,
}

impl PayloadTooLong {
    /// Rejected length.
    pub const fn length(self) -> usize {
        self.length
    }
}

/// One decoded local device-API record.
///
/// The kind remains opaque to framing so a separate session layer can own
/// handshake, control, request, response, and future extension semantics.
#[must_use = "a decoded record must be authenticated, transferred, or explicitly dropped"]
pub struct Record {
    kind: u8,
    session_id: [u8; SESSION_ID_LENGTH],
    sequence: u64,
    payload_length: PayloadLength,
    payload: [u8; PAYLOAD_CAPACITY],
    authentication_tag: [u8; AUTH_TAG_LENGTH],
}

impl Record {
    /// Construct a record from exact fixed-buffer owners.
    pub const fn new(
        kind: u8,
        session_id: [u8; SESSION_ID_LENGTH],
        sequence: u64,
        payload_length: PayloadLength,
        payload: [u8; PAYLOAD_CAPACITY],
        authentication_tag: [u8; AUTH_TAG_LENGTH],
    ) -> Self {
        Self {
            kind,
            session_id,
            sequence,
            payload_length,
            payload,
            authentication_tag,
        }
    }

    /// Session-layer record kind.
    pub const fn kind(&self) -> u8 {
        self.kind
    }

    /// Session identifier derived from the authenticated handshake.
    pub const fn session_id(&self) -> &[u8; SESSION_ID_LENGTH] {
        &self.session_id
    }

    /// Direction-local record sequence selected by the session layer.
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Valid opaque payload length.
    pub const fn payload_length(&self) -> PayloadLength {
        self.payload_length
    }

    /// Valid opaque payload bytes.
    pub fn payload(&self) -> &[u8] {
        &self.payload[..self.payload_length.get()]
    }

    /// Complete payload buffer, including bytes beyond the valid payload.
    pub const fn full_payload(&self) -> &[u8; PAYLOAD_CAPACITY] {
        &self.payload
    }

    /// Fixed authentication-tag bytes interpreted by the session layer.
    pub const fn authentication_tag(&self) -> &[u8; AUTH_TAG_LENGTH] {
        &self.authentication_tag
    }

    /// Consume the record into exact field and buffer owners.
    pub fn into_parts(
        self,
    ) -> (
        u8,
        [u8; SESSION_ID_LENGTH],
        u64,
        PayloadLength,
        [u8; PAYLOAD_CAPACITY],
        [u8; AUTH_TAG_LENGTH],
    ) {
        (
            self.kind,
            self.session_id,
            self.sequence,
            self.payload_length,
            self.payload,
            self.authentication_tag,
        )
    }

    /// Write the exact canonical bytes the session must authenticate.
    ///
    /// The session additionally domain-separates the bearer protocol and
    /// direction (or uses independent directional keys). The authentication
    /// tag itself is deliberately excluded.
    pub fn write_authenticated_data(
        &self,
        output: &mut [u8; AUTHENTICATED_DATA_CAPACITY],
    ) -> usize {
        self.write_authenticated_data_into(output)
    }

    fn write_authenticated_data_into(&self, output: &mut [u8]) -> usize {
        output[MAGIC_RANGE].copy_from_slice(&MAGIC);
        output[VERSION_INDEX] = PROTOCOL_VERSION;
        output[KIND_INDEX] = self.kind;
        output[RESERVED_RANGE].fill(0);
        output[SESSION_ID_RANGE].copy_from_slice(&self.session_id);
        output[SEQUENCE_RANGE].copy_from_slice(&self.sequence.to_le_bytes());
        output[LENGTH_RANGE].copy_from_slice(&(self.payload_length.0).to_le_bytes());

        let payload_end = HEADER_LENGTH + self.payload_length.get();
        output[HEADER_LENGTH..payload_end].copy_from_slice(self.payload());
        payload_end
    }

    pub(crate) fn write_decoded(&self, output: &mut [u8; DECODED_RECORD_CAPACITY]) -> usize {
        let payload_end = self.write_authenticated_data_into(output);
        let record_end = payload_end + AUTH_TAG_LENGTH;
        output[payload_end..record_end].copy_from_slice(&self.authentication_tag);
        record_end
    }

    pub(crate) fn decode(input: &[u8]) -> Result<Self, RecordDecodeError> {
        if input.len() < HEADER_LENGTH + AUTH_TAG_LENGTH {
            return Err(RecordDecodeError::TooShort);
        }
        if input[MAGIC_RANGE] != MAGIC {
            return Err(RecordDecodeError::BadMagic);
        }
        if input[VERSION_INDEX] != PROTOCOL_VERSION {
            return Err(RecordDecodeError::UnsupportedVersion {
                observed: input[VERSION_INDEX],
            });
        }
        if input[RESERVED_RANGE] != [0, 0] {
            return Err(RecordDecodeError::ReservedBitsSet);
        }

        let payload_length = usize::from(u16::from_le_bytes([
            input[LENGTH_RANGE.start],
            input[LENGTH_RANGE.start + 1],
        ]));
        let payload_length =
            PayloadLength::new(payload_length).map_err(|_| RecordDecodeError::PayloadTooLong)?;
        let expected = HEADER_LENGTH + payload_length.get() + AUTH_TAG_LENGTH;
        if input.len() != expected {
            return Err(RecordDecodeError::LengthMismatch {
                declared: payload_length.get(),
                decoded: input.len(),
            });
        }

        let mut payload = [0_u8; PAYLOAD_CAPACITY];
        let payload_end = HEADER_LENGTH + payload_length.get();
        payload[..payload_length.get()].copy_from_slice(&input[HEADER_LENGTH..payload_end]);
        let mut authentication_tag = [0_u8; AUTH_TAG_LENGTH];
        authentication_tag.copy_from_slice(&input[payload_end..expected]);

        Ok(Self {
            kind: input[KIND_INDEX],
            session_id: input[SESSION_ID_RANGE]
                .try_into()
                .map_err(|_| RecordDecodeError::TooShort)?,
            sequence: u64::from_le_bytes(
                input[SEQUENCE_RANGE]
                    .try_into()
                    .map_err(|_| RecordDecodeError::TooShort)?,
            ),
            payload_length,
            payload,
            authentication_tag,
        })
    }
}

/// A COBS body decoded but did not contain one canonical record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordDecodeError {
    /// Body cannot contain the fixed header and authentication tag.
    TooShort,
    /// Fixed record magic did not match.
    BadMagic,
    /// Version is not supported by this framing implementation.
    UnsupportedVersion {
        /// Rejected version byte.
        observed: u8,
    },
    /// Reserved header bits were nonzero.
    ReservedBitsSet,
    /// Declared payload exceeded the fixed payload owner.
    PayloadTooLong,
    /// Decoded body length did not equal header plus declared payload plus tag.
    LengthMismatch {
        /// Declared payload length.
        declared: usize,
        /// Actual complete decoded-body length.
        decoded: usize,
    },
}
