//! Long-running USB appliance service for the first usable LXMF client.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod bindings;
mod onboarding;
mod profile;
mod runtime;
mod serial;
mod web;

pub use bindings::{MAX_JSON_SAFE_INTEGER, NoContent, render_api_bindings};
pub use onboarding::{
    OnboardingConfig, OnboardingError, OnboardingFault, OnboardingHandle, OnboardingSnapshot,
    OnboardingStage, OnboardingState, start_onboarding,
};
pub use profile::{DeviceProfile, ProfileRoot};
pub use runtime::{
    ApplianceConfig, ApplianceHandle, ApplianceSnapshot, ConnectionState, ServiceError,
    start_appliance,
};
pub use serial::{
    SerialConnectionGate, SerialConnectorConfig, discover_usb_serials, normalize_usb_serial,
    resolve_usb_port,
};
pub use web::{WebConfig, WebServer, serve_web, serve_web_with_onboarding};
