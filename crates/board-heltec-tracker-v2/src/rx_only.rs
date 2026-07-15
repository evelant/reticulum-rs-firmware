//! Fail-closed ownership of the Tracker V2 external receive path.

use embedded_hal::digital::{OutputPin, StatefulOutputPin};
use embedded_hal_async::delay::DelayNs;

/// Provisional delay after enabling the KCT8103L supply.
pub const FEM_POWER_SETTLE_MS: u32 = 5;

/// Provisional delay after enabling the KCT8103L chip.
pub const FEM_CSD_SETTLE_MS: u32 = 5;

/// A safety-critical Tracker FEM pin operation failed.
#[derive(Debug, Eq, PartialEq)]
pub enum RxOnlyFemError<PowerError, CsdError, CtxError> {
    /// CTX could not be driven low.
    ContextWrite(CtxError),
    /// The configured CTX output state could not be inspected.
    ContextRead(CtxError),
    /// CTX did not report low after being driven low.
    ContextNotLow,
    /// CSD could not be changed.
    ChipEnable(CsdError),
    /// VFEM power could not be changed.
    Power(PowerError),
}

impl<PowerError, CsdError, CtxError> RxOnlyFemError<PowerError, CsdError, CtxError> {
    /// Bounded pin classification suitable for target diagnostics.
    pub const fn kind(&self) -> RxOnlyFemFault {
        match self {
            Self::ContextWrite(_) => RxOnlyFemFault::ContextWrite,
            Self::ContextRead(_) => RxOnlyFemFault::ContextRead,
            Self::ContextNotLow => RxOnlyFemFault::ContextNotLow,
            Self::ChipEnable(_) => RxOnlyFemFault::ChipEnable,
            Self::Power(_) => RxOnlyFemFault::Power,
        }
    }
}

/// Allocation-free classification of an external-FEM pin fault.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RxOnlyFemFault {
    ContextWrite,
    ContextRead,
    ContextNotLow,
    ChipEnable,
    Power,
}

type FemErrorFor<Power, Csd, Ctx> = RxOnlyFemError<
    <Power as embedded_hal::digital::ErrorType>::Error,
    <Csd as embedded_hal::digital::ErrorType>::Error,
    <Ctx as embedded_hal::digital::ErrorType>::Error,
>;

type TrackerInterlockFailureParts<Power, Csd, Ctx> = (
    TrackerRxInterlock<Power, Csd, Ctx>,
    FemErrorFor<Power, Csd, Ctx>,
);

/// Opaque owner of KCT8103L power, chip-enable and receive/TX selection.
///
/// CTX is never exposed and this type has no operation capable of driving it
/// high. Dropping the owner performs a best-effort fail-closed transition in
/// CTX, CSD, power order.
pub(crate) struct RxOnlyFem<Power, Csd, Ctx>
where
    Power: OutputPin,
    Csd: OutputPin,
    Ctx: StatefulOutputPin,
{
    power: Power,
    csd: Csd,
    ctx: Ctx,
    enabled: bool,
}

