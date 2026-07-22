//! Board-neutral `lora-phy` SX126x ownership for an RNode physical link.
//!
//! Board crates retain pin topology, RF-front-end sequencing, admitted
//! configurations and power policy. This crate owns the shared initialized
//! radio state machine: persistent continuous receive with bounded software
//! scheduler yields, channel activity detection, and one atomic one-or-two-
//! frame RNode transmission through [`SoleRnodeRadio`].

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::{cell::RefCell, future::Future, num::NonZeroU64};

use critical_section::Mutex;
use embassy_futures::select::{Either, select};
use embedded_hal_async::{delay::DelayNs, spi::SpiDevice};
use lora_phy::{
    LoRa, RxMode,
    mod_params::{ModulationParams, PacketParams, RadioError, ReceiveIrq},
    mod_traits::InterfaceVariant,
    sx126x::{Config, Sx126x, Sx126xVariant},
};
use reticulum_radio_interface::{
    BoundedRxObservation, BoundedRxOutcome, CadObservation, FrameSignal, LabRxProfile,
    PacketTxFault, PacketTxObservation, PacketTxProgress, RadioConfigurationFingerprint,
    RnodeTxFrames, SX1262_FRAME_MTU, SoleRadioFaultClass, SoleRadioFaultPhase,
    SoleRadioFaultSummary, SoleRnodeRadio,
};

/// Immutable driver settings selected and validated by a board policy crate.
///
/// This low-level type is intentionally constructible: it is not a regional or
/// board authorization boundary. Product firmware should expose only an opaque
/// board-owned configuration which produces these settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Sx126xRnodeSettings {
    profile: LabRxProfile,
    fingerprint: RadioConfigurationFingerprint,
    tx_power_dbm: i32,
    rx_symbol_timeout: u16,
    public_network: bool,
    use_dcdc: bool,
    rx_boost: bool,
    maximum_receive_operation_us: NonZeroU64,
}

impl Sx126xRnodeSettings {
    /// Construct a complete, immutable SX126x operation contract.
    #[allow(
        clippy::too_many_arguments,
        reason = "the value binds one complete radio contract"
    )]
    pub const fn new(
        profile: LabRxProfile,
        fingerprint: RadioConfigurationFingerprint,
        tx_power_dbm: i32,
        rx_symbol_timeout: u16,
        public_network: bool,
        use_dcdc: bool,
        rx_boost: bool,
        maximum_receive_operation_us: NonZeroU64,
    ) -> Self {
        Self {
            profile,
            fingerprint,
            tx_power_dbm,
            rx_symbol_timeout,
            public_network,
            use_dcdc,
            rx_boost,
            maximum_receive_operation_us,
        }
    }

    /// Validated LoRa modulation and packet profile.
    pub const fn profile(self) -> LabRxProfile {
        self.profile
    }

    /// Complete board-owned configuration fingerprint.
    pub const fn fingerprint(self) -> RadioConfigurationFingerprint {
        self.fingerprint
    }

    /// Commanded SX126x output power in dBm.
    pub const fn tx_power_dbm(self) -> i32 {
        self.tx_power_dbm
    }

    /// Bounded preamble-search timeout in LoRa symbols.
    pub const fn rx_symbol_timeout(self) -> u16 {
        self.rx_symbol_timeout
    }

    /// Whether the public LoRa sync word is selected.
    pub const fn public_network(self) -> bool {
        self.public_network
    }

    /// Whether the SX126x internal DC-DC regulator is selected.
    pub const fn uses_dcdc(self) -> bool {
        self.use_dcdc
    }

    /// Whether the SX126x boosted receive gain is selected.
    pub const fn boosted_receive(self) -> bool {
        self.rx_boost
    }

    /// Configuration-bound watchdog for one complete receive operation.
    pub const fn maximum_receive_operation_us(self) -> NonZeroU64 {
        self.maximum_receive_operation_us
    }
}

/// Board hook used to authorize or prepare an external transmit path.
///
/// An integrated-DIO2 board normally uses [`NoopTxHooks`]. A board with an
/// external FEM can implement a one-shot arm consumed by its private
/// [`InterfaceVariant`] hooks.
pub trait Sx126xTxHooks {
    /// Return the board path to its disarmed receive-safe state.
    fn disarm(&self);

    /// Authorize the upcoming `prepare_for_tx` call exactly once.
    fn arm_for_prepare(&self);
}

