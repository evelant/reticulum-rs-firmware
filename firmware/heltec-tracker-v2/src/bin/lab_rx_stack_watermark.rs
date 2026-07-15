//! Lab-only CPU0/main-executor stack watermark for the ESP32-S3.
//!
//! The reset hook is intentionally implemented as Xtensa leaf assembly. A
//! normal Rust function may create a stack frame before it can observe `sp`,
//! which would make painting the startup stack unsound. All later calculation
//! is delegated to the host-tested portable implementation.

use core::{
    arch::{asm, global_asm},
    cmp::min,
    ptr::{addr_of, read_volatile},
};

use reticulum_radio_interface::{
    STACK_WATERMARK_PATTERN_SEED, STACK_WATERMARK_WORD_BYTES, StackWatermarkLayout,
    StackWatermarkLayoutError, scan_stack_watermark,
};

#[cfg(not(target_arch = "xtensa"))]
compile_error!("the lab stack watermark reset hook is implemented only for Xtensa");

const STARTUP_MARKER_MAGIC: u32 = 0x5253_574d; // "RSWM"
const STARTUP_MARKER_MAGIC_INVERSE: u32 = !STARTUP_MARKER_MAGIC;

/// Written before normal BSS clearing, then validated before the radio is
/// activated. This section is `NOLOAD` and excluded from `_bss_start.._bss_end`
/// by the pinned esp-hal linker scripts.
#[repr(C)]
#[derive(Clone, Copy)]
struct StartupMarker {
    magic: u32,
    magic_inverse: u32,
    stack_bottom: u32,
    stack_top: u32,
    paint_top: u32,
    stack_guard: u32,
    stack_guard_value: u32,
    pattern_seed: u32,
}

// Assembly below writes this C layout directly. Keep every offset and the
// total size compile-time checked so a Rust refactor cannot silently change
// the reset ABI.
const _: () = {
    assert!(core::mem::align_of::<StartupMarker>() == 4);
    assert!(core::mem::size_of::<StartupMarker>() == 32);
    assert!(core::mem::offset_of!(StartupMarker, magic) == 0);
    assert!(core::mem::offset_of!(StartupMarker, magic_inverse) == 4);
    assert!(core::mem::offset_of!(StartupMarker, stack_bottom) == 8);
    assert!(core::mem::offset_of!(StartupMarker, stack_top) == 12);
    assert!(core::mem::offset_of!(StartupMarker, paint_top) == 16);
    assert!(core::mem::offset_of!(StartupMarker, stack_guard) == 20);
    assert!(core::mem::offset_of!(StartupMarker, stack_guard_value) == 24);
    assert!(core::mem::offset_of!(StartupMarker, pattern_seed) == 28);
    assert!(STACK_WATERMARK_WORD_BYTES == 4);
};

#[used]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".noinit.reticulum_stack_watermark")]
static mut RETICULUM_STACK_WATERMARK_MARKER: StartupMarker = StartupMarker {
    magic: 0,
    magic_inverse: 0,
    stack_bottom: 0,
    stack_top: 0,
    paint_top: 0,
    stack_guard: 0,
    stack_guard_value: 0,
    pattern_seed: 0,
};

unsafe extern "C" {
    static _stack_end_cpu0: u32;
    static _stack_start_cpu0: u32;
    static __stack_chk_guard: u32;
}

