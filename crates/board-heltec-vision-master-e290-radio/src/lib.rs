//! Bidirectional HT-RA62/SX1262 owner for the Vision Master E290.
//!
//! This board layer admits one opaque NA915 development configuration. The
//! shared `reticulum-radio-lora-phy` state machine owns bounded RX, CAD, and
//! atomic one-or-two-frame RNode TX. This crate retains the E290-specific
//! module topology, reset containment, frequency and power policy.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::num::NonZeroU64;

use embedded_hal::digital::OutputPin;
use embedded_hal_async::{delay::DelayNs, digital::Wait, spi::SpiDevice};
use lora_phy::{
    mod_params::RadioError,
    mod_traits::InterfaceVariant,
    sx126x::{Sx1262, TcxoCtrlVoltage},
};
use reticulum_board_heltec_vision_master_e290::validate_lab_rx_profile;
use reticulum_radio_interface::{
    BoundedRxOutcome, CadObservation, LabRxProfile, LabRxProfileConfig, PacketTxFault,
    PacketTxObservation, RadioConfigurationFingerprint, RnodeTxFrames, SX1262_FRAME_MTU,
    SoleRadioFaultSummary, SoleRnodeRadio,
};
use reticulum_radio_lora_phy::{
    IrqTimestampCapture, NoopTxHooks, Sx126xRadioError, Sx126xRadioOperation, Sx126xReceivedFrame,
    Sx126xRnodeRadio, Sx126xRnodeSettings,
};

/// `lora-phy` private-network sync word selected by the E290 owner.
pub const E290_PRIVATE_SYNC_WORD: u16 = 0x1424;

/// DIO3-controlled TCXO voltage fitted inside the HT-RA62 module.
pub const E290_TCXO_CONTROL_VOLTAGE: TcxoCtrlVoltage = TcxoCtrlVoltage::Ctrl1V8;

/// TCXO wake time pinned by the reviewed `lora-phy` SX126x driver.
pub const E290_TCXO_WAKEUP_MS: u32 = 10;

/// Requested SX1262 output of the fixed development configuration.
///
/// The pinned driver realizes this target with the Semtech Rev. 2.2
/// Table 13-21 optimal +14 dBm PA row: `SetPaConfig(0x02, 0x02, 0x00,
/// 0x01)` and raw `SetTxParams(+22)`. This is still not a calibrated
/// conducted or radiated path claim.
pub const E290_NA915_DEV_REQUESTED_POWER_DBM: i32 = 14;

/// Raw `SetTxParams` power selected by the current Semtech optimal +14 dBm row.
pub const E290_NA915_DEV_RAW_SET_TX_PARAMS_DBM: i8 = 22;

/// Maximum preamble-search symbol timeout of the fixed configuration.
pub const E290_RX_SYMBOL_TIMEOUT: u16 = 248;

/// Minimum non-airtime allowance inside the whole-operation RX watchdog.
pub const E290_RX_WATCHDOG_MINIMUM_MARGIN_US: u64 = 250_000;

/// Conservative configuration-bound deadline for one complete receive operation.
///
/// This covers the 248-symbol search, a maximum frame after preamble
/// detection, SPI/IRQ/standby cleanup and substantial executor margin.
pub const E290_MAXIMUM_RECEIVE_OPERATION_US: NonZeroU64 = match NonZeroU64::new(1_500_000) {
    Some(bound) => bound,
    None => panic!("E290 receive watchdog must be non-zero"),
};

/// Stable identity for one immutable E290 radio configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum E290RadioConfigurationId {
    /// NA915 at 915 MHz, SF7/BW125/CR4/5 and requested 14 dBm output.
    Na915DevRequested14Dbm,
}

/// Full configuration identity for the first E290 NA915 profile.
pub const E290_NA915_DEV_CONFIGURATION_FINGERPRINT: RadioConfigurationFingerprint =
    RadioConfigurationFingerprint::new([
        0x45, 0x32, 0x39, 0x30, 0x4e, 0x41, 0x39, 0x31, 0x35, 0x44, 0x45, 0x56, 0x30, 0x30, 0x30,
        0x31,
    ]);

/// Fixed RNode-compatible NA915 LoRa profile admitted by this board owner.
pub const E290_NA915_DEV_PROFILE: LabRxProfile = match validate_lab_rx_profile(LabRxProfileConfig {
    frequency_hz: Some(915_000_000),
    spreading_factor: 7,
    bandwidth_hz: 125_000,
    coding_rate_denominator: 5,
    preamble_symbols: 24,
    explicit_header: true,
    crc: true,
    iq_inverted: false,
}) {
    Ok(profile) => profile,
    Err(_) => panic!("invalid fixed E290 NA915 development profile"),
};

