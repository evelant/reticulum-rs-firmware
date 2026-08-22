//! E290 product policy expressed through PRNS's native LoRa types.
//!
//! PRNS owns the SX126x driver, modulation vocabulary, packet framing, channel
//! access, and interface behavior. This module contributes only fitted-board
//! facts: RF matching limits, qualified power points, oscillator/regulator
//! controls, and the product's Internal interface mode.

use personal_rns::interfaces::lora::{
    AirtimePolicy, AirtimePolicyError, CodingRate, Frequency, LoraBandwidth, Modulation,
    PreambleSymbols, RadioProfile, Region, SpreadingFactor, TxPower,
};
use personal_rns::interfaces::{
    ConfiguredInterfacePolicy, InterfaceDescriptor, InterfaceId, InterfaceKind, InterfaceMode,
};
use personal_rns::radios::sx126x::{BoardConfig, TcxoVoltage};
use reticulum_network_config_store::LoraRadioProfile;

use crate::board::{RF_MATCHING_MAX_HZ, RF_MATCHING_MIN_HZ};

/// RNode-compatible preamble retained by the product radio profile.
pub const E290_LORA_PREAMBLE_SYMBOLS: u16 = 24;

/// PRNS SX126x hardware configuration for the fitted HT-RA62-HF module.
pub const E290_SX126X_BOARD_CONFIG: BoardConfig = BoardConfig {
    tcxo_voltage: Some(TcxoVoltage::V1_8),
    use_dcdc: true,
    rx_boost: false,
    dio2_as_rf_switch: true,
    external_rx_gain_db: 0,
    enter_transmit: None,
    enter_receive: None,
};

/// PRNS airtime policy used until a persisted regulatory region is selected.
///
/// The product profile records a frequency but no regulatory region.
/// `Unlimited` therefore preserves explicit operator responsibility without
/// pretending that a region was selected. A later product schema can persist a
/// region and use PRNS's ordinary regional policy without changing PRNS.
pub const E290_PRNS_AIRTIME_POLICY: AirtimePolicy = AirtimePolicy::Regional;

/// Product configuration cannot be represented by the selected PRNS radio or
/// does not fit the E290-HF RF path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrnsRadioProfileError {
    /// The product store admits a canonical SX126x width PRNS does not expose.
    UnsupportedBandwidthHz(u32),
    /// The configured spreading factor is outside PRNS's vocabulary.
    UnsupportedSpreadingFactor(u8),
    /// The configured coding rate is outside PRNS's vocabulary.
    UnsupportedCodingRateDenominator(u8),
    /// The complete occupied channel exceeds the fitted 863--928 MHz path.
    ChannelOutsideFittedRange {
        /// Configured center frequency.
        frequency_hz: u32,
        /// Configured occupied bandwidth.
        bandwidth_hz: u32,
        /// Inclusive conservative lower occupied edge.
        lower_edge_hz: u64,
        /// Inclusive conservative upper occupied edge.
        upper_edge_hz: u64,
    },
}

/// Translate one persisted product profile into PRNS's public radio profile.
///
/// The persistent model validates numeric shape. This boundary additionally
/// restricts the profile to PRNS's public bandwidth set and requires the whole
/// occupied channel to remain inside the fitted HT-RA62-HF matching range.
pub const fn prns_radio_profile(
    product: LoraRadioProfile,
) -> Result<RadioProfile, PrnsRadioProfileError> {
    let spreading_factor = match product.spreading_factor() {
        5 => SpreadingFactor::Sf5,
        6 => SpreadingFactor::Sf6,
        7 => SpreadingFactor::Sf7,
        8 => SpreadingFactor::Sf8,
        9 => SpreadingFactor::Sf9,
        10 => SpreadingFactor::Sf10,
        11 => SpreadingFactor::Sf11,
        12 => SpreadingFactor::Sf12,
        value => return Err(PrnsRadioProfileError::UnsupportedSpreadingFactor(value)),
    };
    let bandwidth = match product.bandwidth_hz() {
        125_000 => LoraBandwidth::Bw125kHz,
        250_000 => LoraBandwidth::Bw250kHz,
        500_000 => LoraBandwidth::Bw500kHz,
        value => return Err(PrnsRadioProfileError::UnsupportedBandwidthHz(value)),
    };
    let coding_rate = match product.coding_rate_denominator() {
        5 => CodingRate::Cr45,
        6 => CodingRate::Cr46,
        7 => CodingRate::Cr47,
        8 => CodingRate::Cr48,
        value => {
            return Err(PrnsRadioProfileError::UnsupportedCodingRateDenominator(
                value,
            ));
        }
    };

    let frequency_hz = product.frequency_hz();
    let bandwidth_hz = bandwidth.hz();
    let half_bandwidth_hz = (bandwidth_hz / 2) as u64;
    let lower_edge_hz = (frequency_hz as u64).saturating_sub(half_bandwidth_hz);
    let upper_edge_hz = (frequency_hz as u64).saturating_add(half_bandwidth_hz);
    if lower_edge_hz < RF_MATCHING_MIN_HZ as u64 || upper_edge_hz > RF_MATCHING_MAX_HZ as u64 {
        return Err(PrnsRadioProfileError::ChannelOutsideFittedRange {
            frequency_hz,
            bandwidth_hz,
            lower_edge_hz,
            upper_edge_hz,
        });
    }

    Ok(RadioProfile {
        frequency: Frequency::new(frequency_hz),
        modulation: Modulation::Lora {
            spreading_factor,
            bandwidth,
            coding_rate,
        },
        tx_power: TxPower::new(product.tx_power().requested_dbm() as i8),
        preamble: PreambleSymbols::new(E290_LORA_PREAMBLE_SYMBOLS),
        region: Region::Unlimited,
    })
}

