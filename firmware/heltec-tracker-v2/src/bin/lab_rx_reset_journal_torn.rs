//! Explicitly armed RF-inert retained-journal torn-write HIL image.

#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]
#![deny(
    clippy::mem_forget,
    reason = "forgetting esp-hal peripherals can leave hardware in an unsafe state"
)]
#![deny(clippy::large_stack_frames)]

#[cfg(not(feature = "lab-rx-reset-journal-torn-hil"))]
compile_error!(
    "the torn-write artifact requires --no-default-features --features lab-rx-reset-journal-torn-hil"
);

include!("lab_rx_reset_journal_hil.rs");
