//! Explicit, validated receive-only LoRa configuration.
//!
//! This module deliberately has no board or radio initialization API. A board
//! supplies its fitted RF range, and construction yields an opaque profile
//! only after every project-owned check has passed.

pub use lora_modulation::{Bandwidth, CodingRate, SpreadingFactor};

/// Minimum RNode-compatible preamble selected by current RNode firmware.
pub const RNODE_MIN_PREAMBLE_SYMBOLS: u16 = 18;

/// Fixed scheduling/rearm guard added after airtime-based fragment sizing.
pub const RNODE_FRAGMENT_TIMEOUT_GUARD_US: u64 = 5_000_000;

/// Inclusive receive range supported by one fitted radio/antenna path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReceiveFrequencyRange {
    minimum_hz: u32,
    maximum_hz: u32,
}

impl ReceiveFrequencyRange {
    /// Validate and construct an inclusive receive-frequency range.
    pub const fn try_new(
        minimum_hz: u32,
        maximum_hz: u32,
    ) -> Result<Self, ReceiveFrequencyRangeError> {
        if minimum_hz == 0 {
            return Err(ReceiveFrequencyRangeError::ZeroMinimum);
        }
        if minimum_hz > maximum_hz {
            return Err(ReceiveFrequencyRangeError::Reversed {
                minimum_hz,
                maximum_hz,
            });
        }
        Ok(Self {
            minimum_hz,
            maximum_hz,
        })
    }

    /// Lowest accepted frequency, inclusive.
    pub const fn minimum_hz(self) -> u32 {
        self.minimum_hz
    }

    /// Highest accepted frequency, inclusive.
    pub const fn maximum_hz(self) -> u32 {
        self.maximum_hz
    }

    const fn contains(self, frequency_hz: u32) -> bool {
        frequency_hz >= self.minimum_hz && frequency_hz <= self.maximum_hz
    }
}

/// Invalid board/radio receive-range definition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiveFrequencyRangeError {
    /// Zero cannot describe a usable radio frequency.
    ZeroMinimum,
    /// The inclusive bounds were supplied in descending order.
    Reversed {
        /// Supplied lower bound.
        minimum_hz: u32,
        /// Supplied upper bound.
        maximum_hz: u32,
    },
}

/// Untrusted numeric configuration supplied by an explicit lab build.
///
/// This type intentionally has no [`Default`] implementation. `None` is
/// retained for frequency so configuration loaders can reject an omitted
/// value explicitly instead of substituting a regional guess.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LabRxProfileConfig {
    pub frequency_hz: Option<u32>,
    pub spreading_factor: u8,
    pub bandwidth_hz: u32,
    pub coding_rate_denominator: u8,
    pub preamble_symbols: u16,
    pub explicit_header: bool,
    pub crc: bool,
    pub iq_inverted: bool,
}

/// Receive-only LoRa settings validated for one board/radio path.
///
/// Fields are private so a caller cannot mutate a checked profile before it
/// reaches the radio wrapper. There is intentionally no transmit-power field
/// and no [`Default`] implementation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LabRxProfile {
    frequency_hz: u32,
    spreading_factor: SpreadingFactor,
    bandwidth: Bandwidth,
    coding_rate: CodingRate,
    preamble_symbols: u16,
    explicit_header: bool,
    crc: bool,
    iq_inverted: bool,
}

