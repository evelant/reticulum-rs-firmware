//! Fail-closed policy for recovering a cancelled E290 radio operation.

/// Minimum healthy boot uptime before another radio-recovery reset is allowed.
///
/// A pending CAD, RX, or TX future owns the initialized SX1262 driver. Dropping
/// that future deliberately destroys the physical owner, so the only safe
/// recovery is a whole-chip reset through the ordinary boot path. A second
/// cancellation inside this window leaves LoRa fail-stopped instead of creating
/// a reboot loop.
pub const RADIO_RECOVERY_RESET_REARM_UPTIME_MS: u64 = 600_000;

/// Complete RTC marker written immediately before a radio-recovery reset.
///
/// The second word is the complement of the `LORA` marker. Any partial or
/// corrupt pair is conservatively treated as a previous reset attempt.
pub const RADIO_RECOVERY_RESET_MARKER_WORDS: [u32; 2] = [0x4c4f_5241, !0x4c4f_5241];

/// Validated state of the RTC-retained radio-recovery marker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadioRecoveryResetMarkerState {
    /// Power-on initialization left both words clear.
    Clean,
    /// Both words contain the complete complementary marker.
    Armed,
    /// At least one word is nonzero, but the pair is incomplete or corrupt.
    Corrupt,
}

impl RadioRecoveryResetMarkerState {
    /// Stable label for production diagnostics.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Armed => "armed",
            Self::Corrupt => "corrupt",
        }
    }

    /// Whether no recovery reset has been attempted since RTC initialization.
    pub const fn is_clean(self) -> bool {
        matches!(self, Self::Clean)
    }
}

/// Action after a cancelled radio owner has been reconciled exactly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadioRecoveryDisposition {
    /// Reconstruct every physical owner through one whole-chip software reset.
    SoftwareReset,
    /// Keep LoRa fail-stopped until a power cycle to prevent a reset loop.
    FailStopUntilPowerCycle,
}

/// Validate the two-word RTC marker without trusting partial writes.
pub const fn classify_radio_recovery_reset_marker(
    marker: [u32; 2],
) -> RadioRecoveryResetMarkerState {
    if marker[0] == 0 && marker[1] == 0 {
        RadioRecoveryResetMarkerState::Clean
    } else if marker[0] == RADIO_RECOVERY_RESET_MARKER_WORDS[0]
        && marker[1] == RADIO_RECOVERY_RESET_MARKER_WORDS[1]
    {
        RadioRecoveryResetMarkerState::Armed
    } else {
        RadioRecoveryResetMarkerState::Corrupt
    }
}

/// Rate-limit whole-chip recovery after one cancelled CAD, RX, or TX owner.
///
/// Stable uptime rearms recovery even though the RTC marker remains set. This
/// admits isolated long-running failures while an early repeat after reboot
/// fails closed and remains visible in diagnostics.
pub const fn radio_recovery_disposition(
    retained_marker_clean: bool,
    boot_uptime_ms: u64,
) -> RadioRecoveryDisposition {
    if retained_marker_clean || boot_uptime_ms >= RADIO_RECOVERY_RESET_REARM_UPTIME_MS {
        RadioRecoveryDisposition::SoftwareReset
    } else {
        RadioRecoveryDisposition::FailStopUntilPowerCycle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_recovery_reset_is_admitted_but_an_early_repeat_fail_stops() {
        assert_eq!(
            radio_recovery_disposition(true, 1),
            RadioRecoveryDisposition::SoftwareReset
        );
        assert_eq!(
            radio_recovery_disposition(
                false,
                RADIO_RECOVERY_RESET_REARM_UPTIME_MS.saturating_sub(1)
            ),
            RadioRecoveryDisposition::FailStopUntilPowerCycle
        );
        assert_eq!(
            radio_recovery_disposition(false, RADIO_RECOVERY_RESET_REARM_UPTIME_MS),
            RadioRecoveryDisposition::SoftwareReset
        );
    }

    #[test]
    fn torn_or_corrupt_markers_are_never_treated_as_clean() {
        assert_eq!(
            classify_radio_recovery_reset_marker([0, 0]),
            RadioRecoveryResetMarkerState::Clean
        );
        assert_eq!(
            classify_radio_recovery_reset_marker(RADIO_RECOVERY_RESET_MARKER_WORDS),
            RadioRecoveryResetMarkerState::Armed
        );
        for marker in [
            [RADIO_RECOVERY_RESET_MARKER_WORDS[0], 0],
            [0, RADIO_RECOVERY_RESET_MARKER_WORDS[1]],
            [
                RADIO_RECOVERY_RESET_MARKER_WORDS[0] ^ 1,
                RADIO_RECOVERY_RESET_MARKER_WORDS[1],
            ],
        ] {
            assert_eq!(
                classify_radio_recovery_reset_marker(marker),
                RadioRecoveryResetMarkerState::Corrupt
            );
        }
    }
}