// xtensa-lx-rt 0.22 calls `__zero_bss` through `callx4` after setting `sp` to
// `_stack_start_cpu0`. The boolean return is placed in callee a2 (caller a6).
// `entry a1, 0` creates no frame and the function performs no calls, so every
// store is strictly below the current stack pointer. The runtime-owned guard
// word is skipped exactly and its pre-paint value is retained in `.noinit`.
global_asm!(
    r#"
    .section .rwtext,"ax",@progbits
    .literal .Lrswm_stack_bottom, {stack_bottom}
    .literal .Lrswm_stack_top, {stack_top}
    .literal .Lrswm_stack_guard, {stack_guard}
    .literal .Lrswm_marker, {marker}
    .literal .Lrswm_seed, {pattern_seed}
    .literal .Lrswm_magic, {marker_magic}
    .literal .Lrswm_magic_inverse, {marker_magic_inverse}

    .global __zero_bss
    .p2align 2
    .type __zero_bss,@function
__zero_bss:
    entry   a1, 0
    l32r    a2, .Lrswm_stack_bottom
    l32r    a3, .Lrswm_stack_guard
    mov     a4, sp
    l32r    a5, .Lrswm_seed

.Lrswm_paint_loop:
    bgeu    a2, a4, .Lrswm_paint_done
    beq     a2, a3, .Lrswm_skip_guard
    xor     a6, a2, a5
    s32i.n  a6, a2, 0
.Lrswm_skip_guard:
    addi.n  a2, a2, 4
    j       .Lrswm_paint_loop

.Lrswm_paint_done:
    l32r    a2, .Lrswm_marker
    l32r    a5, .Lrswm_magic
    s32i.n  a5, a2, 0
    l32r    a5, .Lrswm_magic_inverse
    s32i.n  a5, a2, 4
    l32r    a5, .Lrswm_stack_bottom
    s32i.n  a5, a2, 8
    l32r    a5, .Lrswm_stack_top
    s32i.n  a5, a2, 12
    s32i.n  a4, a2, 16
    s32i.n  a3, a2, 20
    l32i.n  a6, a3, 0
    s32i.n  a6, a2, 24
    l32r    a5, .Lrswm_seed
    s32i.n  a5, a2, 28
    memw

    movi.n  a2, 1
    retw.n
    .size __zero_bss, .-__zero_bss

    // `global_asm!` fragments are concatenated during fat LTO. Restore the
    // assembler's ordinary text section so a later dependency fragment cannot
    // accidentally place its literal pool in our RAM-text literal section.
    .section .text,"ax",@progbits
    "#,
    stack_bottom = sym _stack_end_cpu0,
    stack_top = sym _stack_start_cpu0,
    stack_guard = sym __stack_chk_guard,
    marker = sym RETICULUM_STACK_WATERMARK_MARKER,
    pattern_seed = const STACK_WATERMARK_PATTERN_SEED,
    marker_magic = const STARTUP_MARKER_MAGIC,
    marker_magic_inverse = const STARTUP_MARKER_MAGIC_INVERSE,
);

/// Failure to prove that the reset hook painted the exact linked stack.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StackWatermarkInitError {
    MarkerMagic,
    LinkerAddressDoesNotFit,
    LinkerAddressMismatch,
    PatternMismatch,
    PaintLimitOutsideStack,
    GuardWasNotPreserved,
    Layout(StackWatermarkLayoutError),
}

/// Monotonic runtime evidence emitted with each lab heartbeat.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StackWatermarkMetrics {
    pub stack_reserved_bytes: u32,
    pub stack_usable_above_guard_bytes: u32,
    pub startup_painted_bytes: u32,
    pub high_water_used_bytes: u32,
    pub minimum_remaining_above_guard_bytes: u32,
    pub stack_guard_offset_bytes: u32,
    pub stack_guard_intact: bool,
    pub scan_valid: bool,
}

/// Sole owner of the CPU0/main-executor stack-watermark state.
pub(crate) struct StackWatermarkMonitor {
    stack_bottom: u32,
    stack_top: u32,
    stack_guard: u32,
    paint_top: u32,
    stack_guard_value: u32,
    high_water_used_bytes: u32,
    stack_guard_intact: bool,
    scan_valid: bool,
}