/// Opaque board-validated modem, module and power configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct E290RadioConfiguration {
    id: E290RadioConfigurationId,
    profile: LabRxProfile,
    fingerprint: RadioConfigurationFingerprint,
    requested_power_dbm: i32,
    rx_symbol_timeout: u16,
    maximum_receive_operation_us: NonZeroU64,
    public_network: bool,
    use_dcdc: bool,
    rx_boost: bool,
}

impl E290RadioConfiguration {
    const fn na915_dev() -> Self {
        Self {
            id: E290RadioConfigurationId::Na915DevRequested14Dbm,
            profile: E290_NA915_DEV_PROFILE,
            fingerprint: E290_NA915_DEV_CONFIGURATION_FINGERPRINT,
            requested_power_dbm: E290_NA915_DEV_REQUESTED_POWER_DBM,
            rx_symbol_timeout: E290_RX_SYMBOL_TIMEOUT,
            maximum_receive_operation_us: E290_MAXIMUM_RECEIVE_OPERATION_US,
            public_network: false,
            use_dcdc: true,
            rx_boost: false,
        }
    }

    /// Stable identity for the complete configuration.
    pub const fn id(self) -> E290RadioConfigurationId {
        self.id
    }

    /// Fixed modulation and packet profile.
    pub const fn profile(self) -> LabRxProfile {
        self.profile
    }

    /// Full permit-binding identity.
    pub const fn fingerprint(self) -> RadioConfigurationFingerprint {
        self.fingerprint
    }

    /// Requested SX1262 output power, without an antenna-path claim.
    pub const fn requested_power_dbm(self) -> i32 {
        self.requested_power_dbm
    }

    /// Maximum preamble-search duration in symbols.
    pub const fn rx_symbol_timeout(self) -> u16 {
        self.rx_symbol_timeout
    }

    /// Whole receive-operation watchdog bound.
    pub const fn maximum_receive_operation_us(self) -> NonZeroU64 {
        self.maximum_receive_operation_us
    }

    /// Whether the public LoRa network sync word is selected.
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

    const fn shared_settings(self) -> Sx126xRnodeSettings {
        Sx126xRnodeSettings::new(
            self.profile,
            self.fingerprint,
            self.requested_power_dbm,
            self.rx_symbol_timeout,
            self.public_network,
            self.use_dcdc,
            self.rx_boost,
            self.maximum_receive_operation_us,
        )
    }
}

/// Immutable first E290 NA915 development configuration.
pub const E290_NA915_DEV_CONFIGURATION: E290RadioConfiguration =
    E290RadioConfiguration::na915_dev();

/// Operation in which the E290 owner failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum E290RadioOperation {
    /// Initial reset-low pin containment.
    InterfaceConstruction,
    /// SX1262 reset and LoRa initialization.
    ChipInitialization,
    /// Modulation parameter construction.
    ModulationConfiguration,
    /// Packet parameter construction.
    PacketConfiguration,
    /// Caller supplied an invalid physical frame.
    FrameValidation,
    /// FIFO, packet, channel and power preparation.
    PrepareTransmit,
    /// Physical transmission.
    Transmit,
    /// Post-transmit standby cleanup.
    TransmitCleanup,
    /// Modem and IRQ preparation for CAD.
    PrepareChannelActivityDetection,
    /// CAD and IRQ completion.
    ChannelActivityDetection,
    /// Post-CAD standby cleanup.
    ChannelActivityCleanup,
    /// Bounded receive preparation.
    PrepareReceive,
    /// Bounded receive execution.
    Receive,
    /// Post-receive standby cleanup.
    ReceiveCleanup,
    /// Successful receive without a final DIO timestamp.
    ReceiveTimestamp,
}

/// Detailed failure from the E290 board boundary.
#[derive(Debug)]
pub struct E290RadioError {
    /// Exact operation that failed.
    pub operation: E290RadioOperation,
    /// Driver classification, when the radio path reported the failure.
    pub radio: Option<RadioError>,
}

impl E290RadioError {
    const fn radio(operation: E290RadioOperation, radio: RadioError) -> Self {
        Self {
            operation,
            radio: Some(radio),
        }
    }
}