impl<Power, Csd, Ctx> RxOnlyFem<Power, Csd, Ctx>
where
    Power: OutputPin,
    Csd: OutputPin,
    Ctx: StatefulOutputPin,
{
    /// Take ownership and establish the fail-closed state immediately.
    pub(crate) fn new(
        power: Power,
        csd: Csd,
        ctx: Ctx,
    ) -> Result<Self, FemErrorFor<Power, Csd, Ctx>> {
        let mut owner = Self {
            power,
            csd,
            ctx,
            enabled: false,
        };
        owner.force_disabled()?;
        Ok(owner)
    }

    /// Enable only the receive path using the provisional Phase 1 ordering.
    ///
    /// The owner is consumed by the future. Cancelling it therefore drops the
    /// owner and reasserts the fail-closed state instead of leaving a partially
    /// completed power transition behind.
    pub(crate) async fn enable<D>(
        mut self,
        delay: &mut D,
    ) -> Result<Self, RxOnlyFemEnableFailure<Power, Csd, Ctx>>
    where
        D: DelayNs,
    {
        if let Err(cause) = self.enable_inner(delay).await {
            self.disable_quietly();
            return Err(RxOnlyFemEnableFailure { owner: self, cause });
        }
        Ok(self)
    }

    /// Reassert and inspect the receive-only CTX interlock.
    pub(crate) fn reassert_receive_interlock(
        &mut self,
    ) -> Result<(), FemErrorFor<Power, Csd, Ctx>> {
        if let Err(cause) = self.ensure_context_low() {
            self.disable_quietly();
            return Err(cause);
        }
        Ok(())
    }

    /// Disable the external path in CTX, CSD, power order.
    ///
    /// Every low transition is attempted even if an earlier pin reports an
    /// error. The first error in safety order is returned after all attempts.
    pub(crate) fn shutdown(&mut self) -> Result<(), FemErrorFor<Power, Csd, Ctx>> {
        self.force_disabled()
    }

    /// Whether the full receive-only enable sequence most recently completed.
    pub(crate) const fn is_enabled(&self) -> bool {
        self.enabled
    }

    async fn enable_inner<D>(&mut self, delay: &mut D) -> Result<(), FemErrorFor<Power, Csd, Ctx>>
    where
        D: DelayNs,
    {
        self.ensure_context_low()?;
        self.power.set_high().map_err(RxOnlyFemError::Power)?;
        delay.delay_ms(FEM_POWER_SETTLE_MS).await;
        self.csd.set_high().map_err(RxOnlyFemError::ChipEnable)?;
        delay.delay_ms(FEM_CSD_SETTLE_MS).await;
        self.ensure_context_low()?;
        self.enabled = true;
        Ok(())
    }

    fn ensure_context_low(&mut self) -> Result<(), FemErrorFor<Power, Csd, Ctx>> {
        self.ctx.set_low().map_err(RxOnlyFemError::ContextWrite)?;
        let is_low = self.ctx.is_set_low().map_err(RxOnlyFemError::ContextRead)?;
        if !is_low {
            return Err(RxOnlyFemError::ContextNotLow);
        }
        Ok(())
    }

    fn force_disabled(&mut self) -> Result<(), FemErrorFor<Power, Csd, Ctx>> {
        let context_error = self.ctx.set_low().err();
        let csd_error = self.csd.set_low().err();
        let power_error = self.power.set_low().err();
        self.enabled = false;

        if let Some(error) = context_error {
            return Err(RxOnlyFemError::ContextWrite(error));
        }
        if let Some(error) = csd_error {
            return Err(RxOnlyFemError::ChipEnable(error));
        }
        if let Some(error) = power_error {
            return Err(RxOnlyFemError::Power(error));
        }
        Ok(())
    }

    fn disable_quietly(&mut self) {
        if self.force_disabled().is_err() {
            // There is no second safe electrical state to select. Callers get
            // the initiating error; this best-effort Drop-style path has no
            // channel for a second failure. Explicit wrapper shutdown paths
            // preserve a separately returned cleanup classification.
        }
    }
}

impl<Power, Csd, Ctx> Drop for RxOnlyFem<Power, Csd, Ctx>
where
    Power: OutputPin,
    Csd: OutputPin,
    Ctx: StatefulOutputPin,
{
    fn drop(&mut self) {
        self.disable_quietly();
    }
}

/// Failed receive-path enable with the disabled owner retained.
pub(crate) struct RxOnlyFemEnableFailure<Power, Csd, Ctx>
where
    Power: OutputPin,
    Csd: OutputPin,
    Ctx: StatefulOutputPin,
{
    owner: RxOnlyFem<Power, Csd, Ctx>,
    cause: FemErrorFor<Power, Csd, Ctx>,
}

/// Tracker receive safety owner for the KCT8103L power, CSD and CTX controls.
///
/// SX1262 DIO2 is wired directly to KCT8103L CPS on Tracker V2.3 and is owned
/// by the radio, not by an ESP32 GPIO. GPIO46 is a separate header signal.
pub struct TrackerRxInterlock<Power, Csd, Ctx>
where
    Power: OutputPin,
    Csd: OutputPin,
    Ctx: StatefulOutputPin,
{
    fem: RxOnlyFem<Power, Csd, Ctx>,
}