/// No-op hooks for modules whose RF switch is controlled entirely by SX126x DIO2.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopTxHooks;

impl Sx126xTxHooks for NoopTxHooks {
    fn disarm(&self) {}

    fn arm_for_prepare(&self) {}
}

/// Serialized timestamp captured as soon as an interface's DIO wait resumes.
///
/// The interface calls [`Self::record_irq_observation`]. The shared owner then
/// consumes the sample only after the driver has established which terminal
/// IRQ completed the operation.
pub struct IrqTimestampCapture {
    now_ticks: fn() -> u64,
    unit: TimestampUnit,
    latest: Mutex<RefCell<Option<u64>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TimestampUnit {
    LegacyTicks,
    MonotonicMicroseconds,
}

impl IrqTimestampCapture {
    /// Construct a legacy tick-domain capture for compatibility receive APIs.
    pub const fn new(now_ticks: fn() -> u64) -> Self {
        Self {
            now_ticks,
            unit: TimestampUnit::LegacyTicks,
            latest: Mutex::new(RefCell::new(None)),
        }
    }

    /// Construct a capture in the channel-access monotonic microsecond domain.
    pub const fn new_monotonic_us(now_us: fn() -> u64) -> Self {
        Self {
            now_ticks: now_us,
            unit: TimestampUnit::MonotonicMicroseconds,
            latest: Mutex::new(RefCell::new(None)),
        }
    }

    /// Record the time at which an interface's DIO wait resumed.
    pub fn record_irq_observation(&self) {
        let ticks = (self.now_ticks)();
        critical_section::with(|cs| self.latest.borrow(cs).replace(Some(ticks)));
    }

    fn begin_operation(&self) {
        critical_section::with(|cs| self.latest.borrow(cs).replace(None));
    }

    fn take_completed_operation(&self) -> Option<u64> {
        critical_section::with(|cs| self.latest.borrow(cs).take())
    }

    const fn supports_sole_radio_us(&self) -> bool {
        matches!(self.unit, TimestampUnit::MonotonicMicroseconds)
    }
}

/// Shared operation in which the SX126x owner failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Sx126xRadioOperation {
    /// SX126x reset and LoRa initialization.
    ChipInitialization,
    /// Modulation parameter construction.
    ModulationConfiguration,
    /// Packet parameter construction.
    PacketConfiguration,
    /// Caller supplied an invalid physical frame.
    FrameValidation,
    /// FIFO, packet, channel, and power preparation.
    PrepareTransmit,
    /// Physical transmission.
    Transmit,
    /// Post-transmit standby and RF-switch cleanup.
    TransmitCleanup,
    /// Modem and IRQ preparation for channel activity detection.
    PrepareChannelActivityDetection,
    /// Channel activity detection and its IRQ completion.
    ChannelActivityDetection,
    /// Post-CAD standby and RF-switch cleanup.
    ChannelActivityCleanup,
    /// Receive preparation, including persistent continuous receive.
    PrepareReceive,
    /// Receive IRQ wait and processing.
    Receive,
    /// Post-receive standby cleanup.
    ReceiveCleanup,
    /// A successful receive lacked its DIO-boundary timestamp.
    ReceiveTimestamp,
}

/// Detailed failure from the shared `lora-phy` owner.
#[derive(Debug)]
pub struct Sx126xRadioError {
    /// Exact operation that failed.
    pub operation: Sx126xRadioOperation,
    /// Driver classification, when failure came from the radio path.
    pub radio: Option<RadioError>,
}

impl Sx126xRadioError {
    const fn invalid_frame() -> Self {
        Self {
            operation: Sx126xRadioOperation::FrameValidation,
            radio: None,
        }
    }

    const fn radio(operation: Sx126xRadioOperation, radio: RadioError) -> Self {
        Self {
            operation,
            radio: Some(radio),
        }
    }

    const fn missing_receive_timestamp() -> Self {
        Self {
            operation: Sx126xRadioOperation::ReceiveTimestamp,
            radio: None,
        }
    }
}

/// One physical frame copied into a caller-owned receive buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Sx126xReceivedFrame {
    /// Number of valid bytes in the caller's buffer.
    pub len: u8,
    /// Packet RSSI reported by the driver.
    pub rssi_dbm: i16,
    /// Packet SNR reported by the driver.
    pub snr_db: i16,
    /// Monotonic sample taken when the final receive DIO wait resumed.
    pub received_at_ticks: u64,
}