/// Describe the fitted LoRa radio as the appliance's Internal mesh edge.
///
/// PRNS defaults a standalone LoRa interface to `Full`. The gateway product
/// deliberately overrides only the public interface mode, leaving PRNS's
/// capabilities, MTU, bitrate, announce limits, and airtime policy intact.
pub fn prns_internal_lora_descriptor(
    profile: &RadioProfile,
    airtime_policy: AirtimePolicy,
) -> Result<InterfaceDescriptor, AirtimePolicyError> {
    let duty = airtime_policy.resolve(profile.region)?;
    let id = InterfaceId::from_channel_tag(
        InterfaceKind::LoRa,
        &personal_rns::interfaces::lora::channel_tag(profile),
    );
    Ok(personal_rns::interfaces::lora::defaults(profile, duty)
        .configured(ConfiguredInterfacePolicy {
            mode: Some(InterfaceMode::Internal),
            ..ConfiguredInterfacePolicy::default()
        })
        .descriptor(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reticulum_network_config_store::LoraTxPower;

    #[test]
    fn default_profile_maps_exactly_without_a_prns_change() {
        let profile = prns_radio_profile(LoraRadioProfile::DEFAULT).unwrap();
        assert_eq!(profile.frequency.hz(), 915_000_000);
        assert_eq!(profile.tx_power.dbm(), 14);
        assert_eq!(profile.preamble.count(), 24);
        assert_eq!(profile.region, Region::Unlimited);
        assert_eq!(
            profile.modulation,
            Modulation::Lora {
                spreading_factor: SpreadingFactor::Sf7,
                bandwidth: LoraBandwidth::Bw125kHz,
                coding_rate: CodingRate::Cr45,
            }
        );
        assert_eq!(profile.validate(), Ok(()));
    }

    #[test]
    fn every_qualified_power_row_reaches_prns_as_requested() {
        for (power, expected) in [
            (LoraTxPower::Dbm14, 14),
            (LoraTxPower::Dbm17, 17),
            (LoraTxPower::Dbm20, 20),
            (LoraTxPower::Dbm22, 22),
        ] {
            let product = LoraRadioProfile::new(915_000_000, 125_000, 7, 5, power).unwrap();
            assert_eq!(
                prns_radio_profile(product).unwrap().tx_power.dbm(),
                expected
            );
        }
    }

    #[test]
    fn unsupported_legacy_width_fails_instead_of_patching_prns() {
        let product = LoraRadioProfile::new(915_000_000, 62_500, 7, 5, LoraTxPower::Dbm14).unwrap();
        assert_eq!(
            prns_radio_profile(product),
            Err(PrnsRadioProfileError::UnsupportedBandwidthHz(62_500)),
        );
    }

    #[test]
    fn complete_channel_must_fit_the_hf_matching_range() {
        let lower_valid = LoraRadioProfile::new(
            RF_MATCHING_MIN_HZ + 62_500,
            125_000,
            7,
            5,
            LoraTxPower::Dbm14,
        )
        .unwrap();
        let upper_valid = LoraRadioProfile::new(
            RF_MATCHING_MAX_HZ - 62_500,
            125_000,
            7,
            5,
            LoraTxPower::Dbm14,
        )
        .unwrap();
        assert!(prns_radio_profile(lower_valid).is_ok());
        assert!(prns_radio_profile(upper_valid).is_ok());

        let frequency_hz = RF_MATCHING_MIN_HZ + 62_499;
        let below = LoraRadioProfile::new(frequency_hz, 125_000, 7, 5, LoraTxPower::Dbm14).unwrap();
        assert_eq!(
            prns_radio_profile(below),
            Err(PrnsRadioProfileError::ChannelOutsideFittedRange {
                frequency_hz,
                bandwidth_hz: 125_000,
                lower_edge_hz: (RF_MATCHING_MIN_HZ - 1) as u64,
                upper_edge_hz: (RF_MATCHING_MIN_HZ + 124_999) as u64,
            })
        );
    }

    #[test]
    fn ht_ra62_uses_only_sx126x_integrated_rf_controls() {
        assert_eq!(
            E290_SX126X_BOARD_CONFIG.tcxo_voltage,
            Some(TcxoVoltage::V1_8)
        );
        assert!(E290_SX126X_BOARD_CONFIG.use_dcdc);
        assert!(E290_SX126X_BOARD_CONFIG.dio2_as_rf_switch);
        assert!(!E290_SX126X_BOARD_CONFIG.rx_boost);
        assert_eq!(E290_SX126X_BOARD_CONFIG.external_rx_gain_db, 0);
        assert!(E290_SX126X_BOARD_CONFIG.enter_transmit.is_none());
        assert!(E290_SX126X_BOARD_CONFIG.enter_receive.is_none());
    }

    #[test]
    fn product_changes_only_the_public_lora_interface_mode() {
        let profile = prns_radio_profile(LoraRadioProfile::DEFAULT).unwrap();
        let descriptor = prns_internal_lora_descriptor(&profile, E290_PRNS_AIRTIME_POLICY).unwrap();
        let defaults = personal_rns::interfaces::lora::descriptor(
            descriptor.id,
            &profile,
            profile.region.regulatory_duty_cycle(),
        );

        assert_eq!(descriptor.mode, InterfaceMode::Internal);
        assert_eq!(descriptor.id, defaults.id);
        assert_eq!(descriptor.capabilities, defaults.capabilities);
        assert_eq!(descriptor.hardware_mtu, defaults.hardware_mtu);
        assert_eq!(descriptor.bitrate, defaults.bitrate);
        assert_eq!(descriptor.airtime_duty_cycle, defaults.airtime_duty_cycle);
    }
}
