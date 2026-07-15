//! Heltec Wireless Tracker V2.3 board facts.
//!
//! This is not yet a complete BSP. It centralizes the reviewed revision, pin
//! map and fail-safe levels before concrete `esp-hal` peripheral ownership is
//! introduced. V2.3 must remain distinct from other Tracker revisions.

#![no_std]
#![forbid(unsafe_code)]

/// Exact board revision described by this crate.
pub const BOARD_REVISION: &str = "2.3";

/// On-package flash capacity.
pub const FLASH_BYTES: usize = 8 * 1024 * 1024;

/// The ESP32-S3FN8 fitted to this revision has no PSRAM.
pub const HAS_PSRAM: bool = false;

/// The scaffold never enables transmission by default.
pub const TX_ENABLED_BY_DEFAULT: bool = false;

/// No legal operating frequency can be inferred from the board alone.
pub const DEFAULT_FREQUENCY_HZ: Option<u32> = None;

/// SX1262 DIO3-powered TCXO voltage.
pub const TCXO_MILLIVOLTS: u16 = 1_800;

/// GPIO assignments from the Tracker V2.3 schematic and working reference.
pub mod pins {
    pub const BUTTON: u8 = 0;
    pub const BATTERY_ADC: u8 = 1;
    pub const BATTERY_DIVIDER_ENABLE: u8 = 2;
    pub const VEXT_ENABLE: u8 = 3;
    pub const FEM_CSD: u8 = 4;
    pub const FEM_CTX: u8 = 5;
    pub const FEM_POWER: u8 = 7;
    pub const LORA_NSS: u8 = 8;
    pub const LORA_SCLK: u8 = 9;
    pub const LORA_MOSI: u8 = 10;
    pub const LORA_MISO: u8 = 11;
    pub const LORA_RESET: u8 = 12;
    pub const LORA_BUSY: u8 = 13;
    pub const LORA_DIO1: u8 = 14;
    pub const USB_D_MINUS: u8 = 19;
    pub const USB_D_PLUS: u8 = 20;
    pub const TFT_BACKLIGHT: u8 = 21;
    pub const GNSS_MCU_RX: u8 = 33;
    pub const GNSS_MCU_TX: u8 = 34;
    pub const GNSS_RESET: u8 = 35;
    pub const GNSS_PPS: u8 = 36;
    pub const TFT_CS: u8 = 38;
    pub const TFT_RESET: u8 = 39;
    pub const TFT_DC: u8 = 40;
    pub const TFT_SCLK: u8 = 41;
    pub const TFT_MOSI: u8 = 42;
}

/// Source controlling the KCT8103L CPS/RF-switch input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FemCpsControl {
    /// The SX1262 drives CPS through its DIO2 RF-switch mode.
    Sx1262Dio2,
}

/// CPS is not an ESP32 GPIO on the V2.3 RF path.
pub const FEM_CPS_CONTROL: FemCpsControl = FemCpsControl::Sx1262Dio2;

/// Logic level to apply to a safety-critical output at inert boot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SafeLevel {
    Low,
    High,
}

/// Fail-safe output state used by the default firmware.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioSafeState {
    pub sx1262_reset: SafeLevel,
    pub fem_power: SafeLevel,
    pub fem_csd: SafeLevel,
    pub fem_ctx: SafeLevel,
}

/// Holding reset and the front end low makes the default image RF-inert.
pub const INERT_RADIO_STATE: RadioSafeState = RadioSafeState {
    sx1262_reset: SafeLevel::Low,
    fem_power: SafeLevel::Low,
    fem_csd: SafeLevel::Low,
    fem_ctx: SafeLevel::Low,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radio_pin_assignments_are_unique() {
        let pins = [
            pins::FEM_CSD,
            pins::FEM_CTX,
            pins::FEM_POWER,
            pins::LORA_NSS,
            pins::LORA_SCLK,
            pins::LORA_MOSI,
            pins::LORA_MISO,
            pins::LORA_RESET,
            pins::LORA_BUSY,
            pins::LORA_DIO1,
        ];

        for (index, pin) in pins.iter().enumerate() {
            assert!(!pins[..index].contains(pin), "duplicate GPIO {pin}");
        }
    }

    #[test]
    fn no_transmit_configuration_exists_at_boot() {
        assert!(!core::hint::black_box(TX_ENABLED_BY_DEFAULT));
        assert_eq!(DEFAULT_FREQUENCY_HZ, None);
        assert_eq!(INERT_RADIO_STATE.fem_power, SafeLevel::Low);
        assert_eq!(INERT_RADIO_STATE.fem_csd, SafeLevel::Low);
        assert_eq!(INERT_RADIO_STATE.fem_ctx, SafeLevel::Low);
        assert_eq!(INERT_RADIO_STATE.sx1262_reset, SafeLevel::Low);
    }
}
