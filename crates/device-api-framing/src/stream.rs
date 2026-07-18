//! Streaming synchronization, decoded ownership, and partial-TX cursor.

use zeroize::{Zeroize, Zeroizing};

use crate::{
    DECODED_RECORD_CAPACITY, Record, RecordDecodeError,
    cobs::{self, DecodeError as CobsDecodeError},
};

const MAX_COBS_BODY_LENGTH: usize = cobs::maximum_encoded_length(DECODED_RECORD_CAPACITY);
/// Maximum complete wire record, including leading and trailing zero delimiters.
pub const WIRE_RECORD_CAPACITY: usize = MAX_COBS_BODY_LENGTH + 2;

/// One streaming decoder observation.
///
/// The intentionally large success variant transfers the unique fixed record
/// owner without allocation or an aliasable decoder-side staging slot.
#[allow(clippy::large_enum_variant)]
#[must_use = "a completed record or framing fault must be handled explicitly"]
pub enum DecodeEvent {
    /// No complete record ended at this byte.
    Pending,
    /// One exact decoded record owner is ready for session validation.
    Record(Record),
    /// A delimited COBS body was malformed.
    MalformedCobs,
    /// A valid COBS body did not contain a canonical record.
    MalformedRecord(RecordDecodeError),
    /// Input exceeded the maximum encoded body and was discarded through the
    /// next zero delimiter.
    Overflow,
}

/// Incremental decoder for a zero-delimited byte stream.
///
/// Bytes before the first zero delimiter are ignored. A delimiter completing
/// one record also starts the synchronization window for the next record, so
/// adjacent records may share one zero byte.
pub struct StreamDecoder {
    synchronized: bool,
    overflowed: bool,
    encoded_length: usize,
    encoded: [u8; MAX_COBS_BODY_LENGTH],
    decoded: [u8; DECODED_RECORD_CAPACITY],
}

impl StreamDecoder {
    /// Construct a decoder that is seeking its first zero delimiter.
    pub const fn new() -> Self {
        Self {
            synchronized: false,
            overflowed: false,
            encoded_length: 0,
            encoded: [0; MAX_COBS_BODY_LENGTH],
            decoded: [0; DECODED_RECORD_CAPACITY],
        }
    }

    /// Consume one byte and report a completed record or recoverable fault.
    pub fn push(&mut self, byte: u8) -> DecodeEvent {
        if !self.synchronized {
            if byte == 0 {
                self.synchronized = true;
            }
            return DecodeEvent::Pending;
        }

        if byte != 0 {
            if self.overflowed {
                return DecodeEvent::Pending;
            }
            if self.encoded_length == self.encoded.len() {
                self.overflowed = true;
                return DecodeEvent::Pending;
            }
            self.encoded[self.encoded_length] = byte;
            self.encoded_length += 1;
            return DecodeEvent::Pending;
        }

        if self.overflowed {
            self.reset_record();
            return DecodeEvent::Overflow;
        }
        if self.encoded_length == 0 {
            return DecodeEvent::Pending;
        }

        let encoded_length = self.encoded_length;
        let event = match cobs::decode(&self.encoded[..encoded_length], &mut self.decoded) {
            Ok(decoded_length) => match Record::decode(&self.decoded[..decoded_length]) {
                Ok(record) => DecodeEvent::Record(record),
                Err(error) => DecodeEvent::MalformedRecord(error),
            },
            Err(
                CobsDecodeError::ZeroCode
                | CobsDecodeError::TruncatedBlock
                | CobsDecodeError::OutputTooSmall,
            ) => DecodeEvent::MalformedCobs,
        };
        self.reset_record();
        event
    }

    /// Discard a partial record and require a fresh leading delimiter.
    pub fn reset(&mut self) {
        self.zeroize();
    }

    /// Whether the decoder has observed a leading delimiter.
    pub const fn is_synchronized(&self) -> bool {
        self.synchronized
    }