fn map_shared_operation(operation: Sx126xRadioOperation) -> E290RadioOperation {
    match operation {
        Sx126xRadioOperation::ChipInitialization => E290RadioOperation::ChipInitialization,
        Sx126xRadioOperation::ModulationConfiguration => {
            E290RadioOperation::ModulationConfiguration
        }
        Sx126xRadioOperation::PacketConfiguration => E290RadioOperation::PacketConfiguration,
        Sx126xRadioOperation::FrameValidation => E290RadioOperation::FrameValidation,
        Sx126xRadioOperation::PrepareTransmit => E290RadioOperation::PrepareTransmit,
        Sx126xRadioOperation::Transmit => E290RadioOperation::Transmit,
        Sx126xRadioOperation::TransmitCleanup => E290RadioOperation::TransmitCleanup,
        Sx126xRadioOperation::PrepareChannelActivityDetection => {
            E290RadioOperation::PrepareChannelActivityDetection
        }
        Sx126xRadioOperation::ChannelActivityDetection => {
            E290RadioOperation::ChannelActivityDetection
        }
        Sx126xRadioOperation::ChannelActivityCleanup => E290RadioOperation::ChannelActivityCleanup,
        Sx126xRadioOperation::PrepareReceive => E290RadioOperation::PrepareReceive,
        Sx126xRadioOperation::Receive => E290RadioOperation::Receive,
        Sx126xRadioOperation::ReceiveCleanup => E290RadioOperation::ReceiveCleanup,
        Sx126xRadioOperation::ReceiveTimestamp => E290RadioOperation::ReceiveTimestamp,
    }
}

fn map_shared_error(error: Sx126xRadioError) -> E290RadioError {
    E290RadioError {
        operation: map_shared_operation(error.operation),
        radio: error.radio,
    }
}

/// One physical frame received into a caller-owned buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct E290ReceivedFrame {
    /// Number of valid bytes in the caller's buffer.
    pub len: u8,
    /// Packet RSSI reported by the SX1262.
    pub rssi_dbm: i16,
    /// Packet SNR reported by the SX1262.
    pub snr_db: i16,
    /// Monotonic sample taken when the final DIO1 wait resumed.
    pub received_at_ticks: u64,
}

struct E290Interface<Reset, Dio1, Busy>
where
    Reset: OutputPin,
{
    reset: Reset,
    dio1: Dio1,
    busy: Busy,
    timestamps: &'static IrqTimestampCapture,
}

impl<Reset, Dio1, Busy> E290Interface<Reset, Dio1, Busy>
where
    Reset: OutputPin,
{
    fn new(
        reset: Reset,
        dio1: Dio1,
        busy: Busy,
        timestamps: &'static IrqTimestampCapture,
    ) -> Result<Self, RadioError> {
        let mut owner = Self {
            reset,
            dio1,
            busy,
            timestamps,
        };
        owner.reset.set_low().map_err(|_| RadioError::Reset)?;
        Ok(owner)
    }
}

impl<Reset, Dio1, Busy> InterfaceVariant for E290Interface<Reset, Dio1, Busy>
where
    Reset: OutputPin,
    Dio1: Wait,
    Busy: Wait,
{
    async fn reset(&mut self, delay: &mut impl DelayNs) -> Result<(), RadioError> {
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
        self.timestamps.record_irq_observation();
        Ok(())
    }

    async fn enable_rf_switch_rx(&mut self) -> Result<(), RadioError> {
        Ok(())
    }

    async fn enable_rf_switch_tx(&mut self) -> Result<(), RadioError> {
        Ok(())
    }

    async fn disable_rf_switch(&mut self) -> Result<(), RadioError> {
        Ok(())
    }
}

impl<Reset, Dio1, Busy> Drop for E290Interface<Reset, Dio1, Busy>
where
    Reset: OutputPin,
{
    fn drop(&mut self) {
        let _ = self.reset.set_low();
    }
}

static NOOP_TX_HOOKS: NoopTxHooks = NoopTxHooks;

type E290Core<Spi, Reset, Dio1, Busy, RadioDelay> =
    Sx126xRnodeRadio<Spi, E290Interface<Reset, Dio1, Busy>, Sx1262, RadioDelay, NoopTxHooks>;

/// Opaque, reset-scoped E290 HT-RA62 RX/CAD/TX owner.
pub struct E290Radio<Spi, Reset, Dio1, Busy, RadioDelay>
where
    Spi: SpiDevice<u8>,
    Reset: OutputPin,
    Dio1: Wait,
    Busy: Wait,
    RadioDelay: DelayNs,
{
    core: E290Core<Spi, Reset, Dio1, Busy, RadioDelay>,
    configuration: E290RadioConfiguration,
}

