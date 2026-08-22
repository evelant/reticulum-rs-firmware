//! Heltec Vision Master E290 board facts.
//!
//! This is not a BSP. It centralizes the supplied V0.3.1 schematic's pin
//! ownership, the fitted HT-RA62-HF matching range, conservative memory facts,
//! and inert boot policy. Firmware owns the concrete `esp-hal` peripherals.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// Product model described by the supplied design documents.
pub const BOARD_MODEL: &str = "Heltec Vision Master E290";

/// Revision of the schematic used by the board integration.
pub const DOCUMENTED_SCHEMATIC_REVISION: &str = "V0.3.1";

/// Fitted high-frequency radio-module variant required for NA915 operation.
pub const FITTED_RADIO_MODULE: &str = "HT-RA62-HF";

/// Fitted monochrome e-paper panel named by the supplied board datasheet.
pub const FITTED_EINK_PANEL: &str = "DEPG0290BNS800F6 V2.1";

/// Controller identified by the supplied panel module mechanical drawing.
pub const FITTED_EINK_CONTROLLER: &str = "SSD1680Z8";

/// Landscape pixel width exposed to the appliance display renderer.
pub const EINK_WIDTH_PIXELS: usize = 296;

/// Landscape pixel height exposed to the appliance display renderer.
pub const EINK_HEIGHT_PIXELS: usize = 128;

/// Exact bytes in one fitted-panel monochrome frame.
pub const EINK_MONOCHROME_FRAME_BYTES: usize = EINK_WIDTH_PIXELS * EINK_HEIGHT_PIXELS / 8;

/// Conservative SPI clock used by Heltec's V0.3.1 E290 display example.
pub const EINK_SPI_FREQUENCY_HZ: u32 = 6_000_000;

/// The SSD1680 BUSY output forbids commands while driven high.
pub const EINK_BUSY_ACTIVE_LEVEL: SafeLevel = SafeLevel::High;

/// The fitted panel reset input is asserted low.
pub const EINK_RESET_ACTIVE_LEVEL: SafeLevel = SafeLevel::Low;

/// The board's switched `Ve_3V3` display supply is enabled high.
pub const EINK_POWER_ENABLE_LEVEL: SafeLevel = SafeLevel::High;

/// Conservative PSRAM floor implied by the schematic's ESP32-S3R8 part.
///
/// Firmware still verifies the mapped capacity before using it.
pub const DESIGN_PSRAM_FLOOR_BYTES: usize = 8 * 1024 * 1024;

/// The board-facts layer never enables transmission by default.
pub const TX_ENABLED_BY_DEFAULT: bool = false;

/// No legal operating frequency can be inferred from the board alone.
pub const DEFAULT_FREQUENCY_HZ: Option<u32> = None;

/// No conducted or radiated transmit power is selected by the board alone.
pub const DEFAULT_TX_POWER_DBM: Option<i8> = None;

/// Lowest frequency supported by the fitted HT-RA62-HF matching network.
pub const RF_MATCHING_MIN_HZ: u32 = 863_000_000;

/// Highest frequency supported by the fitted HT-RA62-HF matching network.
pub const RF_MATCHING_MAX_HZ: u32 = 928_000_000;

/// Peripheral owner that must hold the corresponding GPIO capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GpioOwner {
    /// The sole E-Ink display actor.
    Display,
    /// The battery-monitor owner.
    BatteryMonitor,
    /// The sole HT-RA62/SX1262 radio owner.
    Radio,
    /// The QuickLink I2C-bus owner.
    QuickLink,
    /// The sole native-USB controller owner.
    Usb,
    /// The physical user-input owner.
    UserInput,
    /// The exposed UART owner.
    Uart,
}