impl LabRxProfile {
    /// Validate explicit numeric configuration against a fitted receive path.
    pub const fn validate(
        config: LabRxProfileConfig,
        supported_range: ReceiveFrequencyRange,
    ) -> Result<Self, LabRxProfileError> {
        let frequency_hz = match config.frequency_hz {
            Some(frequency_hz) => frequency_hz,
            None => return Err(LabRxProfileError::MissingFrequency),
        };
        let spreading_factor = match config.spreading_factor {
            7 => SpreadingFactor::_7,
            8 => SpreadingFactor::_8,
            9 => SpreadingFactor::_9,
            10 => SpreadingFactor::_10,
            11 => SpreadingFactor::_11,
            12 => SpreadingFactor::_12,
            value => return Err(LabRxProfileError::UnsupportedSpreadingFactor(value)),
        };
        let bandwidth = match config.bandwidth_hz {
            7_800 | 7_810 => Bandwidth::_7KHz,
            10_400 | 10_420 => Bandwidth::_10KHz,
            15_600 | 15_630 => Bandwidth::_15KHz,
            20_800 | 20_830 => Bandwidth::_20KHz,
            31_250 => Bandwidth::_31KHz,
            41_670 | 41_700 => Bandwidth::_41KHz,
            62_500 => Bandwidth::_62KHz,
            125_000 => Bandwidth::_125KHz,
            250_000 => Bandwidth::_250KHz,
            500_000 => Bandwidth::_500KHz,
            value => return Err(LabRxProfileError::UnsupportedBandwidthHz(value)),
        };
        let coding_rate = match config.coding_rate_denominator {
            5 => CodingRate::_4_5,
            6 => CodingRate::_4_6,
            7 => CodingRate::_4_7,
            8 => CodingRate::_4_8,
            value => return Err(LabRxProfileError::UnsupportedCodingRateDenominator(value)),
        };

        let canonical_bandwidth_hz = bandwidth.hz();
        // The SX126x register codes are exact divisors of its 32 MHz clock;
        // several public decimal labels are rounded. Use a whole-Hz ceiling
        // for fitted-path containment so a rounded-down label cannot place a
        // physical channel edge outside the supported range.
        let channel_edge_bandwidth_hz = sx126x_bandwidth_ceil_hz(bandwidth);
        let lower_half_hz = channel_edge_bandwidth_hz / 2;
        let upper_half_hz = channel_edge_bandwidth_hz - lower_half_hz;
        let frequency_hz_u64 = frequency_hz as u64;
        let lower_edge_hz = frequency_hz_u64.saturating_sub(lower_half_hz as u64);
        let upper_edge_hz = frequency_hz_u64 + upper_half_hz as u64;
        if !supported_range.contains(frequency_hz)
            || lower_edge_hz < supported_range.minimum_hz as u64
            || upper_edge_hz > supported_range.maximum_hz as u64
        {
            return Err(LabRxProfileError::ChannelOutsideSupportedRange {
                frequency_hz,
                bandwidth_hz: canonical_bandwidth_hz,
                lower_edge_hz,
                upper_edge_hz,
                minimum_hz: supported_range.minimum_hz,
                maximum_hz: supported_range.maximum_hz,
            });
        }
        if config.preamble_symbols < RNODE_MIN_PREAMBLE_SYMBOLS {
            return Err(LabRxProfileError::PreambleTooShort {
                actual: config.preamble_symbols,
                minimum: RNODE_MIN_PREAMBLE_SYMBOLS,
            });
        }
        if !config.explicit_header {
            return Err(LabRxProfileError::ExplicitHeaderRequired);
        }
        if !config.crc {
            return Err(LabRxProfileError::CrcRequired);
        }
        if config.iq_inverted {
            return Err(LabRxProfileError::NormalIqRequired);
        }
        if frequency_hz < 400_000_000
            && matches!(bandwidth, Bandwidth::_250KHz | Bandwidth::_500KHz)
        {
            return Err(LabRxProfileError::WideBandwidthBelow400MHz {
                frequency_hz,
                bandwidth_hz: config.bandwidth_hz,
            });
        }
        if has_unverified_rnode_ldro(config.spreading_factor, bandwidth) {
            return Err(LabRxProfileError::UnverifiedRnodeLdroCombination {
                spreading_factor: config.spreading_factor,
                bandwidth_hz: canonical_bandwidth_hz,
            });
        }

        Ok(Self {
            frequency_hz,
            spreading_factor,
            bandwidth,
            coding_rate,
            preamble_symbols: config.preamble_symbols,
            explicit_header: config.explicit_header,
            crc: config.crc,
            iq_inverted: config.iq_inverted,
        })
    }

    pub const fn frequency_hz(self) -> u32 {
        self.frequency_hz
    }

    pub const fn spreading_factor(self) -> SpreadingFactor {
        self.spreading_factor
    }

    pub const fn bandwidth(self) -> Bandwidth {
        self.bandwidth
    }

    pub const fn coding_rate(self) -> CodingRate {
        self.coding_rate
    }

    pub const fn preamble_symbols(self) -> u16 {
        self.preamble_symbols
    }

    pub const fn explicit_header(self) -> bool {
        self.explicit_header
    }

    pub const fn crc(self) -> bool {
        self.crc
    }

    pub const fn iq_inverted(self) -> bool {
        self.iq_inverted
    }

