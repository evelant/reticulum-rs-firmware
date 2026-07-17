//! Allocation-free ownership handoff for the local device API.
//!
//! One boot-lifetime bearer manager and one node/storage owner exchange exact
//! request and reply values through independent depth-one channels. A bearer
//! manager retains its role across any number of connection/session epochs;
//! dropping that role is a fatal runtime teardown, not ordinary disconnect
//! handling. The handoff is deliberately transport-neutral: it knows nothing
//! about USB, BLE, Wi-Fi, framing, storage, Reticulum, or radios.
//!
//! The request's `G` parameter is the session layer's opaque authenticated
//! grant. A session implementation can therefore keep the grant's principal
//! and permission fields private and make the grant non-cloneable. This crate
//! neither constructs nor interprets it; it transfers the exact grant to the
//! node owner with the encoded request.
//!
//! Awaiting capacity never moves an owner into a future. The caller retains
//! the exact request or reply across cancellation, then uses `try_send`
//! immediately after readiness. Receive futures are also cancellation-safe:
//! a pending receive leaves the queued owner in its channel. Once a request is
//! enqueued, connection loss or a session-epoch change cannot cancel the node
//! mutation. The persistent bearer manager may discard a stale reply by
//! comparing its epoch and correlation while an idempotent retry becomes a new
//! request owner.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod handoff;
mod model;

pub use handoff::{
    BearerHandoff, DeviceApiHandoff, NodeHandoff, PairedHandoff, ReplyReceiver, ReplySender,
    RequestReceiver, RequestSender, SendPressure,
};
pub use model::{
    CorrelationId, LocalApiReply, LocalApiRequest, MESSAGE_CAPACITY, MessageLength, MessageTooLong,
    OwnedMessage, RequestKey, SessionEpoch,
};

#[cfg(test)]
mod tests;