/// Every internal signal assigned by the supplied E290 pin map and schematic.
///
/// The GPIO number is the discriminant, so duplicate assignments are rejected
/// by the compiler in addition to the exhaustive collision test.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum BoardSignal {
    /// E-Ink serial-data input.
    EinkSdi = 1,
    /// E-Ink serial clock.
    EinkClock = 2,
    /// E-Ink chip select.
    EinkChipSelect = 3,
    /// E-Ink data/command selection.
    EinkDataCommand = 4,
    /// E-Ink reset.
    EinkReset = 5,
    /// E-Ink busy indication.
    EinkBusy = 6,
    /// Battery-voltage ADC input.
    BatteryAdc = 7,
    /// HT-RA62 SPI chip select.
    RadioNss = 8,
    /// HT-RA62 SPI clock.
    RadioSck = 9,
    /// HT-RA62 SPI controller-output/peripheral-input signal.
    RadioMosi = 10,
    /// HT-RA62 SPI controller-input/peripheral-output signal.
    RadioMiso = 11,
    /// HT-RA62 reset.
    RadioReset = 12,
    /// HT-RA62 busy indication.
    RadioBusy = 13,
    /// HT-RA62 DIO1 interrupt.
    RadioDio1 = 14,
    /// Active-high enable for the switched E-Ink `Ve_3V3` rail.
    EinkPowerEnable = 18,
    /// Native USB D- routed through the Type-C connector.
    UsbDataMinus = 19,
    /// Native USB D+ routed through the Type-C connector.
    UsbDataPlus = 20,
    /// Active-low user key with an external pull-up.
    UserKey = 21,
    /// QuickLink I2C clock.
    QuickLinkScl = 38,
    /// QuickLink I2C data.
    QuickLinkSda = 39,
    /// Exposed UART transmit signal.
    UartTx = 43,
    /// Exposed UART receive signal.
    UartRx = 44,
}

impl BoardSignal {
    /// ESP32-S3 GPIO number assigned to this signal.
    pub const fn gpio(self) -> u8 {
        self as u8
    }

    /// Sole peripheral owner for this signal.
    pub const fn owner(self) -> GpioOwner {
        match self {
            Self::EinkSdi
            | Self::EinkClock
            | Self::EinkChipSelect
            | Self::EinkDataCommand
            | Self::EinkReset
            | Self::EinkBusy
            | Self::EinkPowerEnable => GpioOwner::Display,
            Self::BatteryAdc => GpioOwner::BatteryMonitor,
            Self::RadioNss
            | Self::RadioSck
            | Self::RadioMosi
            | Self::RadioMiso
            | Self::RadioReset
            | Self::RadioBusy
            | Self::RadioDio1 => GpioOwner::Radio,
            Self::UsbDataMinus | Self::UsbDataPlus => GpioOwner::Usb,
            Self::UserKey => GpioOwner::UserInput,
            Self::QuickLinkScl | Self::QuickLinkSda => GpioOwner::QuickLink,
            Self::UartTx | Self::UartRx => GpioOwner::Uart,
        }
    }
}

/// Exhaustive set of internal GPIO assignments described by this crate.
pub const BOARD_SIGNALS: [BoardSignal; 22] = [
    BoardSignal::EinkSdi,
    BoardSignal::EinkClock,
    BoardSignal::EinkChipSelect,
    BoardSignal::EinkDataCommand,
    BoardSignal::EinkReset,
    BoardSignal::EinkBusy,
    BoardSignal::BatteryAdc,
    BoardSignal::RadioNss,
    BoardSignal::RadioSck,
    BoardSignal::RadioMosi,
    BoardSignal::RadioMiso,
    BoardSignal::RadioReset,
    BoardSignal::RadioBusy,
    BoardSignal::RadioDio1,
    BoardSignal::EinkPowerEnable,
    BoardSignal::UsbDataMinus,
    BoardSignal::UsbDataPlus,
    BoardSignal::UserKey,
    BoardSignal::QuickLinkScl,
    BoardSignal::QuickLinkSda,
    BoardSignal::UartTx,
    BoardSignal::UartRx,
];

/// GPIO assignments as named constants for concrete BSP construction.
pub mod pins {
    use super::BoardSignal;