impl<Spi, Reset, Dio1, Busy, RadioDelay> E290Radio<Spi, Reset, Dio1, Busy, RadioDelay>
where
    Spi: SpiDevice<u8>,
    Reset: OutputPin,
    Dio1: Wait,
    Busy: Wait,
    RadioDelay: DelayNs,
{
    /// Assert reset low, then initialize the HT-RA62 with its internal DIO2
    /// RF switch, DIO3 1.8 V TCXO and DC-DC regulator policy.
    pub async fn new(
        spi: Spi,
        reset: Reset,
        dio1: Dio1,
        busy: Busy,
        radio_delay: RadioDelay,
        timestamps: &'static IrqTimestampCapture,
        configuration: E290RadioConfiguration,
    ) -> Result<Self, E290RadioError> {
        let interface = E290Interface::new(reset, dio1, busy, timestamps).map_err(|radio| {
            E290RadioError::radio(E290RadioOperation::InterfaceConstruction, radio)
        })?;
        let core = Sx126xRnodeRadio::new(
            spi,
            interface,
            Sx1262,
            radio_delay,
            &NOOP_TX_HOOKS,
            timestamps,
            configuration.shared_settings(),
            Some(E290_TCXO_CONTROL_VOLTAGE),
        )
        .await
        .map_err(map_shared_error)?;
        Ok(Self {
            core,
            configuration,
        })
    }

    /// Immutable configuration selected for this owner.
    pub const fn configuration(&self) -> E290RadioConfiguration {
        self.configuration
    }

    /// Transmit exactly one physical frame using the requested 14 dBm profile.
    pub async fn transmit_frame(&mut self, frame: &[u8]) -> Result<(), E290RadioError> {
        self.core
            .transmit_frame(frame)
            .await
            .map_err(map_shared_error)
    }

    /// Run one low-level LoRa channel activity detection operation.
    pub async fn channel_activity_detected(&mut self) -> Result<bool, E290RadioError> {
        self.core
            .channel_activity_detected()
            .await
            .map_err(map_shared_error)
    }

    /// Run one receive operation with bounded preamble search.
    pub fn receive_frame<'a>(
        &'a mut self,
        buffer: &'a mut [u8; SX1262_FRAME_MTU],
    ) -> impl core::future::Future<Output = Result<Option<E290ReceivedFrame>, E290RadioError>> + 'a
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
                         }| E290ReceivedFrame {
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

    /// Drop initialized ownership and assert module reset low.
    pub fn shutdown(&mut self) {
        self.core.shutdown();
    }

    /// Whether initialized radio hardware remains usable.
    pub const fn is_active(&self) -> bool {
        self.core.is_active()
    }
}

impl<Spi, Reset, Dio1, Busy, RadioDelay> SoleRnodeRadio
    for E290Radio<Spi, Reset, Dio1, Busy, RadioDelay>
where
    Spi: SpiDevice<u8>,
    Reset: OutputPin,
    Dio1: Wait,
    Busy: Wait,
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

#[cfg(test)]
mod tests {
    extern crate std;

    use core::{
        cell::{Cell, RefCell},
        future::Future,
        task::Poll,
    };
    use std::{boxed::Box, rc::Rc, vec::Vec};

    use embedded_hal::{
        digital::{ErrorType, PinState},
        spi::{ErrorKind, ErrorType as SpiErrorType, Operation},
    };
    use reticulum_radio_interface::{SoleRadioFault, SoleRadioFaultClass, SoleRadioFaultPhase};

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

    std::thread_local! {
        static CLOCK_US: Cell<u64> = const { Cell::new(0) };
    }

    fn stepped_us() -> u64 {
        CLOCK_US.with(|clock| {
            let next = clock.get().saturating_add(1);
            clock.set(next);
            next
        })
    }

    #[derive(Clone)]
    struct PinProbe(Rc<Cell<bool>>);

    impl PinProbe {
        fn is_high(&self) -> bool {
            self.0.get()
        }
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct MockPinError;

    impl embedded_hal::digital::Error for MockPinError {
        fn kind(&self) -> embedded_hal::digital::ErrorKind {
            embedded_hal::digital::ErrorKind::Other
        }
    }

