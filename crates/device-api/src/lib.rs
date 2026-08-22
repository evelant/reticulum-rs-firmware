//! Portable logical protocol for the local Reticulum device API.
//!
//! This crate defines bounded CBOR messages and common authorization policy.
//! It deliberately contains no transport framing, executor, radio, board, or
//! network-engine dependency. Application code supplies a trusted [`DispatchContext`]
//! separately from untrusted request bytes.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod cbor;
mod model;
mod ota;
mod reticulum_management;

pub use cbor::{
    DecodeError, EncodeError, MAX_CBOR_NESTING_DEPTH, RequiredField, decode_request,
    decode_response, encode_request, encode_response,
};
pub use model::*;
pub use ota::*;
pub use reticulum_management::*;
