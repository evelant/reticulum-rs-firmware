//! Explicitly hazardous bidirectional SX1262 owner for Tracker V2 HIL tests.
//!
//! This crate is deliberately separate from the frozen receive-only board
//! boundary. It is not a product radio API. The first hardware qualification
//! is fixed to NA915 and the lowest calibrated Tracker antenna-path setting:
//! 14 dBm effective target power, produced by 0 dBm at the SX1262 plus the
//! characterized external-FEM gain. An explicit diagnostic-only feature can
//! instead request the SX1262's -9 dBm minimum to test a colocated receiver
//! for overload; that antenna-path result is not treated as calibrated.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::cell::Cell;

use critical_section::Mutex;
use embedded_hal::digital::{OutputPin, StatefulOutputPin};
use embedded_hal_async::{delay::DelayNs, digital::Wait, spi::SpiDevice};
use lora_phy::{
    LoRa, RxMode,
    mod_params::{ModulationParams, PacketParams, RadioError},
    mod_traits::InterfaceVariant,
    sx126x::{Config, DeviceSel, HighPowerPaOverride, Sx126x, Sx126xVariant, TcxoCtrlVoltage},
};
use reticulum_board_heltec_tracker_v2::{
    FEM_CSD_SETTLE_MS, FEM_POWER_SETTLE_MS, validate_lab_rx_profile,
};
use reticulum_radio_interface::{LabRxProfile, LabRxProfileConfig, SX1262_FRAME_MTU};

/// Only calibrated antenna-path target power admitted by the first TX HIL.
#[cfg(not(feature = "near-field-attenuation-hil"))]
pub const TRACKER_TX_HIL_TARGET_POWER_DBM: i32 = 14;

/// SX1262 output corresponding to the calibrated 14 dBm antenna-path target.
#[cfg(not(feature = "near-field-attenuation-hil"))]
pub const TRACKER_TX_HIL_MODEM_OUTPUT_DBM: i32 = 0;

/// Diagnostic antenna-path target estimate used only for near-field testing.
#[cfg(feature = "near-field-attenuation-hil")]
pub const TRACKER_TX_HIL_TARGET_POWER_DBM: i32 = 5;

/// Minimum SX1262 output used only for diagnostic near-field attenuation.
#[cfg(feature = "near-field-attenuation-hil")]
pub const TRACKER_TX_HIL_MODEM_OUTPUT_DBM: i32 = -9;

/// Power profile compiled into this isolated HIL image.
#[cfg(not(feature = "near-field-attenuation-hil"))]
pub const TRACKER_TX_HIL_POWER_PROFILE: &str = "calibrated-minimum";

/// Power profile compiled into this isolated HIL image.
#[cfg(feature = "near-field-attenuation-hil")]
pub const TRACKER_TX_HIL_POWER_PROFILE: &str = "near-field-attenuation-uncalibrated";

/// Maximum symbol timeout accepted by the pinned SX1262 driver.
pub const TRACKER_TX_HIL_RX_SYMBOL_TIMEOUT: u16 = 248;

/// SX1262 standby clock used by the pinned driver.
pub const TRACKER_TX_HIL_STANDBY_CLOCK: &str = "rc";

/// External FEM CTX assertion point used by this isolated HIL experiment.
pub const TRACKER_TX_HIL_CTX_ASSERTION: &str = "before-packet-and-fifo-prepare";

/// External FEM power policy used by this isolated HIL experiment.
pub const TRACKER_TX_HIL_FEM_POWER_POLICY: &str = "prepowered-during-radio-init";

struct TrackerTxHilSx1262;

impl Sx126xVariant for TrackerTxHilSx1262 {
    fn get_device_sel(&self) -> DeviceSel {
        DeviceSel::HighPowerPA
    }

    fn high_power_pa_override(
        &self,
        requested_output_power: i32,
        clamped_output_power: i32,
    ) -> Option<HighPowerPaOverride> {
        if requested_output_power == TRACKER_TX_HIL_MODEM_OUTPUT_DBM
            && clamped_output_power == TRACKER_TX_HIL_MODEM_OUTPUT_DBM
        {
            Some(HighPowerPaOverride {
                pa_duty_cycle: 0x04,
                hp_max: 0x07,
                tx_params_power: TRACKER_TX_HIL_MODEM_OUTPUT_DBM as u8,
                ocp: Some(0x28),
            })
        } else {
            None
        }
    }
}

/// The sole radio profile admitted by the first Tracker V2 TX HIL boundary.
///
/// This is deliberately constructed inside the board-specific crate so a
/// caller cannot validate a different frequency against an unrelated radio
/// range and then pass it into a transmit-capable owner.
pub const TRACKER_TX_HIL_PROFILE: LabRxProfile = match validate_lab_rx_profile(LabRxProfileConfig {
    frequency_hz: Some(915_000_000),
    spreading_factor: 7,
    bandwidth_hz: 125_000,
    coding_rate_denominator: 5,
    // Pinned RNode 1.86 selects 24 symbols for SF7/BW125. The SX1261/2
    // receiver periodically restarts preamble detection, so the datasheet
    // requires the receiver and transmitter preamble lengths to match.
    preamble_symbols: 24,
    explicit_header: true,
    crc: true,
    iq_inverted: false,
}) {
    Ok(profile) => profile,
    Err(_) => panic!("invalid fixed Tracker V2 TX HIL profile"),
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TrackerTxArmState {
    Disarmed,
    ArmedForPrepare,
    Prepared,
}

/// Shared one-shot authorization consumed across the two private TX hooks.
///
/// Firmware must place this value in static storage and pass it to the radio
/// constructor. Callers cannot arm it directly. Only [`TrackerTxHilRadio`]
/// can authorize CTX assertion during packet preparation; the final transmit
/// hook then consumes the prepared state immediately before `SetTx`.
pub struct TrackerTxArm {
    state: Mutex<Cell<TrackerTxArmState>>,
}

impl TrackerTxArm {
    /// Construct a disarmed reset-scoped authorization cell.
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(Cell::new(TrackerTxArmState::Disarmed)),
        }
    }

    fn disarm(&self) {
        critical_section::with(|cs| {
            self.state.borrow(cs).set(TrackerTxArmState::Disarmed);
        });
    }

    fn arm_for_prepare(&self) {
        critical_section::with(|cs| {
            self.state
                .borrow(cs)
                .set(TrackerTxArmState::ArmedForPrepare);
        });
    }

    fn transition(&self, expected: TrackerTxArmState, next: TrackerTxArmState) -> bool {
        critical_section::with(|cs| {
            let state = self.state.borrow(cs);
            if state.get() != expected {
                return false;
            }
            state.set(next);
            true
        })
    }

    fn consume_prepare_arm(&self) -> bool {
        self.transition(
            TrackerTxArmState::ArmedForPrepare,
            TrackerTxArmState::Prepared,
        )
    }

    fn consume_prepared(&self) -> bool {
        self.transition(TrackerTxArmState::Prepared, TrackerTxArmState::Disarmed)
    }
}