    /// Conservative deadline interval for a matching split continuation.
    ///
    /// RNode sends the second physical frame immediately after the first. The
    /// receiver observes the first only after its airtime has elapsed, so the
    /// pending slot must remain valid for at least one maximum-size second-frame
    /// airtime. Phase 1 allows twice that duration plus a five-second scheduling
    /// guard. This scales correctly for narrow-band profiles instead of using a
    /// short fixed timeout that rejects a valid frame before RX completion.
    pub const fn fragment_timeout_us(self) -> u64 {
        self.maximum_frame_time_on_air_us()
            .saturating_mul(2)
            .saturating_add(RNODE_FRAGMENT_TIMEOUT_GUARD_US)
    }

    /// Conservative whole-microsecond airtime ceiling for a full 255-byte frame.
    pub const fn maximum_frame_time_on_air_us(self) -> u64 {
        match conservative_lora_frame_time_on_air_us(self, u8::MAX) {
            Some(airtime_us) => airtime_us,
            // Validated profiles cannot reach this path. Remaining
            // fail-conservative preserves the deadline helper's infallible API.
            None => u64::MAX,
        }
    }
}

/// Calculate one SX126x frame's LoRa airtime as an exact rational number of
/// quarter symbols, then round up once to a whole microsecond.
///
/// Payload-symbol rounding and the low-data-rate optimization decision match
/// `lora-modulation`. The SX126x bandwidth-code divisors, rather than their
/// rounded public decimal labels, define symbol duration. Keeping both that
/// bandwidth and time rational until the final division avoids
/// under-reservation.
pub(crate) const fn conservative_lora_frame_time_on_air_us(
    profile: LabRxProfile,
    frame_len: u8,
) -> Option<u64> {
    let spreading_factor = profile.spreading_factor.factor();
    let symbols_per_chirp = match 1_u64.checked_shl(spreading_factor) {
        Some(symbols) => symbols,
        None => return None,
    };
    let labelled_symbol_time_numerator_us = match symbols_per_chirp.checked_mul(1_000_000) {
        Some(numerator) => numerator,
        None => return None,
    };
    let labelled_bandwidth_hz = profile.bandwidth.hz() as u64;

    // Preserve `BaseBandModulationParams::new` behavior exactly: it compares
    // the floor-valued whole-microsecond symbol duration with 16.384 ms.
    let low_data_rate_optimize =
        labelled_symbol_time_numerator_us / labelled_bandwidth_hz >= 16_384;
    let optimization_symbols = if low_data_rate_optimize { 2 } else { 0 };
    let payload_denominator_base = match (spreading_factor as u64).checked_sub(optimization_symbols)
    {
        Some(value) => value,
        None => return None,
    };
    let payload_denominator = match payload_denominator_base.checked_mul(4) {
        Some(denominator) if denominator != 0 => denominator,
        _ => return None,
    };

    let payload_bits = match (frame_len as u64).checked_mul(8) {
        Some(value) => value,
        None => return None,
    };
    let payload_with_fixed_bits = match payload_bits.checked_add(28) {
        Some(value) => value,
        None => return None,
    };
    let payload_positive =
        match payload_with_fixed_bits.checked_add(if profile.crc { 16 } else { 0 }) {
            Some(value) => value,
            None => return None,
        };
    let spreading_factor_bits = match (spreading_factor as u64).checked_mul(4) {
        Some(value) => value,
        None => return None,
    };
    let payload_negative =
        match spreading_factor_bits.checked_add(if profile.explicit_header { 0 } else { 20 }) {
            Some(value) => value,
            None => return None,
        };
    let payload_ratio = if payload_positive <= payload_negative {
        0
    } else {
        match checked_ceil_div(payload_positive - payload_negative, payload_denominator) {
            Some(value) => value,
            None => return None,
        }
    };
    let coded_payload_symbols = match payload_ratio.checked_mul(profile.coding_rate.denom() as u64)
    {
        Some(symbols) => symbols,
        None => return None,
    };
    let payload_symbols = match coded_payload_symbols.checked_add(8) {
        Some(symbols) => symbols,
        None => return None,
    };

    let preamble_symbol_quarters = match (profile.preamble_symbols as u64).checked_mul(4) {
        Some(symbols) => symbols,
        None => return None,
    };
    let preamble_quarter_symbols = match preamble_symbol_quarters.checked_add(17) {
        Some(symbols) => symbols,
        None => return None,
    };
    let payload_quarter_symbols = match payload_symbols.checked_mul(4) {
        Some(symbols) => symbols,
        None => return None,
    };
    let total_quarter_symbols = match preamble_quarter_symbols.checked_add(payload_quarter_symbols)
    {
        Some(symbols) => symbols,
        None => return None,
    };
    let bandwidth_divisor = sx126x_bandwidth_divisor(profile.bandwidth);
    let exact_symbol_time_numerator_us =
        match labelled_symbol_time_numerator_us.checked_mul(bandwidth_divisor) {
            Some(numerator) => numerator,
            None => return None,
        };
    let total_time_numerator_us =
        match total_quarter_symbols.checked_mul(exact_symbol_time_numerator_us) {
            Some(numerator) => numerator,
            None => return None,
        };
    let total_time_denominator = match 32_000_000_u64.checked_mul(4) {
        Some(denominator) => denominator,
        None => return None,
    };
    checked_ceil_div(total_time_numerator_us, total_time_denominator)
}