impl<Power, Csd, Ctx> TrackerRxInterlock<Power, Csd, Ctx>
where
    Power: OutputPin,
    Csd: OutputPin,
    Ctx: StatefulOutputPin,
{
    /// Claim all external-path controls in their fail-closed modes.
    pub fn new(power: Power, csd: Csd, ctx: Ctx) -> Result<Self, FemErrorFor<Power, Csd, Ctx>> {
        Ok(Self {
            fem: RxOnlyFem::new(power, csd, ctx)?,
        })
    }

    /// Enable the receive-only FEM.
    pub(crate) async fn enable<D>(
        self,
        delay: &mut D,
    ) -> Result<Self, TrackerRxInterlockEnableFailure<Power, Csd, Ctx>>
    where
        D: DelayNs,
    {
        let Self { fem } = self;
        match fem.enable(delay).await {
            Ok(fem) => Ok(Self { fem }),
            Err(failure) => Err(TrackerRxInterlockEnableFailure {
                owner: Self { fem: failure.owner },
                cause: failure.cause,
            }),
        }
    }

    /// Reassert the CTX receive selection without exposing its pin.
    pub(crate) fn reassert_receive_interlock(
        &mut self,
    ) -> Result<(), FemErrorFor<Power, Csd, Ctx>> {
        self.fem.reassert_receive_interlock()
    }

    /// Disable the external path.
    pub(crate) fn shutdown(&mut self) -> Result<(), FemErrorFor<Power, Csd, Ctx>> {
        self.fem.shutdown()
    }

    /// Whether the complete receive-only transition has completed.
    pub(crate) const fn is_enabled(&self) -> bool {
        self.fem.is_enabled()
    }
}

/// Failed Tracker receive-path enable with every critical pin still owned.
pub(crate) struct TrackerRxInterlockEnableFailure<Power, Csd, Ctx>
where
    Power: OutputPin,
    Csd: OutputPin,
    Ctx: StatefulOutputPin,
{
    owner: TrackerRxInterlock<Power, Csd, Ctx>,
    cause: FemErrorFor<Power, Csd, Ctx>,
}