    fn reset_record(&mut self) {
        self.overflowed = false;
        self.encoded_length = 0;
        self.encoded.zeroize();
        self.decoded.zeroize();
    }

    #[cfg(test)]
    pub(super) fn scratch_is_zeroed(&self) -> bool {
        self.encoded.iter().all(|byte| *byte == 0) && self.decoded.iter().all(|byte| *byte == 0)
    }
}

impl Zeroize for StreamDecoder {
    fn zeroize(&mut self) {
        self.synchronized = false;
        self.reset_record();
    }
}

impl Drop for StreamDecoder {
    fn drop(&mut self) {
        self.zeroize();
    }
}

impl Default for StreamDecoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Encoding a canonical record unexpectedly exceeded its compile-time owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameEncodeError;

/// A partial-write acknowledgement exceeded the remaining framed bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TxAdvanceError {
    acknowledged: usize,
    remaining: usize,
}

impl TxAdvanceError {
    /// Backend-reported byte count that was rejected.
    pub const fn acknowledged(self) -> usize {
        self.acknowledged
    }

    /// Bytes that actually remained before the rejected acknowledgement.
    pub const fn remaining(self) -> usize {
        self.remaining
    }
}

/// Exact encoded wire-record owner with an explicit partial-transmit cursor.
#[must_use = "a framed record must be completely written, retained, or explicitly dropped"]
pub struct FramedRecord {
    length: usize,
    offset: usize,
    bytes: [u8; WIRE_RECORD_CAPACITY],
}

impl FramedRecord {
    /// Encode one decoded record into a zero-prefixed and zero-suffixed COBS owner.
    pub fn encode(record: &Record) -> Result<Self, FrameEncodeError> {
        let mut decoded = Zeroizing::new([0_u8; DECODED_RECORD_CAPACITY]);
        let decoded_length = record.write_decoded(&mut decoded);
        let mut bytes = Zeroizing::new([0_u8; WIRE_RECORD_CAPACITY]);
        bytes[0] = 0;
        let encoded_length = cobs::encode(
            &decoded[..decoded_length],
            &mut bytes[1..WIRE_RECORD_CAPACITY - 1],
        )
        .map_err(|_| FrameEncodeError)?;
        let length = encoded_length + 2;
        bytes[length - 1] = 0;
        Ok(Self {
            length,
            offset: 0,
            bytes: *bytes,
        })
    }

    /// Complete encoded record bytes, independent of cursor position.
    pub fn encoded(&self) -> &[u8] {
        &self.bytes[..self.length]
    }

    /// Bytes that have not yet been acknowledged by the byte-I/O backend.
    pub fn remaining(&self) -> &[u8] {
        &self.bytes[self.offset..self.length]
    }

    /// At most `maximum` remaining bytes for one bounded backend write.
    pub fn next_chunk(&self, maximum: usize) -> &[u8] {
        let end = self.offset.saturating_add(maximum).min(self.length);
        &self.bytes[self.offset..end]
    }

    /// Advance only after the backend reports that exact count as written.
    pub fn advance(&mut self, acknowledged: usize) -> Result<(), TxAdvanceError> {
        let remaining = self.length - self.offset;
        if acknowledged > remaining {
            return Err(TxAdvanceError {
                acknowledged,
                remaining,
            });
        }
        self.offset += acknowledged;
        Ok(())
    }

    /// Whether the complete frame has been acknowledged.
    pub const fn is_complete(&self) -> bool {
        self.offset == self.length
    }

    /// Number of bytes already acknowledged.
    pub const fn acknowledged(&self) -> usize {
        self.offset
    }

    /// Total encoded wire length.
    pub const fn length(&self) -> usize {
        self.length
    }
}

impl Zeroize for FramedRecord {
    fn zeroize(&mut self) {
        self.bytes.zeroize();
    }
}

impl Drop for FramedRecord {
    fn drop(&mut self) {
        self.zeroize();
    }
}
