//! Protected SPI fault hook for the Phase-1 returned-radio-fault HIL image.
//!
//! This crate implements one deliberately narrow test sequence. The wrapper
//! forwards every SPI transaction until it has successfully forwarded an
//! SX1262 `SetRx` command. It then rejects the first following
//! `GetIrqStatus` command without forwarding it to the physical SPI device.
//! All later transactions are forwarded, while the evidence remains sticky.
//!
//! The wrapper belongs below the board-owned receive-only opcode firewall and
//! above the real `ExclusiveDevice`. It has no configurable opcode, ordinal or
//! general-purpose fault-injection API.

#![no_std]
#![forbid(unsafe_code)]

use core::sync::atomic::{AtomicU8, Ordering};

use embedded_hal::spi::{Error as SpiError, ErrorKind as SpiErrorKind, ErrorType};
use embedded_hal_async::spi::{Operation, SpiDevice};

const SX1262_SET_RX: u8 = 0x82;
const SX1262_GET_IRQ_STATUS: u8 = 0x12;

/// Sticky state of the fixed returned-fault sequence in the current boot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ReturnedFaultState {
    /// No decorator has registered with this evidence object.
    NotConstructed = 0,
    /// The decorator is forwarding commands and waiting for a successful
    /// SX1262 `SetRx` transaction.
    WaitingForSetRx = 1,
    /// An allowed `SetRx` reached physical SPI successfully; the next
    /// `GetIrqStatus` will be rejected.
    ArmedAfterSetRx = 2,
    /// One `GetIrqStatus` was rejected before physical SPI.
    Fired = 3,
}

impl ReturnedFaultState {
    const fn from_encoded(encoded: u8) -> Self {
        match encoded {
            0 => Self::NotConstructed,
            1 => Self::WaitingForSetRx,
            2 => Self::ArmedAfterSetRx,
            3 => Self::Fired,
            // Only this crate can write the atomic. Treat an impossible value
            // as no affirmative injection evidence rather than claiming that
            // a protected fault fired.
            _ => Self::NotConstructed,
        }
    }
}

/// Copyable diagnostic view of the current boot's returned-fault evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReturnedFaultEvidenceSnapshot {
    state: ReturnedFaultState,
}

impl ReturnedFaultEvidenceSnapshot {
    /// Exact sticky state observed by this snapshot.
    pub const fn state(self) -> ReturnedFaultState {
        self.state
    }

    /// Whether an allowed `SetRx` transaction was physically forwarded.
    pub const fn set_rx_forwarded(self) -> bool {
        matches!(
            self.state,
            ReturnedFaultState::ArmedAfterSetRx | ReturnedFaultState::Fired
        )
    }

    /// Whether `GetIrqStatus` was rejected before physical SPI.
    pub const fn get_irq_status_rejected_before_spi(self) -> bool {
        matches!(self.state, ReturnedFaultState::Fired)
    }

    /// Whether the one-shot fault hook fired in this boot.
    pub const fn fired(self) -> bool {
        matches!(self.state, ReturnedFaultState::Fired)
    }
}

/// Boot-sticky evidence shared with the target fatal-fault path.
///
/// The target creates one static instance. A decorator transitions it
/// monotonically from [`ReturnedFaultState::WaitingForSetRx`] through
/// [`ReturnedFaultState::Fired`], and the fatal path reads a snapshot before
/// requesting the digital-core reset. This object is deliberately not retained
/// across resets; the reset-quarantine journal owns cross-reset correlation.
#[repr(transparent)]
pub struct ReturnedFaultEvidence {
    state: AtomicU8,
}

impl ReturnedFaultEvidence {
    /// Construct evidence for which no decorator has yet been built.
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(ReturnedFaultState::NotConstructed as u8),
        }
    }

    /// Read an acquire-ordered snapshot suitable for fatal-path diagnostics.
    pub fn snapshot(&self) -> ReturnedFaultEvidenceSnapshot {
        ReturnedFaultEvidenceSnapshot {
            state: ReturnedFaultState::from_encoded(self.state.load(Ordering::Acquire)),
        }
    }

    fn record(&self, state: ReturnedFaultState) {
        self.state.store(state as u8, Ordering::Release);
    }
}

impl Default for ReturnedFaultEvidence {
    fn default() -> Self {
        Self::new()
    }
}

const _: () = {
    assert!(core::mem::size_of::<ReturnedFaultEvidence>() == 1);
    assert!(core::mem::align_of::<ReturnedFaultEvidence>() == 1);
};

/// SPI error returned by the protected HIL decorator.
#[derive(Debug, Eq, PartialEq)]
pub enum ReturnedFaultSpiError<InnerError> {
    /// The physical SPI device returned an ordinary error.
    Inner(InnerError),
    /// The fixed one-shot hook rejected `GetIrqStatus` after a successful
    /// physical `SetRx` transaction.
    InjectedGetIrqStatusAfterSetRx,
}