type Driver<Spi, Interface, Chip> = Sx126x<Spi, Interface, Chip>;
type DriverLoRa<Spi, Interface, Chip, RadioDelay> = LoRa<Driver<Spi, Interface, Chip>, RadioDelay>;

struct Active<Spi, Interface, Chip, RadioDelay>
where
    Spi: SpiDevice<u8>,
    Interface: InterfaceVariant,
    Chip: Sx126xVariant,
    RadioDelay: DelayNs,
{
    lora: DriverLoRa<Spi, Interface, Chip, RadioDelay>,
    modulation: ModulationParams,
    tx_packet: PacketParams,
    rx_packet: PacketParams,
}

/// Reset-scoped owner of one initialized SX126x physical link.
///
/// Every sole-radio operation synchronously removes initialized hardware from
/// this wrapper before returning its future. Dropping even an unpolled future
/// therefore drops the board's private interface. That interface's `Drop`
/// implementation is responsible for asserting the board-specific inert state.
pub struct Sx126xRnodeRadio<Spi, Interface, Chip, RadioDelay, TxHooks>
where
    Spi: SpiDevice<u8>,
    Interface: InterfaceVariant,
    Chip: Sx126xVariant,
    RadioDelay: DelayNs,
    TxHooks: Sx126xTxHooks + 'static,
{
    active: Option<Active<Spi, Interface, Chip, RadioDelay>>,
    settings: Sx126xRnodeSettings,
    tx_hooks: &'static TxHooks,
    timestamps: &'static IrqTimestampCapture,
    continuous_rx_active: bool,
}

impl<Spi, Interface, Chip, RadioDelay, TxHooks>
    Sx126xRnodeRadio<Spi, Interface, Chip, RadioDelay, TxHooks>
