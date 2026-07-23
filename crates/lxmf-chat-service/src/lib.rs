//! Long-running local-bearer appliance service for the first usable LXMF client.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod bindings;
mod ble;
mod onboarding;
mod profile;
mod serial;
mod web;

pub use bindings::{NoContent, render_api_bindings};
pub use ble::{BleConnector, BleConnectorConfig};
pub use onboarding::{
    OnboardingConfig, OnboardingError, OnboardingFault, OnboardingHandle, OnboardingSnapshot,
    OnboardingStage, OnboardingState, start_onboarding,
};
pub use profile::{DeviceProfile, ProfileRoot};
pub use reticulum_lxmf_chat_runtime::{
    ApplianceConfig, ApplianceHandle, ApplianceSnapshot, BytesEncoding, BytesView,
    ConnectedSession, ConnectionMetadata, ConnectionState, ConnectionTransport, Connector,
    ContactView, DeviceView, MAX_JSON_SAFE_INTEGER, ServiceError, TimelineDirection,
    TimelineStatus, TimelineView, start_appliance,
};
pub use serial::{
    SerialConnectionGate, SerialConnector, SerialConnectorConfig, discover_usb_serials,
    normalize_usb_serial, resolve_usb_port, usb_serial_eui48,
};
pub use web::{WebConfig, WebServer, serve_web, serve_web_with_onboarding};
