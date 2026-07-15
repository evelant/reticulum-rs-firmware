//! ESP32-S3 RTC-fast backing storage for the retained reset journal.
//!
//! The normal receive-only image serializes access around this storage. The
//! separately compiled reset-journal HIL images are single-threaded and use it
//! directly before constructing any executor timer, SPI device or watchdog.

use core::{
    ptr::{addr_of, addr_of_mut, read_volatile, write_volatile},
    sync::atomic::{Ordering, compiler_fence},
};

use reticulum_radio_interface::{RESET_QUARANTINE_JOURNAL_WORDS, ResetQuarantineStorage};

/// Two-slot reset journal in the ESP32-S3 RTC_FAST persistent section.
///
/// The pinned `esp-hal` startup zeros this section for `ChipPowerOn` (and an
/// unrecognized reset reason) and preserves it across `CoreSw`, MWDT and other
/// digital-core resets. The policy treats an unrecognized reason as retained
/// and therefore fails closed instead of assuming it was a true power cycle.
/// ESP32-S3 ROM reason `0x01` is unfortunately shared with brownout/super-WDT
/// cases and is exposed by the HAL as `ChipPowerOn`; that hardware/HAL
/// ambiguity remains a powered qualification limitation.
#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut RESET_QUARANTINE_JOURNAL: [u32; RESET_QUARANTINE_JOURNAL_WORDS] =
    [0; RESET_QUARANTINE_JOURNAL_WORDS];

pub(crate) struct RtcFastResetJournal;

impl RtcFastResetJournal {
    pub(crate) const fn new() -> Self {
        Self
    }
}

impl ResetQuarantineStorage for RtcFastResetJournal {
    fn read_word(&self, index: usize) -> u32 {
        assert!(index < RESET_QUARANTINE_JOURNAL_WORDS);
        // SAFETY: the bounds check above keeps the raw address within the
        // aligned u32 array. Callers serialize access; volatile access is
        // required because this memory survives digital-core resets.
        unsafe {
            let base = addr_of!(RESET_QUARANTINE_JOURNAL).cast::<u32>();
            read_volatile(base.add(index))
        }
    }

    fn write_word(&mut self, index: usize, value: u32) {
        assert!(index < RESET_QUARANTINE_JOURNAL_WORDS);
        // SAFETY: the bounds check above keeps the raw address within the
        // aligned u32 array, and callers serialize every writer.
        unsafe {
            let base = addr_of_mut!(RESET_QUARANTINE_JOURNAL).cast::<u32>();
            write_volatile(base.add(index), value);
        }
    }

    fn write_barrier(&mut self) {
        compiler_fence(Ordering::SeqCst);
        // SAFETY: `memw` has no operands and only waits for preceding Xtensa
        // memory writes to complete. This is the target half of the journal's
        // commit-last barrier; the compiler fence provides the compiler half.
        unsafe { core::arch::asm!("memw", options(nostack)) };
        compiler_fence(Ordering::SeqCst);
    }
}
