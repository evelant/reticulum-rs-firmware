//! Bidirectional SX1262 and external-FEM owner for the Tracker V2.
//!
//! This crate is deliberately separate from the frozen receive-only board
//! boundary. Its first product configuration is fixed to NA915 and the lowest
//! calibrated Tracker antenna-path setting:
//! 14 dBm effective target power, produced by 0 dBm at the SX1262 plus the
//! characterized external-FEM gain. An explicit diagnostic-only feature also
//! exposes an opaque configuration using the SX1262's -9 dBm minimum to test a
//! colocated receiver for overload; that antenna-path result is not treated as
//! calibrated and never replaces the calibrated product configuration.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::{cell::Cell, num::NonZeroU64};

use critical_section::Mutex;
use embedded_hal::digital::{OutputPin, StatefulOutputPin};
use embedded_hal_async::{delay::DelayNs, digital::Wait, spi::SpiDevice};
use lora_phy::{
    mod_params::RadioError,
    mod_traits::InterfaceVariant,
    sx126x::{DeviceSel, HighPowerPaOverride, Sx126xVariant, TcxoCtrlVoltage},
};
use reticulum_board_heltec_tracker_v2::{
    FEM_CSD_SETTLE_MS, FEM_POWER_SETTLE_MS, validate_lab_rx_profile,
};
use reticulum_radio_interface::{
    BoundedRxOutcome, CadObservation, LabRxProfile, LabRxProfileConfig, PacketTxFault,
    PacketTxObservation, RadioConfigurationFingerprint, RnodeTxFrames, SX1262_FRAME_MTU,
    SoleRadioFaultSummary, SoleRnodeRadio,
};
use reticulum_radio_lora_phy::{
    IrqTimestampCapture, Sx126xRadioError, Sx126xRadioOperation, Sx126xReceivedFrame,
    Sx126xRnodeRadio, Sx126xRnodeSettings, Sx126xTxHooks,
};

/// `lora-phy`'s SX126x private-network sync word selected by this owner.
pub const TRACKER_PRIVATE_SYNC_WORD: u16 = 0x1424;

/// Named antenna-path power admitted by the Tracker radio owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TrackerRadioPower {
    /// Characterized 14 dBm antenna-path target from 0 dBm SX1262 output.
    CalibratedMinimum,
    /// Diagnostic-only estimated 5 dBm path from -9 dBm SX1262 output.
    DiagnosticNearFieldAttenuation,
}

impl TrackerRadioPower {
    /// Effective antenna-path target used for policy and diagnostics.
    pub const fn target_power_dbm(self) -> i32 {
        match self {
            Self::CalibratedMinimum => 14,
            Self::DiagnosticNearFieldAttenuation => 5,
        }
    }

    /// Exact SX1262 output accepted by the board-specific PA override.
    pub const fn modem_output_dbm(self) -> i32 {
        match self {
            Self::CalibratedMinimum => 0,
            Self::DiagnosticNearFieldAttenuation => -9,
        }
    }
}

/// Stable identity for one immutable Tracker radio configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TrackerRadioConfigurationId {
    /// NA915 SF7/BW125/CR4/5 with the calibrated minimum power path.
    Na915DevCalibratedMinimum,
    /// NA915 development profile with diagnostic near-field attenuation.
    Na915DevDiagnosticNearFieldAttenuation,
}

/// Full Tracker configuration fingerprint for calibrated NA915 development.
///
/// This versioned identity binds the fixed modem/packet profile, power path,
/// private sync word, network selection, regulator, PA and FEM policy. Any
/// change to one of those values requires a different fingerprint.
pub const TRACKER_NA915_DEV_CONFIGURATION_FINGERPRINT: RadioConfigurationFingerprint =
    RadioConfigurationFingerprint::new([
        0x54, 0x52, 0x4b, 0x32, 0x4e, 0x41, 0x39, 0x31, 0x35, 0x43, 0x41, 0x4c, 0x30, 0x30, 0x30,
        0x31,
    ]);

const TRACKER_NA915_NEAR_FIELD_DIAGNOSTIC_CONFIGURATION_FINGERPRINT: RadioConfigurationFingerprint =
    RadioConfigurationFingerprint::new([
        0x54, 0x52, 0x4b, 0x32, 0x4e, 0x41, 0x39, 0x31, 0x35, 0x44, 0x49, 0x41, 0x30, 0x30, 0x30,
        0x31,
    ]);

/// Opaque board-validated modem, receive, and power configuration.
///
/// There is deliberately no public constructor: a caller cannot pair an
/// arbitrary receive profile or numeric output power with the Tracker PA/FEM
/// path. Dispatcher authorization can bind to [`Self::id`] before it accounts
/// airtime or grants use of the hardware.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrackerRadioConfiguration {
    id: TrackerRadioConfigurationId,
    profile: LabRxProfile,
    power: TrackerRadioPower,
    rx_symbol_timeout: u16,
    public_network: bool,
    use_dcdc: bool,
    rx_boost: bool,
}

impl TrackerRadioConfiguration {
    const fn new(
        id: TrackerRadioConfigurationId,
        profile: LabRxProfile,
        power: TrackerRadioPower,
    ) -> Self {
        Self {
            id,
            profile,
            power,
            rx_symbol_timeout: 248,
            public_network: false,
            use_dcdc: false,
            rx_boost: false,
        }
    }

    /// Stable identity to bind into radio reservations and grants.
    pub const fn id(self) -> TrackerRadioConfigurationId {
        self.id
    }

    /// Validated RNode-compatible LoRa modulation and packet profile.
    pub const fn profile(self) -> LabRxProfile {
        self.profile
    }

    /// Opaque full configuration fingerprint used for permit binding.
    pub const fn fingerprint(self) -> RadioConfigurationFingerprint {
        match self.id {
            TrackerRadioConfigurationId::Na915DevCalibratedMinimum => {
                TRACKER_NA915_DEV_CONFIGURATION_FINGERPRINT
            }
            TrackerRadioConfigurationId::Na915DevDiagnosticNearFieldAttenuation => {
                TRACKER_NA915_NEAR_FIELD_DIAGNOSTIC_CONFIGURATION_FINGERPRINT
            }
        }
    }

    /// Named, board-qualified power selection.
    pub const fn power(self) -> TrackerRadioPower {
        self.power
    }

