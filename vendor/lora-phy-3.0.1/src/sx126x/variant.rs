use super::DeviceSel;

/// Atomic board-specific override for the SX1262 high-power PA command set.
///
/// `tx_params_power` is the raw signed two's-complement power byte accepted by
/// `SetTxParams`. `ocp` is the raw six-bit OCP trim written to register
/// `0x08E7` after `SetPaConfig`, when present.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HighPowerPaOverride {
    /// `SetPaConfig` PA duty-cycle byte (at most `0x04` for SX1262).
    pub pa_duty_cycle: u8,
    /// `SetPaConfig` high-power maximum byte (at most `0x07`).
    pub hp_max: u8,
    /// Raw signed two's-complement `SetTxParams` power byte.
    pub tx_params_power: u8,
    /// Optional raw six-bit OCP trim for register `0x08E7`.
    pub ocp: Option<u8>,
}

/// Implement this trait on your custom variant or use provided impls
pub trait Sx126xVariant {
    /// whether to use high or low power PA
    fn get_device_sel(&self) -> DeviceSel;

    /// use dio2 as rf switch output
    fn use_dio2_as_rfswitch(&self) -> bool {
        true
    }

    /// Override the complete high-power PA command set for this requested and
    /// driver-clamped output power.
    ///
    /// Existing variants return `None` and retain the driver's original PA,
    /// OCP and encoded-power behavior.
    fn high_power_pa_override(
        &self,
        _requested_output_power: i32,
        _clamped_output_power: i32,
    ) -> Option<HighPowerPaOverride> {
        None
    }
}

/// Sx1261 uses only LowPowerPA
pub struct Sx1261;
impl Sx126xVariant for Sx1261 {
    fn get_device_sel(&self) -> super::DeviceSel {
        super::DeviceSel::LowPowerPA
    }
}

/// Sx1262 uses only HighPowerPA
pub struct Sx1262;

impl Sx126xVariant for Sx1262 {
    fn get_device_sel(&self) -> super::DeviceSel {
        super::DeviceSel::HighPowerPA
    }
}

/// Stm32wl variant.
pub struct Stm32wl {
    /// select which output to use. (Switching is not supported)
    pub use_high_power_pa: bool,
}
impl Sx126xVariant for Stm32wl {
    fn get_device_sel(&self) -> super::DeviceSel {
        if self.use_high_power_pa {
            DeviceSel::HighPowerPA
        } else {
            DeviceSel::LowPowerPA
        }
    }
    fn use_dio2_as_rfswitch(&self) -> bool {
        false
    }
}
