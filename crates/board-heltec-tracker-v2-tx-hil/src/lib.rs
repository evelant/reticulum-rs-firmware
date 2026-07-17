//! Compatibility facade for the historical Tracker V2 TX HIL.
//!
//! The qualified SX1262, external-FEM, RX, and TX implementation now lives in
//! [`reticulum_board_heltec_tracker_v2_radio`]. This crate retains the old HIL
//! names and feature boundary so committed hardware procedures remain
//! reproducible while permanent firmware uses the product-named surface.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub use reticulum_board_heltec_tracker_v2_radio::{
    TRACKER_NA915_DEV_CONFIGURATION, TRACKER_NA915_DEV_MODEM_OUTPUT_DBM, TRACKER_NA915_DEV_PROFILE,
    TRACKER_NA915_DEV_TARGET_POWER_DBM, TRACKER_PRIVATE_SYNC_WORD, TRACKER_RX_SYMBOL_TIMEOUT,
    TrackerRadio, TrackerRadioConfiguration, TrackerRadioConfigurationId, TrackerRadioError,
    TrackerRadioOperation, TrackerRadioPower, TrackerReceivedFrame, TrackerRxTimestampCapture,
    TrackerTxArm,
};

#[cfg(feature = "near-field-attenuation-hil")]
pub use reticulum_board_heltec_tracker_v2_radio::{
    TRACKER_NA915_NEAR_FIELD_DIAGNOSTIC_CONFIGURATION,
    TRACKER_NA915_NEAR_FIELD_DIAGNOSTIC_MODEM_OUTPUT_DBM,
    TRACKER_NA915_NEAR_FIELD_DIAGNOSTIC_TARGET_POWER_DBM,
};

pub use reticulum_board_heltec_tracker_v2_radio::{
    TRACKER_NA915_DEV_PROFILE as TRACKER_TX_HIL_PROFILE, TrackerRadio as TrackerTxHilRadio,
    TrackerRadioError as TrackerTxHilError, TrackerRadioOperation as TrackerTxHilOperation,
    TrackerReceivedFrame as TrackerTxHilReceivedFrame,
};

/// Opaque radio configuration explicitly selected by the historical HIL.
#[cfg(not(feature = "near-field-attenuation-hil"))]
pub const TRACKER_TX_HIL_CONFIGURATION: TrackerRadioConfiguration = TRACKER_NA915_DEV_CONFIGURATION;

/// Opaque diagnostic radio configuration explicitly selected by the HIL.
#[cfg(feature = "near-field-attenuation-hil")]
pub const TRACKER_TX_HIL_CONFIGURATION: TrackerRadioConfiguration =
    TRACKER_NA915_NEAR_FIELD_DIAGNOSTIC_CONFIGURATION;

/// Effective antenna-path target selected by the historical HIL.
pub const TRACKER_TX_HIL_TARGET_POWER_DBM: i32 =
    TRACKER_TX_HIL_CONFIGURATION.power().target_power_dbm();

/// Exact SX1262 output selected by the historical HIL.
pub const TRACKER_TX_HIL_MODEM_OUTPUT_DBM: i32 =
    TRACKER_TX_HIL_CONFIGURATION.power().modem_output_dbm();

/// Maximum preamble-search timeout selected by the historical HIL.
pub const TRACKER_TX_HIL_RX_SYMBOL_TIMEOUT: u16 = TRACKER_TX_HIL_CONFIGURATION.rx_symbol_timeout();

/// HIL log label for the external FEM CTX assertion point.
pub const TRACKER_TX_HIL_CTX_ASSERTION: &str = "before-packet-and-fifo-prepare";

/// HIL log label for the external FEM power policy.
pub const TRACKER_TX_HIL_FEM_POWER_POLICY: &str = "prepowered-during-radio-init";

/// HIL log label for the pinned SX1262 standby clock.
pub const TRACKER_TX_HIL_STANDBY_CLOCK: &str = "rc";

/// HIL log label for the calibrated minimum power selection.
#[cfg(not(feature = "near-field-attenuation-hil"))]
pub const TRACKER_TX_HIL_POWER_PROFILE: &str = "calibrated-minimum";

/// HIL log label for diagnostic near-field attenuation.
#[cfg(feature = "near-field-attenuation-hil")]
pub const TRACKER_TX_HIL_POWER_PROFILE: &str = "near-field-attenuation-uncalibrated";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_configuration_is_invariant_and_legacy_hil_names_follow_explicit_selection() {
        assert_eq!(
            TRACKER_NA915_DEV_CONFIGURATION.id(),
            TrackerRadioConfigurationId::Na915DevCalibratedMinimum
        );
        assert_eq!(TRACKER_NA915_DEV_TARGET_POWER_DBM, 14);
        assert_eq!(TRACKER_NA915_DEV_MODEM_OUTPUT_DBM, 0);
        assert_eq!(
            TRACKER_TX_HIL_PROFILE,
            TRACKER_TX_HIL_CONFIGURATION.profile()
        );
        assert_eq!(
            TRACKER_TX_HIL_TARGET_POWER_DBM,
            TRACKER_TX_HIL_CONFIGURATION.power().target_power_dbm()
        );
        assert_eq!(
            TRACKER_TX_HIL_MODEM_OUTPUT_DBM,
            TRACKER_TX_HIL_CONFIGURATION.power().modem_output_dbm()
        );
        assert_eq!(
            TRACKER_TX_HIL_RX_SYMBOL_TIMEOUT,
            TRACKER_TX_HIL_CONFIGURATION.rx_symbol_timeout()
        );
        assert_eq!(TRACKER_TX_HIL_STANDBY_CLOCK, "rc");
        assert_eq!(
            TRACKER_TX_HIL_CTX_ASSERTION,
            "before-packet-and-fifo-prepare"
        );
        assert_eq!(
            TRACKER_TX_HIL_FEM_POWER_POLICY,
            "prepowered-during-radio-init"
        );

        #[cfg(not(feature = "near-field-attenuation-hil"))]
        {
            assert_eq!(
                TRACKER_TX_HIL_CONFIGURATION.id(),
                TrackerRadioConfigurationId::Na915DevCalibratedMinimum
            );
            assert_eq!(TRACKER_TX_HIL_POWER_PROFILE, "calibrated-minimum");
        }
        #[cfg(feature = "near-field-attenuation-hil")]
        {
            assert_eq!(
                TRACKER_TX_HIL_CONFIGURATION.id(),
                TrackerRadioConfigurationId::Na915DevDiagnosticNearFieldAttenuation
            );
            assert_eq!(
                TRACKER_TX_HIL_POWER_PROFILE,
                "near-field-attenuation-uncalibrated"
            );
        }
    }
}