    struct MockOutput {
        probe: PinProbe,
        events: Rc<RefCell<Vec<bool>>>,
        fail_high: Rc<Cell<bool>>,
    }

    type PinEvents = Rc<RefCell<Vec<bool>>>;
    type FailHighProbe = Rc<Cell<bool>>;

    impl MockOutput {
        fn new(initial: PinState) -> (Self, PinProbe, PinEvents, FailHighProbe) {
            let probe = PinProbe(Rc::new(Cell::new(initial == PinState::High)));
            let events = Rc::new(RefCell::new(Vec::new()));
            let fail_high = Rc::new(Cell::new(false));
            (
                Self {
                    probe: probe.clone(),
                    events: events.clone(),
                    fail_high: fail_high.clone(),
                },
                probe,
                events,
                fail_high,
            )
        }
    }

    impl ErrorType for MockOutput {
        type Error = MockPinError;
    }

    impl OutputPin for MockOutput {
        fn set_low(&mut self) -> Result<(), Self::Error> {
            self.probe.0.set(false);
            self.events.borrow_mut().push(false);
            Ok(())
        }

        fn set_high(&mut self) -> Result<(), Self::Error> {
            if self.fail_high.get() {
                return Err(MockPinError);
            }
            self.probe.0.set(true);
            self.events.borrow_mut().push(true);
            Ok(())
        }
    }

    struct MockWait;

    impl ErrorType for MockWait {
        type Error = core::convert::Infallible;
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

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct MockSpiError;

    impl embedded_hal::spi::Error for MockSpiError {
        fn kind(&self) -> ErrorKind {
            ErrorKind::Other
        }
    }

    type CommandLog = Rc<RefCell<Vec<Vec<u8>>>>;

    struct MockSpi {
        commands: CommandLog,
        cad_activity: Rc<Cell<bool>>,
        cad_pending: bool,
        rx_success: Rc<Cell<bool>>,
        rx_timeout: Rc<Cell<bool>>,
        rx_preamble_pending: bool,
        rx_done_pending: bool,
        rx_timeout_pending: bool,
        fail_opcode: Rc<Cell<Option<u8>>>,
        fail_set_tx_number: Rc<Cell<u8>>,
        set_tx_count: u8,
    }

    struct MockSpiControls {
        commands: CommandLog,
        cad_activity: Rc<Cell<bool>>,
        rx_success: Rc<Cell<bool>>,
        rx_timeout: Rc<Cell<bool>>,
        fail_opcode: Rc<Cell<Option<u8>>>,
        fail_set_tx_number: Rc<Cell<u8>>,
    }

    impl MockSpi {
        fn new() -> (Self, MockSpiControls) {
            let commands = Rc::new(RefCell::new(Vec::new()));
            let cad_activity = Rc::new(Cell::new(false));
            let rx_success = Rc::new(Cell::new(false));
            let rx_timeout = Rc::new(Cell::new(false));
            let fail_opcode = Rc::new(Cell::new(None));
            let fail_set_tx_number = Rc::new(Cell::new(0));
            (
                Self {
                    commands: commands.clone(),
                    cad_activity: cad_activity.clone(),
                    cad_pending: false,
                    rx_success: rx_success.clone(),
                    rx_timeout: rx_timeout.clone(),
                    rx_preamble_pending: false,
                    rx_done_pending: false,
                    rx_timeout_pending: false,
                    fail_opcode: fail_opcode.clone(),
                    fail_set_tx_number: fail_set_tx_number.clone(),
                    set_tx_count: 0,
                },
                MockSpiControls {
                    commands,
                    cad_activity,
                    rx_success,
                    rx_timeout,
                    fail_opcode,
                    fail_set_tx_number,
                },
            )
        }
    }

    impl SpiErrorType for MockSpi {
        type Error = MockSpiError;
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
            let opcode = command.first().copied();
            self.commands.borrow_mut().push(command);

            if opcode == Some(0x83) {
                self.set_tx_count = self.set_tx_count.saturating_add(1);
                if self.fail_set_tx_number.get() == self.set_tx_count {
                    return Err(MockSpiError);
                }
            }
            if opcode.is_some() && opcode == self.fail_opcode.get() {
                return Err(MockSpiError);
            }
            if opcode == Some(0xc5) {
                self.cad_pending = true;
            }
            if opcode == Some(0x82) {
                if self.rx_success.get() {
                    self.rx_preamble_pending = true;
                } else if self.rx_timeout.get() {
                    self.rx_timeout_pending = true;
                }
            }

