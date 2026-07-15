//! Opaque receive-only SX1262 owner for the Tracker V2.

use core::cell::RefCell;

use critical_section::Mutex;
use embedded_hal::{
    digital::{OutputPin, StatefulOutputPin},
    spi::{Error as SpiError, ErrorKind as SpiErrorKind, ErrorType as SpiErrorType},
};
use embedded_hal_async::{
    delay::DelayNs,
    digital::Wait,
    spi::{Operation, SpiDevice},
};
use lora_phy::{
    LoRa, RxMode,
    mod_params::{PacketParams, RadioError},
    mod_traits::InterfaceVariant,
    sx126x::{Config, Sx126x, Sx1262, TcxoCtrlVoltage},
};
use reticulum_radio_interface::{
    FrameSignal, LabRxProfile, RadioRxFaultClass, RadioRxFaultClassification, SX1262_FRAME_MTU,
};

use crate::{RxOnlyFemFault, TrackerRxInterlock};

const SX1262_SET_TX: u8 = 0x83;
const SX1262_WRITE_BUFFER: u8 = 0x0e;
const SX1262_SET_TX_CONTINUOUS_WAVE: u8 = 0xd1;
const SX1262_SET_TX_CONTINUOUS_PREAMBLE: u8 = 0xd2;

/// SX1262 regulator selection for one explicitly identified RX artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrackerRxRegulator {
    Ldo,
    DcDc,
}

/// SX1262 receive-gain selection for one explicitly identified RX artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrackerRxGain {
    Unboosted,
    Boosted,
}

/// Typed board-radio configuration with no transmit-power surface.
///
/// Private fields prevent callers from bypassing the two reviewed choices.
/// The electrical HIL image constructs all four combinations explicitly; the
/// ordinary Phase-1 image continues to use [`TRACKER_RX_CONFIGURATION`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrackerRxConfiguration {
    regulator: TrackerRxRegulator,
    gain: TrackerRxGain,
}

impl TrackerRxConfiguration {
    /// Construct one explicit regulator and receive-gain combination.
    pub const fn new(regulator: TrackerRxRegulator, gain: TrackerRxGain) -> Self {
        Self { regulator, gain }
    }

    /// Selected regulator mode.
    pub const fn regulator(self) -> TrackerRxRegulator {
        self.regulator
    }

    /// Selected receive-gain mode.
    pub const fn gain(self) -> TrackerRxGain {
        self.gain
    }

    /// Whether this configuration requests the SX1262 DC-DC regulator.
    pub const fn uses_dcdc(self) -> bool {
        matches!(self.regulator, TrackerRxRegulator::DcDc)
    }

    /// Whether this configuration requests boosted SX1262 receive gain.
    pub const fn boosted(self) -> bool {
        matches!(self.gain, TrackerRxGain::Boosted)
    }
}

/// Pinned ordinary Phase-1 policy; electrical HIL variants do not change it.
pub const TRACKER_RX_CONFIGURATION: TrackerRxConfiguration =
    TrackerRxConfiguration::new(TrackerRxRegulator::Ldo, TrackerRxGain::Unboosted);

/// `lora-phy`'s SX126x private-network sync word selected by this owner.
pub const TRACKER_RX_SYNC_WORD: u16 = 0x1424;

/// Metadata for one frame copied into the caller's 255-byte buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReceivedFrame {
    /// Number of valid bytes in the caller's frame buffer.
    pub len: u8,
    /// PHY signal measurements reported by the SX1262 driver.
    pub signal: FrameSignal,
    /// Monotonic tick sampled when the DIO1 wait resumed for this receive.
    pub received_at_ticks: u64,
}

/// Shared capture slot used at the DIO1 boundary of the opaque radio owner.
///
/// The clock callback must use the same monotonic tick epoch as Reticulum
/// ingress. A successful receive returns the timestamp sampled when the final
/// DIO1 async wait resumed and `lora-phy` subsequently identified `RxDone`.
/// This includes GPIO wake and scheduler latency; it is not a hardware-edge
/// timestamp. It still precedes IRQ-status, payload and packet-status SPI reads.
pub struct RxEventTimestampCapture {
    now_ticks: fn() -> u64,
    latest: Mutex<RefCell<Option<u64>>>,
}

impl RxEventTimestampCapture {
    /// Construct an empty timestamp slot around one monotonic clock callback.
    pub const fn new(now_ticks: fn() -> u64) -> Self {
        Self {
            now_ticks,
            latest: Mutex::new(RefCell::new(None)),
        }
    }

    fn begin_receive(&self) {
        critical_section::with(|cs| self.latest.borrow(cs).replace(None));
    }

    fn record_irq_observation(&self) {
        let ticks = (self.now_ticks)();
        critical_section::with(|cs| self.latest.borrow(cs).replace(Some(ticks)));
    }

    fn take_completed_receive(&self) -> Option<u64> {
        critical_section::with(|cs| self.latest.borrow(cs).take())
    }
}

/// Receive-only operation in which a Tracker radio failure was observed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrackerRxRadioOperation {
    OwnerState,
    InterfaceConstruction,
    ChipInitialization,
    ModulationConfiguration,
    PacketConfiguration,
    FemEnable,
    InterlockReassertion,
    PrepareReceive,
    Receive,
    Shutdown,
}

/// Exhaustive, allocation-free copy of a pinned `lora-phy` radio error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrackerRadioFaultCode {
    Spi,
    Reset,
    RfSwitchRx,
    RfSwitchTx,
    Busy,
    Irq,
    Dio1,
    InvalidConfiguration,
    InvalidRadioMode,
    Operation(u8),
    InvalidBaseAddress { transmit: usize, receive: usize },
    PayloadSizeUnexpected(usize),
    PayloadSizeMismatch { expected: usize, actual: usize },
    UnavailableSpreadingFactor,
    UnavailableBandwidth,
    InvalidBandwidthForFrequency,
    InvalidSf6ExplicitHeaderRequest,
    InvalidOutputPowerForFrequency,
    TransmitTimeout,
    ReceiveTimeout,
    DutyCycleUnsupported,
    RngUnsupported,
}

impl TrackerRadioFaultCode {
    const fn from_radio_error(error: &RadioError) -> Self {
        match error {
            RadioError::SPI => Self::Spi,
            RadioError::Reset => Self::Reset,
            RadioError::RfSwitchRx => Self::RfSwitchRx,
            RadioError::RfSwitchTx => Self::RfSwitchTx,
            RadioError::Busy => Self::Busy,
            RadioError::Irq => Self::Irq,
            RadioError::DIO1 => Self::Dio1,
            RadioError::InvalidConfiguration => Self::InvalidConfiguration,
            RadioError::InvalidRadioMode => Self::InvalidRadioMode,
            RadioError::OpError(status) => Self::Operation(*status),
            RadioError::InvalidBaseAddress(transmit, receive) => Self::InvalidBaseAddress {
                transmit: *transmit,
                receive: *receive,
            },
            RadioError::PayloadSizeUnexpected(actual) => Self::PayloadSizeUnexpected(*actual),
            RadioError::PayloadSizeMismatch(expected, actual) => Self::PayloadSizeMismatch {
                expected: *expected,
                actual: *actual,
            },
            RadioError::UnavailableSpreadingFactor => Self::UnavailableSpreadingFactor,
            RadioError::UnavailableBandwidth => Self::UnavailableBandwidth,
            RadioError::InvalidBandwidthForFrequency => Self::InvalidBandwidthForFrequency,
            RadioError::InvalidSF6ExplicitHeaderRequest => Self::InvalidSf6ExplicitHeaderRequest,
            RadioError::InvalidOutputPowerForFrequency => Self::InvalidOutputPowerForFrequency,
            RadioError::TransmitTimeout => Self::TransmitTimeout,
            RadioError::ReceiveTimeout => Self::ReceiveTimeout,
            RadioError::DutyCycleUnsupported => Self::DutyCycleUnsupported,
            RadioError::RngUnsupported => Self::RngUnsupported,
        }
    }