    /// E-Ink serial-data input.
    pub const EINK_SDI: u8 = BoardSignal::EinkSdi.gpio();
    /// E-Ink serial clock.
    pub const EINK_CLOCK: u8 = BoardSignal::EinkClock.gpio();
    /// E-Ink chip select.
    pub const EINK_CHIP_SELECT: u8 = BoardSignal::EinkChipSelect.gpio();
    /// E-Ink data/command selection.
    pub const EINK_DATA_COMMAND: u8 = BoardSignal::EinkDataCommand.gpio();
    /// E-Ink reset.
    pub const EINK_RESET: u8 = BoardSignal::EinkReset.gpio();
    /// E-Ink busy indication.
    pub const EINK_BUSY: u8 = BoardSignal::EinkBusy.gpio();
    /// Active-high enable for the switched E-Ink `Ve_3V3` rail.
    pub const EINK_POWER_ENABLE: u8 = BoardSignal::EinkPowerEnable.gpio();
    /// Battery-voltage ADC input.
    pub const BATTERY_ADC: u8 = BoardSignal::BatteryAdc.gpio();
    /// HT-RA62 SPI chip select.
    pub const RADIO_NSS: u8 = BoardSignal::RadioNss.gpio();
    /// HT-RA62 SPI clock.
    pub const RADIO_SCK: u8 = BoardSignal::RadioSck.gpio();
    /// HT-RA62 SPI controller-output/peripheral-input signal.
    pub const RADIO_MOSI: u8 = BoardSignal::RadioMosi.gpio();
    /// HT-RA62 SPI controller-input/peripheral-output signal.
    pub const RADIO_MISO: u8 = BoardSignal::RadioMiso.gpio();
    /// HT-RA62 reset.
    pub const RADIO_RESET: u8 = BoardSignal::RadioReset.gpio();
    /// HT-RA62 busy indication.
    pub const RADIO_BUSY: u8 = BoardSignal::RadioBusy.gpio();
    /// HT-RA62 DIO1 interrupt.
    pub const RADIO_DIO1: u8 = BoardSignal::RadioDio1.gpio();
    /// Native USB D-.
    pub const USB_DATA_MINUS: u8 = BoardSignal::UsbDataMinus.gpio();
    /// Native USB D+.
    pub const USB_DATA_PLUS: u8 = BoardSignal::UsbDataPlus.gpio();
    /// Active-low user key.
    pub const USER_KEY: u8 = BoardSignal::UserKey.gpio();
    /// QuickLink I2C clock.
    pub const QUICKLINK_SCL: u8 = BoardSignal::QuickLinkScl.gpio();
    /// QuickLink I2C data.
    pub const QUICKLINK_SDA: u8 = BoardSignal::QuickLinkSda.gpio();
    /// Exposed UART transmit signal.
    pub const UART_TX: u8 = BoardSignal::UartTx.gpio();
    /// Exposed UART receive signal.
    pub const UART_RX: u8 = BoardSignal::UartRx.gpio();
}

/// Logic level to apply to a safety-critical output at inert boot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SafeLevel {
    /// Drive the output low.
    Low,
    /// Drive the output high.
    High,
}

/// Fail-safe state applied before any radio initialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioSafeState {
    /// Level applied to the active-low SX1262 reset input.
    pub sx1262_reset: SafeLevel,
    /// Level applied to the active-low SPI chip-select input.
    pub sx1262_nss: SafeLevel,
}

/// Reset-low and NSS-high contain the HT-RA62 without external FEM controls.
pub const INERT_RADIO_STATE: RadioSafeState = RadioSafeState {
    sx1262_reset: SafeLevel::Low,
    sx1262_nss: SafeLevel::High,
};

#[cfg(test)]
mod tests {
    use super::*;

    const EXPECTED_GPIOS: [u8; 22] = [
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 18, 19, 20, 21, 38, 39, 43, 44,
    ];

    #[test]
    fn documented_gpio_map_is_exact_and_collision_free() {
        let actual = BOARD_SIGNALS.map(BoardSignal::gpio);
        assert_eq!(actual, EXPECTED_GPIOS);

        for (index, gpio) in actual.iter().enumerate() {
            assert!(!actual[..index].contains(gpio), "duplicate GPIO {gpio}");
        }
    }

