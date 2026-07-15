//! Allocation-free diagnostics shared by receive-only radio owners.

use crate::FrameSignal;

/// Coarse fault class used for stable counters across radio implementations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadioRxFaultClass {
    /// SPI command or bus failure.
    Spi,
    /// Radio reset-pin or reset-sequence failure.
    Reset,
    /// Radio-integrated RF-switch transition failure.
    RfSwitch,
    /// BUSY pin or ready-wait failure.
    Busy,
    /// IRQ status processing or interrupt-pin failure.
    Interrupt,
    /// Static modulation, mode or frequency configuration failure.
    Configuration,
    /// Radio-reported operational error.
    Operation,
    /// Payload length or buffer contract failure.
    Payload,
    /// Receive or transmit timeout reported by the driver.
    Timeout,
    /// Requested radio capability is unsupported.
    Unsupported,
    /// External front-end pin or state failure.
    Fem,
    /// An operation was attempted after the opaque owner had faulted.
    Faulted,
}

/// Stable phase in which a receive-only radio fault was observed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadioRxFaultPhase {
    /// Construction and initial receive-path configuration.
    Initialization,
    /// Awaiting or collecting a physical receive frame.
    Receive,
    /// Explicit receive-path teardown.
    Shutdown,
}

/// One primary fault class and an optional cleanup failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioRxFaultClassification {
    /// Class of the operation that originally failed.
    pub primary: RadioRxFaultClass,
    /// Independent failure encountered while containing the primary fault.
    pub cleanup: Option<RadioRxFaultClass>,
}

impl RadioRxFaultClassification {
    /// Classify a fault with no independently observed cleanup failure.
    pub const fn single(primary: RadioRxFaultClass) -> Self {
        Self {
            primary,
            cleanup: None,
        }
    }

    /// Preserve both the initiating fault and a separate cleanup failure.
    pub const fn with_cleanup(primary: RadioRxFaultClass, cleanup: RadioRxFaultClass) -> Self {
        Self {
            primary,
            cleanup: Some(cleanup),
        }
    }
}

/// Saturating per-class fault counters.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RadioRxFaultCounters {
    pub spi: u64,
    pub reset: u64,
    pub rf_switch: u64,
    pub busy: u64,
    pub interrupt: u64,
    pub configuration: u64,
    pub operation: u64,
    pub payload: u64,
    pub timeout: u64,
    pub unsupported: u64,
    pub fem: u64,
    pub faulted: u64,
}

impl RadioRxFaultCounters {
    fn record(&mut self, class: RadioRxFaultClass) {
        let counter = match class {
            RadioRxFaultClass::Spi => &mut self.spi,
            RadioRxFaultClass::Reset => &mut self.reset,
            RadioRxFaultClass::RfSwitch => &mut self.rf_switch,
            RadioRxFaultClass::Busy => &mut self.busy,
            RadioRxFaultClass::Interrupt => &mut self.interrupt,
            RadioRxFaultClass::Configuration => &mut self.configuration,
            RadioRxFaultClass::Operation => &mut self.operation,
            RadioRxFaultClass::Payload => &mut self.payload,
            RadioRxFaultClass::Timeout => &mut self.timeout,
            RadioRxFaultClass::Unsupported => &mut self.unsupported,
            RadioRxFaultClass::Fem => &mut self.fem,
            RadioRxFaultClass::Faulted => &mut self.faulted,
        };
        *counter = counter.saturating_add(1);
    }
}

/// Fixed-size receive-radio diagnostics with a target-specific last fault.
///
/// This snapshot retains no frame bytes and performs no allocation. Counters
/// saturate instead of wrapping. A compound radio-plus-cleanup failure counts
/// as one fault occurrence while incrementing both applicable class counters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioRxDiagnostics<Fault> {
    pub initialization_attempts: u64,
    pub successful_initializations: u64,
    pub reinitialization_attempts: u64,
    pub frames_received: u64,
    pub bytes_received: u64,
    pub last_frame_len: u16,
    pub last_signal: Option<FrameSignal>,
    pub last_received_at_ticks: Option<u64>,
    pub faults: u64,
    pub initialization_faults: u64,
    pub receive_faults: u64,
    pub shutdown_faults: u64,
    pub fault_classes: RadioRxFaultCounters,
    pub system_reset_requests: u64,
    pub last_fault_phase: Option<RadioRxFaultPhase>,
    pub last_fault_classification: Option<RadioRxFaultClassification>,
    pub last_fault: Option<Fault>,
}

impl<Fault> RadioRxDiagnostics<Fault> {
    /// Construct an empty diagnostic snapshot.
    pub const fn new() -> Self {
        Self {
            initialization_attempts: 0,
            successful_initializations: 0,
            reinitialization_attempts: 0,
            frames_received: 0,
            bytes_received: 0,
            last_frame_len: 0,
            last_signal: None,
            last_received_at_ticks: None,
            faults: 0,
            initialization_faults: 0,
            receive_faults: 0,
            shutdown_faults: 0,
            fault_classes: RadioRxFaultCounters {
                spi: 0,
                reset: 0,
                rf_switch: 0,
                busy: 0,
                interrupt: 0,
                configuration: 0,
                operation: 0,
                payload: 0,
                timeout: 0,
                unsupported: 0,
                fem: 0,
                faulted: 0,
            },
            system_reset_requests: 0,
            last_fault_phase: None,
            last_fault_classification: None,
            last_fault: None,
        }
    }