    /// Stable diagnostic class for this exact driver failure.
    pub const fn class(self) -> RadioRxFaultClass {
        match self {
            Self::Spi => RadioRxFaultClass::Spi,
            Self::Reset => RadioRxFaultClass::Reset,
            Self::RfSwitchRx | Self::RfSwitchTx => RadioRxFaultClass::RfSwitch,
            Self::Busy => RadioRxFaultClass::Busy,
            Self::Irq | Self::Dio1 => RadioRxFaultClass::Interrupt,
            Self::InvalidConfiguration
            | Self::InvalidRadioMode
            | Self::InvalidBaseAddress { .. }
            | Self::UnavailableSpreadingFactor
            | Self::UnavailableBandwidth
            | Self::InvalidBandwidthForFrequency
            | Self::InvalidSf6ExplicitHeaderRequest
            | Self::InvalidOutputPowerForFrequency => RadioRxFaultClass::Configuration,
            Self::Operation(_) => RadioRxFaultClass::Operation,
            Self::PayloadSizeUnexpected(_) | Self::PayloadSizeMismatch { .. } => {
                RadioRxFaultClass::Payload
            }
            Self::TransmitTimeout | Self::ReceiveTimeout => RadioRxFaultClass::Timeout,
            Self::DutyCycleUnsupported | Self::RngUnsupported => RadioRxFaultClass::Unsupported,
        }
    }
}

/// Exact primary or cleanup fault at the opaque Tracker boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrackerRxRadioFault {
    Radio(TrackerRadioFaultCode),
    Fem(RxOnlyFemFault),
    CaptureTimestampUnavailable,
    Faulted,
}

impl TrackerRxRadioFault {
    /// Stable diagnostic class for this board-specific fault.
    pub const fn class(self) -> RadioRxFaultClass {
        match self {
            Self::Radio(code) => code.class(),
            Self::Fem(_) => RadioRxFaultClass::Fem,
            Self::CaptureTimestampUnavailable => RadioRxFaultClass::Interrupt,
            Self::Faulted => RadioRxFaultClass::Faulted,
        }
    }
}

/// Failure from the opaque Tracker receive-only radio boundary.
///
/// When explicit wrapper teardown returns a cleanup fault, it never replaces
/// its initiating cause. Both are retained in fixed-size form. Best-effort
/// cleanup from `Drop` or a partially failed FEM enable has no second error
/// return channel and remains a hardware-qualification limitation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrackerRxRadioError {
    pub operation: TrackerRxRadioOperation,
    pub primary: TrackerRxRadioFault,
    pub cleanup: Option<TrackerRxRadioFault>,
}

impl TrackerRxRadioError {
    fn radio(operation: TrackerRxRadioOperation, error: RadioError) -> Self {
        Self {
            operation,
            primary: TrackerRxRadioFault::Radio(TrackerRadioFaultCode::from_radio_error(&error)),
            cleanup: None,
        }
    }

    const fn fem(operation: TrackerRxRadioOperation, fault: RxOnlyFemFault) -> Self {
        Self {
            operation,
            primary: TrackerRxRadioFault::Fem(fault),
            cleanup: None,
        }
    }

    fn radio_with_fem_cleanup(
        operation: TrackerRxRadioOperation,
        error: RadioError,
        cleanup: RxOnlyFemFault,
    ) -> Self {
        Self {
            operation,
            primary: TrackerRxRadioFault::Radio(TrackerRadioFaultCode::from_radio_error(&error)),
            cleanup: Some(TrackerRxRadioFault::Fem(cleanup)),
        }
    }

    const fn faulted(operation: TrackerRxRadioOperation) -> Self {
        Self {
            operation,
            primary: TrackerRxRadioFault::Faulted,
            cleanup: None,
        }
    }

    const fn missing_capture_timestamp(cleanup: Option<RxOnlyFemFault>) -> Self {
        Self {
            operation: TrackerRxRadioOperation::Receive,
            primary: TrackerRxRadioFault::CaptureTimestampUnavailable,
            cleanup: match cleanup {
                Some(fault) => Some(TrackerRxRadioFault::Fem(fault)),
                None => None,
            },
        }
    }

    /// Stable primary-plus-cleanup classification for bounded counters.
    pub const fn classification(self) -> RadioRxFaultClassification {
        match self.cleanup {
            Some(cleanup) => {
                RadioRxFaultClassification::with_cleanup(self.primary.class(), cleanup.class())
            }
            None => RadioRxFaultClassification::single(self.primary.class()),
        }
    }
}

type TrackerLoRa<Spi, Reset, Dio1, Busy, RadioDelay> = LoRa<
    Sx126x<RxOnlySpiDevice<Spi>, TrackerRxInterfaceVariant<Reset, Dio1, Busy>, Sx1262>,
    RadioDelay,
>;

/// The only active state. Field order is safety-significant: the FEM owner is
/// dropped (CTX, CSD, power low) before the LoRa/interface owner drops RESET low.
struct Active<Spi, Reset, Dio1, Busy, RadioDelay, Power, Csd, Ctx>
where
    Spi: SpiDevice<u8>,
    Reset: OutputPin,
    Dio1: Wait,
    Busy: Wait,
    RadioDelay: DelayNs,
    Power: OutputPin,
    Csd: OutputPin,
    Ctx: StatefulOutputPin,
{
    interlock: TrackerRxInterlock<Power, Csd, Ctx>,
    lora: TrackerLoRa<Spi, Reset, Dio1, Busy, RadioDelay>,
    packet_params: PacketParams,
}

type OptionalActive<Spi, Reset, Dio1, Busy, RadioDelay, Power, Csd, Ctx> =
    Option<Active<Spi, Reset, Dio1, Busy, RadioDelay, Power, Csd, Ctx>>;

/// Opaque RX-only `lora-phy` wrapper for the exact Tracker V2 radio path.
///
/// The public surface contains no TX, CAD, standby, inner-radio, SPI or pin
/// escape. A receive error or cancellation consumes the internal active state,
/// attempts to shut down the FEM and asserts SX1262 reset on a best-effort
/// basis. Explicit error paths report a teardown pin failure.
pub struct TrackerRxRadio<Spi, Reset, Dio1, Busy, RadioDelay, Power, Csd, Ctx>
where
    Spi: SpiDevice<u8>,
    Reset: OutputPin,
    Dio1: Wait,
    Busy: Wait,
    RadioDelay: DelayNs,
    Power: OutputPin,
    Csd: OutputPin,
    Ctx: StatefulOutputPin,
{
    active: OptionalActive<Spi, Reset, Dio1, Busy, RadioDelay, Power, Csd, Ctx>,
    rx_timestamps: &'static RxEventTimestampCapture,
}

