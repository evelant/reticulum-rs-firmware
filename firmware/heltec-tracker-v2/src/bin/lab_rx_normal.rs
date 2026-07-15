//! Explicitly configured Phase 1 receive-only lab image.

#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]
#![deny(
    clippy::mem_forget,
    reason = "forgetting esp-hal peripherals can leave hardware in an unsafe state"
)]
#![deny(clippy::large_stack_frames)]

#[cfg(feature = "lab-rx-backpressure")]
compile_error!(
    "the normal lab-rx binary cannot be built with lab-rx-backpressure; build the explicitly named reticulum-heltec-tracker-v2-lab-rx-backpressure binary"
);

#[cfg(feature = "lab-rx-electrical-hil")]
compile_error!(
    "the normal lab-rx binary cannot be built with lab-rx-electrical-hil; build the explicitly named reticulum-heltec-tracker-v2-lab-rx-electrical-hil binary"
);

#[cfg(feature = "lab-rx-returned-fault-hil")]
compile_error!(
    "the normal lab-rx binary cannot be built with lab-rx-returned-fault-hil; build the explicitly named reticulum-heltec-tracker-v2-lab-rx-returned-fault-hil binary"
);

include!("lab_rx.rs");