    #[test]
    fn gpio_ownership_is_exhaustive_and_exact() {
        let actual = BOARD_SIGNALS.map(BoardSignal::owner);
        assert_eq!(actual[..6], [GpioOwner::Display; 6]);
        assert_eq!(actual[6], GpioOwner::BatteryMonitor);
        assert_eq!(actual[7..14], [GpioOwner::Radio; 7]);
        assert_eq!(actual[14], GpioOwner::Display);
        assert_eq!(actual[15..17], [GpioOwner::Usb; 2]);
        assert_eq!(actual[17], GpioOwner::UserInput);
        assert_eq!(actual[18..20], [GpioOwner::QuickLink; 2]);
        assert_eq!(actual[20..], [GpioOwner::Uart; 2]);
    }

    #[test]
    fn radio_pins_are_disjoint_from_display_and_battery_pins() {
        let display_and_battery = &EXPECTED_GPIOS[..7];
        let radio = &EXPECTED_GPIOS[7..14];
        assert_eq!(display_and_battery, &[1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(radio, &[8, 9, 10, 11, 12, 13, 14]);
        assert!(
            radio
                .iter()
                .all(|radio_gpio| !display_and_battery.contains(radio_gpio))
        );
    }

    #[test]
    fn named_pin_constants_match_every_signal() {
        let named = [
            pins::EINK_SDI,
            pins::EINK_CLOCK,
            pins::EINK_CHIP_SELECT,
            pins::EINK_DATA_COMMAND,
            pins::EINK_RESET,
            pins::EINK_BUSY,
            pins::BATTERY_ADC,
            pins::RADIO_NSS,
            pins::RADIO_SCK,
            pins::RADIO_MOSI,
            pins::RADIO_MISO,
            pins::RADIO_RESET,
            pins::RADIO_BUSY,
            pins::RADIO_DIO1,
            pins::EINK_POWER_ENABLE,
            pins::USB_DATA_MINUS,
            pins::USB_DATA_PLUS,
            pins::USER_KEY,
            pins::QUICKLINK_SCL,
            pins::QUICKLINK_SDA,
            pins::UART_TX,
            pins::UART_RX,
        ];
        assert_eq!(named, EXPECTED_GPIOS);
    }

    #[test]
    fn radio_boot_policy_is_inert_and_has_no_implicit_profile() {
        assert_eq!(INERT_RADIO_STATE.sx1262_reset, SafeLevel::Low);
        assert_eq!(INERT_RADIO_STATE.sx1262_nss, SafeLevel::High);
        assert!(!core::hint::black_box(TX_ENABLED_BY_DEFAULT));
        assert_eq!(DEFAULT_FREQUENCY_HZ, None);
        assert_eq!(DEFAULT_TX_POWER_DBM, None);
    }

    #[test]
    fn fitted_range_and_design_memory_floor_are_exact() {
        assert_eq!(FITTED_RADIO_MODULE, "HT-RA62-HF");
        assert_eq!(RF_MATCHING_MIN_HZ, 863_000_000);
        assert_eq!(RF_MATCHING_MAX_HZ, 928_000_000);
        assert_eq!(DESIGN_PSRAM_FLOOR_BYTES, 8_388_608);
    }

    #[test]
    fn fitted_display_contract_matches_supplied_board_and_panel_documents() {
        assert_eq!(FITTED_EINK_PANEL, "DEPG0290BNS800F6 V2.1");
        assert_eq!(FITTED_EINK_CONTROLLER, "SSD1680Z8");
        assert_eq!((EINK_WIDTH_PIXELS, EINK_HEIGHT_PIXELS), (296, 128));
        assert_eq!(EINK_MONOCHROME_FRAME_BYTES, 4_736);
        assert_eq!(EINK_SPI_FREQUENCY_HZ, 6_000_000);
        assert_eq!(EINK_BUSY_ACTIVE_LEVEL, SafeLevel::High);
        assert_eq!(EINK_RESET_ACTIVE_LEVEL, SafeLevel::Low);
        assert_eq!(EINK_POWER_ENABLE_LEVEL, SafeLevel::High);
    }
}