/// Exact denominator `D` for the SX126x bandwidth `32 MHz / D`.
const fn sx126x_bandwidth_divisor(bandwidth: Bandwidth) -> u64 {
    match bandwidth {
        Bandwidth::_7KHz => 4_096,
        Bandwidth::_10KHz => 3_072,
        Bandwidth::_15KHz => 2_048,
        Bandwidth::_20KHz => 1_536,
        Bandwidth::_31KHz => 1_024,
        Bandwidth::_41KHz => 768,
        Bandwidth::_62KHz => 512,
        Bandwidth::_125KHz => 256,
        Bandwidth::_250KHz => 128,
        Bandwidth::_500KHz => 64,
    }
}

/// Conservative whole-Hz width for fitted-path edge validation.
const fn sx126x_bandwidth_ceil_hz(bandwidth: Bandwidth) -> u32 {
    let divisor = sx126x_bandwidth_divisor(bandwidth);
    match checked_ceil_div(32_000_000, divisor) {
        Some(hz) => hz as u32,
        None => u32::MAX,
    }
}

const fn checked_ceil_div(numerator: u64, denominator: u64) -> Option<u64> {
    if denominator == 0 {
        return None;
    }
    let quotient = numerator / denominator;
    if numerator.is_multiple_of(denominator) {
        Some(quotient)
    } else {
        quotient.checked_add(1)
    }
}

/// Rejection from project-owned lab-profile validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LabRxProfileError {
    MissingFrequency,
    ChannelOutsideSupportedRange {
        frequency_hz: u32,
        bandwidth_hz: u32,
        lower_edge_hz: u64,
        upper_edge_hz: u64,
        minimum_hz: u32,
        maximum_hz: u32,
    },
    UnsupportedSpreadingFactor(u8),
    UnsupportedBandwidthHz(u32),
    UnsupportedCodingRateDenominator(u8),
    PreambleTooShort {
        actual: u16,
        minimum: u16,
    },
    ExplicitHeaderRequired,
    CrcRequired,
    NormalIqRequired,
    WideBandwidthBelow400MHz {
        frequency_hz: u32,
        bandwidth_hz: u32,
    },
    UnverifiedRnodeLdroCombination {
        spreading_factor: u8,
        bandwidth_hz: u32,
    },
}