            let is_irq = opcode == Some(0x12);
            let irq = if self.cad_pending && is_irq {
                self.cad_pending = false;
                if self.cad_activity.get() {
                    0x0180_u16
                } else {
                    0x0080_u16
                }
            } else if is_irq && self.rx_preamble_pending {
                self.rx_preamble_pending = false;
                self.rx_done_pending = true;
                0x0004_u16
            } else if is_irq && self.rx_done_pending {
                self.rx_done_pending = false;
                0x0002_u16
            } else if is_irq && self.rx_timeout_pending {
                self.rx_timeout_pending = false;
                0x0200_u16
            } else {
                0x0001_u16
            };

            let mut read_index = 0_usize;
            for operation in operations.iter_mut() {
                if let Operation::Read(bytes) = operation {
                    bytes.fill(0);
                    if is_irq && read_index == 1 && bytes.len() >= 2 {
                        bytes[0] = (irq >> 8) as u8;
                        bytes[1] = irq as u8;
                    } else if opcode == Some(0x13) && read_index == 1 && bytes.len() >= 2 {
                        bytes[0] = 3;
                        bytes[1] = 0;
                    } else if opcode == Some(0x1e) {
                        for (output, input) in bytes.iter_mut().zip([0xa1, 0xb2, 0xc3]) {
                            *output = input;
                        }
                    } else if opcode == Some(0x14) && read_index == 1 && bytes.len() >= 3 {
                        bytes[0] = 100;
                        bytes[1] = 8;
                        bytes[2] = 104;
                    }
                    read_index = read_index.saturating_add(1);
                }
            }
            Ok(())
        }
    }

    type TestRadio = E290Radio<MockSpi, MockOutput, MockWait, MockWait, NoopDelay>;

    struct TestRig {
        radio: TestRadio,
        controls: MockSpiControls,
        reset: PinProbe,
        reset_events: Rc<RefCell<Vec<bool>>>,
    }

    fn build_radio() -> TestRig {
        let (spi, controls) = MockSpi::new();
        let (reset, reset_probe, reset_events, _) = MockOutput::new(PinState::High);
        let timestamps = Box::leak(Box::new(IrqTimestampCapture::new_monotonic_us(stepped_us)));
        let radio = block_on(E290Radio::new(
            spi,
            reset,
            MockWait,
            MockWait,
            NoopDelay,
            timestamps,
            E290_NA915_DEV_CONFIGURATION,
        ))
        .unwrap();
        TestRig {
            radio,
            controls,
            reset: reset_probe,
            reset_events,
        }
    }

    fn command_index(commands: &[Vec<u8>], command: &[u8]) -> usize {
        commands
            .iter()
            .position(|actual| actual.as_slice() == command)
            .unwrap_or_else(|| panic!("missing command {command:02x?}"))
    }

    #[test]
    fn opaque_configuration_is_exact_and_watchdog_is_conservative() {
        let configuration = E290_NA915_DEV_CONFIGURATION;
        let profile = configuration.profile();
        assert_eq!(
            configuration.id(),
            E290RadioConfigurationId::Na915DevRequested14Dbm
        );
        assert_eq!(
            configuration.fingerprint(),
            E290_NA915_DEV_CONFIGURATION_FINGERPRINT
        );
        assert_eq!(profile.frequency_hz(), 915_000_000);
        assert_eq!(profile.spreading_factor().factor(), 7);
        assert_eq!(profile.bandwidth().hz(), 125_000);
        assert_eq!(profile.coding_rate().denom(), 5);
        assert_eq!(profile.preamble_symbols(), 24);
        assert!(profile.explicit_header());
        assert!(profile.crc());
        assert!(!profile.iq_inverted());
        assert_eq!(configuration.requested_power_dbm(), 14);
        assert_eq!(E290_NA915_DEV_RAW_SET_TX_PARAMS_DBM, 22);
        assert_eq!(configuration.rx_symbol_timeout(), 248);
        assert!(!configuration.public_network());
        assert!(configuration.uses_dcdc());
        assert!(!configuration.boosted_receive());
        assert_eq!(E290_PRIVATE_SYNC_WORD, 0x1424);
        assert_eq!(E290_TCXO_WAKEUP_MS, 10);

        let symbol_duration_us = (1_u64 << profile.spreading_factor().factor())
            .saturating_mul(1_000_000)
            .div_ceil(u64::from(profile.bandwidth().hz()));
        let required_us = symbol_duration_us
            .saturating_mul(u64::from(configuration.rx_symbol_timeout()))
            .saturating_add(profile.maximum_frame_time_on_air_us())
            .saturating_add(E290_RX_WATCHDOG_MINIMUM_MARGIN_US);
        assert!(configuration.maximum_receive_operation_us().get() >= required_us);
    }

