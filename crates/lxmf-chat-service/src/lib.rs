//! Long-running USB appliance service for the first usable LXMF client.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod runtime;
mod serial;
mod web;

pub use runtime::{
    ApplianceConfig, ApplianceHandle, ApplianceSnapshot, ConnectionState, ServiceError,
    start_appliance,
};
pub use serial::{SerialConnectorConfig, normalize_usb_serial};
pub use web::{WebConfig, WebServer, serve_web};