impl Default for TrackerTxArm {
    fn default() -> Self {
        Self::new()
    }
}

/// Operation in which a bidirectional HIL radio failure occurred.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrackerTxHilOperation {
    /// Initial fail-closed pin ownership while the FEM remains disabled.
    InterfaceConstruction,
    /// SX1262 reset and LoRa initialization.
    ChipInitialization,
    /// Modulation parameter construction.
    ModulationConfiguration,
    /// Packet parameter construction.
    PacketConfiguration,
    /// Caller supplied an invalid physical frame.
    FrameValidation,
    /// FIFO, packet, channel, and power preparation.
    PrepareTransmit,
    /// One-shot arm consumption and physical transmission.
    Transmit,
    /// Post-TX standby and RF-switch cleanup.
    TransmitCleanup,
    /// Bounded receive preparation.
    PrepareReceive,
    /// Bounded receive execution.
    Receive,
}

/// Failure from the guarded Tracker bidirectional HIL boundary.
#[derive(Debug)]
pub struct TrackerTxHilError {
    /// Exact operation that failed.
    pub operation: TrackerTxHilOperation,
    /// Pinned driver classification, when failure came from the radio path.
    pub radio: Option<RadioError>,
}

impl TrackerTxHilError {
    const fn invalid_frame() -> Self {
        Self {
            operation: TrackerTxHilOperation::FrameValidation,
            radio: None,
        }
    }

    const fn radio(operation: TrackerTxHilOperation, radio: RadioError) -> Self {
        Self {
            operation,
            radio: Some(radio),
        }
    }
}

/// One physical frame copied out of a caller-deadline-bounded receive window.
#[derive(Clone, Copy, Debug)]
pub struct TrackerTxHilReceivedFrame {
    /// Number of valid bytes in the caller's buffer.
    pub len: u8,
    /// Packet RSSI reported by the SX1262 driver.
    pub rssi_dbm: i16,
    /// Packet SNR reported by the SX1262 driver.
    pub snr_db: i16,
}

type TrackerLoRa<Spi, Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay, RadioDelay> = LoRa<
    Sx126x<
        Spi,
        TrackerTxHilInterface<Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay>,
        TrackerTxHilSx1262,
    >,
    RadioDelay,
>;

struct Active<Spi, Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay, RadioDelay>
where
    Spi: SpiDevice<u8>,
    Reset: OutputPin,
    Dio1: Wait,
    Busy: Wait,
    Power: OutputPin,
    Csd: OutputPin,
    Ctx: StatefulOutputPin,
    FemDelay: DelayNs,
    RadioDelay: DelayNs,
{
    lora: TrackerLoRa<Spi, Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay, RadioDelay>,
    modulation: ModulationParams,
    tx_packet: PacketParams,
    rx_packet: PacketParams,
}

type OptionalActive<Spi, Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay, RadioDelay> =
    Option<Active<Spi, Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay, RadioDelay>>;

/// Opaque, reset-scoped Tracker V2 TX/RX owner for bounded HIL use only.
///
/// Every async operation takes the active radio out of this wrapper. If the
/// future is cancelled, the local owner is dropped and its private interface
/// first asserts SX1262 reset and disables CSD, then drives CTX and VFEM power
/// low. The wrapper remains permanently faulted instead of reusing uncertain
/// hardware state.
pub struct TrackerTxHilRadio<Spi, Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay, RadioDelay>
where
    Spi: SpiDevice<u8>,
    Reset: OutputPin,
    Dio1: Wait,
    Busy: Wait,
    Power: OutputPin,
    Csd: OutputPin,
    Ctx: StatefulOutputPin,
    FemDelay: DelayNs,
    RadioDelay: DelayNs,
{
    active: OptionalActive<Spi, Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay, RadioDelay>,
    tx_arm: &'static TrackerTxArm,
}

impl<Spi, Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay, RadioDelay>
    TrackerTxHilRadio<Spi, Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay, RadioDelay>
