//! Portable calculation for a downward-growing stack watermark.
//!
//! Target startup and volatile memory access deliberately remain in the
//! firmware crate. This module owns only address validation and the pure scan
//! algorithm, which keeps the reset-boundary `unsafe` code small and makes the
//! high-water calculation host-testable.

/// Size and required alignment of each painted stack word.
pub const STACK_WATERMARK_WORD_BYTES: u32 = 4;

/// Seed mixed with each word's address by the startup painter.
///
/// An address-derived pattern makes a coincidental untouched-looking stack
/// word substantially less likely than a repeated byte or word sentinel.
pub const STACK_WATERMARK_PATTERN_SEED: u32 = 0xa53c_9e71;

/// Address bounds needed to scan one downward-growing stack.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StackWatermarkLayout {
    stack_bottom: u32,
    stack_top: u32,
    stack_guard: u32,
    scan_limit: u32,
}

impl StackWatermarkLayout {
    /// Validate linker/startup addresses before any target memory is read.
    ///
    /// `scan_limit` is the lower of the startup paint limit and the current
    /// stack pointer. The guard word lies inside the range but is never read as
    /// a watermark word.
    pub const fn try_new(
        stack_bottom: u32,
        stack_top: u32,
        stack_guard: u32,
        scan_limit: u32,
    ) -> Result<Self, StackWatermarkLayoutError> {
        if stack_bottom & (STACK_WATERMARK_WORD_BYTES - 1) != 0
            || stack_top & (STACK_WATERMARK_WORD_BYTES - 1) != 0
            || stack_guard & (STACK_WATERMARK_WORD_BYTES - 1) != 0
            || scan_limit & (STACK_WATERMARK_WORD_BYTES - 1) != 0
        {
            return Err(StackWatermarkLayoutError::UnalignedAddress);
        }
        if stack_bottom >= stack_top {
            return Err(StackWatermarkLayoutError::EmptyStack);
        }
        if stack_guard < stack_bottom
            || stack_guard >= stack_top
            || stack_guard > u32::MAX - STACK_WATERMARK_WORD_BYTES
        {
            return Err(StackWatermarkLayoutError::GuardOutsideStack);
        }
        if scan_limit <= stack_guard || scan_limit > stack_top {
            return Err(StackWatermarkLayoutError::ScanLimitOutsideUsableStack);
        }

        Ok(Self {
            stack_bottom,
            stack_top,
            stack_guard,
            scan_limit,
        })
    }

    /// Lowest address reserved for the stack.
    pub const fn stack_bottom(self) -> u32 {
        self.stack_bottom
    }

    /// One-past-highest address reserved for the stack.
    pub const fn stack_top(self) -> u32 {
        self.stack_top
    }

    /// Address of the runtime-owned canary word excluded from painting.
    pub const fn stack_guard(self) -> u32 {
        self.stack_guard
    }

    /// Exclusive upper bound that is safe to inspect during this sample.
    pub const fn scan_limit(self) -> u32 {
        self.scan_limit
    }

    /// Bytes between the top of the guard and the top of the stack.
    pub const fn usable_bytes_above_guard(self) -> u32 {
        self.stack_top - (self.stack_guard + STACK_WATERMARK_WORD_BYTES)
    }
}

/// Why linker/startup addresses cannot describe a safe watermark scan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StackWatermarkLayoutError {
    /// At least one address is not word-aligned.
    UnalignedAddress,
    /// The stack bottom is not below its top.
    EmptyStack,
    /// The runtime stack-canary word is not completely inside the stack.
    GuardOutsideStack,
    /// The scan limit is not above the guard and at or below the stack top.
    ScanLimitOutsideUsableStack,
}

/// Result of one watermark scan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StackWatermarkScan {
    /// Conservative lowest address reached by this or an earlier stack frame.
    pub lowest_observed_address: u32,
    /// Bytes from `lowest_observed_address` to the stack top.
    pub high_water_used_bytes: u32,
    /// Remaining bytes between the guard word and the lowest observed address.
    pub remaining_above_guard_bytes: u32,
    /// Number of volatile word reads requested from the caller.
    pub words_read: u32,
}

/// Return the exact word expected at `address`.
pub const fn stack_watermark_word(address: u32) -> u32 {
    address ^ STACK_WATERMARK_PATTERN_SEED
}