    #[test]
    fn initialization_programs_exact_module_topology_before_operation() {
        let TestRig {
            radio,
            controls,
            reset,
            reset_events,
        } = build_radio();
        assert!(radio.is_active());
        assert!(reset.is_high());
        assert_eq!(reset_events.borrow().as_slice(), &[false, false, true]);

        let commands = controls.commands.borrow();
        let regulator = command_index(&commands, &[0x96, 0x01]);
        let dio2_switch = command_index(&commands, &[0x9d, 0x01]);
        let tcxo = command_index(&commands, &[0x97, 0x02, 0x00, 0x02, 0x80]);
        assert!(regulator < dio2_switch && dio2_switch < tcxo);
        command_index(&commands, &[0x0d, 0x07, 0x40, 0x14, 0x24]);
        assert!(
            commands
                .iter()
                .any(|command| command == &[0x95, 0x02, 0x02, 0x00, 0x01])
        );
        assert!(
            !commands
                .iter()
                .any(|command| command.starts_with(&[0x0d, 0x08, 0xe7]))
        );
        assert!(
            !commands
                .iter()
                .any(|command| matches!(command.first(), Some(0x82 | 0x83 | 0xc5)))
        );
    }

    #[test]
    fn one_and_two_frame_tx_use_rev_2_2_optimal_14_dbm_commands_and_report_final_irqs() {
        let TestRig {
            mut radio,
            controls,
            reset,
            ..
        } = build_radio();
        controls.commands.borrow_mut().clear();
        CLOCK_US.with(|clock| clock.set(100));

        let mut first_output = [0_u8; SX1262_FRAME_MTU];
        let mut second_output = [0_u8; SX1262_FRAME_MTU];
        let one = reticulum_radio_interface::frame_rns_packet(
            b"e290-one",
            1,
            &mut first_output,
            &mut second_output,
        )
        .unwrap();
        let observation = block_on(SoleRnodeRadio::transmit(&mut radio, one)).unwrap();
        assert_eq!(observation.progress().completed_frame_count(), 1);
        assert_eq!(observation.progress().frame_completed_at_us(0), Some(101));
        let commands = controls.commands.borrow();
        command_index(&commands, &[0x95, 0x02, 0x02, 0x00, 0x01]);
        command_index(&commands, &[0x8e, 0x16, 0x02]);
        assert_eq!(
            commands
                .iter()
                .filter(|command| command.as_slice() == [0x83, 0, 0, 0])
                .count(),
            1
        );
        assert!(
            !commands
                .iter()
                .any(|command| command.starts_with(&[0x0d, 0x08, 0xe7]))
        );
        drop(commands);

        controls.commands.borrow_mut().clear();
        CLOCK_US.with(|clock| clock.set(200));
        let packet = [0x5a; 255];
        let two = reticulum_radio_interface::frame_rns_packet(
            &packet,
            2,
            &mut first_output,
            &mut second_output,
        )
        .unwrap();
        assert_eq!(two.frame_count(), 2);
        let observation = block_on(SoleRnodeRadio::transmit(&mut radio, two)).unwrap();
        assert_eq!(observation.progress().completed_frame_count(), 2);
        assert_eq!(observation.progress().frame_completed_at_us(0), Some(201));
        assert_eq!(observation.progress().frame_completed_at_us(1), Some(202));
        let commands = controls.commands.borrow();
        assert_eq!(
            commands
                .iter()
                .filter(|command| command.as_slice() == [0x95, 0x02, 0x02, 0x00, 0x01])
                .count(),
            2
        );
        assert_eq!(
            commands
                .iter()
                .filter(|command| command.as_slice() == [0x8e, 0x16, 0x02])
                .count(),
            2
        );
        assert!(radio.is_active());
        assert!(reset.is_high());
    }