impl<Spi, Reset, Dio1, Busy, RadioDelay, Power, Csd, Ctx>
    TrackerRxRadio<Spi, Reset, Dio1, Busy, RadioDelay, Power, Csd, Ctx>
where
    Spi: SpiDevice<u8>,
    Reset: OutputPin,
    Dio1: Wait,
    Busy: Wait,
    RadioDelay: DelayNs,
    Power: OutputPin,
    Csd: OutputPin,
    Ctx: StatefulOutputPin,
{
    /// Initialize the SX1262 with the FEM off, then enable only the receive path.
    ///
    /// The validated profile contains no power setting. This constructor fixes
    /// the private network sync word, 1.8 V DIO3 TCXO and DIO2 RF switch. The
    /// typed configuration selects only regulator and receive gain; it has no
    /// transmit-power or transmit-mode capability.
    #[allow(
        clippy::too_many_arguments,
        reason = "each safety-critical peripheral is consumed exactly once at the board boundary"
    )]
    pub async fn new<FemDelay>(
        spi: Spi,
        reset: Reset,
        dio1: Dio1,
        busy: Busy,
        radio_delay: RadioDelay,
        fem_delay: &mut FemDelay,
        interlock: TrackerRxInterlock<Power, Csd, Ctx>,
        rx_timestamps: &'static RxEventTimestampCapture,
        profile: LabRxProfile,
        configuration: TrackerRxConfiguration,
    ) -> Result<Self, TrackerRxRadioError>
    where
        FemDelay: DelayNs,
    {
        if interlock.is_enabled() {
            return Err(TrackerRxRadioError::faulted(
                TrackerRxRadioOperation::OwnerState,
            ));
        }

        let interface =
            TrackerRxInterfaceVariant::new(reset, dio1, busy, rx_timestamps).map_err(|error| {
                TrackerRxRadioError::radio(TrackerRxRadioOperation::InterfaceConstruction, error)
            })?;
        let sx1262 = Sx126x::new(
            RxOnlySpiDevice::new(spi),
            interface,
            Config {
                chip: Sx1262,
                tcxo_ctrl: Some(TcxoCtrlVoltage::Ctrl1V8),
                use_dcdc: configuration.uses_dcdc(),
                rx_boost: configuration.boosted(),
            },
        );
        let mut lora = LoRa::new(sx1262, false, radio_delay)
            .await
            .map_err(|error| {
                TrackerRxRadioError::radio(TrackerRxRadioOperation::ChipInitialization, error)
            })?;

        let modulation = lora
            .create_modulation_params(
                profile.spreading_factor(),
                profile.bandwidth(),
                profile.coding_rate(),
                profile.frequency_hz(),
            )
            .map_err(|error| {
                TrackerRxRadioError::radio(TrackerRxRadioOperation::ModulationConfiguration, error)
            })?;
        let packet_params = lora
            .create_rx_packet_params(
                profile.preamble_symbols(),
                !profile.explicit_header(),
                SX1262_FRAME_MTU as u8,
                profile.crc(),
                profile.iq_inverted(),
                &modulation,
            )
            .map_err(|error| {
                TrackerRxRadioError::radio(TrackerRxRadioOperation::PacketConfiguration, error)
            })?;

        let mut interlock = match interlock.enable(fem_delay).await {
            Ok(interlock) => interlock,
            Err(failure) => {
                let (disabled, cause) = failure.into_parts();
                let fault = cause.kind();
                drop(disabled);
                return Err(TrackerRxRadioError::fem(
                    TrackerRxRadioOperation::FemEnable,
                    fault,
                ));
            }
        };
        if let Err(error) = interlock.reassert_receive_interlock() {
            let fault = error.kind();
            drop(interlock);
            drop(lora);
            return Err(TrackerRxRadioError::fem(
                TrackerRxRadioOperation::InterlockReassertion,
                fault,
            ));
        }
        if let Err(error) = lora
            .prepare_for_rx(RxMode::Continuous, &modulation, &packet_params)
            .await
        {
            let cleanup_fault = interlock.shutdown().err().map(|fault| fault.kind());
            drop(interlock);
            drop(lora);
            return Err(match cleanup_fault {
                Some(fault) => TrackerRxRadioError::radio_with_fem_cleanup(
                    TrackerRxRadioOperation::PrepareReceive,
                    error,
                    fault,
                ),
                None => TrackerRxRadioError::radio(TrackerRxRadioOperation::PrepareReceive, error),
            });
        }

        Ok(Self {
            active: Some(Active {
                interlock,
                lora,
                packet_params,
            }),
            rx_timestamps,
        })
    }

    /// Receive one complete physical frame without exposing a cancellable IRQ sub-step.
    ///
    /// The owning radio task must await this future to completion. If another
    /// caller nevertheless cancels it, the local active state is dropped and
    /// the wrapper remains permanently faulted after best-effort FEM shutdown
    /// and reset assertion.
    pub async fn receive(
        &mut self,
        buffer: &mut [u8; SX1262_FRAME_MTU],
    ) -> Result<ReceivedFrame, TrackerRxRadioError> {
        let mut active = self
            .active
            .take()
            .ok_or_else(|| TrackerRxRadioError::faulted(TrackerRxRadioOperation::Receive))?;
        self.rx_timestamps.begin_receive();
        let result = active.lora.rx(&active.packet_params, buffer).await;
        match result {
            Ok((len, status)) => {
                let Some(received_at_ticks) = self.rx_timestamps.take_completed_receive() else {
                    let cleanup_fault = active.interlock.shutdown().err().map(|fault| fault.kind());
                    drop(active);
                    return Err(TrackerRxRadioError::missing_capture_timestamp(
                        cleanup_fault,
                    ));
                };
                let frame = ReceivedFrame {
                    len,
                    signal: FrameSignal {
                        rssi_dbm: status.rssi,
                        snr_db: status.snr,
                    },
                    received_at_ticks,
                };
                self.active = Some(active);
                Ok(frame)
            }
            Err(error) => {
                let cleanup_fault = active.interlock.shutdown().err().map(|fault| fault.kind());
                drop(active);
                Err(match cleanup_fault {
                    Some(fault) => TrackerRxRadioError::radio_with_fem_cleanup(
                        TrackerRxRadioOperation::Receive,
                        error,
                        fault,
                    ),
                    None => TrackerRxRadioError::radio(TrackerRxRadioOperation::Receive, error),
                })
            }
        }
    }

    /// Explicitly tear down the active RX path. Calling it twice is harmless.
    pub fn shutdown(&mut self) -> Result<(), TrackerRxRadioError> {
        let Some(mut active) = self.active.take() else {
            return Ok(());
        };
        let cleanup_fault = active.interlock.shutdown().err().map(|fault| fault.kind());
        drop(active);
        match cleanup_fault {
            Some(fault) => Err(TrackerRxRadioError::fem(
                TrackerRxRadioOperation::Shutdown,
                fault,
            )),
            None => Ok(()),
        }
    }

    /// Whether the wrapper still owns a configured receive path.
    pub const fn is_active(&self) -> bool {
        self.active.is_some()
    }
}

/// Private interface variant whose TX hook always fails before an SX1262 command.
struct TrackerRxInterfaceVariant<Reset, Dio1, Busy>
where
    Reset: OutputPin,
{
    reset: Reset,
    dio1: Dio1,
    busy: Busy,
    rx_timestamps: &'static RxEventTimestampCapture,
}