    /// Record the start of an initial or in-process reinitialization attempt.
    pub fn record_initialization_attempt(&mut self, reinitialization: bool) {
        self.initialization_attempts = self.initialization_attempts.saturating_add(1);
        if reinitialization {
            self.reinitialization_attempts = self.reinitialization_attempts.saturating_add(1);
        }
    }

    /// Record a fully configured receive path.
    pub fn record_initialized(&mut self) {
        self.successful_initializations = self.successful_initializations.saturating_add(1);
    }

    /// Record one complete physical frame and its radio metadata.
    pub fn record_frame(&mut self, frame_len: u8, signal: FrameSignal, received_at_ticks: u64) {
        self.frames_received = self.frames_received.saturating_add(1);
        self.bytes_received = self.bytes_received.saturating_add(u64::from(frame_len));
        self.last_frame_len = u16::from(frame_len);
        self.last_signal = Some(signal);
        self.last_received_at_ticks = Some(received_at_ticks);
    }

    /// Record one typed operation failure and any independent cleanup fault.
    pub fn record_fault(
        &mut self,
        phase: RadioRxFaultPhase,
        classification: RadioRxFaultClassification,
        fault: Fault,
    ) {
        self.faults = self.faults.saturating_add(1);
        let phase_counter = match phase {
            RadioRxFaultPhase::Initialization => &mut self.initialization_faults,
            RadioRxFaultPhase::Receive => &mut self.receive_faults,
            RadioRxFaultPhase::Shutdown => &mut self.shutdown_faults,
        };
        *phase_counter = phase_counter.saturating_add(1);
        self.fault_classes.record(classification.primary);
        if let Some(cleanup) = classification.cleanup
            && cleanup != classification.primary
        {
            self.fault_classes.record(cleanup);
        }
        self.last_fault_phase = Some(phase);
        self.last_fault_classification = Some(classification);
        self.last_fault = Some(fault);
    }

    /// Record the explicit decision to recover through a target reset primitive.
    pub fn record_system_reset_request(&mut self) {
        self.system_reset_requests = self.system_reset_requests.saturating_add(1);
    }
}

impl<Fault> Default for RadioRxDiagnostics<Fault> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum TestFault {
        ReceiveAndCleanup,
    }

    #[test]
    fn records_frames_and_compound_fault_without_payload_retention() {
        let mut diagnostics = RadioRxDiagnostics::new();
        diagnostics.record_initialization_attempt(false);
        diagnostics.record_initialized();
        diagnostics.record_frame(255, FrameSignal::new(-101, -7), 1234);
        diagnostics.record_fault(
            RadioRxFaultPhase::Receive,
            RadioRxFaultClassification::with_cleanup(
                RadioRxFaultClass::Spi,
                RadioRxFaultClass::Fem,
            ),
            TestFault::ReceiveAndCleanup,
        );
        diagnostics.record_system_reset_request();

        assert_eq!(diagnostics.initialization_attempts, 1);
        assert_eq!(diagnostics.successful_initializations, 1);
        assert_eq!(diagnostics.reinitialization_attempts, 0);
        assert_eq!(diagnostics.frames_received, 1);
        assert_eq!(diagnostics.bytes_received, 255);
        assert_eq!(diagnostics.last_frame_len, 255);
        assert_eq!(diagnostics.last_signal, Some(FrameSignal::new(-101, -7)));
        assert_eq!(diagnostics.last_received_at_ticks, Some(1234));
        assert_eq!(diagnostics.faults, 1);
        assert_eq!(diagnostics.receive_faults, 1);
        assert_eq!(diagnostics.fault_classes.spi, 1);
        assert_eq!(diagnostics.fault_classes.fem, 1);
        assert_eq!(diagnostics.system_reset_requests, 1);
        assert_eq!(diagnostics.last_fault, Some(TestFault::ReceiveAndCleanup));
    }

    #[test]
    fn all_counters_saturate_and_duplicate_classes_count_once() {
        let mut diagnostics = RadioRxDiagnostics::new();
        diagnostics.initialization_attempts = u64::MAX;
        diagnostics.successful_initializations = u64::MAX;
        diagnostics.reinitialization_attempts = u64::MAX;
        diagnostics.frames_received = u64::MAX;
        diagnostics.bytes_received = u64::MAX;
        diagnostics.faults = u64::MAX;
        diagnostics.initialization_faults = u64::MAX;
        diagnostics.fault_classes.timeout = u64::MAX;
        diagnostics.system_reset_requests = u64::MAX;

        diagnostics.record_initialization_attempt(true);
        diagnostics.record_initialized();
        diagnostics.record_frame(255, FrameSignal::new(-1, -1), u64::MAX);
        diagnostics.record_fault(
            RadioRxFaultPhase::Initialization,
            RadioRxFaultClassification::with_cleanup(
                RadioRxFaultClass::Timeout,
                RadioRxFaultClass::Timeout,
            ),
            TestFault::ReceiveAndCleanup,
        );
        diagnostics.record_system_reset_request();

        assert_eq!(diagnostics.initialization_attempts, u64::MAX);
        assert_eq!(diagnostics.successful_initializations, u64::MAX);
        assert_eq!(diagnostics.reinitialization_attempts, u64::MAX);
        assert_eq!(diagnostics.frames_received, u64::MAX);
        assert_eq!(diagnostics.bytes_received, u64::MAX);
        assert_eq!(diagnostics.faults, u64::MAX);
        assert_eq!(diagnostics.initialization_faults, u64::MAX);
        assert_eq!(diagnostics.fault_classes.timeout, u64::MAX);
        assert_eq!(diagnostics.system_reset_requests, u64::MAX);
    }
}