/// Scan painted words from the stack bottom towards the current stack pointer.
///
/// The first changed word is the deepest observed stack use. When every safe
/// word is still painted, `scan_limit` itself provides a conservative bound for
/// the currently executing scanner. The runtime-owned canary word is skipped
/// without calling `read_word`.
#[inline(always)]
pub fn scan_stack_watermark(
    layout: StackWatermarkLayout,
    mut read_word: impl FnMut(u32) -> u32,
) -> StackWatermarkScan {
    let mut address = layout.stack_bottom;
    let mut first_changed = None;
    let mut words_read = 0_u32;

    while address < layout.scan_limit {
        if address != layout.stack_guard {
            words_read = words_read.saturating_add(1);
            if read_word(address) != stack_watermark_word(address) {
                first_changed = Some(address);
                break;
            }
        }
        // All validated addresses are aligned, and the loop stops below an
        // aligned limit, so this addition cannot pass u32::MAX.
        address += STACK_WATERMARK_WORD_BYTES;
    }

    let lowest_observed_address = match first_changed {
        Some(address) => address,
        None => layout.scan_limit,
    };
    let guard_end = layout.stack_guard + STACK_WATERMARK_WORD_BYTES;

    StackWatermarkScan {
        lowest_observed_address,
        high_water_used_bytes: layout.stack_top - lowest_observed_address,
        remaining_above_guard_bytes: lowest_observed_address.saturating_sub(guard_end),
        words_read,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOTTOM: u32 = 0x1000;
    const GUARD: u32 = 0x103c;
    const TOP: u32 = 0x1100;

    fn layout(scan_limit: u32) -> StackWatermarkLayout {
        StackWatermarkLayout::try_new(BOTTOM, TOP, GUARD, scan_limit).unwrap()
    }

    #[test]
    fn expected_words_are_address_derived() {
        assert_ne!(
            stack_watermark_word(BOTTOM),
            stack_watermark_word(BOTTOM + 4)
        );
        assert_eq!(
            stack_watermark_word(BOTTOM),
            BOTTOM ^ STACK_WATERMARK_PATTERN_SEED
        );
    }

    #[test]
    fn untouched_words_report_the_current_stack_depth() {
        let scan = scan_stack_watermark(layout(0x10e0), stack_watermark_word);

        assert_eq!(scan.lowest_observed_address, 0x10e0);
        assert_eq!(scan.high_water_used_bytes, 0x20);
        assert_eq!(scan.remaining_above_guard_bytes, 0xa0);
        assert_eq!(scan.words_read, (0xe0 / 4) - 1);
    }

    #[test]
    fn first_changed_word_is_the_deepest_observed_use() {
        let scan = scan_stack_watermark(layout(TOP), |address| {
            if address >= 0x10a0 {
                0
            } else {
                stack_watermark_word(address)
            }
        });

        assert_eq!(scan.lowest_observed_address, 0x10a0);
        assert_eq!(scan.high_water_used_bytes, 0x60);
        assert_eq!(scan.remaining_above_guard_bytes, 0x60);
    }

    #[test]
    fn scanner_never_reads_the_runtime_guard_word() {
        let mut guard_was_read = false;
        let scan = scan_stack_watermark(layout(TOP), |address| {
            if address == GUARD {
                guard_was_read = true;
            }
            stack_watermark_word(address)
        });

        assert!(!guard_was_read);
        assert_eq!(scan.words_read, ((TOP - BOTTOM) / 4) - 1);
    }

    #[test]
    fn use_below_the_guard_saturates_remaining_margin() {
        let scan = scan_stack_watermark(layout(TOP), |address| {
            if address >= BOTTOM + 8 {
                0
            } else {
                stack_watermark_word(address)
            }
        });

        assert_eq!(scan.lowest_observed_address, BOTTOM + 8);
        assert_eq!(scan.remaining_above_guard_bytes, 0);
        assert!(scan.high_water_used_bytes > layout(TOP).usable_bytes_above_guard());
    }

    #[test]
    fn invalid_layouts_are_rejected_before_scanning() {
        assert_eq!(
            StackWatermarkLayout::try_new(BOTTOM + 1, TOP, GUARD, TOP),
            Err(StackWatermarkLayoutError::UnalignedAddress)
        );
        assert_eq!(
            StackWatermarkLayout::try_new(TOP, TOP, GUARD, TOP),
            Err(StackWatermarkLayoutError::EmptyStack)
        );
        assert_eq!(
            StackWatermarkLayout::try_new(BOTTOM, TOP, TOP, TOP),
            Err(StackWatermarkLayoutError::GuardOutsideStack)
        );
        assert_eq!(
            StackWatermarkLayout::try_new(BOTTOM, TOP, GUARD, GUARD),
            Err(StackWatermarkLayoutError::ScanLimitOutsideUsableStack)
        );
    }
}
