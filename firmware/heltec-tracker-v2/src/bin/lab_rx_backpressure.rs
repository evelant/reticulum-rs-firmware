//! Explicitly named hazardous Phase 1 backpressure lab image.

#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]
#![deny(
    clippy::mem_forget,
    reason = "forgetting esp-hal peripherals can leave hardware in an unsafe state"
)]
#![deny(clippy::large_stack_frames)]

#[cfg(not(feature = "lab-rx-backpressure"))]
compile_error!(
    "the backpressure lab binary requires --no-default-features --features lab-rx-backpressure"
);

#[cfg(feature = "lab-rx-electrical-hil")]
compile_error!("the backpressure and electrical HIL artifacts are mutually exclusive");

#[cfg(feature = "lab-rx-returned-fault-hil")]
compile_error!("the backpressure and returned-fault HIL artifacts are mutually exclusive");

include!("lab_rx.rs");