impl<InnerError> SpiError for ReturnedFaultSpiError<InnerError>
where
    InnerError: SpiError,
{
    fn kind(&self) -> SpiErrorKind {
        match self {
            Self::Inner(error) => error.kind(),
            Self::InjectedGetIrqStatusAfterSetRx => SpiErrorKind::Other,
        }
    }
}

/// Fixed, one-shot SX1262 receive-fault decorator.
///
/// Construct this only in the separately named returned-fault HIL artifact,
/// below the board-owned receive-only opcode firewall. Construction arms the
/// evidence owner to wait for `SetRx`; it does not itself fail any operation.
pub struct ReturnedFaultSpiDevice<'evidence, Spi> {
    inner: Spi,
    evidence: &'evidence ReturnedFaultEvidence,
    state: ReturnedFaultState,
}

impl<'evidence, Spi> ReturnedFaultSpiDevice<'evidence, Spi> {
    /// Wrap the real SPI device with the single reviewed fault sequence.
    pub fn new(inner: Spi, evidence: &'evidence ReturnedFaultEvidence) -> Self {
        evidence.record(ReturnedFaultState::WaitingForSetRx);
        Self {
            inner,
            evidence,
            state: ReturnedFaultState::WaitingForSetRx,
        }
    }
}

impl<Spi> ErrorType for ReturnedFaultSpiDevice<'_, Spi>
where
    Spi: ErrorType,
    Spi::Error: SpiError,
{
    type Error = ReturnedFaultSpiError<Spi::Error>;
}