/// Whether pinned `lora-phy` and the working RNode firmware select different
/// SX1262 low-data-rate optimization values for this tuple.
///
/// RNode uses integer `(2^SF)/(BW/1000) > 16`; `lora-phy` 3.0.1 uses a small
/// hard-coded tuple list. Rejecting the difference is safer than silently
/// claiming an interoperability profile until the driver is fixed or HIL
/// proves a specific combination.
const fn has_unverified_rnode_ldro(spreading_factor: u8, bandwidth: Bandwidth) -> bool {
    match bandwidth {
        Bandwidth::_7KHz => spreading_factor >= 7,
        Bandwidth::_10KHz | Bandwidth::_15KHz => spreading_factor >= 8,
        Bandwidth::_20KHz => spreading_factor >= 9,
        Bandwidth::_31KHz | Bandwidth::_41KHz => spreading_factor >= 10,
        Bandwidth::_62KHz => spreading_factor >= 11,
        Bandwidth::_125KHz => spreading_factor == 11,
        Bandwidth::_250KHz => spreading_factor == 12,
        Bandwidth::_500KHz => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lora_modulation::BaseBandModulationParams;

    const TRACKER_RANGE: ReceiveFrequencyRange =
        match ReceiveFrequencyRange::try_new(863_000_000, 928_000_000) {
            Ok(range) => range,
            Err(_) => panic!("invalid test range"),
        };

    const fn valid_config() -> LabRxProfileConfig {
        LabRxProfileConfig {
            frequency_hz: Some(915_000_000),
            spreading_factor: 7,
            bandwidth_hz: 125_000,
            coding_rate_denominator: 5,
            preamble_symbols: 18,
            explicit_header: true,
            crc: true,
            iq_inverted: false,
        }
    }

    #[test]
    fn explicit_rnode_profile_is_preserved() {
        let profile = LabRxProfile::validate(valid_config(), TRACKER_RANGE).unwrap();
        assert_eq!(profile.frequency_hz(), 915_000_000);
        assert_eq!(profile.spreading_factor(), SpreadingFactor::_7);
        assert_eq!(profile.bandwidth(), Bandwidth::_125KHz);
        assert_eq!(profile.coding_rate(), CodingRate::_4_5);
        assert_eq!(profile.preamble_symbols(), 18);
        assert!(profile.explicit_header());
        assert!(profile.crc());
        assert!(!profile.iq_inverted());
    }

    #[test]
    fn sx126x_bandwidth_codes_use_exact_divisors_not_decimal_labels() {
        let cases = [
            (Bandwidth::_7KHz, 7_813),
            (Bandwidth::_10KHz, 10_417),
            (Bandwidth::_15KHz, 15_625),
            (Bandwidth::_20KHz, 20_834),
            (Bandwidth::_31KHz, 31_250),
            (Bandwidth::_41KHz, 41_667),
            (Bandwidth::_62KHz, 62_500),
            (Bandwidth::_125KHz, 125_000),
            (Bandwidth::_250KHz, 250_000),
            (Bandwidth::_500KHz, 500_000),
        ];
        for (bandwidth, conservative_whole_hz) in cases {
            assert_eq!(sx126x_bandwidth_ceil_hz(bandwidth), conservative_whole_hz);
            let divisor = sx126x_bandwidth_divisor(bandwidth);
            assert!((conservative_whole_hz as u64) * divisor >= 32_000_000);
            assert!((conservative_whole_hz as u64 - 1) * divisor < 32_000_000);
        }
    }

    #[test]
    fn fragment_timeout_scales_from_full_frame_airtime() {
        let profile = LabRxProfile::validate(valid_config(), TRACKER_RANGE).unwrap();
        let modulation = BaseBandModulationParams::new(
            profile.spreading_factor(),
            profile.bandwidth(),
            profile.coding_rate(),
        );
        let expected_airtime = u64::from(modulation.time_on_air_us(
            Some(u8::try_from(profile.preamble_symbols()).unwrap()),
            profile.explicit_header(),
            u8::MAX,
        ));

        assert_eq!(profile.maximum_frame_time_on_air_us(), expected_airtime);
        assert_eq!(
            profile.fragment_timeout_us(),
            expected_airtime * 2 + RNODE_FRAGMENT_TIMEOUT_GUARD_US
        );

        let mut slow_config = valid_config();
        slow_config.spreading_factor = 12;
        let slow = LabRxProfile::validate(slow_config, TRACKER_RANGE).unwrap();
        assert!(slow.fragment_timeout_us() > profile.fragment_timeout_us());
    }

    #[test]
    fn missing_frequency_never_falls_back() {
        let mut config = valid_config();
        config.frequency_hz = None;
        assert_eq!(
            LabRxProfile::validate(config, TRACKER_RANGE),
            Err(LabRxProfileError::MissingFrequency)
        );
    }

    #[test]
    fn fitted_frequency_bounds_are_inclusive_and_enforced() {
        for frequency_hz in [863_062_500, 927_937_500] {
            let mut config = valid_config();
            config.frequency_hz = Some(frequency_hz);
            assert!(LabRxProfile::validate(config, TRACKER_RANGE).is_ok());
        }

        for frequency_hz in [863_062_499, 927_937_501] {
            let mut config = valid_config();
            config.frequency_hz = Some(frequency_hz);
            assert_eq!(
                LabRxProfile::validate(config, TRACKER_RANGE),
                Err(LabRxProfileError::ChannelOutsideSupportedRange {
                    frequency_hz,
                    bandwidth_hz: 125_000,
                    lower_edge_hz: frequency_hz as u64 - 62_500,
                    upper_edge_hz: frequency_hz as u64 + 62_500,
                    minimum_hz: 863_000_000,
                    maximum_hz: 928_000_000,
                })
            );
        }
    }

    #[test]
    fn unsupported_modulation_values_are_rejected() {
        let mut config = valid_config();
        config.spreading_factor = 6;
        assert_eq!(
            LabRxProfile::validate(config, TRACKER_RANGE),
            Err(LabRxProfileError::UnsupportedSpreadingFactor(6))
        );

        config = valid_config();
        config.bandwidth_hz = 100_000;
        assert_eq!(
            LabRxProfile::validate(config, TRACKER_RANGE),
            Err(LabRxProfileError::UnsupportedBandwidthHz(100_000))
        );

        config = valid_config();
        config.coding_rate_denominator = 9;
        assert_eq!(
            LabRxProfile::validate(config, TRACKER_RANGE),
            Err(LabRxProfileError::UnsupportedCodingRateDenominator(9))
        );
    }

    #[test]
    fn rnode_preamble_floor_is_enforced() {
        let mut config = valid_config();
        config.preamble_symbols = RNODE_MIN_PREAMBLE_SYMBOLS - 1;
        assert_eq!(
            LabRxProfile::validate(config, TRACKER_RANGE),
            Err(LabRxProfileError::PreambleTooShort {
                actual: 17,
                minimum: 18,
            })
        );
    }

    #[test]
    fn rnode_header_crc_and_iq_are_fixed_for_this_slice() {
        let mut config = valid_config();
        config.explicit_header = false;
        assert_eq!(
            LabRxProfile::validate(config, TRACKER_RANGE),
            Err(LabRxProfileError::ExplicitHeaderRequired)
        );

        config = valid_config();
        config.crc = false;
        assert_eq!(
            LabRxProfile::validate(config, TRACKER_RANGE),
            Err(LabRxProfileError::CrcRequired)
        );

        config = valid_config();
        config.iq_inverted = true;
        assert_eq!(
            LabRxProfile::validate(config, TRACKER_RANGE),
            Err(LabRxProfileError::NormalIqRequired)
        );
    }

    #[test]
    fn sx126x_wide_bandwidth_low_frequency_combination_is_rejected() {
        let broad_range = ReceiveFrequencyRange::try_new(137_000_000, 1_020_000_000).unwrap();
        for bandwidth_hz in [250_000, 500_000] {
            let mut config = valid_config();
            config.frequency_hz = Some(399_999_999);
            config.bandwidth_hz = bandwidth_hz;
            assert_eq!(
                LabRxProfile::validate(config, broad_range),
                Err(LabRxProfileError::WideBandwidthBelow400MHz {
                    frequency_hz: 399_999_999,
                    bandwidth_hz,
                })
            );
        }
    }

    #[test]
    fn unverified_rnode_ldro_combinations_are_rejected() {
        let cases = [
            (7, 7_800, 7_810),
            (8, 10_400, 10_420),
            (8, 15_600, 15_630),
            (9, 20_800, 20_830),
            (10, 31_250, 31_250),
            (10, 41_700, 41_670),
            (11, 62_500, 62_500),
            (11, 125_000, 125_000),
            (12, 250_000, 250_000),
        ];
        for (spreading_factor, input_bandwidth_hz, canonical_bandwidth_hz) in cases {
            let mut config = valid_config();
            config.spreading_factor = spreading_factor;
            config.bandwidth_hz = input_bandwidth_hz;
            assert_eq!(
                LabRxProfile::validate(config, TRACKER_RANGE),
                Err(LabRxProfileError::UnverifiedRnodeLdroCombination {
                    spreading_factor,
                    bandwidth_hz: canonical_bandwidth_hz,
                })
            );
        }

        let mut verified = valid_config();
        verified.spreading_factor = 12;
        verified.bandwidth_hz = 125_000;
        assert!(LabRxProfile::validate(verified, TRACKER_RANGE).is_ok());
    }

    #[test]
    fn invalid_frequency_range_definitions_are_rejected() {
        assert_eq!(
            ReceiveFrequencyRange::try_new(0, 928_000_000),
            Err(ReceiveFrequencyRangeError::ZeroMinimum)
        );
        assert_eq!(
            ReceiveFrequencyRange::try_new(928_000_000, 863_000_000),
            Err(ReceiveFrequencyRangeError::Reversed {
                minimum_hz: 928_000_000,
                maximum_hz: 863_000_000,
            })
        );
    }
}
