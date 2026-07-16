//! Portable logical protocol for the local Reticulum device API.
//!
//! This crate defines bounded CBOR messages and common authorization policy.
//! It deliberately contains no transport framing, executor, radio, board, or
//! Rete dependency. Session code supplies a trusted [`DispatchContext`]
//! separately from untrusted request bytes.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

#[cfg(all(feature = "host-sim", target_os = "none"))]
compile_error!("the experimental `host-sim` device API is unavailable on target_os=none");

mod cbor;
mod model;

pub use cbor::{
    DecodeError, EncodeError, MAX_CBOR_NESTING_DEPTH, RequiredField, decode_request,
    decode_response, encode_request, encode_response,
};
pub use model::*;