where
    Spi: SpiDevice<u8>,
    Reset: OutputPin,
    Dio1: Wait,
    Busy: Wait,
    Power: OutputPin,
    Csd: OutputPin,
    Ctx: StatefulOutputPin,
    FemDelay: DelayNs,
    RadioDelay: DelayNs,
{
    /// Establish fail-closed pin levels, initialize the SX1262, then power and
    /// settle the external FEM in receive state.
    ///
    /// FEM power-up occurs only after SX1262 initialization has configured
    /// DIO2 RF-switch control. It then remains powered across operations while
    /// CTX selects receive or authorized transmit state. The exact NA915
    /// profile is fixed inside this board-specific boundary.
    #[allow(
        clippy::too_many_arguments,
        reason = "every safety-critical peripheral is consumed exactly once"
    )]
    pub async fn new(
        spi: Spi,
        reset: Reset,
        dio1: Dio1,
        busy: Busy,
        power: Power,
        csd: Csd,
        ctx: Ctx,
        radio_delay: RadioDelay,
        fem_delay: FemDelay,
        tx_arm: &'static TrackerTxArm,
    ) -> Result<Self, TrackerTxHilError> {
        tx_arm.disarm();
        let interface =
            TrackerTxHilInterface::new(reset, dio1, busy, power, csd, ctx, fem_delay, tx_arm)
                .map_err(|radio| {
                    TrackerTxHilError::radio(TrackerTxHilOperation::InterfaceConstruction, radio)
                })?;
        let sx1262 = Sx126x::new(
            spi,
            interface,
            Config {
                chip: TrackerTxHilSx1262,
                tcxo_ctrl: Some(TcxoCtrlVoltage::Ctrl1V8),
                use_dcdc: false,
                rx_boost: false,
            },
        );
        let mut lora = LoRa::new(sx1262, false, radio_delay)
            .await
            .map_err(|radio| {
                TrackerTxHilError::radio(TrackerTxHilOperation::ChipInitialization, radio)
            })?;
        let modulation = lora
            .create_modulation_params(
                TRACKER_TX_HIL_PROFILE.spreading_factor(),
                TRACKER_TX_HIL_PROFILE.bandwidth(),
                TRACKER_TX_HIL_PROFILE.coding_rate(),
                TRACKER_TX_HIL_PROFILE.frequency_hz(),
            )
            .map_err(|radio| {
                TrackerTxHilError::radio(TrackerTxHilOperation::ModulationConfiguration, radio)
            })?;
        let tx_packet = lora
            .create_tx_packet_params(
                TRACKER_TX_HIL_PROFILE.preamble_symbols(),
                !TRACKER_TX_HIL_PROFILE.explicit_header(),
                TRACKER_TX_HIL_PROFILE.crc(),
                TRACKER_TX_HIL_PROFILE.iq_inverted(),
                &modulation,
            )
            .map_err(|radio| {
                TrackerTxHilError::radio(TrackerTxHilOperation::PacketConfiguration, radio)
            })?;
        let rx_packet = lora
            .create_rx_packet_params(
                TRACKER_TX_HIL_PROFILE.preamble_symbols(),
                !TRACKER_TX_HIL_PROFILE.explicit_header(),
                SX1262_FRAME_MTU as u8,
                TRACKER_TX_HIL_PROFILE.crc(),
                TRACKER_TX_HIL_PROFILE.iq_inverted(),
                &modulation,
            )
            .map_err(|radio| {
                TrackerTxHilError::radio(TrackerTxHilOperation::PacketConfiguration, radio)
            })?;

        Ok(Self {
            active: Some(Active {
                lora,
                modulation,
                tx_packet,
                rx_packet,
            }),
            tx_arm,
        })
    }

    /// Transmit exactly one physical frame at the fixed 14 dBm HIL setting.
    ///
    /// The private interface consumes a one-shot arm and asserts CTX after
    /// modem/power/standby preparation but before packet parameters and FIFO
    /// writes. Its final transmit hook consumes the prepared state immediately
    /// before `SetTx`. Successful `TxDone` is followed by an explicit standby
    /// call because pinned `lora-phy` does not disable the RF switch on its
    /// successful TX path.
    pub async fn transmit_frame(&mut self, frame: &[u8]) -> Result<(), TrackerTxHilError> {
        if frame.is_empty() || frame.len() > SX1262_FRAME_MTU {
            return Err(TrackerTxHilError::invalid_frame());
        }
        let mut active = self.active.take().ok_or_else(|| {
            TrackerTxHilError::radio(
                TrackerTxHilOperation::Transmit,
                RadioError::InvalidRadioMode,
            )
        })?;
        self.tx_arm.disarm();
        self.tx_arm.arm_for_prepare();
        if let Err(radio) = active
            .lora
            .prepare_for_tx(
                &active.modulation,
                &mut active.tx_packet,
                TRACKER_TX_HIL_MODEM_OUTPUT_DBM,
                frame,
            )
            .await
        {
            return Err(TrackerTxHilError::radio(
                TrackerTxHilOperation::PrepareTransmit,
                radio,
            ));
        }
        if let Err(radio) = active.lora.tx().await {
            self.tx_arm.disarm();
            return Err(TrackerTxHilError::radio(
                TrackerTxHilOperation::Transmit,
                radio,
            ));
        }
        self.tx_arm.disarm();
        if let Err(radio) = active.lora.enter_standby().await {
            return Err(TrackerTxHilError::radio(
                TrackerTxHilOperation::TransmitCleanup,
                radio,
            ));
        }
        self.active = Some(active);
        Ok(())
    }

    /// Run one SX1262 receive operation with a bounded preamble search.
    ///
    /// The symbol timeout bounds only detection before a preamble. Firmware
    /// must wrap this future in an outer monotonic deadline because the SX1262
    /// stops its timer after preamble detection. Cancelling that outer future
    /// drops the active owner fail-closed. A normal no-preamble symbol timeout
    /// returns `Ok(None)` and leaves the owner usable; every other error drops
    /// it into the fail-closed state.
    pub async fn receive_frame(
        &mut self,
        buffer: &mut [u8; SX1262_FRAME_MTU],
    ) -> Result<Option<TrackerTxHilReceivedFrame>, TrackerTxHilError> {
        let mut active = self.active.take().ok_or_else(|| {
            TrackerTxHilError::radio(TrackerTxHilOperation::Receive, RadioError::InvalidRadioMode)
        })?;
        self.tx_arm.disarm();
        if let Err(radio) = active
            .lora
            .prepare_for_rx(
                RxMode::Single(TRACKER_TX_HIL_RX_SYMBOL_TIMEOUT),
                &active.modulation,
                &active.rx_packet,
            )
            .await
        {
            return Err(TrackerTxHilError::radio(
                TrackerTxHilOperation::PrepareReceive,
                radio,
            ));
        }
        let received = active.lora.rx(&active.rx_packet, buffer).await;
        match received {
            Ok((len, status)) => {
                if let Err(radio) = active.lora.enter_standby().await {
                    return Err(TrackerTxHilError::radio(
                        TrackerTxHilOperation::Receive,
                        radio,
                    ));
                }
                self.active = Some(active);
                Ok(Some(TrackerTxHilReceivedFrame {
                    len,
                    rssi_dbm: status.rssi,
                    snr_db: status.snr,
                }))
            }
            Err(RadioError::ReceiveTimeout) => {
                self.active = Some(active);
                Ok(None)
            }
            Err(radio) => Err(TrackerTxHilError::radio(
                TrackerTxHilOperation::Receive,
                radio,
            )),
        }
    }

    /// Drop the active radio owner into its fail-closed state.
    pub fn shutdown(&mut self) {
        self.tx_arm.disarm();
        self.active.take();
    }

    /// Whether this wrapper still owns initialized radio hardware.
    pub const fn is_active(&self) -> bool {
        self.active.is_some()
    }
}

struct TrackerTxHilInterface<Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay>
where
    Reset: OutputPin,
    Power: OutputPin,
    Csd: OutputPin,
    Ctx: StatefulOutputPin,
    FemDelay: DelayNs,
{
    reset: Reset,
    dio1: Dio1,
    busy: Busy,
    power: Power,
    csd: Csd,
    ctx: Ctx,
    fem_delay: FemDelay,
    fem_enabled: bool,
    tx_arm: &'static TrackerTxArm,
}

impl<Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay>
    TrackerTxHilInterface<Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay>
