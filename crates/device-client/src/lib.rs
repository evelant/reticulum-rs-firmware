//! Reusable synchronous host client for the authenticated Reticulum device API.
//!
//! The crate accepts an already-open [`std::io::Read`] + [`std::io::Write`]
//! stream and does not select or configure a serial implementation. It likewise
//! validates the existing activated-credential image but deliberately leaves
//! filesystem ownership, anti-symlink checks, secure storage, and pairing UI to
//! the application.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod client;
mod credential;

pub use client::{
    BasicLxmfSend, ClientConfig, ClientError, ClientSessionProfile, ClientTransport, DeviceClient,
    FramingFault, LxmfMessage, Operation,
};
pub use credential::{ACTIVATED_CREDENTIAL_STATE_BYTES, ActivatedCredential, CredentialStateError};
