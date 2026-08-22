//! Reusable application services between persistent chat storage and one
//! authenticated Reticulum device session.
//!
//! This crate owns use-case ordering: commit-before-send, exact reconciliation,
//! status projection, and incremental inbox import. It does not select serial
//! ports, own reconnect timing, expose an HTTP API, or choose a user interface.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod engine;
mod session;

pub use engine::{ChatEngine, EngineError, InboxStep, ReconcileStep};
pub use session::{
    DeviceApiRequestSession, DeviceApiRequester, DeviceSessionError, InboxCursor, InboxSummary,
    LxmfSession,
};
