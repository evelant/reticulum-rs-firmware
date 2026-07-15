//! Explicitly armed RF-inert retained-journal corruption HIL image.

#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]
#![deny(
    clippy::mem_forget,
    reason = "forgetting esp-hal peripherals can leave hardware in an unsafe state"
)]
#![deny(clippy::large_stack_frames)]

#[cfg(not(feature = "lab-rx-reset-journal-corrupt-hil"))]
compile_error!(
    "the corruption artifact requires --no-default-features --features lab-rx-reset-journal-corrupt-hil"
);

include!("lab_rx_reset_journal_hil.rs");