where
    Reset: OutputPin,
    Power: OutputPin,
    Csd: OutputPin,
    Ctx: StatefulOutputPin,
    FemDelay: DelayNs,
{
    #[allow(
        clippy::too_many_arguments,
        reason = "the private interface must own every radio and FEM control plus its arm"
    )]
    fn new(
        reset: Reset,
        dio1: Dio1,
        busy: Busy,
        power: Power,
        csd: Csd,
        ctx: Ctx,
        fem_delay: FemDelay,
        tx_arm: &'static TrackerTxArm,
    ) -> Result<Self, RadioError> {
        let mut owner = Self {
            reset,
            dio1,
            busy,
            power,
            csd,
            ctx,
            fem_delay,
            fem_enabled: false,
            tx_arm,
        };
        owner.force_disabled()?;
        Ok(owner)
    }

    async fn ensure_fem_enabled(&mut self) -> Result<(), RadioError> {
        if self.fem_enabled {
            return Ok(());
        }

        // DIO2 RF-switch control has already been initialized by LoRa::new.
        // Keep CTX in receive position while the external FEM powers up.
        if let Err(error) = self.set_ctx_low() {
            let _ = self.force_disabled();
            return Err(error);
        }
        if self.power.set_high().is_err() {
            let _ = self.force_disabled();
            return Err(RadioError::RfSwitchRx);
        }
        self.fem_delay.delay_ms(FEM_POWER_SETTLE_MS).await;
        if self.csd.set_high().is_err() {
            let _ = self.force_disabled();
            return Err(RadioError::RfSwitchRx);
        }
        self.fem_delay.delay_ms(FEM_CSD_SETTLE_MS).await;
        if let Err(error) = self.set_ctx_low() {
            let _ = self.force_disabled();
            return Err(error);
        }
        self.fem_enabled = true;
        Ok(())
    }

    fn set_ctx_low(&mut self) -> Result<(), RadioError> {
        self.ctx.set_low().map_err(|_| RadioError::RfSwitchRx)?;
        if !self.ctx.is_set_low().map_err(|_| RadioError::RfSwitchRx)? {
            return Err(RadioError::RfSwitchRx);
        }
        Ok(())
    }

    fn set_ctx_high(&mut self) -> Result<(), RadioError> {
        self.ctx.set_high().map_err(|_| RadioError::RfSwitchTx)?;
        if !self.ctx.is_set_high().map_err(|_| RadioError::RfSwitchTx)? {
            return Err(RadioError::RfSwitchTx);
        }
        Ok(())
    }

    fn force_disabled(&mut self) -> Result<(), RadioError> {
        self.tx_arm.disarm();
        // Quench the SX1262 and external amplifier before changing CTX from a
        // possible active-TX position. This order is also used by Drop when an
        // outer software deadline cancels a TX/RX future.
        let reset = self.reset.set_low().map_err(|_| RadioError::Reset);
        let csd = self.csd.set_low().map_err(|_| RadioError::RfSwitchRx);
        let ctx = self.ctx.set_low().map_err(|_| RadioError::RfSwitchRx);
        let power = self.power.set_low().map_err(|_| RadioError::RfSwitchRx);
        self.fem_enabled = false;
        reset.and(csd).and(ctx).and(power)
    }
}

impl<Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay> InterfaceVariant
    for TrackerTxHilInterface<Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay>
where
    Reset: OutputPin,
    Dio1: Wait,
    Busy: Wait,
    Power: OutputPin,
    Csd: OutputPin,
    Ctx: StatefulOutputPin,
    FemDelay: DelayNs,
{
    async fn reset(&mut self, delay: &mut impl DelayNs) -> Result<(), RadioError> {
        self.tx_arm.disarm();
        self.set_ctx_low()?;
        delay.delay_ms(10).await;
        self.reset.set_low().map_err(|_| RadioError::Reset)?;
        delay.delay_ms(20).await;
        self.reset.set_high().map_err(|_| RadioError::Reset)?;
        delay.delay_ms(10).await;
        Ok(())
    }

    async fn wait_on_busy(&mut self) -> Result<(), RadioError> {
        self.busy.wait_for_low().await.map_err(|_| RadioError::Busy)
    }

    async fn await_irq(&mut self) -> Result<(), RadioError> {
        self.dio1
            .wait_for_high()
            .await
            .map_err(|_| RadioError::DIO1)
    }

    async fn complete_rf_switch_initialization(&mut self) -> Result<(), RadioError> {
        self.ensure_fem_enabled().await?;
        self.set_ctx_low()
    }

    async fn enable_rf_switch_rx(&mut self) -> Result<(), RadioError> {
        self.tx_arm.disarm();
        self.ensure_fem_enabled().await?;
        self.set_ctx_low()
    }

    async fn prepare_rf_switch_tx(&mut self) -> Result<(), RadioError> {
        if !self.tx_arm.consume_prepare_arm() {
            self.set_ctx_low()?;
            return Err(RadioError::InvalidConfiguration);
        }
        self.ensure_fem_enabled().await?;
        if let Err(error) = self.set_ctx_high() {
            let _ = self.force_disabled();
            return Err(error);
        }
        Ok(())
    }

    async fn enable_rf_switch_tx(&mut self) -> Result<(), RadioError> {
        if !self.tx_arm.consume_prepared() || !self.fem_enabled {
            self.set_ctx_low()?;
            return Err(RadioError::InvalidConfiguration);
        }
        if !self.ctx.is_set_high().map_err(|_| RadioError::RfSwitchTx)? {
            self.set_ctx_low()?;
            return Err(RadioError::RfSwitchTx);
        }
        Ok(())
    }

    async fn disable_rf_switch(&mut self) -> Result<(), RadioError> {
        self.tx_arm.disarm();
        self.set_ctx_low()
    }
}

impl<Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay> Drop
    for TrackerTxHilInterface<Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay>
