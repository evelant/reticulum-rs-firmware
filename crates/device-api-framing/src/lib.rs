//! Allocation-free records for local device-API byte-stream bearers.
//!
//! The wire form is a zero-delimited COBS record. Its decoded body contains a
//! fixed magic, version, record kind, reserved field, session ID, sequence,
//! payload length, payload, and a fixed 16-byte authentication-tag slot. This
//! crate owns only byte framing. A session layer decides which record kinds are
//! legal, constructs and verifies tags, enforces sequence policy, and mints an
//! authenticated application grant.
//!
//! The streaming decoder starts only after a zero byte and returns to that
//! synchronization point after malformed or overlong input. The transmit
//! owner exposes an explicit cursor: callers advance it only after a byte-I/O
//! backend reports a completed partial write. A backend write future whose
//! cancellation semantics are unknown must still be driven to completion.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod cobs;
mod record;
mod stream;

pub use record::{
    AUTH_TAG_LENGTH, AUTHENTICATED_DATA_CAPACITY, DECODED_RECORD_CAPACITY, HEADER_LENGTH, MAGIC,
    PAYLOAD_CAPACITY, PROTOCOL_VERSION, PayloadLength, PayloadTooLong, Record, RecordDecodeError,
    SESSION_ID_LENGTH,
};
pub use stream::{
    DecodeEvent, FrameEncodeError, FramedRecord, StreamDecoder, TxAdvanceError,
    WIRE_RECORD_CAPACITY,
};

#[cfg(test)]
mod tests;