impl<Reset, Dio1, Busy> TrackerRxInterfaceVariant<Reset, Dio1, Busy>
where
    Reset: OutputPin,
{
    fn new(
        mut reset: Reset,
        dio1: Dio1,
        busy: Busy,
        rx_timestamps: &'static RxEventTimestampCapture,
    ) -> Result<Self, RadioError> {
        reset.set_low().map_err(|_| RadioError::Reset)?;
        Ok(Self {
            reset,
            dio1,
            busy,
            rx_timestamps,
        })
    }
}

impl<Reset, Dio1, Busy> InterfaceVariant for TrackerRxInterfaceVariant<Reset, Dio1, Busy>
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
        self.rx_timestamps.record_irq_observation();
        Ok(())
    }

    async fn enable_rf_switch_rx(&mut self) -> Result<(), RadioError> {
        Ok(())
    }

    async fn enable_rf_switch_tx(&mut self) -> Result<(), RadioError> {
        Err(RadioError::InvalidConfiguration)
    }

    async fn disable_rf_switch(&mut self) -> Result<(), RadioError> {
        Ok(())
    }
}

impl<Reset, Dio1, Busy> Drop for TrackerRxInterfaceVariant<Reset, Dio1, Busy>
where
    Reset: OutputPin,
{
    fn drop(&mut self) {
        if self.reset.set_low().is_err() {
            // Best effort is the only operation available during Drop. Every
            // normal error path performs explicit coordinated teardown first.
        }
    }
}

#[derive(Debug)]
enum RxOnlySpiError<InnerError> {
    Inner(InnerError),
    ForbiddenOpcode(u8),
}

impl<InnerError> SpiError for RxOnlySpiError<InnerError>
where
    InnerError: SpiError,
{
    fn kind(&self) -> SpiErrorKind {
        match self {
            Self::Inner(error) => error.kind(),
            Self::ForbiddenOpcode(opcode) => {
                let _ = opcode;
                SpiErrorKind::Other
            }
        }
    }
}

struct RxOnlySpiDevice<Spi> {
    inner: Spi,
}

impl<Spi> RxOnlySpiDevice<Spi> {
    const fn new(inner: Spi) -> Self {
        Self { inner }
    }
}

impl<Spi> SpiErrorType for RxOnlySpiDevice<Spi>
where
    Spi: SpiErrorType,
    Spi::Error: SpiError,
{
    type Error = RxOnlySpiError<Spi::Error>;
}