impl<Power, Csd, Ctx> TrackerRxInterlockEnableFailure<Power, Csd, Ctx>
where
    Power: OutputPin,
    Csd: OutputPin,
    Ctx: StatefulOutputPin,
{
    /// Recover the disabled opaque owner and the originating pin failure.
    pub(crate) fn into_parts(self) -> TrackerInterlockFailureParts<Power, Csd, Ctx> {
        (self.owner, self.cause)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::{
        cell::Cell,
        future::{Future, pending},
        pin::pin,
        task::{Context, Poll, Waker},
    };
    use std::{cell::RefCell, rc::Rc, vec, vec::Vec};

    use embedded_hal::digital::{ErrorKind, ErrorType};

    use super::*;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum PinName {
        Power,
        Csd,
        Ctx,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Event {
        Low(PinName),
        High(PinName),
        DelayNs(u32),
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct MockError;

    impl embedded_hal::digital::Error for MockError {
        fn kind(&self) -> ErrorKind {
            ErrorKind::Other
        }
    }

    #[derive(Clone)]
    struct PinProbe {
        level_high: Rc<Cell<bool>>,
    }

    impl PinProbe {
        fn is_low(&self) -> bool {
            !self.level_high.get()
        }
    }

    struct MockOutput {
        name: PinName,
        level_high: Rc<Cell<bool>>,
        log: Rc<RefCell<Vec<Event>>>,
        fail_high: bool,
        report_not_low: bool,
    }

    impl MockOutput {
        fn new(name: PinName, log: Rc<RefCell<Vec<Event>>>) -> (Self, PinProbe) {
            let level_high = Rc::new(Cell::new(true));
            let probe = PinProbe {
                level_high: level_high.clone(),
            };
            (
                Self {
                    name,
                    level_high,
                    log,
                    fail_high: false,
                    report_not_low: false,
                },
                probe,
            )
        }
    }

    impl ErrorType for MockOutput {
        type Error = MockError;
    }

    impl OutputPin for MockOutput {
        fn set_low(&mut self) -> Result<(), Self::Error> {
            self.log.borrow_mut().push(Event::Low(self.name));
            self.level_high.set(false);
            Ok(())
        }

        fn set_high(&mut self) -> Result<(), Self::Error> {
            self.log.borrow_mut().push(Event::High(self.name));
            if self.fail_high {
                return Err(MockError);
            }
            self.level_high.set(true);
            Ok(())
        }
    }

    impl StatefulOutputPin for MockOutput {
        fn is_set_high(&mut self) -> Result<bool, Self::Error> {
            Ok(!self.is_set_low()?)
        }

        fn is_set_low(&mut self) -> Result<bool, Self::Error> {
            Ok(!self.report_not_low && !self.level_high.get())
        }
    }

    struct ReadyDelay {
        log: Rc<RefCell<Vec<Event>>>,
    }

    impl DelayNs for ReadyDelay {
        async fn delay_ns(&mut self, ns: u32) {
            self.log.borrow_mut().push(Event::DelayNs(ns));
        }
    }

    struct PendingDelay;

    impl DelayNs for PendingDelay {
        async fn delay_ns(&mut self, _ns: u32) {
            pending::<()>().await;
        }
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

    type Interlock = TrackerRxInterlock<MockOutput, MockOutput, MockOutput>;

    fn new_interlock(log: Rc<RefCell<Vec<Event>>>) -> (Interlock, PinProbe, PinProbe, PinProbe) {
        let (power, power_probe) = MockOutput::new(PinName::Power, log.clone());
        let (csd, csd_probe) = MockOutput::new(PinName::Csd, log.clone());
        let (ctx, ctx_probe) = MockOutput::new(PinName::Ctx, log);
        let owner = TrackerRxInterlock::new(power, csd, ctx).unwrap();
        (owner, power_probe, csd_probe, ctx_probe)
    }

    #[test]
    fn construction_establishes_fail_closed_order() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let (owner, power, csd, ctx) = new_interlock(log.clone());
        assert_eq!(
            *log.borrow(),
            vec![
                Event::Low(PinName::Ctx),
                Event::Low(PinName::Csd),
                Event::Low(PinName::Power),
            ]
        );
        assert!(!owner.is_enabled());
        assert!(power.is_low() && csd.is_low() && ctx.is_low());
    }

    #[test]
    fn receive_enable_sequence_never_drives_context_high() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let (owner, power, csd, ctx) = new_interlock(log.clone());
        log.borrow_mut().clear();

        let mut delay = ReadyDelay { log: log.clone() };
        let owner = run_ready(owner.enable(&mut delay)).unwrap_or_else(|_| panic!("enable failed"));

        assert_eq!(
            *log.borrow(),
            vec![
                Event::Low(PinName::Ctx),
                Event::High(PinName::Power),
                Event::DelayNs(FEM_POWER_SETTLE_MS * 1_000_000),
                Event::High(PinName::Csd),
                Event::DelayNs(FEM_CSD_SETTLE_MS * 1_000_000),
                Event::Low(PinName::Ctx),
            ]
        );
        assert!(owner.is_enabled());
        assert!(!power.is_low() && !csd.is_low() && ctx.is_low());
        assert!(!log.borrow().contains(&Event::High(PinName::Ctx)));
    }

    #[test]
    fn enable_failure_reasserts_every_safe_level_and_retains_owner() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let (power, power_probe) = MockOutput::new(PinName::Power, log.clone());
        let (mut csd, csd_probe) = MockOutput::new(PinName::Csd, log.clone());
        csd.fail_high = true;
        let (ctx, ctx_probe) = MockOutput::new(PinName::Ctx, log.clone());
        let owner = TrackerRxInterlock::new(power, csd, ctx).unwrap();
        log.borrow_mut().clear();

        let mut delay = ReadyDelay { log: log.clone() };
        let failure = match run_ready(owner.enable(&mut delay)) {
            Ok(_) => panic!("fault-injected enable succeeded"),
            Err(failure) => failure,
        };
        let (disabled, cause) = failure.into_parts();
        assert_eq!(cause, RxOnlyFemError::ChipEnable(MockError));
        assert!(power_probe.is_low() && csd_probe.is_low() && ctx_probe.is_low());
        assert!(!disabled.is_enabled());
        assert!(!log.borrow().contains(&Event::High(PinName::Ctx)));
    }

    #[test]
    fn cancelling_enable_future_is_fail_closed() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let (owner, power, csd, ctx) = new_interlock(log);
        let mut delay = PendingDelay;
        {
            let mut future = pin!(owner.enable(&mut delay));
            let waker = Waker::noop();
            let mut context = Context::from_waker(waker);

            assert!(future.as_mut().poll(&mut context).is_pending());
            assert!(!power.is_low());
        }

        assert!(power.is_low() && csd.is_low() && ctx.is_low());
    }

    #[test]
    fn context_readback_failure_disables_an_enabled_path() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let (power, power_probe) = MockOutput::new(PinName::Power, log.clone());
        let (csd, csd_probe) = MockOutput::new(PinName::Csd, log.clone());
        let (mut ctx, ctx_probe) = MockOutput::new(PinName::Ctx, log);
        ctx.report_not_low = true;
        let owner = TrackerRxInterlock::new(power, csd, ctx).unwrap();
        let mut delay = ReadyDelay {
            log: Rc::new(RefCell::new(Vec::new())),
        };

        let failure = match run_ready(owner.enable(&mut delay)) {
            Ok(_) => panic!("bad CTX readback was accepted"),
            Err(failure) => failure,
        };
        let (_, cause) = failure.into_parts();
        assert_eq!(cause, RxOnlyFemError::ContextNotLow);
        assert!(power_probe.is_low() && csd_probe.is_low() && ctx_probe.is_low());
    }
}