    /// Maximum preamble-search symbol timeout admitted by the pinned driver.
    pub const fn rx_symbol_timeout(self) -> u16 {
        self.rx_symbol_timeout
    }

    /// Whether the public LoRa sync word is selected.
    pub const fn public_network(self) -> bool {
        self.public_network
    }

    /// Whether the SX1262 internal DC-DC regulator is selected.
    pub const fn uses_dcdc(self) -> bool {
        self.use_dcdc
    }

    /// Whether boosted receive gain is selected.
    pub const fn boosted_receive(self) -> bool {
        self.rx_boost
    }
}

struct TrackerSx1262 {
    power: TrackerRadioPower,
}

impl TrackerSx1262 {
    const fn new(power: TrackerRadioPower) -> Self {
        Self { power }
    }
}

impl Sx126xVariant for TrackerSx1262 {
    fn get_device_sel(&self) -> DeviceSel {
        DeviceSel::HighPowerPA
    }

    fn high_power_pa_override(
        &self,
        requested_output_power: i32,
        clamped_output_power: i32,
    ) -> Option<HighPowerPaOverride> {
        let modem_output_dbm = self.power.modem_output_dbm();
        if requested_output_power == modem_output_dbm && clamped_output_power == modem_output_dbm {
            Some(HighPowerPaOverride {
                pa_duty_cycle: 0x04,
                hp_max: 0x07,
                tx_params_power: modem_output_dbm as u8,
                ocp: Some(0x28),
            })
        } else {
            None
        }
    }
}

/// The sole radio profile admitted by the first Tracker V2 product boundary.
///
/// This is deliberately constructed inside the board-specific crate so a
/// caller cannot validate a different frequency against an unrelated radio
/// range and then pass it into a transmit-capable owner.
pub const TRACKER_NA915_DEV_PROFILE: LabRxProfile =
    match validate_lab_rx_profile(LabRxProfileConfig {
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
        Err(_) => panic!("invalid fixed Tracker V2 NA915 development profile"),
    };

/// Immutable calibrated NA915 development configuration.
///
/// This value is invariant under every crate feature. Firmware must pass the
/// exact opaque configuration it intends to use to [`TrackerRadio::new`].
pub const TRACKER_NA915_DEV_CONFIGURATION: TrackerRadioConfiguration =
    TrackerRadioConfiguration::new(
        TrackerRadioConfigurationId::Na915DevCalibratedMinimum,
        TRACKER_NA915_DEV_PROFILE,
        TrackerRadioPower::CalibratedMinimum,
    );

/// Diagnostic near-field configuration with uncalibrated antenna-path power.
///
/// Merely enabling this feature does not select this value. A HIL or other
/// diagnostic caller must pass it explicitly to [`TrackerRadio::new`].
#[cfg(feature = "near-field-attenuation")]
pub const TRACKER_NA915_NEAR_FIELD_DIAGNOSTIC_CONFIGURATION: TrackerRadioConfiguration =
    TrackerRadioConfiguration::new(
        TrackerRadioConfigurationId::Na915DevDiagnosticNearFieldAttenuation,
        TRACKER_NA915_DEV_PROFILE,
        TrackerRadioPower::DiagnosticNearFieldAttenuation,
    );

/// Effective antenna-path target of the calibrated NA915 configuration.
pub const TRACKER_NA915_DEV_TARGET_POWER_DBM: i32 =
    TRACKER_NA915_DEV_CONFIGURATION.power().target_power_dbm();

/// Exact SX1262 output of the calibrated NA915 configuration.
pub const TRACKER_NA915_DEV_MODEM_OUTPUT_DBM: i32 =
    TRACKER_NA915_DEV_CONFIGURATION.power().modem_output_dbm();

/// Estimated antenna-path target of the diagnostic near-field configuration.
#[cfg(feature = "near-field-attenuation")]
pub const TRACKER_NA915_NEAR_FIELD_DIAGNOSTIC_TARGET_POWER_DBM: i32 =
    TRACKER_NA915_NEAR_FIELD_DIAGNOSTIC_CONFIGURATION
        .power()
        .target_power_dbm();

/// Exact SX1262 output of the diagnostic near-field configuration.
#[cfg(feature = "near-field-attenuation")]
pub const TRACKER_NA915_NEAR_FIELD_DIAGNOSTIC_MODEM_OUTPUT_DBM: i32 =
    TRACKER_NA915_NEAR_FIELD_DIAGNOSTIC_CONFIGURATION
        .power()
        .modem_output_dbm();

/// Maximum preamble-search symbol timeout of the calibrated configuration.
pub const TRACKER_RX_SYMBOL_TIMEOUT: u16 = TRACKER_NA915_DEV_CONFIGURATION.rx_symbol_timeout();

/// Conservative whole-operation RX watchdog for the fixed Tracker profile.
///
/// This covers 248 SF7/BW125 preamble-search symbols, a maximum 255-byte LoRa
/// frame after detection, driver SPI/standby cleanup, and ample executor
/// margin. Expiry is a fail-closed cancellation and reconstruction event, not
/// a normal no-preamble timeout. Changing this fingerprint-bound product value
/// requires a new radio configuration identity.
pub const TRACKER_MAXIMUM_RECEIVE_OPERATION_US: NonZeroU64 = match NonZeroU64::new(1_500_000) {
    Some(bound) => bound,
    None => panic!("Tracker receive watchdog must be non-zero"),
};

/// Minimum non-airtime margin reserved inside the whole-operation RX watchdog.
///
/// This covers IRQ/status SPI work, standby cleanup, and executor scheduling
/// beyond preamble search plus maximum-frame airtime.
pub const TRACKER_RX_WATCHDOG_MINIMUM_MARGIN_US: u64 = 100_000;
const _: () = assert!(TRACKER_RX_WATCHDOG_MINIMUM_MARGIN_US > 0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TrackerTxArmState {
    Disarmed,
    ArmedForPrepare,
    Prepared,
}

/// Shared one-shot authorization consumed across the two private TX hooks.
///
/// Firmware must place this value in static storage and pass it to the radio
/// constructor. Callers cannot arm it directly. Only [`TrackerRadio`]
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

impl Sx126xTxHooks for TrackerTxArm {
    fn disarm(&self) {
        Self::disarm(self);
    }

    fn arm_for_prepare(&self) {
        Self::arm_for_prepare(self);
    }
}

/// Operation in which the bidirectional radio failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrackerRadioOperation {
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
    /// Modem and IRQ preparation for channel activity detection.
    PrepareChannelActivityDetection,
    /// Channel activity detection and its IRQ completion.
    ChannelActivityDetection,
    /// Post-CAD standby and RF-switch cleanup.
    ChannelActivityCleanup,
    /// Bounded receive preparation.
    PrepareReceive,
    /// Bounded receive execution.
    Receive,
    /// Post-receive standby cleanup.
    ReceiveCleanup,
    /// A successful receive lacked its DIO1-boundary timestamp.
    ReceiveTimestamp,
}

/// Failure from the guarded Tracker bidirectional boundary.
#[derive(Debug)]
pub struct TrackerRadioError {
    /// Exact operation that failed.
    pub operation: TrackerRadioOperation,
    /// Pinned driver classification, when failure came from the radio path.
    pub radio: Option<RadioError>,
}

impl TrackerRadioError {
    const fn radio(operation: TrackerRadioOperation, radio: RadioError) -> Self {
        Self {
            operation,
            radio: Some(radio),
        }
    }
}

/// Shared capture slot sampled when the radio's DIO1 wait resumes.
///
/// The historical board name remains as a type alias while the implementation
/// is shared by every `lora-phy` SX126x board owner.
pub type TrackerRxTimestampCapture = IrqTimestampCapture;

/// One physical frame copied out of a caller-deadline-bounded receive window.
#[derive(Clone, Copy, Debug)]
pub struct TrackerReceivedFrame {
    /// Number of valid bytes in the caller's buffer.
    pub len: u8,
    /// Packet RSSI reported by the SX1262 driver.
    pub rssi_dbm: i16,
    /// Packet SNR reported by the SX1262 driver.
    pub snr_db: i16,
    /// Monotonic tick sampled when the final receive DIO1 wait resumed.
    pub received_at_ticks: u64,
}

type TrackerCore<Spi, Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay, RadioDelay> = Sx126xRnodeRadio<
    Spi,
    TrackerInterface<Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay>,
    TrackerSx1262,
    RadioDelay,
    TrackerTxArm,
>;

/// Opaque, reset-scoped Tracker V2 RX/CAD/TX owner.
///
/// Board-specific pin, external-FEM, PA and configuration policy remain here.
/// The initialized LoRa operation state machine is owned by the shared
/// reticulum-radio-lora-phy core. Cancelling a core operation drops the
/// private Tracker interface, which asserts reset and disables the FEM.
pub struct TrackerRadio<Spi, Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay, RadioDelay>
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
    core: TrackerCore<Spi, Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay, RadioDelay>,
    configuration: TrackerRadioConfiguration,
}

