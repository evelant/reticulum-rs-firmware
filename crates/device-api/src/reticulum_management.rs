//! Stable Reticulum application names and request paths for appliance management.
//!
//! These constants are product protocol, not bearer configuration and not
//! PRNS engine policy. Firmware, native clients, and host clients share them so
//! a transport change cannot silently create a second management protocol.

/// Application name for the shared appliance management and OTA destination.
pub const MANAGEMENT_APPLICATION_NAME: &str = "reticulum";
/// Aspects for the shared appliance management and OTA destination.
pub const MANAGEMENT_ASPECTS: &[&str] = &["embedded-node"];
/// Read-only Device API path available before enrollment.
pub const MANAGEMENT_PUBLIC_PATH: &str = "/reticulum/embedded-node/public";
/// Privileged Device API path admitted by the Reticulum identity allow-list.
pub const MANAGEMENT_REQUEST_PATH: &str = "/reticulum/embedded-node/api";
/// Physical-presence enrollment path for an identified Link peer.
pub const MANAGEMENT_ENROLLMENT_PATH: &str = "/reticulum/embedded-node/enroll";
/// Canonical MessagePack `nil` used as the empty enrollment request.
pub const MANAGEMENT_ENROLLMENT_REQUEST_VALUE: [u8; 1] = [0xc0];
/// Canonical MessagePack `true` returned after durable authorization is live.
pub const MANAGEMENT_ENROLLMENT_SUCCESS_RESPONSE: [u8; 1] = [0xc3];