where
    Spi: SpiDevice<u8>,
    Interface: InterfaceVariant,
    Chip: Sx126xVariant,
    RadioDelay: DelayNs,
    TxHooks: Sx126xTxHooks + 'static,
{
    /// Initialize the driver and bind all packet parameters once.
    #[allow(
        clippy::too_many_arguments,
        reason = "SPI, private interface, chip policy, delay, hooks, timestamps and immutable settings are distinct owners"
    )]
    pub async fn new(
        spi: Spi,
        interface: Interface,
        chip: Chip,
        radio_delay: RadioDelay,
        tx_hooks: &'static TxHooks,
        timestamps: &'static IrqTimestampCapture,
        settings: Sx126xRnodeSettings,
        tcxo_ctrl: Option<lora_phy::sx126x::TcxoCtrlVoltage>,
    ) -> Result<Self, Sx126xRadioError> {
        tx_hooks.disarm();
        let profile = settings.profile();
        let sx126x = Sx126x::new(
            spi,
            interface,
            Config {
                chip,
                tcxo_ctrl,
                use_dcdc: settings.uses_dcdc(),
                rx_boost: settings.boosted_receive(),
            },
        );
        let mut lora = LoRa::new(sx126x, settings.public_network(), radio_delay)
            .await
            .map_err(|radio| {
                Sx126xRadioError::radio(Sx126xRadioOperation::ChipInitialization, radio)
            })?;
        let modulation = lora
            .create_modulation_params(
                profile.spreading_factor(),
                profile.bandwidth(),
                profile.coding_rate(),
                profile.frequency_hz(),
            )
            .map_err(|radio| {
                Sx126xRadioError::radio(Sx126xRadioOperation::ModulationConfiguration, radio)
            })?;
        let tx_packet = lora
            .create_tx_packet_params(
                profile.preamble_symbols(),
                !profile.explicit_header(),
                profile.crc(),
                profile.iq_inverted(),
                &modulation,
            )
            .map_err(|radio| {
                Sx126xRadioError::radio(Sx126xRadioOperation::PacketConfiguration, radio)
            })?;
        let rx_packet = lora
            .create_rx_packet_params(
                profile.preamble_symbols(),
                !profile.explicit_header(),
                SX1262_FRAME_MTU as u8,
                profile.crc(),
                profile.iq_inverted(),
                &modulation,
            )
            .map_err(|radio| {
                Sx126xRadioError::radio(Sx126xRadioOperation::PacketConfiguration, radio)
            })?;

        Ok(Self {
            active: Some(Active {
                lora,
                modulation,
                tx_packet,
                rx_packet,
            }),
            settings,
            tx_hooks,
            timestamps,
            continuous_rx_active: false,
        })
    }

    /// Immutable low-level settings selected by the board wrapper.
    pub const fn settings(&self) -> Sx126xRnodeSettings {
        self.settings
    }

    /// Transmit exactly one physical frame.
    pub async fn transmit_frame(&mut self, frame: &[u8]) -> Result<(), Sx126xRadioError> {
        if frame.is_empty() || frame.len() > SX1262_FRAME_MTU {
            return Err(Sx126xRadioError::invalid_frame());
        }
        self.continuous_rx_active = false;
        let mut active = self.active.take().ok_or_else(|| {
            Sx126xRadioError::radio(Sx126xRadioOperation::Transmit, RadioError::InvalidRadioMode)
        })?;
        self.tx_hooks.disarm();
        self.tx_hooks.arm_for_prepare();
        if let Err(radio) = active
            .lora
            .prepare_for_tx(
                &active.modulation,
                &mut active.tx_packet,
                self.settings.tx_power_dbm(),
                frame,
            )
            .await
        {
            return Err(Sx126xRadioError::radio(
                Sx126xRadioOperation::PrepareTransmit,
                radio,
            ));
        }
        if let Err(radio) = active.lora.tx().await {
            self.tx_hooks.disarm();
            return Err(Sx126xRadioError::radio(
                Sx126xRadioOperation::Transmit,
                radio,
            ));
        }
        self.tx_hooks.disarm();
        if let Err(radio) = active.lora.enter_standby().await {
            return Err(Sx126xRadioError::radio(
                Sx126xRadioOperation::TransmitCleanup,
                radio,
            ));
        }
        self.active = Some(active);
        Ok(())
    }

    /// Run one low-level LoRa channel activity detection operation.
    pub async fn channel_activity_detected(&mut self) -> Result<bool, Sx126xRadioError> {
        self.continuous_rx_active = false;
        let mut active = self.active.take().ok_or_else(|| {
            Sx126xRadioError::radio(
                Sx126xRadioOperation::ChannelActivityDetection,
                RadioError::InvalidRadioMode,
            )
        })?;
        self.tx_hooks.disarm();
        if let Err(radio) = active.lora.prepare_for_cad(&active.modulation).await {
            return Err(Sx126xRadioError::radio(
                Sx126xRadioOperation::PrepareChannelActivityDetection,
                radio,
            ));
        }
        let detected = active.lora.cad(&active.modulation).await.map_err(|radio| {
            Sx126xRadioError::radio(Sx126xRadioOperation::ChannelActivityDetection, radio)
        })?;
        if let Err(radio) = active.lora.enter_standby().await {
            return Err(Sx126xRadioError::radio(
                Sx126xRadioOperation::ChannelActivityCleanup,
                radio,
            ));
        }
        self.active = Some(active);
        Ok(detected)
    }

    /// Run one receive operation with a bounded preamble-search window.
    pub fn receive_frame<'a>(
        &'a mut self,
        buffer: &'a mut [u8; SX1262_FRAME_MTU],
    ) -> impl core::future::Future<Output = Result<Option<Sx126xReceivedFrame>, Sx126xRadioError>> + 'a
    {
        self.continuous_rx_active = false;
        let active = self.active.take();
        self.tx_hooks.disarm();
        let timestamps = self.timestamps;
        let symbol_timeout = self.settings.rx_symbol_timeout();

        async move {
            let mut active = active.ok_or_else(|| {
                Sx126xRadioError::radio(Sx126xRadioOperation::Receive, RadioError::InvalidRadioMode)
            })?;
            timestamps.begin_operation();
            if let Err(radio) = active
                .lora
                .prepare_for_rx(
                    RxMode::Single(symbol_timeout),
                    &active.modulation,
                    &active.rx_packet,
                )
                .await
            {
                return Err(Sx126xRadioError::radio(
                    Sx126xRadioOperation::PrepareReceive,
                    radio,
                ));
            }
            match active.lora.rx(&active.rx_packet, buffer).await {
                Ok((len, status)) => {
                    let Some(received_at_ticks) = timestamps.take_completed_operation() else {
                        return Err(Sx126xRadioError::missing_receive_timestamp());
                    };
                    if let Err(radio) = active.lora.enter_standby().await {
                        return Err(Sx126xRadioError::radio(
                            Sx126xRadioOperation::ReceiveCleanup,
                            radio,
                        ));
                    }
                    self.active = Some(active);
                    Ok(Some(Sx126xReceivedFrame {
                        len,
                        rssi_dbm: status.rssi,
                        snr_db: status.snr,
                        received_at_ticks,
                    }))
                }
                Err(RadioError::ReceiveTimeout) => {
                    let _ = timestamps.take_completed_operation();
                    self.active = Some(active);
                    Ok(None)
                }
                Err(radio) => Err(Sx126xRadioError::radio(
                    Sx126xRadioOperation::Receive,
                    radio,
                )),
            }
        }
    }

    /// Drop initialized hardware into the private interface's inert state.
    pub fn shutdown(&mut self) {
        self.continuous_rx_active = false;
        self.tx_hooks.disarm();
        self.active.take();
    }

    /// Whether initialized hardware is still owned and usable.
    pub const fn is_active(&self) -> bool {
        self.active.is_some()
    }
}