    #[test]
    fn second_frame_spi_fault_retains_partial_progress_and_fails_closed() {
        let TestRig {
            mut radio,
            controls,
            reset,
            ..
        } = build_radio();
        controls.fail_set_tx_number.set(2);
        CLOCK_US.with(|clock| clock.set(300));
        let packet = [0x5a; 255];
        let mut first_output = [0_u8; SX1262_FRAME_MTU];
        let mut second_output = [0_u8; SX1262_FRAME_MTU];
        let frames = reticulum_radio_interface::frame_rns_packet(
            &packet,
            3,
            &mut first_output,
            &mut second_output,
        )
        .unwrap();
        let fault = block_on(SoleRnodeRadio::transmit(&mut radio, frames)).unwrap_err();
        assert_eq!(fault.progress().completed_frame_count(), 1);
        assert_eq!(fault.progress().frame_completed_at_us(0), Some(301));
        assert_eq!(
            fault.fault().phase(),
            SoleRadioFaultPhase::FrameTransmission
        );
        assert_eq!(fault.fault().class(), SoleRadioFaultClass::Spi);
        assert!(!radio.is_active());
        assert!(!reset.is_high());
    }

    #[test]
    fn cad_and_bounded_receive_report_terminal_irq_observations() {
        let TestRig {
            mut radio,
            controls,
            reset,
            ..
        } = build_radio();
        CLOCK_US.with(|clock| clock.set(400));
        controls.cad_activity.set(false);
        let idle = block_on(SoleRnodeRadio::cad(&mut radio)).unwrap();
        assert!(!idle.activity_detected());
        assert_eq!(idle.observed_at_us(), 401);

        controls.cad_activity.set(true);
        let busy = block_on(SoleRnodeRadio::cad(&mut radio)).unwrap();
        assert!(busy.activity_detected());
        assert_eq!(busy.observed_at_us(), 402);

        let mut buffer = [0xee_u8; SX1262_FRAME_MTU];
        controls.rx_timeout.set(true);
        assert_eq!(
            block_on(SoleRnodeRadio::receive_bounded(&mut radio, &mut buffer)).unwrap(),
            BoundedRxOutcome::NoPreambleTimeout
        );
        assert_eq!(buffer, [0xee; SX1262_FRAME_MTU]);
        assert!(radio.is_active());

        controls.rx_timeout.set(false);
        controls.rx_success.set(true);
        let outcome = block_on(SoleRnodeRadio::receive_bounded(&mut radio, &mut buffer)).unwrap();
        let BoundedRxOutcome::Frame(frame) = outcome else {
            panic!("mock must return a frame")
        };
        assert_eq!(frame.len(), 3);
        assert_eq!(frame.payload(&buffer), Some(&[0xa1, 0xb2, 0xc3][..]));
        assert_eq!(frame.signal().rssi_dbm, -50);
        assert_eq!(frame.signal().snr_db, 2);
        assert_eq!(frame.received_at_us(), 405);
        assert!(radio.is_active());
        assert!(reset.is_high());
    }

    #[test]
    fn cancellation_and_spi_fault_assert_reset_and_quarantine_owner() {
        let TestRig {
            mut radio, reset, ..
        } = build_radio();
        let mut buffer = [0_u8; SX1262_FRAME_MTU];
        let receive = SoleRnodeRadio::receive_bounded(&mut radio, &mut buffer);
        drop(receive);
        assert!(!radio.is_active());
        assert!(!reset.is_high());

        let TestRig {
            mut radio,
            controls,
            reset,
            ..
        } = build_radio();
        controls.fail_opcode.set(Some(0xc5));
        let fault = block_on(SoleRnodeRadio::cad(&mut radio)).unwrap_err();
        assert_eq!(fault.phase(), SoleRadioFaultPhase::ChannelActivityDetection);
        assert_eq!(fault.class(), SoleRadioFaultClass::Spi);
        assert!(!radio.is_active());
        assert!(!reset.is_high());
    }

    #[test]
    fn reset_failure_during_initialization_remains_reset_low() {
        let (spi, _) = MockSpi::new();
        let (reset, reset_probe, _, fail_high) = MockOutput::new(PinState::High);
        fail_high.set(true);
        let timestamps = Box::leak(Box::new(IrqTimestampCapture::new_monotonic_us(stepped_us)));
        let result = block_on(E290Radio::new(
            spi,
            reset,
            MockWait,
            MockWait,
            NoopDelay,
            timestamps,
            E290_NA915_DEV_CONFIGURATION,
        ));
        let Err(error) = result else {
            panic!("reset-high failure must reject construction")
        };
        assert_eq!(error.operation, E290RadioOperation::ChipInitialization);
        assert!(matches!(error.radio, Some(RadioError::Reset)));
        assert!(!reset_probe.is_high());
    }
}