fn map_shared_operation(operation: Sx126xRadioOperation) -> TrackerRadioOperation {
    match operation {
        Sx126xRadioOperation::ChipInitialization => TrackerRadioOperation::ChipInitialization,
        Sx126xRadioOperation::ModulationConfiguration => {
            TrackerRadioOperation::ModulationConfiguration
        }
        Sx126xRadioOperation::PacketConfiguration => TrackerRadioOperation::PacketConfiguration,
        Sx126xRadioOperation::FrameValidation => TrackerRadioOperation::FrameValidation,
        Sx126xRadioOperation::PrepareTransmit => TrackerRadioOperation::PrepareTransmit,
        Sx126xRadioOperation::Transmit => TrackerRadioOperation::Transmit,
        Sx126xRadioOperation::TransmitCleanup => TrackerRadioOperation::TransmitCleanup,
        Sx126xRadioOperation::PrepareChannelActivityDetection => {
            TrackerRadioOperation::PrepareChannelActivityDetection
        }
        Sx126xRadioOperation::ChannelActivityDetection => {
            TrackerRadioOperation::ChannelActivityDetection
        }
        Sx126xRadioOperation::ChannelActivityCleanup => {
            TrackerRadioOperation::ChannelActivityCleanup
        }
        Sx126xRadioOperation::PrepareReceive => TrackerRadioOperation::PrepareReceive,
        Sx126xRadioOperation::Receive => TrackerRadioOperation::Receive,
        Sx126xRadioOperation::ReceiveCleanup => TrackerRadioOperation::ReceiveCleanup,
        Sx126xRadioOperation::ReceiveTimestamp => TrackerRadioOperation::ReceiveTimestamp,
    }
}

fn map_shared_error(error: Sx126xRadioError) -> TrackerRadioError {
    TrackerRadioError {
        operation: map_shared_operation(error.operation),
        radio: error.radio,
    }
}

impl<Spi, Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay, RadioDelay>
    TrackerRadio<Spi, Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay, RadioDelay>
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
        rx_timestamps: &'static TrackerRxTimestampCapture,
        configuration: TrackerRadioConfiguration,
    ) -> Result<Self, TrackerRadioError> {
        tx_arm.disarm();
        let interface = TrackerInterface::new(
            reset,
            dio1,
            busy,
            power,
            csd,
            ctx,
            fem_delay,
            tx_arm,
            rx_timestamps,
        )
        .map_err(|radio| {
            TrackerRadioError::radio(TrackerRadioOperation::InterfaceConstruction, radio)
        })?;
        let settings = Sx126xRnodeSettings::new(
            configuration.profile(),
            configuration.fingerprint(),
            configuration.power().modem_output_dbm(),
            configuration.rx_symbol_timeout(),
            configuration.public_network(),
            configuration.uses_dcdc(),
            configuration.boosted_receive(),
            TRACKER_MAXIMUM_RECEIVE_OPERATION_US,
        );
        let core = Sx126xRnodeRadio::new(
            spi,
            interface,
            TrackerSx1262::new(configuration.power()),
            radio_delay,
            tx_arm,
            rx_timestamps,
            settings,
            Some(TcxoCtrlVoltage::Ctrl1V8),
        )
        .await
        .map_err(map_shared_error)?;

        Ok(Self {
            core,
            configuration,
        })
    }

    /// Immutable configuration explicitly selected for this owner.
    pub const fn configuration(&self) -> TrackerRadioConfiguration {
        self.configuration
    }

    /// Transmit exactly one physical frame at the selected named power.
    pub async fn transmit_frame(&mut self, frame: &[u8]) -> Result<(), TrackerRadioError> {
        self.core
            .transmit_frame(frame)
            .await
            .map_err(map_shared_error)
    }

    /// Run one low-level LoRa channel activity detection operation.
    pub async fn channel_activity_detected(&mut self) -> Result<bool, TrackerRadioError> {
        self.core
            .channel_activity_detected()
            .await
            .map_err(map_shared_error)
    }

    /// Run one SX1262 receive operation with a bounded preamble search.
    pub fn receive_frame<'a>(
        &'a mut self,
        buffer: &'a mut [u8; SX1262_FRAME_MTU],
    ) -> impl core::future::Future<Output = Result<Option<TrackerReceivedFrame>, TrackerRadioError>> + 'a
    {
        let receive = self.core.receive_frame(buffer);
        async move {
            receive
                .await
                .map(|frame| {
                    frame.map(
                        |Sx126xReceivedFrame {
                             len,
                             rssi_dbm,
                             snr_db,
                             received_at_ticks,
                         }| TrackerReceivedFrame {
                            len,
                            rssi_dbm,
                            snr_db,
                            received_at_ticks,
                        },
                    )
                })
                .map_err(map_shared_error)
        }
    }

    /// Drop the active radio owner into its fail-closed state.
    pub fn shutdown(&mut self) {
        self.core.shutdown();
    }

    /// Whether this wrapper still owns initialized radio hardware.
    pub const fn is_active(&self) -> bool {
        self.core.is_active()
    }
}