fn sole_fault_class(radio: &RadioError) -> SoleRadioFaultClass {
    match radio {
        RadioError::SPI => SoleRadioFaultClass::Spi,
        RadioError::Reset => SoleRadioFaultClass::Reset,
        RadioError::RfSwitchRx | RadioError::RfSwitchTx => SoleRadioFaultClass::RfSwitch,
        RadioError::Busy => SoleRadioFaultClass::Busy,
        RadioError::Irq | RadioError::DIO1 => SoleRadioFaultClass::Interrupt,
        RadioError::InvalidConfiguration
        | RadioError::InvalidRadioMode
        | RadioError::InvalidBaseAddress(_, _)
        | RadioError::UnavailableSpreadingFactor
        | RadioError::UnavailableBandwidth
        | RadioError::InvalidBandwidthForFrequency
        | RadioError::InvalidSF6ExplicitHeaderRequest
        | RadioError::InvalidOutputPowerForFrequency => SoleRadioFaultClass::Configuration,
        RadioError::OpError(_) => SoleRadioFaultClass::Operation,
        RadioError::PayloadSizeUnexpected(_) | RadioError::PayloadSizeMismatch(_, _) => {
            SoleRadioFaultClass::Payload
        }
        RadioError::TransmitTimeout | RadioError::ReceiveTimeout => SoleRadioFaultClass::Timeout,
        RadioError::DutyCycleUnsupported | RadioError::RngUnsupported => {
            SoleRadioFaultClass::Unsupported
        }
    }
}

fn sole_radio_fault(phase: SoleRadioFaultPhase, radio: &RadioError) -> SoleRadioFaultSummary {
    SoleRadioFaultSummary::new(phase, sole_fault_class(radio))
}

fn sole_receive_fault(error: &Sx126xRadioError) -> SoleRadioFaultSummary {
    let phase = match error.operation {
        Sx126xRadioOperation::PrepareReceive => SoleRadioFaultPhase::ReceivePreparation,
        Sx126xRadioOperation::Receive => SoleRadioFaultPhase::Receive,
        Sx126xRadioOperation::ReceiveCleanup => SoleRadioFaultPhase::ReceiveCleanup,
        Sx126xRadioOperation::ReceiveTimestamp => SoleRadioFaultPhase::ReceiveTimestampCapture,
        _ => SoleRadioFaultPhase::Receive,
    };
    let class = match error.radio.as_ref() {
        Some(radio) => sole_fault_class(radio),
        None if error.operation == Sx126xRadioOperation::ReceiveTimestamp => {
            SoleRadioFaultClass::Timestamp
        }
        None => SoleRadioFaultClass::Operation,
    };
    SoleRadioFaultSummary::new(phase, class)
}

const fn inactive_fault(phase: SoleRadioFaultPhase) -> SoleRadioFaultSummary {
    SoleRadioFaultSummary::new(phase, SoleRadioFaultClass::Faulted)
}

const fn timestamp_fault() -> SoleRadioFaultSummary {
    SoleRadioFaultSummary::new(
        SoleRadioFaultPhase::TimestampCapture,
        SoleRadioFaultClass::Timestamp,
    )
}

impl<Spi, Interface, Chip, RadioDelay, TxHooks> SoleRnodeRadio
    for Sx126xRnodeRadio<Spi, Interface, Chip, RadioDelay, TxHooks>
