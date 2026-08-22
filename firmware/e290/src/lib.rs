//! Host-checkable product policy for the PRNS-based E290 appliance firmware.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod appliance_settings;
pub mod board;
pub mod display_coordinator;
pub mod display_handoff;
#[cfg(feature = "display")]
pub mod display_render;
#[cfg(feature = "gateway")]
pub mod dns_wire;
pub mod lxmf_delivery;
pub mod management_authorization;
pub mod ota;
pub mod partition_contract;
pub mod prns_applications;
pub mod prns_events;
pub mod prns_lora;
pub mod prns_node;
pub mod prns_peer_discovery;
pub mod prns_persistence;
pub mod prns_requests;
pub mod prns_storage;
pub mod product_config;
pub mod product_identity;
pub mod product_outbox;
pub mod wifi_driver_metrics;
#[cfg(feature = "gateway")]
pub mod wifi_station_profile;
#[cfg(feature = "gateway")]
pub mod wifi_tcp_profile;

#[cfg(test)]
extern crate std;