impl<Spi, Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay, RadioDelay> SoleRnodeRadio
    for TrackerRadio<Spi, Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay, RadioDelay>
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
    type Fault = SoleRadioFaultSummary;

    fn configuration_fingerprint(&self) -> RadioConfigurationFingerprint {
        SoleRnodeRadio::configuration_fingerprint(&self.core)
    }

    fn airtime_profile(&self) -> LabRxProfile {
        SoleRnodeRadio::airtime_profile(&self.core)
    }

    fn maximum_receive_operation_us(&self) -> NonZeroU64 {
        SoleRnodeRadio::maximum_receive_operation_us(&self.core)
    }

    fn receive_bounded<'a>(
        &'a mut self,
        buffer: &'a mut [u8; SX1262_FRAME_MTU],
    ) -> impl core::future::Future<Output = Result<BoundedRxOutcome, Self::Fault>> + 'a {
        SoleRnodeRadio::receive_bounded(&mut self.core, buffer)
    }

    fn receive_continuous_until<'a, SchedulerYield, ProgressDeadline>(
        &'a mut self,
        buffer: &'a mut [u8; SX1262_FRAME_MTU],
        scheduler_yield: SchedulerYield,
        progress_deadline: ProgressDeadline,
    ) -> impl core::future::Future<Output = Result<BoundedRxOutcome, Self::Fault>> + 'a
    where
        SchedulerYield: core::future::Future<Output = ()> + 'a,
        ProgressDeadline: core::future::Future<Output = ()> + 'a,
    {
        SoleRnodeRadio::receive_continuous_until(
            &mut self.core,
            buffer,
            scheduler_yield,
            progress_deadline,
        )
    }

    fn invalidate_receive_session(&mut self) {
        SoleRnodeRadio::invalidate_receive_session(&mut self.core);
    }

    fn cad(
        &mut self,
    ) -> impl core::future::Future<Output = Result<CadObservation, Self::Fault>> + '_ {
        SoleRnodeRadio::cad(&mut self.core)
    }

    fn transmit<'a>(
        &'a mut self,
        frames: RnodeTxFrames<'a>,
    ) -> impl core::future::Future<Output = Result<PacketTxObservation, PacketTxFault<Self::Fault>>> + 'a
    {
        SoleRnodeRadio::transmit(&mut self.core, frames)
    }

    fn shutdown(&mut self) {
        self.core.shutdown();
    }

    fn is_active(&self) -> bool {
        self.core.is_active()
    }
}

struct TrackerInterface<Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay>
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
    rx_timestamps: &'static TrackerRxTimestampCapture,
}

