//! Fixed, transport-neutral outbound routing between one Reticulum node owner
//! and bounded per-interface actors.
//!
//! Node-core has already resolved `Only`, `All`, and `AllExcept` into one
//! selected [`PacketInterfaceId`] before a job reaches this crate. The router
//! therefore does not reinterpret Reticulum targets or perform parallel
//! fan-out. It validates that selected interface against an authoritative
//! fixed registry, stamps the current interface lease and configuration
//! snapshot, and moves the exact DATA or ordinary owner into that interface's
//! queue. Node-core remains the only component that may turn a completion into
//! the next serialized fan-out hop.
//!
//! This boundary deliberately knows nothing about LoRa, RNode, CAD, airtime,
//! frequencies, USB, BLE, Wi-Fi, or any concrete driver. An interface actor
//! receives only the queue capability for its fixed queue slot. Queue
//! pressure, offline interfaces, stale leases, crossed completions, and MTU
//! rejection all return the unchanged non-`Copy` owner to the caller.
//!
//! The same fixed fabric owns a transport-neutral native-packet ingress pool
//! for every actor. Concrete actors borrow mutable packet capacity only while
//! holding an [`AvailableIngressBuffer`], seal a validated packet length, and
//! submit that exact non-`Copy` owner with registry-issued provenance. The
//! router validates the observed queue and current lease before immutable
//! bytes reach node-core, then recycles the exact buffer to its original actor
//! queue.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub(crate) use core::{
    array, fmt,
    future::poll_fn,
    mem,
    task::{Context, Poll},
};

pub(crate) use embassy_sync::{
    blocking_mutex::raw::RawMutex,
    channel::{Channel, TrySendError},
};
pub(crate) use reticulum_node_core::{
    InterfaceSet, OrdinaryPreparedPacket, OrdinaryTxCompletion, OrdinaryTxJob, PACKET_CAPACITY,
    PacketInterfaceId, PreparedPacket, RoutedTxJob, TxCompletion,
};

mod types;
pub use types::*;
mod registry;
pub use registry::*;
mod ingress;
pub use ingress::*;
mod jobs;
pub use jobs::*;
mod fabric;
pub use fabric::*;
mod router;
pub use router::*;

#[cfg(test)]
mod tests;