where
    Reset: OutputPin,
    Power: OutputPin,
    Csd: OutputPin,
    Ctx: StatefulOutputPin,
    FemDelay: DelayNs,
{
    fn drop(&mut self) {
        let _ = self.force_disabled();
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::{cell::RefCell, convert::Infallible, future::Future, task::Poll};
    use std::{boxed::Box, rc::Rc, vec::Vec};

    use embedded_hal::{
        digital::{ErrorType, PinState},
        spi::{ErrorType as SpiErrorType, Operation},
    };
    use lora_phy::sx126x::Sx1262;

    use super::*;

    fn block_on<F: Future>(future: F) -> F::Output {
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let mut future = Box::pin(future);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(output) => return output,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    #[derive(Clone)]
    struct PinProbe(Rc<core::cell::Cell<bool>>);

    impl PinProbe {
        fn is_high(&self) -> bool {
            self.0.get()
        }
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct PinEvent {
        pin: &'static str,
        high: bool,
    }

    type EventLog = Rc<RefCell<Vec<PinEvent>>>;

    struct MockOutput {
        probe: PinProbe,
        pin: &'static str,
        events: EventLog,
    }

    impl MockOutput {
        fn new(initial: PinState, pin: &'static str, events: EventLog) -> (Self, PinProbe) {
            let probe = PinProbe(Rc::new(core::cell::Cell::new(initial == PinState::High)));
            (
                Self {
                    probe: probe.clone(),
                    pin,
                    events,
                },
                probe,
            )
        }
    }

    impl ErrorType for MockOutput {
        type Error = Infallible;
    }

    impl OutputPin for MockOutput {
        fn set_low(&mut self) -> Result<(), Self::Error> {
            self.probe.0.set(false);
            self.events.borrow_mut().push(PinEvent {
                pin: self.pin,
                high: false,
            });
            Ok(())
        }

        fn set_high(&mut self) -> Result<(), Self::Error> {
            self.probe.0.set(true);
            self.events.borrow_mut().push(PinEvent {
                pin: self.pin,
                high: true,
            });
            Ok(())
        }
    }

    impl StatefulOutputPin for MockOutput {
        fn is_set_high(&mut self) -> Result<bool, Self::Error> {
            Ok(self.probe.is_high())
        }

        fn is_set_low(&mut self) -> Result<bool, Self::Error> {
            Ok(!self.probe.is_high())
        }
    }

    struct MockWait;

    impl ErrorType for MockWait {
        type Error = Infallible;
    }

    impl Wait for MockWait {
        async fn wait_for_high(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }

        async fn wait_for_low(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }

        async fn wait_for_rising_edge(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }

        async fn wait_for_falling_edge(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }

        async fn wait_for_any_edge(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    struct NoopDelay;

    impl DelayNs for NoopDelay {
        async fn delay_ns(&mut self, _ns: u32) {}
    }

    type DelayLog = Rc<RefCell<Vec<u32>>>;

    struct ProbeDelay {
        delays_ns: DelayLog,
    }

    impl ProbeDelay {
        fn new() -> (Self, DelayLog) {
            let delays_ns = Rc::new(RefCell::new(Vec::new()));
            (
                Self {
                    delays_ns: delays_ns.clone(),
                },
                delays_ns,
            )
        }
    }

    impl DelayNs for ProbeDelay {
        async fn delay_ns(&mut self, ns: u32) {
            self.delays_ns.borrow_mut().push(ns);
        }
    }

    type CommandLog = Rc<RefCell<Vec<Vec<u8>>>>;
    type CommandCtxLog = Rc<RefCell<Vec<bool>>>;

    struct MockSpi {
        commands: CommandLog,
        command_ctx: CommandCtxLog,
        ctx: PinProbe,
    }

    impl MockSpi {
        fn new(ctx: PinProbe) -> (Self, CommandLog, CommandCtxLog) {
            let commands = Rc::new(RefCell::new(Vec::new()));
            let command_ctx = Rc::new(RefCell::new(Vec::new()));
            (
                Self {
                    commands: commands.clone(),
                    command_ctx: command_ctx.clone(),
                    ctx,
                },
                commands,
                command_ctx,
            )
        }
    }

    impl SpiErrorType for MockSpi {
        type Error = Infallible;
    }

    impl embedded_hal_async::spi::SpiDevice<u8> for MockSpi {
        async fn transaction(
            &mut self,
            operations: &mut [Operation<'_, u8>],
        ) -> Result<(), Self::Error> {
            let mut command = Vec::new();
            for operation in operations.iter() {
                match operation {
                    Operation::Write(bytes) => command.extend_from_slice(bytes),
                    Operation::Transfer(_, bytes) => command.extend_from_slice(bytes),
                    Operation::TransferInPlace(bytes) => command.extend_from_slice(bytes),
                    Operation::Read(_) | Operation::DelayNs(_) => {}
                }
            }
            let is_get_irq_status = command.first() == Some(&0x12);
            self.commands.borrow_mut().push(command);
            self.command_ctx.borrow_mut().push(self.ctx.is_high());

            let mut read_index = 0;
            for operation in operations.iter_mut() {
                if let Operation::Read(bytes) = operation {
                    bytes.fill(0);
                    if is_get_irq_status && read_index == 1 && bytes.len() >= 2 {
                        bytes[1] = 0x03;
                    }
                    read_index += 1;
                }
            }
            Ok(())
        }
    }

    type TestInterface = TrackerTxHilInterface<
        MockOutput,
        MockWait,
        MockWait,
        MockOutput,
        MockOutput,
        MockOutput,
        ProbeDelay,
    >;
    type TestLoRa<C> = LoRa<Sx126x<MockSpi, TestInterface, C>, NoopDelay>;

    struct TestRadio<L> {
        lora: L,
        commands: CommandLog,
        command_ctx: CommandCtxLog,
        arm: &'static TrackerTxArm,
        ctx: PinProbe,
        power: PinProbe,
        csd: PinProbe,
        fem_delays: DelayLog,
    }

    fn try_build_lora<C>(chip: C) -> TestRadio<Result<TestLoRa<C>, RadioError>>
    where
        C: Sx126xVariant,
    {
        let arm = Box::leak(Box::new(TrackerTxArm::new()));
        let events = Rc::new(RefCell::new(Vec::new()));
        let (reset, _) = MockOutput::new(PinState::Low, "reset", events.clone());
        let (power, power_probe) = MockOutput::new(PinState::Low, "power", events.clone());
        let (csd, csd_probe) = MockOutput::new(PinState::Low, "csd", events.clone());
        let (ctx, ctx_probe) = MockOutput::new(PinState::Low, "ctx", events);
        let (fem_delay, fem_delays) = ProbeDelay::new();
        let interface =
            TrackerTxHilInterface::new(reset, MockWait, MockWait, power, csd, ctx, fem_delay, arm)
                .unwrap();
        let (spi, commands, command_ctx) = MockSpi::new(ctx_probe.clone());
        let radio = Sx126x::new(
            spi,
            interface,
            Config {
                chip,
                tcxo_ctrl: Some(TcxoCtrlVoltage::Ctrl1V8),
                use_dcdc: false,
                rx_boost: false,
            },
        );
        let lora = block_on(LoRa::new(radio, false, NoopDelay));
        TestRadio {
            lora,
            commands,
            command_ctx,
            arm,
            ctx: ctx_probe,
            power: power_probe,
            csd: csd_probe,
            fem_delays,
        }
    }

    fn build_lora<C>(chip: C) -> TestRadio<TestLoRa<C>>
    where
        C: Sx126xVariant,
    {
        let TestRadio {
            lora,
            commands,
            command_ctx,
            arm,
            ctx,
            power,
            csd,
            fem_delays,
        } = try_build_lora(chip);
        TestRadio {
            lora: lora.unwrap(),
            commands,
            command_ctx,
            arm,
            ctx,
            power,
            csd,
            fem_delays,
        }
    }

    fn prepare_lora<C>(lora: &mut TestLoRa<C>, arm: &TrackerTxArm)
    where
        C: Sx126xVariant,
    {
        let modulation = lora
            .create_modulation_params(
                TRACKER_TX_HIL_PROFILE.spreading_factor(),
                TRACKER_TX_HIL_PROFILE.bandwidth(),
                TRACKER_TX_HIL_PROFILE.coding_rate(),
                TRACKER_TX_HIL_PROFILE.frequency_hz(),
            )
            .unwrap();
        let mut packet = lora
            .create_tx_packet_params(
                TRACKER_TX_HIL_PROFILE.preamble_symbols(),
                !TRACKER_TX_HIL_PROFILE.explicit_header(),
                TRACKER_TX_HIL_PROFILE.crc(),
                TRACKER_TX_HIL_PROFILE.iq_inverted(),
                &modulation,
            )
            .unwrap();
        arm.arm_for_prepare();
        block_on(lora.prepare_for_tx(
            &modulation,
            &mut packet,
            TRACKER_TX_HIL_MODEM_OUTPUT_DBM,
            b"RETICULUM-HIL-PING",
        ))
        .unwrap();
    }

    fn assert_contiguous_commands(commands: &[Vec<u8>], expected: &[&[u8]]) {
        assert!(
            commands.windows(expected.len()).any(|window| {
                window
                    .iter()
                    .zip(expected)
                    .all(|(actual, expected)| actual.as_slice() == *expected)
            }),
            "missing ordered command sequence {expected:02x?} in {commands:02x?}"
        );
    }

    #[cfg(not(feature = "near-field-attenuation-hil"))]
    fn normalize_tracker_pa_override(commands: &[Vec<u8>]) -> Vec<Vec<u8>> {
        commands
            .iter()
            .filter_map(|command| match command.as_slice() {
                [0x0d, 0x08, 0xe7, 0x28] => None,
                [0x95, 0x04, 0x07, 0x00, 0x01] => Some(Vec::from([0x95, 0x02, 0x02, 0x00, 0x01])),
                [0x8e, 0x00, 0x04] => Some(Vec::from([0x8e, 0x08, 0x04])),
                [0x8e, 0x00, 0x02] => Some(Vec::from([0x8e, 0x08, 0x02])),
                _ => Some(command.clone()),
            })
            .collect()
    }

    struct MalformedOverride(HighPowerPaOverride);

    impl Sx126xVariant for MalformedOverride {
        fn get_device_sel(&self) -> DeviceSel {
            DeviceSel::HighPowerPA
        }

        fn high_power_pa_override(
            &self,
            _requested_output_power: i32,
            _clamped_output_power: i32,
        ) -> Option<HighPowerPaOverride> {
            Some(self.0)
        }
    }

    #[test]
    fn first_hil_power_is_exactly_the_calibrated_minimum() {
        #[cfg(not(feature = "near-field-attenuation-hil"))]
        {
            assert_eq!(TRACKER_TX_HIL_TARGET_POWER_DBM, 14);
            assert_eq!(TRACKER_TX_HIL_MODEM_OUTPUT_DBM, 0);
            assert_eq!(TRACKER_TX_HIL_POWER_PROFILE, "calibrated-minimum");
        }
        #[cfg(feature = "near-field-attenuation-hil")]
        {
            assert_eq!(TRACKER_TX_HIL_TARGET_POWER_DBM, 5);
            assert_eq!(TRACKER_TX_HIL_MODEM_OUTPUT_DBM, -9);
            assert_eq!(
                TRACKER_TX_HIL_POWER_PROFILE,
                "near-field-attenuation-uncalibrated"
            );
        }
    }

    #[test]
    fn first_hil_profile_is_exactly_the_fixed_na915_profile() {
        assert_eq!(TRACKER_TX_HIL_PROFILE.frequency_hz(), 915_000_000);
        assert_eq!(TRACKER_TX_HIL_PROFILE.spreading_factor().factor(), 7);
        assert_eq!(TRACKER_TX_HIL_PROFILE.bandwidth().hz(), 125_000);
        assert_eq!(TRACKER_TX_HIL_PROFILE.coding_rate().denom(), 5);
        assert_eq!(TRACKER_TX_HIL_PROFILE.preamble_symbols(), 24);
        assert!(TRACKER_TX_HIL_PROFILE.explicit_header());
        assert!(TRACKER_TX_HIL_PROFILE.crc());
        assert!(!TRACKER_TX_HIL_PROFILE.iq_inverted());
        assert_eq!(TRACKER_TX_HIL_STANDBY_CLOCK, "rc");
        assert_eq!(
            TRACKER_TX_HIL_CTX_ASSERTION,
            "before-packet-and-fifo-prepare"
        );
        assert_eq!(
            TRACKER_TX_HIL_FEM_POWER_POLICY,
            "prepowered-during-radio-init"
        );
        assert_eq!(
            TrackerTxHilSx1262.high_power_pa_override(
                TRACKER_TX_HIL_MODEM_OUTPUT_DBM,
                TRACKER_TX_HIL_MODEM_OUTPUT_DBM,
            ),
            Some(HighPowerPaOverride {
                pa_duty_cycle: 0x04,
                hp_max: 0x07,
                tx_params_power: TRACKER_TX_HIL_MODEM_OUTPUT_DBM as u8,
                ocp: Some(0x28),
            })
        );
        for unexpected_power in [-8, -1, 1, 22] {
            assert_eq!(
                TrackerTxHilSx1262.high_power_pa_override(unexpected_power, unexpected_power),
                None
            );
        }
        assert_eq!(Sx1262.high_power_pa_override(0, 0), None);
    }

    #[test]
    fn malformed_atomic_pa_overrides_fail_before_tx_clamp_or_pa_writes() {
        let valid = HighPowerPaOverride {
            pa_duty_cycle: 0x04,
            hp_max: 0x07,
            tx_params_power: 0x00,
            ocp: Some(0x28),
        };
        let malformed = [
            HighPowerPaOverride {
                pa_duty_cycle: 0x05,
                ..valid
            },
            HighPowerPaOverride {
                hp_max: 0x08,
                ..valid
            },
            HighPowerPaOverride {
                tx_params_power: 0x17,
                ..valid
            },
            HighPowerPaOverride {
                tx_params_power: 0xf6,
                ..valid
            },
            HighPowerPaOverride {
                ocp: Some(0x40),
                ..valid
            },
        ];

        for settings in malformed {
            let TestRadio { lora, commands, .. } = try_build_lora(MalformedOverride(settings));
            assert!(matches!(lora, Err(RadioError::InvalidConfiguration)));
            assert!(!commands.borrow().iter().any(|command| {
                command.starts_with(&[0x1d, 0x08, 0xd8])
                    || command.first() == Some(&0x95)
                    || command.first() == Some(&0x8e)
            }));
        }
    }

    #[test]
    #[cfg(not(feature = "near-field-attenuation-hil"))]
    fn tracker_pa_override_is_the_only_init_and_prepare_trace_difference() {
        let TestRadio {
            lora: mut stock_lora,
            commands: stock_command_log,
            command_ctx: stock_command_ctx,
            arm: stock_arm,
            ctx: stock_ctx,
            ..
        } = build_lora(Sx1262);
        let TestRadio {
            lora: mut tracker_lora,
            commands: tracker_command_log,
            command_ctx: tracker_command_ctx,
            arm: tracker_arm,
            ctx: tracker_ctx,
            ..
        } = build_lora(TrackerTxHilSx1262);
        let stock_init = stock_command_log.borrow().clone();
        let tracker_init = tracker_command_log.borrow().clone();
        assert!(!stock_init.iter().any(|command| command == &[0x80, 0x01]));
        assert!(!tracker_init.iter().any(|command| command == &[0x80, 0x01]));
        assert_contiguous_commands(
            &stock_init,
            &[
                &[0x1d, 0x08, 0xd8, 0x00],
                &[0x0d, 0x08, 0xd8, 0x1e],
                &[0x95, 0x02, 0x02, 0x00, 0x01],
                &[0x8e, 0x08, 0x04],
            ],
        );
        assert_contiguous_commands(
            &tracker_init,
            &[
                &[0x1d, 0x08, 0xd8, 0x00],
                &[0x0d, 0x08, 0xd8, 0x1e],
                &[0x95, 0x04, 0x07, 0x00, 0x01],
                &[0x0d, 0x08, 0xe7, 0x28],
                &[0x8e, 0x00, 0x04],
            ],
        );
        assert_eq!(normalize_tracker_pa_override(&tracker_init), stock_init);

        stock_command_log.borrow_mut().clear();
        tracker_command_log.borrow_mut().clear();
        stock_command_ctx.borrow_mut().clear();
        tracker_command_ctx.borrow_mut().clear();
        prepare_lora(&mut stock_lora, stock_arm);
        prepare_lora(&mut tracker_lora, tracker_arm);
        assert!(stock_ctx.is_high());
        assert!(tracker_ctx.is_high());
        let stock_prepare = stock_command_log.borrow().clone();
        let tracker_prepare = tracker_command_log.borrow().clone();
        assert!(!stock_prepare.iter().any(|command| command == &[0x80, 0x01]));
        assert!(
            !tracker_prepare
                .iter()
                .any(|command| command == &[0x80, 0x01])
        );
        assert_contiguous_commands(
            &stock_prepare,
            &[
                &[0x1d, 0x08, 0xd8, 0x00],
                &[0x0d, 0x08, 0xd8, 0x1e],
                &[0x95, 0x02, 0x02, 0x00, 0x01],
                &[0x8e, 0x08, 0x02],
            ],
        );
        assert_contiguous_commands(
            &tracker_prepare,
            &[
                &[0x1d, 0x08, 0xd8, 0x00],
                &[0x0d, 0x08, 0xd8, 0x1e],
                &[0x95, 0x04, 0x07, 0x00, 0x01],
                &[0x0d, 0x08, 0xe7, 0x28],
                &[0x8e, 0x00, 0x02],
            ],
        );
        assert_eq!(
            normalize_tracker_pa_override(&tracker_prepare),
            stock_prepare
        );
        let tracker_prepare_ctx = tracker_command_ctx.borrow().clone();
        assert_eq!(tracker_prepare_ctx.len(), tracker_prepare.len());
        for opcode in [0x8c, 0x86, 0x0e, 0x08] {
            let index = tracker_prepare
                .iter()
                .position(|command| command.first() == Some(&opcode))
                .unwrap_or_else(|| panic!("missing command opcode 0x{opcode:02x}"));
            assert!(
                tracker_prepare_ctx[index],
                "CTX must be high for command opcode 0x{opcode:02x}"
            );
        }
        for command in [
            &[0x1d, 0x08, 0xd8, 0x00][..],
            &[0x0d, 0x08, 0xd8, 0x1e],
            &[0x95, 0x04, 0x07, 0x00, 0x01],
            &[0x0d, 0x08, 0xe7, 0x28],
            &[0x8e, 0x00, 0x02],
        ] {
            let index = tracker_prepare
                .iter()
                .position(|actual| actual.as_slice() == command)
                .unwrap_or_else(|| panic!("missing PA preparation command {command:02x?}"));
            assert!(
                !tracker_prepare_ctx[index],
                "CTX must remain low for PA preparation command {command:02x?}"
            );
        }

        block_on(tracker_lora.tx()).unwrap();
        let tracker_tx_commands = tracker_command_log.borrow().clone();
        let tracker_tx_ctx = tracker_command_ctx.borrow().clone();
        let set_tx_index = tracker_tx_commands
            .iter()
            .position(|command| command == &[0x83, 0x00, 0x00, 0x00])
            .expect("SetTx command must be present");
        assert!(tracker_tx_ctx[set_tx_index]);
        assert!(tracker_ctx.is_high());

        let before_standby = tracker_command_log.borrow().len();
        block_on(tracker_lora.enter_standby()).unwrap();
        assert!(!tracker_ctx.is_high());
        let tracker_commands = tracker_command_log.borrow();
        assert_eq!(tracker_commands.len(), before_standby + 1);
        assert_eq!(
            tracker_commands.last().map(Vec::as_slice),
            Some(&[0x80, 0x00][..])
        );
    }

    #[test]
    #[cfg(feature = "near-field-attenuation-hil")]
    fn attenuation_feature_keeps_rnode_pa_topology_at_minimum_encoded_power() {
        let TestRadio {
            mut lora,
            commands,
            command_ctx,
            arm,
            ctx,
            ..
        } = build_lora(TrackerTxHilSx1262);
        commands.borrow_mut().clear();
        command_ctx.borrow_mut().clear();

        prepare_lora(&mut lora, arm);

        let commands = commands.borrow();
        assert_contiguous_commands(
            &commands,
            &[
                &[0x1d, 0x08, 0xd8, 0x00],
                &[0x0d, 0x08, 0xd8, 0x1e],
                &[0x95, 0x04, 0x07, 0x00, 0x01],
                &[0x0d, 0x08, 0xe7, 0x28],
                &[0x8e, 0xf7, 0x02],
            ],
        );
        let command_ctx = command_ctx.borrow();
        for command in [
            &[0x1d, 0x08, 0xd8, 0x00][..],
            &[0x0d, 0x08, 0xd8, 0x1e],
            &[0x95, 0x04, 0x07, 0x00, 0x01],
            &[0x0d, 0x08, 0xe7, 0x28],
            &[0x8e, 0xf7, 0x02],
        ] {
            let index = commands
                .iter()
                .position(|actual| actual.as_slice() == command)
                .unwrap_or_else(|| panic!("missing attenuated PA command {command:02x?}"));
            assert!(!command_ctx[index]);
        }
        assert!(ctx.is_high());
    }

    #[test]
    fn one_shot_arm_requires_prepare_then_final_tx_consumption() {
        static ARM: TrackerTxArm = TrackerTxArm::new();
        ARM.disarm();
        assert!(!ARM.consume_prepare_arm());
        assert!(!ARM.consume_prepared());
        ARM.arm_for_prepare();
        assert!(ARM.consume_prepare_arm());
        assert!(!ARM.consume_prepare_arm());
        assert!(ARM.consume_prepared());
        assert!(!ARM.consume_prepared());
    }

    #[test]
    fn fem_is_powered_and_settled_during_radio_initialization_only() {
        let TestRadio {
            mut lora,
            arm,
            ctx,
            power,
            csd,
            fem_delays,
            ..
        } = build_lora(TrackerTxHilSx1262);

        assert!(power.is_high());
        assert!(csd.is_high());
        assert!(!ctx.is_high());
        assert_eq!(
            fem_delays.borrow().as_slice(),
            &[
                FEM_POWER_SETTLE_MS * 1_000_000,
                FEM_CSD_SETTLE_MS * 1_000_000,
            ]
        );

        fem_delays.borrow_mut().clear();
        prepare_lora(&mut lora, arm);
        assert!(ctx.is_high());
        assert!(power.is_high());
        assert!(csd.is_high());
        assert!(fem_delays.borrow().is_empty());

        block_on(lora.enter_standby()).unwrap();
        assert!(!ctx.is_high());
        assert!(power.is_high());
        assert!(csd.is_high());
        assert!(fem_delays.borrow().is_empty());
    }

    #[test]
    fn successful_rx_standby_allows_the_next_early_ctx_tx_prepare() {
        let TestRadio {
            mut lora, arm, ctx, ..
        } = build_lora(TrackerTxHilSx1262);
        let modulation = lora
            .create_modulation_params(
                TRACKER_TX_HIL_PROFILE.spreading_factor(),
                TRACKER_TX_HIL_PROFILE.bandwidth(),
                TRACKER_TX_HIL_PROFILE.coding_rate(),
                TRACKER_TX_HIL_PROFILE.frequency_hz(),
            )
            .unwrap();
        let rx_packet = lora
            .create_rx_packet_params(
                TRACKER_TX_HIL_PROFILE.preamble_symbols(),
                !TRACKER_TX_HIL_PROFILE.explicit_header(),
                SX1262_FRAME_MTU as u8,
                TRACKER_TX_HIL_PROFILE.crc(),
                TRACKER_TX_HIL_PROFILE.iq_inverted(),
                &modulation,
            )
            .unwrap();
        block_on(lora.prepare_for_rx(RxMode::Single(1), &modulation, &rx_packet)).unwrap();
        let mut receive_buffer = [0_u8; SX1262_FRAME_MTU];
        let (received_len, _) = block_on(lora.rx(&rx_packet, &mut receive_buffer)).unwrap();
        assert_eq!(received_len, 0);
        block_on(lora.enter_standby()).unwrap();
        assert!(!ctx.is_high());

        let mut tx_packet = lora
            .create_tx_packet_params(
                TRACKER_TX_HIL_PROFILE.preamble_symbols(),
                !TRACKER_TX_HIL_PROFILE.explicit_header(),
                TRACKER_TX_HIL_PROFILE.crc(),
                TRACKER_TX_HIL_PROFILE.iq_inverted(),
                &modulation,
            )
            .unwrap();
        arm.arm_for_prepare();
        block_on(lora.prepare_for_tx(
            &modulation,
            &mut tx_packet,
            TRACKER_TX_HIL_MODEM_OUTPUT_DBM,
            b"RETICULUM-HIL-PONG",
        ))
        .unwrap();
        assert!(ctx.is_high());
    }

    #[test]
    fn private_switch_hook_requires_one_shot_arm_and_drop_fails_closed() {
        static ARM: TrackerTxArm = TrackerTxArm::new();
        ARM.disarm();
        let events = Rc::new(RefCell::new(Vec::new()));
        let (reset, reset_probe) = MockOutput::new(PinState::High, "reset", events.clone());
        let (power, power_probe) = MockOutput::new(PinState::High, "power", events.clone());
        let (csd, csd_probe) = MockOutput::new(PinState::High, "csd", events.clone());
        let (ctx, ctx_probe) = MockOutput::new(PinState::High, "ctx", events.clone());
        let mut interface =
            TrackerTxHilInterface::new(reset, MockWait, MockWait, power, csd, ctx, NoopDelay, &ARM)
                .unwrap();

        assert!(!reset_probe.is_high());
        assert!(!power_probe.is_high());
        assert!(!csd_probe.is_high());
        assert!(!ctx_probe.is_high());

        assert_eq!(
            block_on(interface.enable_rf_switch_tx()),
            Err(RadioError::InvalidConfiguration)
        );
        assert!(!ctx_probe.is_high());

        ARM.arm_for_prepare();
        assert_eq!(block_on(interface.prepare_rf_switch_tx()), Ok(()));
        assert!(power_probe.is_high());
        assert!(csd_probe.is_high());
        assert!(ctx_probe.is_high());
        assert!(!ARM.consume_prepare_arm());

        assert_eq!(block_on(interface.enable_rf_switch_tx()), Ok(()));
        assert!(ctx_probe.is_high());
        assert!(!ARM.consume_prepared());

        assert_eq!(
            block_on(interface.enable_rf_switch_tx()),
            Err(RadioError::InvalidConfiguration)
        );
        assert!(!ctx_probe.is_high());

        ARM.arm_for_prepare();
        assert_eq!(block_on(interface.disable_rf_switch()), Ok(()));
        assert!(!ctx_probe.is_high());
        assert!(!ARM.consume_prepare_arm());

        events.borrow_mut().clear();
        drop(interface);
        assert_eq!(
            events.borrow().as_slice(),
            &[
                PinEvent {
                    pin: "reset",
                    high: false,
                },
                PinEvent {
                    pin: "csd",
                    high: false,
                },
                PinEvent {
                    pin: "ctx",
                    high: false,
                },
                PinEvent {
                    pin: "power",
                    high: false,
                },
            ]
        );
        assert!(!reset_probe.is_high());
        assert!(!power_probe.is_high());
        assert!(!csd_probe.is_high());
        assert!(!ctx_probe.is_high());
    }
}