impl<Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay>
    TrackerInterface<Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay>
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
        rx_timestamps: &'static TrackerRxTimestampCapture,
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
            rx_timestamps,
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
    for TrackerInterface<Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay>
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
            .map_err(|_| RadioError::DIO1)?;
        self.rx_timestamps.record_irq_observation();
        Ok(())
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
    for TrackerInterface<Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay>
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

    use core::{
        cell::{Cell, RefCell},
        convert::Infallible,
        future::Future,
        task::Poll,
    };
    use std::{boxed::Box, rc::Rc, vec::Vec};

    use embedded_hal::{
        digital::{ErrorType, PinState},
        spi::{ErrorType as SpiErrorType, Operation},
    };
    use lora_phy::{
        LoRa, RxMode,
        sx126x::{Config, Sx126x, Sx1262},
    };
    use reticulum_radio_interface::{SoleRadioFaultClass, SoleRadioFaultPhase};

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

    fn test_ticks() -> u64 {
        7
    }

    std::thread_local! {
        static STEPPED_TICKS: Cell<u64> = const { Cell::new(0) };
    }

    fn stepped_ticks() -> u64 {
        STEPPED_TICKS.with(|ticks| {
            let next = ticks.get() + 1;
            ticks.set(next);
            next
        })
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
    type CadActivityProbe = Rc<core::cell::Cell<bool>>;
    type RxPreambleProbe = Rc<core::cell::Cell<bool>>;
    type RxTimeoutProbe = Rc<core::cell::Cell<bool>>;

    struct MockSpi {
        commands: CommandLog,
        command_ctx: CommandCtxLog,
        ctx: PinProbe,
        cad_activity: CadActivityProbe,
        cad_pending: bool,
        rx_preamble: RxPreambleProbe,
        rx_preamble_pending: bool,
        rx_done_pending: bool,
        rx_timeout: RxTimeoutProbe,
        rx_timeout_pending: bool,
    }

    impl MockSpi {
        fn new(
            ctx: PinProbe,
        ) -> (
            Self,
            CommandLog,
            CommandCtxLog,
            CadActivityProbe,
            RxPreambleProbe,
            RxTimeoutProbe,
        ) {
            let commands = Rc::new(RefCell::new(Vec::new()));
            let command_ctx = Rc::new(RefCell::new(Vec::new()));
            let cad_activity = Rc::new(core::cell::Cell::new(false));
            let rx_preamble = Rc::new(core::cell::Cell::new(false));
            let rx_timeout = Rc::new(core::cell::Cell::new(false));
            (
                Self {
                    commands: commands.clone(),
                    command_ctx: command_ctx.clone(),
                    ctx,
                    cad_activity: cad_activity.clone(),
                    cad_pending: false,
                    rx_preamble: rx_preamble.clone(),
                    rx_preamble_pending: false,
                    rx_done_pending: false,
                    rx_timeout: rx_timeout.clone(),
                    rx_timeout_pending: false,
                },
                commands,
                command_ctx,
                cad_activity,
                rx_preamble,
                rx_timeout,
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
            if command.first() == Some(&0xc5) {
                self.cad_pending = true;
            }
            if command.first() == Some(&0x82) {
                if self.rx_preamble.get() {
                    self.rx_preamble_pending = true;
                } else if self.rx_timeout.get() {
                    self.rx_timeout_pending = true;
                }
            }
            self.commands.borrow_mut().push(command);
            self.command_ctx.borrow_mut().push(self.ctx.is_high());

            let irq_status = if self.cad_pending && is_get_irq_status {
                self.cad_pending = false;
                if self.cad_activity.get() {
                    0x0180_u16
                } else {
                    0x0080_u16
                }
            } else if is_get_irq_status && self.rx_preamble_pending {
                self.rx_preamble_pending = false;
                self.rx_done_pending = true;
                0x0004_u16
            } else if is_get_irq_status && self.rx_done_pending {
                self.rx_done_pending = false;
                0x0002_u16
            } else if is_get_irq_status && self.rx_timeout_pending {
                self.rx_timeout_pending = false;
                0x0200_u16
            } else {
                0x0003_u16
            };
            let mut read_index = 0;
            for operation in operations.iter_mut() {
                if let Operation::Read(bytes) = operation {
                    bytes.fill(0);
                    if is_get_irq_status && read_index == 1 && bytes.len() >= 2 {
                        bytes[0] = (irq_status >> 8) as u8;
                        bytes[1] = irq_status as u8;
                    }
                    read_index += 1;
                }
            }
            Ok(())
        }
    }

    type TestInterface = TrackerInterface<
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
        cad_activity: CadActivityProbe,
        rx_preamble: RxPreambleProbe,
        rx_timeout: RxTimeoutProbe,
        arm: &'static TrackerTxArm,
        rx_timestamps: &'static TrackerRxTimestampCapture,
        ctx: PinProbe,
        power: PinProbe,
        csd: PinProbe,
        fem_delays: DelayLog,
    }

    fn try_build_lora_with_ticks<C>(
        chip: C,
        now_ticks: fn() -> u64,
    ) -> TestRadio<Result<TestLoRa<C>, RadioError>>
    where
        C: Sx126xVariant,
    {
        let arm = Box::leak(Box::new(TrackerTxArm::new()));
        let rx_timestamps = Box::leak(Box::new(TrackerRxTimestampCapture::new_monotonic_us(
            now_ticks,
        )));
        let events = Rc::new(RefCell::new(Vec::new()));
        let (reset, _) = MockOutput::new(PinState::Low, "reset", events.clone());
        let (power, power_probe) = MockOutput::new(PinState::Low, "power", events.clone());
        let (csd, csd_probe) = MockOutput::new(PinState::Low, "csd", events.clone());
        let (ctx, ctx_probe) = MockOutput::new(PinState::Low, "ctx", events);
        let (fem_delay, fem_delays) = ProbeDelay::new();
        let interface = TrackerInterface::new(
            reset,
            MockWait,
            MockWait,
            power,
            csd,
            ctx,
            fem_delay,
            arm,
            rx_timestamps,
        )
        .unwrap();
        let (spi, commands, command_ctx, cad_activity, rx_preamble, rx_timeout) =
            MockSpi::new(ctx_probe.clone());
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
            cad_activity,
            rx_preamble,
            rx_timeout,
            arm,
            rx_timestamps,
            ctx: ctx_probe,
            power: power_probe,
            csd: csd_probe,
            fem_delays,
        }
    }

    fn try_build_lora<C>(chip: C) -> TestRadio<Result<TestLoRa<C>, RadioError>>
    where
        C: Sx126xVariant,
    {
        try_build_lora_with_ticks(chip, test_ticks)
    }

    fn build_lora_with_ticks<C>(chip: C, now_ticks: fn() -> u64) -> TestRadio<TestLoRa<C>>
    where
        C: Sx126xVariant,
    {
        let TestRadio {
            lora,
            commands,
            command_ctx,
            cad_activity,
            rx_preamble,
            rx_timeout,
            arm,
            rx_timestamps,
            ctx,
            power,
            csd,
            fem_delays,
        } = try_build_lora_with_ticks(chip, now_ticks);
        TestRadio {
            lora: lora.unwrap(),
            commands,
            command_ctx,
            cad_activity,
            rx_preamble,
            rx_timeout,
            arm,
            rx_timestamps,
            ctx,
            power,
            csd,
            fem_delays,
        }
    }

    fn build_lora<C>(chip: C) -> TestRadio<TestLoRa<C>>
    where
        C: Sx126xVariant,
    {
        build_lora_with_ticks(chip, test_ticks)
    }

    type TestTrackerOwner = TrackerRadio<
        MockSpi,
        MockOutput,
        MockWait,
        MockWait,
        MockOutput,
        MockOutput,
        MockOutput,
        ProbeDelay,
        NoopDelay,
    >;

    struct TestTrackerOwnerRig {
        radio: TestTrackerOwner,
        commands: CommandLog,
        cad_activity: CadActivityProbe,
        rx_preamble: RxPreambleProbe,
        rx_timeout: RxTimeoutProbe,
        ctx: PinProbe,
        power: PinProbe,
        csd: PinProbe,
    }

    fn build_tracker_owner(
        rx_timestamps: &'static TrackerRxTimestampCapture,
    ) -> TestTrackerOwnerRig {
        let arm = Box::leak(Box::new(TrackerTxArm::new()));
        let events = Rc::new(RefCell::new(Vec::new()));
        let (reset, _) = MockOutput::new(PinState::Low, "reset", events.clone());
        let (power, power_probe) = MockOutput::new(PinState::Low, "power", events.clone());
        let (csd, csd_probe) = MockOutput::new(PinState::Low, "csd", events.clone());
        let (ctx, ctx_probe) = MockOutput::new(PinState::Low, "ctx", events);
        let (fem_delay, _) = ProbeDelay::new();
        let (spi, commands, _, cad_activity, rx_preamble, rx_timeout) =
            MockSpi::new(ctx_probe.clone());
        let radio = block_on(TrackerRadio::new(
            spi,
            reset,
            MockWait,
            MockWait,
            power,
            csd,
            ctx,
            NoopDelay,
            fem_delay,
            arm,
            rx_timestamps,
            TRACKER_NA915_DEV_CONFIGURATION,
        ))
        .unwrap();

        TestTrackerOwnerRig {
            radio,
            commands,
            cad_activity,
            rx_preamble,
            rx_timeout,
            ctx: ctx_probe,
            power: power_probe,
            csd: csd_probe,
        }
    }

    fn build_tracker_owner_with_ticks(now_ticks: fn() -> u64) -> TestTrackerOwnerRig {
        build_tracker_owner(Box::leak(Box::new(
            TrackerRxTimestampCapture::new_monotonic_us(now_ticks),
        )))
    }

    fn prepare_lora<C>(
        lora: &mut TestLoRa<C>,
        arm: &TrackerTxArm,
        configuration: TrackerRadioConfiguration,
    ) where
        C: Sx126xVariant,
    {
        let profile = configuration.profile();
        let modulation = lora
            .create_modulation_params(
                profile.spreading_factor(),
                profile.bandwidth(),
                profile.coding_rate(),
                profile.frequency_hz(),
            )
            .unwrap();
        let mut packet = lora
            .create_tx_packet_params(
                profile.preamble_symbols(),
                !profile.explicit_header(),
                profile.crc(),
                profile.iq_inverted(),
                &modulation,
            )
            .unwrap();
        arm.arm_for_prepare();
        block_on(lora.prepare_for_tx(
            &modulation,
            &mut packet,
            configuration.power().modem_output_dbm(),
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
    fn named_power_is_explicit_and_product_configuration_is_feature_invariant() {
        assert_eq!(TRACKER_NA915_DEV_TARGET_POWER_DBM, 14);
        assert_eq!(TRACKER_NA915_DEV_MODEM_OUTPUT_DBM, 0);
        assert_eq!(
            TRACKER_NA915_DEV_CONFIGURATION.id(),
            TrackerRadioConfigurationId::Na915DevCalibratedMinimum
        );
        assert_eq!(
            TRACKER_NA915_DEV_CONFIGURATION.power(),
            TrackerRadioPower::CalibratedMinimum
        );
        assert_eq!(
            TRACKER_NA915_DEV_CONFIGURATION.fingerprint(),
            TRACKER_NA915_DEV_CONFIGURATION_FINGERPRINT
        );

        #[cfg(feature = "near-field-attenuation")]
        {
            assert_eq!(TRACKER_NA915_NEAR_FIELD_DIAGNOSTIC_TARGET_POWER_DBM, 5);
            assert_eq!(TRACKER_NA915_NEAR_FIELD_DIAGNOSTIC_MODEM_OUTPUT_DBM, -9);
            assert_eq!(
                TRACKER_NA915_NEAR_FIELD_DIAGNOSTIC_CONFIGURATION.id(),
                TrackerRadioConfigurationId::Na915DevDiagnosticNearFieldAttenuation
            );
            assert_eq!(
                TRACKER_NA915_NEAR_FIELD_DIAGNOSTIC_CONFIGURATION.power(),
                TrackerRadioPower::DiagnosticNearFieldAttenuation
            );
            assert_ne!(
                TRACKER_NA915_NEAR_FIELD_DIAGNOSTIC_CONFIGURATION.fingerprint(),
                TRACKER_NA915_DEV_CONFIGURATION_FINGERPRINT
            );
        }
    }

    #[test]
    fn product_configuration_is_exactly_the_fixed_na915_profile() {
        let configuration = TRACKER_NA915_DEV_CONFIGURATION;
        let profile = configuration.profile();
        assert_eq!(profile.frequency_hz(), 915_000_000);
        assert_eq!(profile.spreading_factor().factor(), 7);
        assert_eq!(profile.bandwidth().hz(), 125_000);
        assert_eq!(profile.coding_rate().denom(), 5);
        assert_eq!(profile.preamble_symbols(), 24);
        assert!(profile.explicit_header());
        assert!(profile.crc());
        assert!(!profile.iq_inverted());
        assert_eq!(configuration.rx_symbol_timeout(), 248);
        assert_eq!(TRACKER_MAXIMUM_RECEIVE_OPERATION_US.get(), 1_500_000);
        assert!(!configuration.public_network());
        assert_eq!(TRACKER_PRIVATE_SYNC_WORD, 0x1424);
        assert!(!configuration.uses_dcdc());
        assert!(!configuration.boosted_receive());
        let chip = TrackerSx1262::new(configuration.power());
        assert_eq!(
            chip.high_power_pa_override(
                TRACKER_NA915_DEV_MODEM_OUTPUT_DBM,
                TRACKER_NA915_DEV_MODEM_OUTPUT_DBM,
            ),
            Some(HighPowerPaOverride {
                pa_duty_cycle: 0x04,
                hp_max: 0x07,
                tx_params_power: TRACKER_NA915_DEV_MODEM_OUTPUT_DBM as u8,
                ocp: Some(0x28),
            })
        );
        for unexpected_power in [-8, -1, 1, 22] {
            assert_eq!(
                chip.high_power_pa_override(unexpected_power, unexpected_power),
                None
            );
        }
        assert_eq!(Sx1262.high_power_pa_override(0, 0), None);
    }

    #[test]
    fn whole_receive_watchdog_tracks_fixed_profile_airtime_and_search_window() {
        let profile = TRACKER_NA915_DEV_CONFIGURATION.profile();
        let symbols_per_chirp = 1_u64 << profile.spreading_factor().factor();
        let symbol_duration_us = symbols_per_chirp
            .saturating_mul(1_000_000)
            .div_ceil(profile.bandwidth().hz() as u64);
        let search_window_us = symbol_duration_us.saturating_mul(u64::from(
            TRACKER_NA915_DEV_CONFIGURATION.rx_symbol_timeout(),
        ));
        let required_us = search_window_us
            .saturating_add(profile.maximum_frame_time_on_air_us())
            .saturating_add(TRACKER_RX_WATCHDOG_MINIMUM_MARGIN_US);

        assert!(TRACKER_MAXIMUM_RECEIVE_OPERATION_US.get() >= required_us);
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
        } = build_lora(TrackerSx1262::new(TRACKER_NA915_DEV_CONFIGURATION.power()));
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
        prepare_lora(&mut stock_lora, stock_arm, TRACKER_NA915_DEV_CONFIGURATION);
        prepare_lora(
            &mut tracker_lora,
            tracker_arm,
            TRACKER_NA915_DEV_CONFIGURATION,
        );
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
        assert_eq!(tracker_commands.len(), before_standby + 3);
        assert_eq!(
            &tracker_commands[before_standby..],
            &[
                Vec::from([0x80, 0x00]),
                Vec::from([0x08, 0, 0, 0, 0, 0, 0, 0, 0]),
                Vec::from([0x02, 0xff, 0xff]),
            ]
        );
    }

    #[test]
    #[cfg(feature = "near-field-attenuation")]
    fn attenuation_feature_keeps_rnode_pa_topology_at_minimum_encoded_power() {
        let TestRadio {
            mut lora,
            commands,
            command_ctx,
            arm,
            ctx,
            ..
        } = build_lora(TrackerSx1262::new(
            TRACKER_NA915_NEAR_FIELD_DIAGNOSTIC_CONFIGURATION.power(),
        ));
        commands.borrow_mut().clear();
        command_ctx.borrow_mut().clear();

        prepare_lora(
            &mut lora,
            arm,
            TRACKER_NA915_NEAR_FIELD_DIAGNOSTIC_CONFIGURATION,
        );

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
        } = build_lora(TrackerSx1262::new(TRACKER_NA915_DEV_CONFIGURATION.power()));

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
        prepare_lora(&mut lora, arm, TRACKER_NA915_DEV_CONFIGURATION);
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
    fn cad_clear_and_busy_restore_standby_before_timestamped_receive() {
        let TestTrackerOwnerRig {
            mut radio,
            commands,
            cad_activity,
            rx_preamble,
            ctx,
            ..
        } = build_tracker_owner_with_ticks(stepped_ticks);
        assert_eq!(radio.configuration(), TRACKER_NA915_DEV_CONFIGURATION);

        commands.borrow_mut().clear();
        cad_activity.set(false);
        assert!(!block_on(radio.channel_activity_detected()).unwrap());
        assert!(radio.is_active());
        assert!(!ctx.is_high());
        assert!(commands.borrow().iter().any(|command| command == &[0xc5]));
        let commands_after_cad = commands.borrow();
        assert!(commands_after_cad.ends_with(&[
            Vec::from([0x80, 0x00]),
            Vec::from([0x08, 0, 0, 0, 0, 0, 0, 0, 0]),
            Vec::from([0x02, 0xff, 0xff]),
        ]));
        drop(commands_after_cad);

        cad_activity.set(true);
        let cad = block_on(SoleRnodeRadio::cad(&mut radio)).unwrap();
        assert!(cad.activity_detected());
        assert!(radio.is_active());
        assert!(!ctx.is_high());

        STEPPED_TICKS.with(|ticks| ticks.set(200));
        rx_preamble.set(true);
        let mut buffer = [0_u8; SX1262_FRAME_MTU];
        let received = block_on(SoleRnodeRadio::receive_bounded(&mut radio, &mut buffer)).unwrap();
        let BoundedRxOutcome::Frame(received) = received else {
            panic!("mock must return RxDone")
        };
        assert_eq!(received.received_at_us(), 202);
        assert!(radio.is_active());
        assert!(!ctx.is_high());
    }

    #[test]
    fn sole_radio_split_tx_retains_one_owner_and_reports_each_final_irq() {
        let TestTrackerOwnerRig {
            mut radio,
            commands,
            cad_activity,
            ctx,
            ..
        } = build_tracker_owner_with_ticks(stepped_ticks);

        fn assert_sole_radio_impl<Radio: SoleRnodeRadio>() {}
        assert_sole_radio_impl::<TestTrackerOwner>();
        assert_eq!(
            SoleRnodeRadio::configuration_fingerprint(&radio),
            TRACKER_NA915_DEV_CONFIGURATION_FINGERPRINT
        );
        assert_eq!(
            SoleRnodeRadio::airtime_profile(&radio),
            TRACKER_NA915_DEV_CONFIGURATION.profile()
        );
        assert_eq!(
            SoleRnodeRadio::maximum_receive_operation_us(&radio),
            TRACKER_MAXIMUM_RECEIVE_OPERATION_US
        );

        STEPPED_TICKS.with(|ticks| ticks.set(300));
        cad_activity.set(false);
        let cad = block_on(SoleRnodeRadio::cad(&mut radio)).unwrap();
        assert!(!cad.activity_detected());
        assert_eq!(cad.observed_at_us(), 301);
        assert!(SoleRnodeRadio::is_active(&radio));

        let packet = [0x5a; 255];
        let mut first_output = [0_u8; SX1262_FRAME_MTU];
        let mut second_output = [0_u8; SX1262_FRAME_MTU];
        let frames = reticulum_radio_interface::frame_rns_packet(
            &packet,
            7,
            &mut first_output,
            &mut second_output,
        )
        .unwrap();
        assert_eq!(frames.frame_count(), 2);

        commands.borrow_mut().clear();
        STEPPED_TICKS.with(|ticks| ticks.set(400));
        let transmitted = block_on(SoleRnodeRadio::transmit(&mut radio, frames)).unwrap();
        assert_eq!(transmitted.progress().completed_frame_count(), 2);
        assert_eq!(transmitted.progress().frame_completed_at_us(0), Some(401));
        assert_eq!(transmitted.progress().frame_completed_at_us(1), Some(402));
        assert_eq!(
            commands
                .borrow()
                .iter()
                .filter(|command| command.as_slice() == [0x83, 0x00, 0x00, 0x00])
                .count(),
            2
        );
        assert!(SoleRnodeRadio::is_active(&radio));
        assert!(!ctx.is_high());
    }

    #[test]
    fn dropping_unpolled_sole_radio_futures_fails_closed() {
        let TestTrackerOwnerRig {
            mut radio,
            ctx,
            power,
            csd,
            ..
        } = build_tracker_owner_with_ticks(stepped_ticks);
        assert!(power.is_high());
        assert!(csd.is_high());

        let mut first_output = [0_u8; SX1262_FRAME_MTU];
        let mut second_output = [0_u8; SX1262_FRAME_MTU];
        let frames = reticulum_radio_interface::frame_rns_packet(
            b"cancel-before-poll",
            1,
            &mut first_output,
            &mut second_output,
        )
        .unwrap();
        let transmit = SoleRnodeRadio::transmit(&mut radio, frames);
        drop(transmit);

        assert!(!SoleRnodeRadio::is_active(&radio));
        assert!(!ctx.is_high());
        assert!(!power.is_high());
        assert!(!csd.is_high());

        let TestTrackerOwnerRig {
            mut radio,
            ctx,
            power,
            csd,
            ..
        } = build_tracker_owner_with_ticks(stepped_ticks);
        let cad = SoleRnodeRadio::cad(&mut radio);
        drop(cad);

        assert!(!SoleRnodeRadio::is_active(&radio));
        assert!(!ctx.is_high());
        assert!(!power.is_high());
        assert!(!csd.is_high());

        let TestTrackerOwnerRig {
            mut radio,
            ctx,
            power,
            csd,
            ..
        } = build_tracker_owner_with_ticks(stepped_ticks);
        let mut buffer = [0_u8; SX1262_FRAME_MTU];
        let receive = SoleRnodeRadio::receive_bounded(&mut radio, &mut buffer);
        drop(receive);

        assert!(!SoleRnodeRadio::is_active(&radio));
        assert!(!ctx.is_high());
        assert!(!power.is_high());
        assert!(!csd.is_high());
    }

    #[test]
    fn sole_radio_receive_timeout_is_reusable() {
        let TestTrackerOwnerRig {
            mut radio,
            rx_preamble,
            rx_timeout,
            ..
        } = build_tracker_owner_with_ticks(stepped_ticks);
        let mut buffer = [0xa5_u8; SX1262_FRAME_MTU];

        rx_preamble.set(false);
        rx_timeout.set(true);
        assert_eq!(
            block_on(SoleRnodeRadio::receive_bounded(&mut radio, &mut buffer)).unwrap(),
            BoundedRxOutcome::NoPreambleTimeout
        );
        assert_eq!(buffer, [0xa5; SX1262_FRAME_MTU]);
        assert!(SoleRnodeRadio::is_active(&radio));
    }

    #[test]
    fn legacy_tick_capture_is_rejected_before_sole_radio_operation() {
        let TestTrackerOwnerRig {
            mut radio,
            commands,
            ctx,
            power,
            csd,
            ..
        } = build_tracker_owner(Box::leak(Box::new(TrackerRxTimestampCapture::new(
            test_ticks,
        ))));
        commands.borrow_mut().clear();

        let fault = block_on(SoleRnodeRadio::cad(&mut radio)).unwrap_err();
        assert_eq!(
            reticulum_radio_interface::SoleRadioFault::phase(fault),
            SoleRadioFaultPhase::ChannelActivityPreparation
        );
        assert_eq!(
            reticulum_radio_interface::SoleRadioFault::class(fault),
            SoleRadioFaultClass::Configuration
        );
        assert!(
            !commands
                .borrow()
                .iter()
                .any(|command| command.first() == Some(&0xc5))
        );
        assert!(!SoleRnodeRadio::is_active(&radio));
        assert!(!ctx.is_high());
        assert!(!power.is_high());
        assert!(!csd.is_high());

        let TestTrackerOwnerRig {
            mut radio,
            commands,
            ctx,
            power,
            csd,
            ..
        } = build_tracker_owner(Box::leak(Box::new(TrackerRxTimestampCapture::new(
            test_ticks,
        ))));
        commands.borrow_mut().clear();
        let mut buffer = [0_u8; SX1262_FRAME_MTU];
        let fault = block_on(SoleRnodeRadio::receive_bounded(&mut radio, &mut buffer)).unwrap_err();
        assert_eq!(
            reticulum_radio_interface::SoleRadioFault::phase(fault),
            SoleRadioFaultPhase::ReceivePreparation
        );
        assert_eq!(
            reticulum_radio_interface::SoleRadioFault::class(fault),
            SoleRadioFaultClass::Configuration
        );
        assert!(
            !commands
                .borrow()
                .iter()
                .any(|command| command.first() == Some(&0x82))
        );
        assert!(!SoleRnodeRadio::is_active(&radio));
        assert!(!ctx.is_high());
        assert!(!power.is_high());
        assert!(!csd.is_high());
    }

    #[test]
    fn successful_rx_standby_allows_the_next_early_ctx_tx_prepare() {
        let TestRadio {
            mut lora, arm, ctx, ..
        } = build_lora(TrackerSx1262::new(TRACKER_NA915_DEV_CONFIGURATION.power()));
        let modulation = lora
            .create_modulation_params(
                TRACKER_NA915_DEV_PROFILE.spreading_factor(),
                TRACKER_NA915_DEV_PROFILE.bandwidth(),
                TRACKER_NA915_DEV_PROFILE.coding_rate(),
                TRACKER_NA915_DEV_PROFILE.frequency_hz(),
            )
            .unwrap();
        let rx_packet = lora
            .create_rx_packet_params(
                TRACKER_NA915_DEV_PROFILE.preamble_symbols(),
                !TRACKER_NA915_DEV_PROFILE.explicit_header(),
                SX1262_FRAME_MTU as u8,
                TRACKER_NA915_DEV_PROFILE.crc(),
                TRACKER_NA915_DEV_PROFILE.iq_inverted(),
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
                TRACKER_NA915_DEV_PROFILE.preamble_symbols(),
                !TRACKER_NA915_DEV_PROFILE.explicit_header(),
                TRACKER_NA915_DEV_PROFILE.crc(),
                TRACKER_NA915_DEV_PROFILE.iq_inverted(),
                &modulation,
            )
            .unwrap();
        arm.arm_for_prepare();
        block_on(lora.prepare_for_tx(
            &modulation,
            &mut tx_packet,
            TRACKER_NA915_DEV_MODEM_OUTPUT_DBM,
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
        static RX_TIMESTAMPS: TrackerRxTimestampCapture =
            TrackerRxTimestampCapture::new(test_ticks);
        let mut interface = TrackerInterface::new(
            reset,
            MockWait,
            MockWait,
            power,
            csd,
            ctx,
            NoopDelay,
            &ARM,
            &RX_TIMESTAMPS,
        )
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