impl<Spi> SpiDevice<u8> for RxOnlySpiDevice<Spi>
where
    Spi: SpiDevice<u8>,
{
    async fn transaction(
        &mut self,
        operations: &mut [Operation<'_, u8>],
    ) -> Result<(), Self::Error> {
        if let Some(opcode) = first_command_opcode(operations)
            && is_forbidden_opcode(opcode)
        {
            return Err(RxOnlySpiError::ForbiddenOpcode(opcode));
        }
        self.inner
            .transaction(operations)
            .await
            .map_err(RxOnlySpiError::Inner)
    }
}

fn first_command_opcode(operations: &[Operation<'_, u8>]) -> Option<u8> {
    for operation in operations {
        match operation {
            Operation::Write(bytes) if !bytes.is_empty() => return Some(bytes[0]),
            Operation::Transfer(_, bytes) if !bytes.is_empty() => return Some(bytes[0]),
            Operation::TransferInPlace(bytes) if !bytes.is_empty() => return Some(bytes[0]),
            Operation::Read(_)
            | Operation::DelayNs(_)
            | Operation::Write(_)
            | Operation::Transfer(_, _)
            | Operation::TransferInPlace(_) => {}
        }
    }
    None
}

const fn is_forbidden_opcode(opcode: u8) -> bool {
    matches!(
        opcode,
        SX1262_SET_TX
            | SX1262_WRITE_BUFFER
            | SX1262_SET_TX_CONTINUOUS_WAVE
            | SX1262_SET_TX_CONTINUOUS_PREAMBLE
    )
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::{
        cell::Cell,
        future::{Future, pending},
        pin::pin,
        sync::atomic::{AtomicU64, Ordering},
        task::{Context, Poll, Waker},
    };
    use std::{boxed::Box, cell::RefCell, rc::Rc, vec::Vec};

    use embedded_hal::{
        digital::{ErrorType as DigitalErrorType, PinState},
        spi::{ErrorKind, ErrorType},
    };
    use lora_phy::mod_traits::RadioKind;
    use reticulum_lab_rx_returned_fault_hil::{
        ReturnedFaultEvidence, ReturnedFaultSpiDevice, ReturnedFaultState,
    };
    use reticulum_radio_interface::LabRxProfileConfig;

    use super::*;
    use crate::validate_lab_rx_profile;

    const SET_RX: u8 = 0x82;
    const SET_TX_PARAMS: u8 = 0x8e;
    const SET_REGULATOR_MODE: u8 = 0x96;
    const SET_DIO2_AS_RF_SWITCH: u8 = 0x9d;
    const SET_TCXO_MODE: u8 = 0x97;
    const WRITE_REGISTER: u8 = 0x0d;
    const GET_IRQ_STATUS: u8 = 0x12;
    const GET_RX_BUFFER_STATUS: u8 = 0x13;
    const GET_PACKET_STATUS: u8 = 0x14;
    const READ_BUFFER: u8 = 0x1e;
    static MUTABLE_TEST_TICKS: AtomicU64 = AtomicU64::new(0);

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct MockError;

    impl embedded_hal::digital::Error for MockError {
        fn kind(&self) -> embedded_hal::digital::ErrorKind {
            embedded_hal::digital::ErrorKind::Other
        }
    }

    impl SpiError for MockError {
        fn kind(&self) -> ErrorKind {
            ErrorKind::Other
        }
    }

    #[derive(Clone)]
    struct OutputProbe {
        high: Rc<Cell<bool>>,
        ever_high: Rc<Cell<bool>>,
        fail_low: Rc<Cell<bool>>,
    }

    impl OutputProbe {
        fn is_low(&self) -> bool {
            !self.high.get()
        }

        fn was_ever_high(&self) -> bool {
            self.ever_high.get()
        }

        fn set_fail_low(&self, fail: bool) {
            self.fail_low.set(fail);
        }
    }

    struct MockOutput {
        high: Rc<Cell<bool>>,
        ever_high: Rc<Cell<bool>>,
        fail_low: Rc<Cell<bool>>,
    }

    impl MockOutput {
        fn new(initial: PinState) -> (Self, OutputProbe) {
            let high = Rc::new(Cell::new(initial == PinState::High));
            let ever_high = Rc::new(Cell::new(initial == PinState::High));
            let fail_low = Rc::new(Cell::new(false));
            (
                Self {
                    high: high.clone(),
                    ever_high: ever_high.clone(),
                    fail_low: fail_low.clone(),
                },
                OutputProbe {
                    high,
                    ever_high,
                    fail_low,
                },
            )
        }
    }

    impl DigitalErrorType for MockOutput {
        type Error = MockError;
    }

    impl OutputPin for MockOutput {
        fn set_low(&mut self) -> Result<(), Self::Error> {
            if self.fail_low.get() {
                return Err(MockError);
            }
            self.high.set(false);
            Ok(())
        }

        fn set_high(&mut self) -> Result<(), Self::Error> {
            self.high.set(true);
            self.ever_high.set(true);
            Ok(())
        }
    }

    impl StatefulOutputPin for MockOutput {
        fn is_set_high(&mut self) -> Result<bool, Self::Error> {
            Ok(self.high.get())
        }

        fn is_set_low(&mut self) -> Result<bool, Self::Error> {
            Ok(!self.high.get())
        }
    }

    #[derive(Clone)]
    struct WaitControl {
        pending: Rc<Cell<bool>>,
        ticks_on_wait_for_high: Rc<RefCell<Vec<u64>>>,
    }

    struct MockWait {
        pending: WaitControl,
    }

    impl MockWait {
        fn ready() -> (Self, WaitControl) {
            let pending = Rc::new(Cell::new(false));
            let ticks_on_wait_for_high = Rc::new(RefCell::new(Vec::new()));
            let control = WaitControl {
                pending: pending.clone(),
                ticks_on_wait_for_high: ticks_on_wait_for_high.clone(),
            };
            (
                Self {
                    pending: control.clone(),
                },
                control,
            )
        }

        async fn wait(&self) {
            if self.pending.pending.get() {
                pending::<()>().await;
            }
        }
    }

    impl DigitalErrorType for MockWait {
        type Error = MockError;
    }

    impl Wait for MockWait {
        async fn wait_for_high(&mut self) -> Result<(), Self::Error> {
            self.wait().await;
            if !self.pending.ticks_on_wait_for_high.borrow().is_empty() {
                let ticks = self.pending.ticks_on_wait_for_high.borrow_mut().remove(0);
                MUTABLE_TEST_TICKS.store(ticks, Ordering::SeqCst);
            }
            Ok(())
        }

        async fn wait_for_low(&mut self) -> Result<(), Self::Error> {
            self.wait().await;
            Ok(())
        }

        async fn wait_for_rising_edge(&mut self) -> Result<(), Self::Error> {
            self.wait().await;
            Ok(())
        }

        async fn wait_for_falling_edge(&mut self) -> Result<(), Self::Error> {
            self.wait().await;
            Ok(())
        }

        async fn wait_for_any_edge(&mut self) -> Result<(), Self::Error> {
            self.wait().await;
            Ok(())
        }
    }

    #[derive(Clone)]
    struct SpiControl {
        commands: Rc<RefCell<Vec<Vec<u8>>>>,
        fail_opcode: Rc<Cell<Option<u8>>>,
        ticks_after_get_irq_status: Rc<Cell<Option<u64>>>,
        irq_statuses: Rc<RefCell<Vec<[u8; 2]>>>,
    }

    struct MockSpi {
        control: SpiControl,
    }

    impl MockSpi {
        fn new() -> (Self, SpiControl) {
            let control = SpiControl {
                commands: Rc::new(RefCell::new(Vec::new())),
                fail_opcode: Rc::new(Cell::new(None)),
                ticks_after_get_irq_status: Rc::new(Cell::new(None)),
                irq_statuses: Rc::new(RefCell::new(Vec::new())),
            };
            (
                Self {
                    control: control.clone(),
                },
                control,
            )
        }
    }

    impl ErrorType for MockSpi {
        type Error = MockError;
    }

    impl SpiDevice<u8> for MockSpi {
        async fn transaction(
            &mut self,
            operations: &mut [Operation<'_, u8>],
        ) -> Result<(), Self::Error> {
            let opcode = first_command_opcode(operations);
            let mut command = Vec::new();
            for operation in operations.iter() {
                match operation {
                    // The trace is normalized to bytes sent by the driver.
                    // Read buffers hold MISO responses, so their prior contents
                    // are neither command bytes nor deterministic wire input.
                    Operation::Write(bytes) => command.extend_from_slice(bytes),
                    Operation::Transfer(_, bytes) => command.extend_from_slice(bytes),
                    Operation::TransferInPlace(bytes) => command.extend_from_slice(bytes),
                    Operation::Read(_) | Operation::DelayNs(_) => {}
                }
            }
            self.control.commands.borrow_mut().push(command);

            if opcode == Some(GET_IRQ_STATUS)
                && let Some(ticks) = self.control.ticks_after_get_irq_status.get()
            {
                MUTABLE_TEST_TICKS.store(ticks, Ordering::SeqCst);
            }

            if opcode.is_some() && opcode == self.control.fail_opcode.get() {
                return Err(MockError);
            }

            let mut read_index = 0;
            for operation in operations.iter_mut() {
                let Operation::Read(bytes) = operation else {
                    continue;
                };
                if opcode == Some(GET_IRQ_STATUS)
                    && read_index == 1
                    && bytes.len() == 2
                    && !self.control.irq_statuses.borrow().is_empty()
                {
                    bytes.copy_from_slice(&self.control.irq_statuses.borrow_mut().remove(0));
                } else {
                    fill_read(opcode, read_index, bytes);
                }
                read_index += 1;
            }
            Ok(())
        }
    }

    fn fill_read(opcode: Option<u8>, read_index: usize, bytes: &mut [u8]) {
        bytes.fill(0);
        match (opcode, read_index) {
            (Some(GET_IRQ_STATUS), 1) if bytes.len() == 2 => bytes.copy_from_slice(&[0, 2]),
            (Some(GET_RX_BUFFER_STATUS), 1) if bytes.len() == 2 => {
                bytes.copy_from_slice(&[3, 0]);
            }
            (Some(READ_BUFFER), 0) if bytes.len() == 3 => {
                bytes.copy_from_slice(&[0xaa, 0xbb, 0xcc])
            }
            (Some(GET_PACKET_STATUS), 1) if bytes.len() == 3 => {
                bytes.copy_from_slice(&[200, 8, 198]);
            }
            _ => {}
        }
    }

    #[derive(Default)]
    struct ReadyDelay;

    impl DelayNs for ReadyDelay {
        async fn delay_ns(&mut self, _ns: u32) {}
    }

    fn run_ready<F: Future>(future: F) -> F::Output {
        let mut future = pin!(future);
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("test future unexpectedly pending"),
        }
    }

    fn profile() -> LabRxProfile {
        validate_lab_rx_profile(LabRxProfileConfig {
            frequency_hz: Some(915_000_000),
            spreading_factor: 7,
            bandwidth_hz: 125_000,
            coding_rate_denominator: 5,
            preamble_symbols: 18,
            explicit_header: true,
            crc: true,
            iq_inverted: false,
        })
        .unwrap()
    }

    fn test_now_ticks() -> u64 {
        1_234
    }

    fn mutable_test_now_ticks() -> u64 {
        MUTABLE_TEST_TICKS.load(Ordering::SeqCst)
    }

    fn test_timestamp_capture() -> &'static RxEventTimestampCapture {
        Box::leak(Box::new(RxEventTimestampCapture::new(test_now_ticks)))
    }

    type TestRadio = TrackerRxRadio<
        MockSpi,
        MockOutput,
        MockWait,
        MockWait,
        ReadyDelay,
        MockOutput,
        MockOutput,
        MockOutput,
    >;

    struct TestRig {
        radio: TestRadio,
        spi: SpiControl,
        irq: WaitControl,
        reset: OutputProbe,
        power: OutputProbe,
        csd: OutputProbe,
        ctx: OutputProbe,
    }

    fn build_radio() -> TestRig {
        build_radio_with_configuration(TRACKER_RX_CONFIGURATION)
    }

    fn build_radio_with_configuration(configuration: TrackerRxConfiguration) -> TestRig {
        build_radio_with_timestamp_capture(configuration, test_timestamp_capture())
    }

    fn build_radio_with_timestamp_capture(
        configuration: TrackerRxConfiguration,
        timestamps: &'static RxEventTimestampCapture,
    ) -> TestRig {
        let (spi, spi_control) = MockSpi::new();
        let (reset, reset_probe) = MockOutput::new(PinState::Low);
        let (dio1, irq_control) = MockWait::ready();
        let (busy, _) = MockWait::ready();
        let (power, power_probe) = MockOutput::new(PinState::Low);
        let (csd, csd_probe) = MockOutput::new(PinState::Low);
        let (ctx, ctx_probe) = MockOutput::new(PinState::Low);
        let interlock = TrackerRxInterlock::new(power, csd, ctx).unwrap();
        let mut fem_delay = ReadyDelay;
        let radio = run_ready(TrackerRxRadio::new(
            spi,
            reset,
            dio1,
            busy,
            ReadyDelay,
            &mut fem_delay,
            interlock,
            timestamps,
            profile(),
            configuration,
        ))
        .unwrap();
        TestRig {
            radio,
            spi: spi_control,
            irq: irq_control,
            reset: reset_probe,
            power: power_probe,
            csd: csd_probe,
            ctx: ctx_probe,
        }
    }

    fn has_command(commands: &[Vec<u8>], prefix: &[u8]) -> bool {
        commands.iter().any(|command| command.starts_with(prefix))
    }

    fn has_forbidden_command(commands: &[Vec<u8>]) -> bool {
        commands
            .iter()
            .filter_map(|command| command.first().copied())
            .any(is_forbidden_opcode)
    }

    fn assert_command_trace(actual: &[Vec<u8>], expected: &[&[u8]]) {
        assert_eq!(
            actual.len(),
            expected.len(),
            "SPI transaction count changed: {actual:02x?}"
        );
        for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
            assert_eq!(
                actual.as_slice(),
                *expected,
                "normalized SPI command changed at transaction {index}"
            );
        }
    }

    fn receive_trace(configuration: TrackerRxConfiguration) -> Vec<Vec<u8>> {
        let mut rig = build_radio_with_configuration(configuration);
        let mut buffer = [0; SX1262_FRAME_MTU];
        run_ready(rig.radio.receive(&mut buffer)).unwrap();
        rig.spi.commands.borrow().clone()
    }

    fn canonical_electrical_trace(commands: &[Vec<u8>]) -> Vec<Vec<u8>> {
        commands
            .iter()
            .filter(|command| command.as_slice() != [SET_REGULATOR_MODE, 0x01])
            .cloned()
            .map(|mut command| {
                if command.len() == 4
                    && command[..3] == [WRITE_REGISTER, 0x08, 0xac]
                    && matches!(command[3], 0x94 | 0x96)
                {
                    command[3] = 0x94;
                }
                command
            })
            .collect()
    }

    #[test]
    fn spi_firewall_rejects_every_transmit_primitive() {
        for opcode in [
            SX1262_SET_TX,
            SX1262_WRITE_BUFFER,
            SX1262_SET_TX_CONTINUOUS_WAVE,
            SX1262_SET_TX_CONTINUOUS_PREAMBLE,
        ] {
            let (spi, control) = MockSpi::new();
            let mut firewall = RxOnlySpiDevice::new(spi);
            let error = run_ready(firewall.write(&[opcode])).unwrap_err();
            assert!(matches!(error, RxOnlySpiError::ForbiddenOpcode(value) if value == opcode));
            assert!(control.commands.borrow().is_empty());
        }
    }

    #[test]
    fn spi_firewall_remains_outside_returned_fault_decorator() {
        for opcode in [
            SX1262_SET_TX,
            SX1262_WRITE_BUFFER,
            SX1262_SET_TX_CONTINUOUS_WAVE,
            SX1262_SET_TX_CONTINUOUS_PREAMBLE,
        ] {
            let evidence = ReturnedFaultEvidence::new();
            let (spi, control) = MockSpi::new();
            let decorator = ReturnedFaultSpiDevice::new(spi, &evidence);
            let mut firewall = RxOnlySpiDevice::new(decorator);

            let error = run_ready(firewall.write(&[opcode])).unwrap_err();
            assert!(matches!(error, RxOnlySpiError::ForbiddenOpcode(value) if value == opcode));
            assert_eq!(
                evidence.snapshot().state(),
                ReturnedFaultState::WaitingForSetRx
            );
            assert!(control.commands.borrow().is_empty());
        }
    }

    #[test]
    fn returned_fault_decorator_drives_real_receive_teardown_without_physical_irq_query() {
        let evidence = ReturnedFaultEvidence::new();
        let (spi, spi_control) = MockSpi::new();
        let decorator = ReturnedFaultSpiDevice::new(spi, &evidence);
        let (reset, reset_probe) = MockOutput::new(PinState::Low);
        let (dio1, _) = MockWait::ready();
        let (busy, _) = MockWait::ready();
        let (power, power_probe) = MockOutput::new(PinState::Low);
        let (csd, csd_probe) = MockOutput::new(PinState::Low);
        let (ctx, ctx_probe) = MockOutput::new(PinState::Low);
        let interlock = TrackerRxInterlock::new(power, csd, ctx).unwrap();
        let mut fem_delay = ReadyDelay;
        let mut radio = run_ready(TrackerRxRadio::new(
            decorator,
            reset,
            dio1,
            busy,
            ReadyDelay,
            &mut fem_delay,
            interlock,
            test_timestamp_capture(),
            profile(),
            TRACKER_RX_CONFIGURATION,
        ))
        .unwrap();
        let mut buffer = [0; SX1262_FRAME_MTU];

        assert_eq!(
            run_ready(radio.receive(&mut buffer)),
            Err(TrackerRxRadioError {
                operation: TrackerRxRadioOperation::Receive,
                primary: TrackerRxRadioFault::Radio(TrackerRadioFaultCode::Spi),
                cleanup: None,
            })
        );
        let snapshot = evidence.snapshot();
        assert_eq!(snapshot.state(), ReturnedFaultState::Fired);
        assert!(snapshot.set_rx_forwarded());
        assert!(snapshot.get_irq_status_rejected_before_spi());
        assert!(snapshot.fired());

        let commands = spi_control.commands.borrow();
        assert!(has_command(&commands, &[SET_RX]));
        let set_rx = commands
            .iter()
            .rposition(|command| command.first() == Some(&SET_RX))
            .unwrap();
        assert!(
            commands[set_rx + 1..]
                .iter()
                .all(|command| command.first() != Some(&GET_IRQ_STATUS))
        );
        assert!(!has_forbidden_command(&commands));
        assert!(!radio.is_active());
        assert!(reset_probe.is_low());
        assert!(power_probe.is_low() && csd_probe.is_low() && ctx_probe.is_low());
    }

    #[test]
    fn tx_switch_hook_fails_while_receive_interlock_keeps_ctx_low() {
        let (spi, control) = MockSpi::new();
        let (reset, _) = MockOutput::new(PinState::Low);
        let (dio1, _) = MockWait::ready();
        let (busy, _) = MockWait::ready();
        let (power, _) = MockOutput::new(PinState::Low);
        let (csd, _) = MockOutput::new(PinState::Low);
        let (ctx, ctx_probe) = MockOutput::new(PinState::Low);
        let interlock = TrackerRxInterlock::new(power, csd, ctx).unwrap();
        let mut fem_delay = ReadyDelay;
        let mut interlock = match run_ready(interlock.enable(&mut fem_delay)) {
            Ok(interlock) => interlock,
            Err(_) => panic!("receive interlock failed to enable"),
        };
        let interface =
            TrackerRxInterfaceVariant::new(reset, dio1, busy, test_timestamp_capture()).unwrap();
        let mut radio = Sx126x::new(
            RxOnlySpiDevice::new(spi),
            interface,
            Config {
                chip: Sx1262,
                tcxo_ctrl: Some(TcxoCtrlVoltage::Ctrl1V8),
                use_dcdc: TRACKER_RX_CONFIGURATION.uses_dcdc(),
                rx_boost: TRACKER_RX_CONFIGURATION.boosted(),
            },
        );

        assert!(interlock.is_enabled());
        assert!(ctx_probe.is_low());
        assert!(!ctx_probe.was_ever_high());
        assert_eq!(
            run_ready(RadioKind::do_tx(&mut radio)),
            Err(RadioError::InvalidConfiguration)
        );
        assert!(!has_command(&control.commands.borrow(), &[SX1262_SET_TX]));
        assert!(ctx_probe.is_low());
        assert!(!ctx_probe.was_ever_high());
        interlock.shutdown().unwrap();
    }

    #[test]
    fn cold_start_and_receive_trace_is_rx_only() {
        let mut rig = build_radio();
        let initialization = rig.spi.commands.borrow().clone();
        let expected_initialization: &[&[u8]] = &[
            &[0xc0, 0x00],
            &[0x80, 0x00],
            &[SET_DIO2_AS_RF_SWITCH, 0x01],
            &[0x07],
            &[
                SET_TCXO_MODE,
                TcxoCtrlVoltage::Ctrl1V8.value(),
                0x00,
                0x02,
                0x80,
            ],
            &[0x89, 0x7f],
            &[0x8a, 0x01],
            &[WRITE_REGISTER, 0x07, 0x40, 0x14, 0x24],
            &[0x8f, 0x00, 0x00],
            &[0x1d, 0x02, 0x9f, 0x00],
            &[
                0x0d, 0x02, 0x9f, 0x01, 0x08, 0xac, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ],
            &[0x1d, 0x02, 0x9f, 0x00],
            &[
                0x0d, 0x02, 0x9f, 0x01, 0x08, 0x89, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ],
            &[0x1d, 0x08, 0xd8, 0x00],
            &[0x0d, 0x08, 0xd8, 0x1e],
            &[0x95, 0x02, 0x02, 0x00, 0x01],
            &[SET_TX_PARAMS, 0x08, 0x04],
            &[0x08, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00],
            &[0x98, 0xe1, 0xe9],
            &[0x8b, 0x07, 0x04, 0x01, 0x00],
            &[0x1d, 0x08, 0x89, 0x00],
            &[0x0d, 0x08, 0x89, 0x04],
            &[0x8c, 0x00, 0x12, 0x00, 0xff, 0x01, 0x00],
            &[0x1d, 0x07, 0x36, 0x00],
            &[0x0d, 0x07, 0x36, 0x04],
            &[0x86, 0x39, 0x30, 0x00, 0x00],
            &[0x08, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00],
        ];
        assert_command_trace(&initialization, expected_initialization);
        assert!(!has_forbidden_command(&initialization));
        assert!(rig.radio.is_active());
        assert!(!rig.reset.is_low());
        assert!(!rig.power.is_low() && !rig.csd.is_low() && rig.ctx.is_low());

        let mut buffer = [0; SX1262_FRAME_MTU];
        let received = run_ready(rig.radio.receive(&mut buffer)).unwrap();
        assert_eq!(received.len, 3);
        assert_eq!(received.signal.rssi_dbm, -100);
        assert_eq!(received.signal.snr_db, 2);
        assert_eq!(received.received_at_ticks, 1_234);
        assert_eq!(&buffer[..3], &[0xaa, 0xbb, 0xcc]);

        let complete_trace = rig.spi.commands.borrow();
        let expected_receive: &[&[u8]] = &[
            &[0x9f, 0x01],
            &[0xa0, 0x00],
            &[WRITE_REGISTER, 0x08, 0xac, 0x94],
            &[SET_RX, 0xff, 0xff, 0xff],
            &[GET_IRQ_STATUS],
            &[0x02, 0x00, 0x02],
            &[GET_RX_BUFFER_STATUS],
            &[READ_BUFFER, 0x00, 0x00],
            &[GET_PACKET_STATUS],
        ];
        let expected_complete_len = expected_initialization.len() + expected_receive.len();
        assert_eq!(
            complete_trace.len(),
            expected_complete_len,
            "complete SPI transaction count changed: {complete_trace:02x?}"
        );
        assert_command_trace(
            &complete_trace[expected_initialization.len()..],
            expected_receive,
        );
        assert!(!has_forbidden_command(&complete_trace));
        assert!(rig.radio.is_active());
    }

    #[test]
    fn dio1_wait_completion_timestamp_precedes_spi_status_reads() {
        MUTABLE_TEST_TICKS.store(0, Ordering::SeqCst);
        let timestamps = Box::leak(Box::new(RxEventTimestampCapture::new(
            mutable_test_now_ticks,
        )));
        let mut rig = build_radio_with_timestamp_capture(TRACKER_RX_CONFIGURATION, timestamps);
        rig.irq.ticks_on_wait_for_high.borrow_mut().extend([41, 42]);
        rig.spi
            .irq_statuses
            .borrow_mut()
            .extend([[0x00, 0x04], [0x00, 0x02]]);
        rig.spi.ticks_after_get_irq_status.set(Some(99));
        let mut buffer = [0; SX1262_FRAME_MTU];

        let received = run_ready(rig.radio.receive(&mut buffer)).unwrap();

        assert_eq!(received.received_at_ticks, 42);
        assert_eq!(MUTABLE_TEST_TICKS.load(Ordering::SeqCst), 99);
        assert!(!has_forbidden_command(&rig.spi.commands.borrow()));
        assert!(rig.radio.is_active());
    }

    #[test]
    fn electrical_variants_change_only_regulator_and_receive_gain_commands() {
        let variants = [
            TrackerRxConfiguration::new(TrackerRxRegulator::Ldo, TrackerRxGain::Unboosted),
            TrackerRxConfiguration::new(TrackerRxRegulator::Ldo, TrackerRxGain::Boosted),
            TrackerRxConfiguration::new(TrackerRxRegulator::DcDc, TrackerRxGain::Unboosted),
            TrackerRxConfiguration::new(TrackerRxRegulator::DcDc, TrackerRxGain::Boosted),
        ];
        let traces = variants.map(receive_trace);
        let baseline = canonical_electrical_trace(&traces[0]);

        assert_eq!(TRACKER_RX_CONFIGURATION, variants[0]);
        for (configuration, commands) in variants.into_iter().zip(traces.iter()) {
            let regulator_commands = commands
                .iter()
                .filter(|command| command.as_slice() == [SET_REGULATOR_MODE, 0x01])
                .count();
            assert_eq!(
                regulator_commands,
                usize::from(configuration.uses_dcdc()),
                "unexpected regulator trace for {configuration:?}: {commands:02x?}"
            );
            if configuration.uses_dcdc() {
                let index = commands
                    .iter()
                    .position(|command| command.as_slice() == [SET_REGULATOR_MODE, 0x01])
                    .expect("DC-DC command count was already checked");
                assert_eq!(commands[index - 1], [0x80, 0x00]);
                assert_eq!(commands[index + 1], [SET_DIO2_AS_RF_SWITCH, 0x01]);
            } else {
                // lora-phy 3.0.1 selects LDO by omitting SetRegulatorMode
                // after hardware reset; it does not emit an explicit 0x96/0x00.
                assert!(
                    !commands
                        .iter()
                        .any(|command| { command.first() == Some(&SET_REGULATOR_MODE) })
                );
            }

            let gain_commands: Vec<u8> = commands
                .iter()
                .filter_map(|command| {
                    if command.len() == 4 && command[..3] == [WRITE_REGISTER, 0x08, 0xac] {
                        Some(command[3])
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(
                gain_commands,
                [if configuration.boosted() { 0x96 } else { 0x94 }],
                "unexpected receive-gain trace for {configuration:?}: {commands:02x?}"
            );
            assert_eq!(
                canonical_electrical_trace(commands),
                baseline,
                "a command outside regulator/gain changed for {configuration:?}"
            );
            assert!(!has_forbidden_command(commands));
        }
    }

    #[test]
    fn receive_error_tears_down_and_faults_wrapper() {
        let mut rig = build_radio();
        rig.spi.fail_opcode.set(Some(GET_IRQ_STATUS));
        let mut buffer = [0; SX1262_FRAME_MTU];

        assert_eq!(
            run_ready(rig.radio.receive(&mut buffer)),
            Err(TrackerRxRadioError {
                operation: TrackerRxRadioOperation::Receive,
                primary: TrackerRxRadioFault::Radio(TrackerRadioFaultCode::Spi),
                cleanup: None,
            })
        );
        assert!(!rig.radio.is_active());
        assert!(rig.reset.is_low());
        assert!(rig.power.is_low() && rig.csd.is_low() && rig.ctx.is_low());
        assert_eq!(
            run_ready(rig.radio.receive(&mut buffer)),
            Err(TrackerRxRadioError {
                operation: TrackerRxRadioOperation::Receive,
                primary: TrackerRxRadioFault::Faulted,
                cleanup: None,
            })
        );
    }

    #[test]
    fn receive_and_fem_cleanup_failures_are_both_preserved() {
        let mut rig = build_radio();
        rig.spi.fail_opcode.set(Some(GET_IRQ_STATUS));
        rig.power.set_fail_low(true);
        let mut buffer = [0; SX1262_FRAME_MTU];

        let error = run_ready(rig.radio.receive(&mut buffer)).unwrap_err();
        assert_eq!(
            error,
            TrackerRxRadioError {
                operation: TrackerRxRadioOperation::Receive,
                primary: TrackerRxRadioFault::Radio(TrackerRadioFaultCode::Spi),
                cleanup: Some(TrackerRxRadioFault::Fem(RxOnlyFemFault::Power)),
            }
        );
        assert_eq!(
            error.classification(),
            RadioRxFaultClassification::with_cleanup(
                RadioRxFaultClass::Spi,
                RadioRxFaultClass::Fem,
            )
        );
        assert!(!rig.radio.is_active());
        assert!(rig.reset.is_low());
        assert!(!rig.power.is_low(), "fault injection leaves VFEM high");
        assert!(rig.csd.is_low() && rig.ctx.is_low());
    }

    #[test]
    fn radio_error_mapping_is_exhaustive_and_retains_scalar_details() {
        let cases = [
            (RadioError::SPI, TrackerRadioFaultCode::Spi),
            (RadioError::Reset, TrackerRadioFaultCode::Reset),
            (RadioError::RfSwitchRx, TrackerRadioFaultCode::RfSwitchRx),
            (RadioError::RfSwitchTx, TrackerRadioFaultCode::RfSwitchTx),
            (RadioError::Busy, TrackerRadioFaultCode::Busy),
            (RadioError::Irq, TrackerRadioFaultCode::Irq),
            (RadioError::DIO1, TrackerRadioFaultCode::Dio1),
            (
                RadioError::InvalidConfiguration,
                TrackerRadioFaultCode::InvalidConfiguration,
            ),
            (
                RadioError::InvalidRadioMode,
                TrackerRadioFaultCode::InvalidRadioMode,
            ),
            (
                RadioError::OpError(0xa5),
                TrackerRadioFaultCode::Operation(0xa5),
            ),
            (
                RadioError::InvalidBaseAddress(3, 9),
                TrackerRadioFaultCode::InvalidBaseAddress {
                    transmit: 3,
                    receive: 9,
                },
            ),
            (
                RadioError::PayloadSizeUnexpected(501),
                TrackerRadioFaultCode::PayloadSizeUnexpected(501),
            ),
            (
                RadioError::PayloadSizeMismatch(3, 4),
                TrackerRadioFaultCode::PayloadSizeMismatch {
                    expected: 3,
                    actual: 4,
                },
            ),
            (
                RadioError::UnavailableSpreadingFactor,
                TrackerRadioFaultCode::UnavailableSpreadingFactor,
            ),
            (
                RadioError::UnavailableBandwidth,
                TrackerRadioFaultCode::UnavailableBandwidth,
            ),
            (
                RadioError::InvalidBandwidthForFrequency,
                TrackerRadioFaultCode::InvalidBandwidthForFrequency,
            ),
            (
                RadioError::InvalidSF6ExplicitHeaderRequest,
                TrackerRadioFaultCode::InvalidSf6ExplicitHeaderRequest,
            ),
            (
                RadioError::InvalidOutputPowerForFrequency,
                TrackerRadioFaultCode::InvalidOutputPowerForFrequency,
            ),
            (
                RadioError::TransmitTimeout,
                TrackerRadioFaultCode::TransmitTimeout,
            ),
            (
                RadioError::ReceiveTimeout,
                TrackerRadioFaultCode::ReceiveTimeout,
            ),
            (
                RadioError::DutyCycleUnsupported,
                TrackerRadioFaultCode::DutyCycleUnsupported,
            ),
            (
                RadioError::RngUnsupported,
                TrackerRadioFaultCode::RngUnsupported,
            ),
        ];

        for (radio_error, expected) in cases {
            assert_eq!(
                TrackerRxRadioError::radio(TrackerRxRadioOperation::Receive, radio_error).primary,
                TrackerRxRadioFault::Radio(expected),
            );
        }
    }

    #[test]
    fn cancelled_receive_future_tears_down_instead_of_resuming_partial_irq_flow() {
        let mut rig = build_radio();
        rig.irq.pending.set(true);
        let mut buffer = [0; SX1262_FRAME_MTU];
        {
            let mut future = pin!(rig.radio.receive(&mut buffer));
            let waker = Waker::noop();
            let mut context = Context::from_waker(waker);
            assert!(future.as_mut().poll(&mut context).is_pending());
        }

        assert!(!rig.radio.is_active());
        assert!(rig.reset.is_low());
        assert!(rig.power.is_low() && rig.csd.is_low() && rig.ctx.is_low());
        assert!(!has_forbidden_command(&rig.spi.commands.borrow()));
    }
}