impl<Spi> SpiDevice<u8> for ReturnedFaultSpiDevice<'_, Spi>
where
    Spi: SpiDevice<u8>,
{
    async fn transaction(
        &mut self,
        operations: &mut [Operation<'_, u8>],
    ) -> Result<(), Self::Error> {
        let opcode = first_command_opcode(operations);
        if self.state == ReturnedFaultState::ArmedAfterSetRx
            && opcode == Some(SX1262_GET_IRQ_STATUS)
        {
            self.state = ReturnedFaultState::Fired;
            self.evidence.record(ReturnedFaultState::Fired);
            return Err(ReturnedFaultSpiError::InjectedGetIrqStatusAfterSetRx);
        }

        self.inner
            .transaction(operations)
            .await
            .map_err(ReturnedFaultSpiError::Inner)?;

        if self.state == ReturnedFaultState::WaitingForSetRx && opcode == Some(SX1262_SET_RX) {
            self.state = ReturnedFaultState::ArmedAfterSetRx;
            self.evidence.record(ReturnedFaultState::ArmedAfterSetRx);
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    extern crate std;

    use core::{
        cell::Cell,
        future::Future,
        pin::pin,
        task::{Context, Poll, Waker},
    };
    use std::{cell::RefCell, rc::Rc, vec, vec::Vec};

    use embedded_hal::spi::{ErrorKind, ErrorType};

    use super::*;

    const ORDINARY_OPCODE: u8 = 0x55;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct MockError(ErrorKind);

    impl SpiError for MockError {
        fn kind(&self) -> ErrorKind {
            self.0
        }
    }

    #[derive(Clone)]
    struct MockControl {
        commands: Rc<RefCell<Vec<Vec<u8>>>>,
        observed_states: Rc<RefCell<Vec<ReturnedFaultState>>>,
        fail_opcode: Rc<Cell<Option<u8>>>,
    }

    struct MockSpi<'evidence> {
        control: MockControl,
        evidence: Option<&'evidence ReturnedFaultEvidence>,
    }

    impl<'evidence> MockSpi<'evidence> {
        fn new(evidence: Option<&'evidence ReturnedFaultEvidence>) -> (Self, MockControl) {
            let control = MockControl {
                commands: Rc::new(RefCell::new(Vec::new())),
                observed_states: Rc::new(RefCell::new(Vec::new())),
                fail_opcode: Rc::new(Cell::new(None)),
            };
            (
                Self {
                    control: control.clone(),
                    evidence,
                },
                control,
            )
        }
    }

    impl ErrorType for MockSpi<'_> {
        type Error = MockError;
    }

    impl SpiDevice<u8> for MockSpi<'_> {
        async fn transaction(
            &mut self,
            operations: &mut [Operation<'_, u8>],
        ) -> Result<(), Self::Error> {
            if let Some(evidence) = self.evidence {
                self.control
                    .observed_states
                    .borrow_mut()
                    .push(evidence.snapshot().state());
            }
            let opcode = first_command_opcode(operations);
            let mut command = Vec::new();
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => command.extend_from_slice(bytes),
                    Operation::Transfer(_, bytes) => command.extend_from_slice(bytes),
                    Operation::TransferInPlace(bytes) => command.extend_from_slice(bytes),
                    Operation::Read(_) | Operation::DelayNs(_) => {}
                }
            }
            self.control.commands.borrow_mut().push(command);
            if opcode.is_some() && opcode == self.control.fail_opcode.get() {
                Err(MockError(ErrorKind::Overrun))
            } else {
                Ok(())
            }
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

    fn evidence_state(evidence: &ReturnedFaultEvidence) -> ReturnedFaultState {
        evidence.snapshot().state()
    }

    #[test]
    fn evidence_starts_unconstructed_and_constructor_waits_for_set_rx() {
        let evidence = ReturnedFaultEvidence::new();
        assert_eq!(
            evidence_state(&evidence),
            ReturnedFaultState::NotConstructed
        );
        let (spi, _) = MockSpi::new(Some(&evidence));
        let _device = ReturnedFaultSpiDevice::new(spi, &evidence);
        assert_eq!(
            evidence_state(&evidence),
            ReturnedFaultState::WaitingForSetRx
        );

        let snapshot = evidence.snapshot();
        assert!(!snapshot.set_rx_forwarded());
        assert!(!snapshot.get_irq_status_rejected_before_spi());
        assert!(!snapshot.fired());
    }

    #[test]
    fn opcode_parser_covers_every_write_bearing_operation_and_skips_noncommands() {
        let mut read = [0_u8; 2];
        let empty = [];
        let write = [ORDINARY_OPCODE, 1];
        let operations = [
            Operation::DelayNs(10),
            Operation::Read(&mut read),
            Operation::Write(&empty),
            Operation::Write(&write),
        ];
        assert_eq!(first_command_opcode(&operations), Some(ORDINARY_OPCODE));

        let mut transfer_read = [0_u8; 2];
        let transfer_write = [SX1262_SET_RX, 0xff];
        let operations = [Operation::Transfer(&mut transfer_read, &transfer_write)];
        assert_eq!(first_command_opcode(&operations), Some(SX1262_SET_RX));

        let mut transfer_in_place = [SX1262_GET_IRQ_STATUS, 0];
        let operations = [Operation::TransferInPlace(&mut transfer_in_place)];
        assert_eq!(
            first_command_opcode(&operations),
            Some(SX1262_GET_IRQ_STATUS)
        );

        let mut read_only = [0_u8; 1];
        let operations = [Operation::DelayNs(1), Operation::Read(&mut read_only)];
        assert_eq!(first_command_opcode(&operations), None);
    }

    #[test]
    fn ordinary_operations_forward_without_changing_state() {
        let evidence = ReturnedFaultEvidence::new();
        let (spi, control) = MockSpi::new(Some(&evidence));
        let mut device = ReturnedFaultSpiDevice::new(spi, &evidence);

        run_ready(device.write(&[ORDINARY_OPCODE, 1, 2])).unwrap();

        assert_eq!(
            control.commands.borrow().as_slice(),
            &[vec![ORDINARY_OPCODE, 1, 2]]
        );
        assert_eq!(
            evidence_state(&evidence),
            ReturnedFaultState::WaitingForSetRx
        );
    }

    #[test]
    fn failed_set_rx_is_an_inner_error_and_does_not_arm() {
        let evidence = ReturnedFaultEvidence::new();
        let (spi, control) = MockSpi::new(Some(&evidence));
        control.fail_opcode.set(Some(SX1262_SET_RX));
        let mut device = ReturnedFaultSpiDevice::new(spi, &evidence);

        assert_eq!(
            run_ready(device.write(&[SX1262_SET_RX, 0xff, 0xff, 0xff])),
            Err(ReturnedFaultSpiError::Inner(MockError(ErrorKind::Overrun)))
        );
        assert_eq!(
            evidence_state(&evidence),
            ReturnedFaultState::WaitingForSetRx
        );

        control.fail_opcode.set(None);
        run_ready(device.write(&[SX1262_GET_IRQ_STATUS])).unwrap();
        assert_eq!(
            control.commands.borrow().as_slice(),
            &[
                vec![SX1262_SET_RX, 0xff, 0xff, 0xff],
                vec![SX1262_GET_IRQ_STATUS],
            ]
        );
        assert_eq!(
            evidence_state(&evidence),
            ReturnedFaultState::WaitingForSetRx
        );
    }

    #[test]
    fn set_rx_arms_only_after_the_inner_transaction_succeeds() {
        let evidence = ReturnedFaultEvidence::new();
        let (spi, control) = MockSpi::new(Some(&evidence));
        let mut device = ReturnedFaultSpiDevice::new(spi, &evidence);

        run_ready(device.write(&[SX1262_SET_RX, 0xff, 0xff, 0xff])).unwrap();

        assert_eq!(
            control.observed_states.borrow().as_slice(),
            &[ReturnedFaultState::WaitingForSetRx]
        );
        assert_eq!(
            evidence_state(&evidence),
            ReturnedFaultState::ArmedAfterSetRx
        );
        assert!(evidence.snapshot().set_rx_forwarded());
        assert!(!evidence.snapshot().fired());
    }

    #[test]
    fn armed_decorator_forwards_unrelated_transactions_and_remains_armed() {
        let evidence = ReturnedFaultEvidence::new();
        let (spi, control) = MockSpi::new(Some(&evidence));
        let mut device = ReturnedFaultSpiDevice::new(spi, &evidence);
        run_ready(device.write(&[SX1262_SET_RX])).unwrap();

        run_ready(device.write(&[ORDINARY_OPCODE])).unwrap();

        assert_eq!(
            control.commands.borrow().as_slice(),
            &[vec![SX1262_SET_RX], vec![ORDINARY_OPCODE]]
        );
        assert_eq!(
            evidence_state(&evidence),
            ReturnedFaultState::ArmedAfterSetRx
        );
    }

    #[test]
    fn first_get_irq_status_after_set_rx_fires_before_physical_spi() {
        let evidence = ReturnedFaultEvidence::new();
        let (spi, control) = MockSpi::new(Some(&evidence));
        let mut device = ReturnedFaultSpiDevice::new(spi, &evidence);
        run_ready(device.write(&[SX1262_SET_RX, 0xff, 0xff, 0xff])).unwrap();
        let physical_transactions_before_fault = control.commands.borrow().len();

        assert_eq!(
            run_ready(device.write(&[SX1262_GET_IRQ_STATUS])),
            Err(ReturnedFaultSpiError::InjectedGetIrqStatusAfterSetRx)
        );

        assert_eq!(
            control.commands.borrow().len(),
            physical_transactions_before_fault,
            "injected GetIrqStatus reached the physical SPI device"
        );
        let snapshot = evidence.snapshot();
        assert_eq!(snapshot.state(), ReturnedFaultState::Fired);
        assert!(snapshot.set_rx_forwarded());
        assert!(snapshot.get_irq_status_rejected_before_spi());
        assert!(snapshot.fired());
    }

    #[test]
    fn fired_state_is_sticky_but_later_transactions_are_forwarded() {
        let evidence = ReturnedFaultEvidence::new();
        let (spi, control) = MockSpi::new(Some(&evidence));
        let mut device = ReturnedFaultSpiDevice::new(spi, &evidence);
        run_ready(device.write(&[SX1262_SET_RX])).unwrap();
        assert_eq!(
            run_ready(device.write(&[SX1262_GET_IRQ_STATUS])),
            Err(ReturnedFaultSpiError::InjectedGetIrqStatusAfterSetRx)
        );

        run_ready(device.write(&[SX1262_GET_IRQ_STATUS])).unwrap();
        run_ready(device.write(&[SX1262_SET_RX])).unwrap();

        assert_eq!(
            control.commands.borrow().as_slice(),
            &[
                vec![SX1262_SET_RX],
                vec![SX1262_GET_IRQ_STATUS],
                vec![SX1262_SET_RX],
            ]
        );
        assert_eq!(evidence_state(&evidence), ReturnedFaultState::Fired);
    }

    #[test]
    fn transfer_variants_participate_in_the_same_sequence() {
        let evidence = ReturnedFaultEvidence::new();
        let (spi, control) = MockSpi::new(Some(&evidence));
        let mut device = ReturnedFaultSpiDevice::new(spi, &evidence);
        let set_rx_write = [SX1262_SET_RX, 0xff];
        let mut set_rx_read = [0_u8; 2];
        let mut set_rx = [Operation::Transfer(&mut set_rx_read, &set_rx_write)];
        run_ready(device.transaction(&mut set_rx)).unwrap();
        assert_eq!(
            evidence_state(&evidence),
            ReturnedFaultState::ArmedAfterSetRx
        );

        let mut get_irq = [SX1262_GET_IRQ_STATUS, 0];
        let mut operations = [Operation::TransferInPlace(&mut get_irq)];
        assert_eq!(
            run_ready(device.transaction(&mut operations)),
            Err(ReturnedFaultSpiError::InjectedGetIrqStatusAfterSetRx)
        );
        assert_eq!(
            control.commands.borrow().as_slice(),
            &[vec![SX1262_SET_RX, 0xff]]
        );
    }

    #[test]
    fn error_kind_preserves_inner_failures_and_classifies_injection_as_other() {
        let inner = ReturnedFaultSpiError::Inner(MockError(ErrorKind::ModeFault));
        let injected: ReturnedFaultSpiError<MockError> =
            ReturnedFaultSpiError::InjectedGetIrqStatusAfterSetRx;

        assert_eq!(inner.kind(), ErrorKind::ModeFault);
        assert_eq!(injected.kind(), ErrorKind::Other);
    }
}
