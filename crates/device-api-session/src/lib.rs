//! Bounded authenticated sessions for local device-API byte-stream bearers.
//!
//! This crate implements the USB qualification handshake selected by ADR 0006
//! plus separately transcript-bound Wi-Fi and BLE GATT qualification suites. It
//! is allocation-free, `no_std`, transport-independent above a declared bearer
//! binding, and intentionally contains no credential persistence, pairing UI,
//! USB, BLE, or Wi-Fi driver, logical CBOR dispatch, flash owner, Reticulum
//! identity key, or radio capability.
//!
//! A successful server handshake derives one opaque [`AuthenticatedGrant`] for
//! each accepted request. The grant contains only credential identity and
//! generation facts plus session routing metadata. The node must revalidate
//! that generation against its device-owned credential authority immediately
//! before deriving a logical API dispatch context. Client-supplied principal or
//! permission data is never part of this protocol.
//!
//! A portable client mirror owns its credential and derived keys through
//! [`ClientHelloFlight`], [`AwaitingServerHello`], [`AwaitingServerProof`], and
//! [`ClientProofFlight`]. Once established, [`ClientSession`] permits exactly
//! one [`ClientRequestFlight`] and [`AwaitingResponse`] at a time. This makes
//! partial writes, cancellation, authentication faults, and sequence reuse
//! explicit without coupling the client to USB, BLE, Wi-Fi, an allocator, or
//! an async runtime.
//!
//! Established request/reply flow uses typestate. Accepting one authenticated
//! request consumes the idle session and returns [`AwaitingReply`]; no second
//! request can be admitted until the matching reply is completely acknowledged
//! through [`ReplyFlight`]. Dropping any handshake or reply flight destroys the
//! retained key material and never reuses a sequence number.
//! [`SessionEpochAllocator`] must likewise remain the sole boot-lifetime epoch
//! owner so delayed replies cannot alias a reconnect whose correlation starts
//! again at zero.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod client;
mod crypto;
mod protocol;
mod server;

pub use client::{
    AuthenticatedResponse, AwaitingResponse, AwaitingServerHello, AwaitingServerProof,
    ClientCredential, ClientHandshakeError, ClientHelloFlight, ClientParameters, ClientProofFlight,
    ClientRequestFault, ClientRequestFaultKind, ClientRequestFlight, ClientSession,
    ClientSessionFault,
};
pub use protocol::{
    BLE_GATT_QUALIFICATION_SUITE, BearerBinding, ClientHello, DeviceId, HandshakeRecordError,
    PROTOCOL_MAJOR, PROTOCOL_MINOR, QUALIFICATION_SUITE, RECORD_KIND_CLIENT_HELLO,
    RECORD_KIND_CLIENT_PROOF, RECORD_KIND_CLOSE, RECORD_KIND_REQUEST, RECORD_KIND_RESPONSE,
    RECORD_KIND_SERVER_HELLO, RECORD_KIND_SERVER_PROOF, SERVER_FLAG_DEVICE_API,
    SERVER_FLAG_INTEGRITY_ONLY, SERVER_FLAG_QUALIFICATION_ONLY, ServerHello, SessionId,
    SessionSuite, WIFI_QUALIFICATION_SUITE,
};
pub use reticulum_device_api_credentials::{CredentialGeneration, CredentialId};
pub use server::{
    ActiveCredential, AuthenticatedGrant, AuthenticatedRequest, AwaitingReply, HandshakeError,
    PendingClientProof, ReplyFlight, ReplyRouteFault, ReplyRouteFaultKind, ServerHelloFlight,
    ServerParameters, ServerProofFlight, ServerSession, SessionEpochAllocator,
    SessionEpochExhausted, SessionFault,
};

#[cfg(test)]
mod tests;