where
    Spi: SpiDevice<u8>,
    Interface: InterfaceVariant,
    Chip: Sx126xVariant,
    RadioDelay: DelayNs,
    TxHooks: Sx126xTxHooks + 'static,
{
    type Fault = SoleRadioFaultSummary;

    fn configuration_fingerprint(&self) -> RadioConfigurationFingerprint {
        self.settings.fingerprint()
    }

    fn airtime_profile(&self) -> LabRxProfile {
        self.settings.profile()
    }

    fn maximum_receive_operation_us(&self) -> NonZeroU64 {
        self.settings.maximum_receive_operation_us()
    }

    fn receive_bounded<'a>(
        &'a mut self,
        buffer: &'a mut [u8; SX1262_FRAME_MTU],
    ) -> impl core::future::Future<Output = Result<BoundedRxOutcome, Self::Fault>> + 'a {
        let monotonic_us = self.timestamps.supports_sole_radio_us();
        let receive = self.receive_frame(buffer);
        async move {
            if !monotonic_us {
                drop(receive);
                return Err(SoleRadioFaultSummary::new(
                    SoleRadioFaultPhase::ReceivePreparation,
                    SoleRadioFaultClass::Configuration,
                ));
            }
            match receive.await.map_err(|error| sole_receive_fault(&error))? {
                Some(frame) => Ok(BoundedRxOutcome::Frame(BoundedRxObservation::new(
                    usize::from(frame.len),
                    FrameSignal::new(frame.rssi_dbm, frame.snr_db),
                    frame.received_at_ticks,
                ))),
                None => Ok(BoundedRxOutcome::NoPreambleTimeout),
            }
        }
    }

    fn receive_continuous_until<'a, SchedulerYield, ProgressDeadline>(
        &'a mut self,
        buffer: &'a mut [u8; SX1262_FRAME_MTU],
        scheduler_yield: SchedulerYield,
        progress_deadline: ProgressDeadline,
    ) -> impl Future<Output = Result<BoundedRxOutcome, Self::Fault>> + 'a
    where
        SchedulerYield: Future<Output = ()> + 'a,
        ProgressDeadline: Future<Output = ()> + 'a,
    {
        let active = self.active.take();
        self.tx_hooks.disarm();
        let timestamps = self.timestamps;

        async move {
            let mut active =
                active.ok_or_else(|| inactive_fault(SoleRadioFaultPhase::ReceivePreparation))?;
            if !timestamps.supports_sole_radio_us() {
                return Err(SoleRadioFaultSummary::new(
                    SoleRadioFaultPhase::ReceivePreparation,
                    SoleRadioFaultClass::Configuration,
                ));
            }

            if !self.continuous_rx_active {
                if let Err(radio) = active
                    .lora
                    .prepare_for_rx(RxMode::Continuous, &active.modulation, &active.rx_packet)
                    .await
                {
                    return Err(sole_radio_fault(
                        SoleRadioFaultPhase::ReceivePreparation,
                        &radio,
                    ));
                }
                if let Err(radio) = active.lora.start_rx().await {
                    return Err(sole_radio_fault(
                        SoleRadioFaultPhase::ReceivePreparation,
                        &radio,
                    ));
                }
                self.continuous_rx_active = true;
            }

            timestamps.begin_operation();
            match select(active.lora.wait_for_irq(), scheduler_yield).await {
                Either::Second(()) => {
                    let _ = timestamps.take_completed_operation();
                    self.active = Some(active);
                    return Ok(BoundedRxOutcome::SchedulerYield);
                }
                Either::First(Err(radio)) => {
                    return Err(sole_radio_fault(SoleRadioFaultPhase::Receive, &radio));
                }
                Either::First(Ok(())) => {}
            }

            let mut progress_locked = false;
            let mut progress_deadline = core::pin::pin!(progress_deadline);
            loop {
                match active.lora.process_rx_irq(&active.rx_packet, buffer).await {
                    Ok(Some(ReceiveIrq::PacketReceived { len, status })) => {
                        let received_at_us = timestamps
                            .take_completed_operation()
                            .ok_or_else(timestamp_fault)?;
                        self.active = Some(active);
                        return Ok(BoundedRxOutcome::Frame(BoundedRxObservation::new(
                            usize::from(len),
                            FrameSignal::new(status.rssi, status.snr),
                            received_at_us,
                        )));
                    }
                    Ok(Some(ReceiveIrq::PreambleReceived)) => {
                        progress_locked = true;
                        let _ = timestamps.take_completed_operation();
                        timestamps.begin_operation();
                        match select(active.lora.wait_for_irq(), progress_deadline.as_mut()).await {
                            Either::First(Ok(())) => {}
                            Either::First(Err(radio)) => {
                                return Err(sole_radio_fault(SoleRadioFaultPhase::Receive, &radio));
                            }
                            Either::Second(()) => {
                                let _ = timestamps.take_completed_operation();
                                if let Err(radio) = active.lora.start_rx().await {
                                    return Err(sole_radio_fault(
                                        SoleRadioFaultPhase::ReceivePreparation,
                                        &radio,
                                    ));
                                }
                                self.active = Some(active);
                                return Ok(BoundedRxOutcome::InvalidFrame);
                            }
                        }
                    }
                    Ok(None) if !progress_locked => {
                        let _ = timestamps.take_completed_operation();
                        self.active = Some(active);
                        return Ok(BoundedRxOutcome::SchedulerYield);
                    }
                    Ok(None) => {
                        let _ = timestamps.take_completed_operation();
                        timestamps.begin_operation();
                        match select(active.lora.wait_for_irq(), progress_deadline.as_mut()).await {
                            Either::First(Ok(())) => {}
                            Either::First(Err(radio)) => {
                                return Err(sole_radio_fault(SoleRadioFaultPhase::Receive, &radio));
                            }
                            Either::Second(()) => {
                                let _ = timestamps.take_completed_operation();
                                if let Err(radio) = active.lora.start_rx().await {
                                    return Err(sole_radio_fault(
                                        SoleRadioFaultPhase::ReceivePreparation,
                                        &radio,
                                    ));
                                }
                                self.active = Some(active);
                                return Ok(BoundedRxOutcome::InvalidFrame);
                            }
                        }
                    }
                    Err(RadioError::ReceiveTimeout) => {
                        let _ = timestamps.take_completed_operation();
                        self.active = Some(active);
                        return Ok(BoundedRxOutcome::InvalidFrame);
                    }
                    Err(radio) => {
                        return Err(sole_radio_fault(SoleRadioFaultPhase::Receive, &radio));
                    }
                }
            }
        }
    }

    fn invalidate_receive_session(&mut self) {
        self.continuous_rx_active = false;
    }

    fn cad(
        &mut self,
    ) -> impl core::future::Future<Output = Result<CadObservation, Self::Fault>> + '_ {
        self.continuous_rx_active = false;
        let active = self.active.take();
        let tx_hooks = self.tx_hooks;
        let timestamps = self.timestamps;

        async move {
            let mut active = active
                .ok_or_else(|| inactive_fault(SoleRadioFaultPhase::ChannelActivityDetection))?;
            tx_hooks.disarm();
            if !timestamps.supports_sole_radio_us() {
                return Err(SoleRadioFaultSummary::new(
                    SoleRadioFaultPhase::ChannelActivityPreparation,
                    SoleRadioFaultClass::Configuration,
                ));
            }
            timestamps.begin_operation();
            if let Err(radio) = active.lora.prepare_for_cad(&active.modulation).await {
                return Err(sole_radio_fault(
                    SoleRadioFaultPhase::ChannelActivityPreparation,
                    &radio,
                ));
            }
            let activity_detected = active.lora.cad(&active.modulation).await.map_err(|radio| {
                sole_radio_fault(SoleRadioFaultPhase::ChannelActivityDetection, &radio)
            })?;
            let observed_at_us = timestamps
                .take_completed_operation()
                .ok_or_else(timestamp_fault)?;
            if let Err(radio) = active.lora.enter_standby().await {
                return Err(sole_radio_fault(
                    SoleRadioFaultPhase::ChannelActivityCleanup,
                    &radio,
                ));
            }
            self.active = Some(active);
            Ok(CadObservation::new(activity_detected, observed_at_us))
        }
    }

    fn transmit<'a>(
        &'a mut self,
        frames: RnodeTxFrames<'a>,
    ) -> impl core::future::Future<Output = Result<PacketTxObservation, PacketTxFault<Self::Fault>>> + 'a
    {
        self.continuous_rx_active = false;
        let active = self.active.take();
        let settings = self.settings;
        let tx_hooks = self.tx_hooks;
        let timestamps = self.timestamps;

        async move {
            let mut active = active.ok_or_else(|| {
                PacketTxFault::new(
                    inactive_fault(SoleRadioFaultPhase::FrameTransmission),
                    PacketTxProgress::none(),
                )
            })?;
            let first = frames.first();
            let second = frames.second();
            if first.is_empty()
                || first.len() > SX1262_FRAME_MTU
                || second.is_some_and(|frame| frame.is_empty() || frame.len() > SX1262_FRAME_MTU)
            {
                return Err(PacketTxFault::new(
                    SoleRadioFaultSummary::new(
                        SoleRadioFaultPhase::PacketValidation,
                        SoleRadioFaultClass::Payload,
                    ),
                    PacketTxProgress::none(),
                ));
            }
            if !timestamps.supports_sole_radio_us() {
                return Err(PacketTxFault::new(
                    SoleRadioFaultSummary::new(
                        SoleRadioFaultPhase::FramePreparation,
                        SoleRadioFaultClass::Configuration,
                    ),
                    PacketTxProgress::none(),
                ));
            }

            let mut progress = PacketTxProgress::none();
            let mut first_completed_at_us = None;
            let mut second_completed_at_us = None;
            for frame_index in 0..frames.frame_count() {
                let Some(frame) = frames.frame(frame_index) else {
                    return Err(PacketTxFault::new(
                        SoleRadioFaultSummary::new(
                            SoleRadioFaultPhase::PacketValidation,
                            SoleRadioFaultClass::Payload,
                        ),
                        progress,
                    ));
                };

                tx_hooks.disarm();
                tx_hooks.arm_for_prepare();
                if let Err(radio) = active
                    .lora
                    .prepare_for_tx(
                        &active.modulation,
                        &mut active.tx_packet,
                        settings.tx_power_dbm(),
                        frame,
                    )
                    .await
                {
                    return Err(PacketTxFault::new(
                        sole_radio_fault(SoleRadioFaultPhase::FramePreparation, &radio),
                        progress,
                    ));
                }

                timestamps.begin_operation();
                if let Err(radio) = active.lora.tx().await {
                    tx_hooks.disarm();
                    return Err(PacketTxFault::new(
                        sole_radio_fault(SoleRadioFaultPhase::FrameTransmission, &radio),
                        progress,
                    ));
                }
                tx_hooks.disarm();
                progress = if frame_index == 0 {
                    PacketTxProgress::first_completed_timestamp_missing()
                } else {
                    let Some(first_completed_at_us) = first_completed_at_us else {
                        return Err(PacketTxFault::new(
                            SoleRadioFaultSummary::new(
                                SoleRadioFaultPhase::PacketValidation,
                                SoleRadioFaultClass::Operation,
                            ),
                            progress,
                        ));
                    };
                    PacketTxProgress::both_completed_second_timestamp_missing(first_completed_at_us)
                };
                let Some(completed_at_us) = timestamps.take_completed_operation() else {
                    return Err(PacketTxFault::new(timestamp_fault(), progress));
                };
                if frame_index == 0 {
                    first_completed_at_us = Some(completed_at_us);
                    progress = PacketTxProgress::first_completed(completed_at_us);
                } else {
                    let Some(first_completed_at_us) = first_completed_at_us else {
                        return Err(PacketTxFault::new(
                            SoleRadioFaultSummary::new(
                                SoleRadioFaultPhase::PacketValidation,
                                SoleRadioFaultClass::Operation,
                            ),
                            progress,
                        ));
                    };
                    second_completed_at_us = Some(completed_at_us);
                    progress =
                        PacketTxProgress::both_completed(first_completed_at_us, completed_at_us);
                }

                if let Err(radio) = active.lora.enter_standby().await {
                    return Err(PacketTxFault::new(
                        sole_radio_fault(SoleRadioFaultPhase::FrameCleanup, &radio),
                        progress,
                    ));
                }
            }

            let Some(first_completed_at_us) = first_completed_at_us else {
                return Err(PacketTxFault::new(
                    SoleRadioFaultSummary::new(
                        SoleRadioFaultPhase::PacketValidation,
                        SoleRadioFaultClass::Operation,
                    ),
                    progress,
                ));
            };
            let observation = match second_completed_at_us {
                Some(second_completed_at_us) => {
                    PacketTxObservation::two_frames(first_completed_at_us, second_completed_at_us)
                }
                None => PacketTxObservation::one_frame(first_completed_at_us),
            };
            self.active = Some(active);
            Ok(observation)
        }
    }

    fn shutdown(&mut self) {
        Sx126xRnodeRadio::shutdown(self);
    }

    fn is_active(&self) -> bool {
        Sx126xRnodeRadio::is_active(self)
    }
}
