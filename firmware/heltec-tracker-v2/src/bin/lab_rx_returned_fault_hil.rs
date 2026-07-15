//! Protected Phase 1 returned receive-fault image.

#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]
#![deny(
    clippy::mem_forget,
    reason = "forgetting esp-hal peripherals can leave hardware in an unsafe state"
)]
#![deny(clippy::large_stack_frames)]

#[cfg(not(feature = "lab-rx-returned-fault-hil"))]
compile_error!(
    "the returned-fault lab binary requires --no-default-features --features lab-rx-returned-fault-hil"
);

#[cfg(any(feature = "lab-rx-backpressure", feature = "lab-rx-electrical-hil"))]
compile_error!(
    "the returned-fault, backpressure and electrical HIL artifacts are mutually exclusive"
);

include!("lab_rx.rs");