impl StackWatermarkMonitor {
    /// Validate reset-hook evidence against the actual final-link symbols.
    ///
    /// This must run while the RX electrical interlock is still asserted.
    pub(crate) fn initialize() -> Result<Self, StackWatermarkInitError> {
        // SAFETY: the reset hook writes the aligned, linker-retained `.noinit`
        // object before BSS initialization. No other context mutates it after
        // reset, and a volatile copy avoids inventing a shared reference to the
        // `static mut` object.
        let marker = unsafe { read_volatile(addr_of!(RETICULUM_STACK_WATERMARK_MARKER)) };
        if marker.magic != STARTUP_MARKER_MAGIC
            || marker.magic_inverse != STARTUP_MARKER_MAGIC_INVERSE
        {
            return Err(StackWatermarkInitError::MarkerMagic);
        }

        let stack_bottom = pointer_address_u32(addr_of!(_stack_end_cpu0))?;
        let stack_top = pointer_address_u32(addr_of!(_stack_start_cpu0))?;
        let stack_guard = pointer_address_u32(addr_of!(__stack_chk_guard))?;
        if marker.stack_bottom != stack_bottom
            || marker.stack_top != stack_top
            || marker.stack_guard != stack_guard
        {
            return Err(StackWatermarkInitError::LinkerAddressMismatch);
        }
        if marker.pattern_seed != STACK_WATERMARK_PATTERN_SEED {
            return Err(StackWatermarkInitError::PatternMismatch);
        }
        if marker.paint_top <= stack_guard || marker.paint_top > stack_top {
            return Err(StackWatermarkInitError::PaintLimitOutsideStack);
        }

        // SAFETY: the linker symbol denotes one aligned word inside the linked
        // CPU0 stack reservation. Volatile access is required because the
        // runtime and hardware stack-watchpoint logic also observe this word.
        let guard_value = unsafe { read_volatile(stack_guard as *const u32) };
        if guard_value != marker.stack_guard_value {
            return Err(StackWatermarkInitError::GuardWasNotPreserved);
        }

        let scan_limit = min(marker.paint_top, current_stack_pointer());
        StackWatermarkLayout::try_new(stack_bottom, stack_top, stack_guard, scan_limit)
            .map_err(StackWatermarkInitError::Layout)?;

        Ok(Self {
            stack_bottom,
            stack_top,
            stack_guard,
            paint_top: marker.paint_top,
            stack_guard_value: marker.stack_guard_value,
            high_water_used_bytes: 0,
            stack_guard_intact: true,
            scan_valid: true,
        })
    }

    /// Sample the watermark and retain worst-case evidence monotonically.
    pub(crate) fn sample(&mut self) -> StackWatermarkMetrics {
        // A function prologue may lower SP before this snapshot. That only
        // makes the result more conservative. The scan never reads at or above
        // this value, so it does not inspect its live frame.
        let current_sp = current_stack_pointer();
        let scan_limit = min(self.paint_top, current_sp);
        match StackWatermarkLayout::try_new(
            self.stack_bottom,
            self.stack_top,
            self.stack_guard,
            scan_limit,
        ) {
            Ok(layout) => {
                let scan = scan_stack_watermark(layout, |address| {
                    // SAFETY: `layout` proves each aligned address is within
                    // the startup-painted stack range and below the captured
                    // current SP. The hook initialized every such word except
                    // the guard, which the pure scanner never requests.
                    unsafe { read_volatile(address as *const u32) }
                });
                self.high_water_used_bytes =
                    self.high_water_used_bytes.max(scan.high_water_used_bytes);
            }
            Err(_) => {
                // A later out-of-range SP is itself failed qualification. Do
                // not attempt arbitrary volatile reads; retain the prior
                // measurement and make the failure sticky in every heartbeat.
                self.scan_valid = false;
            }
        }

        // SAFETY: same linker-owned canary word validated by `initialize`.
        // Once a mismatch is observed the diagnostic remains failed even if a
        // later write happens to restore the captured value.
        let guard_value = unsafe { read_volatile(self.stack_guard as *const u32) };
        self.stack_guard_intact &= guard_value == self.stack_guard_value;

        let stack_reserved_bytes = self.stack_top - self.stack_bottom;
        let stack_usable_above_guard_bytes =
            self.stack_top - (self.stack_guard + STACK_WATERMARK_WORD_BYTES);
        let startup_painted_bytes =
            (self.paint_top - self.stack_bottom) - STACK_WATERMARK_WORD_BYTES;

        StackWatermarkMetrics {
            stack_reserved_bytes,
            stack_usable_above_guard_bytes,
            startup_painted_bytes,
            high_water_used_bytes: self.high_water_used_bytes,
            minimum_remaining_above_guard_bytes: stack_usable_above_guard_bytes
                .saturating_sub(self.high_water_used_bytes),
            stack_guard_offset_bytes: self.stack_guard - self.stack_bottom,
            stack_guard_intact: self.stack_guard_intact,
            scan_valid: self.scan_valid,
        }
    }
}

fn pointer_address_u32<T>(pointer: *const T) -> Result<u32, StackWatermarkInitError> {
    u32::try_from(pointer as usize).map_err(|_| StackWatermarkInitError::LinkerAddressDoesNotFit)
}

#[inline(always)]
fn current_stack_pointer() -> u32 {
    let stack_pointer: u32;
    // SAFETY: this reads Xtensa's architectural stack-pointer register without
    // changing machine state or dereferencing memory.
    unsafe {
        asm!("mov {0}, sp", out(reg) stack_pointer);
    }
    stack_pointer
}
