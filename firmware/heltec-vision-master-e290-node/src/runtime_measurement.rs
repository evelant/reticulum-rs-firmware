//! Fixed-layout runtime timing and memory evidence for the E290 HIL image.
//!
//! The evidence object is deliberately allocation-free and uses only safe
//! atomics. A debugger can decode its 64 little-endian words from the unique
//! retained symbol without executing target code. Multiword observations are
//! bracketed by leading and trailing odd/even sequence markers; each scalar is
//! independently atomic. The sequence protocol assumes the product's current
//! single-writer, cooperative execution model and is not a multi-writer lock.

use core::sync::atomic::{AtomicU32, Ordering};

/// Four-byte evidence marker, stored in memory as the ASCII bytes `RTME`.
pub const RUNTIME_MEASUREMENT_EVIDENCE_MAGIC: u32 = u32::from_le_bytes(*b"RTME");

/// Version of the debugger-visible runtime-measurement ABI.
pub const RUNTIME_MEASUREMENT_EVIDENCE_VERSION: u32 = 1;

/// Exact number of 32-bit words in [`RuntimeMeasurementEvidence`] version 1.
pub const RUNTIME_MEASUREMENT_EVIDENCE_WORDS: u32 = 64;

/// Exact byte size of [`RuntimeMeasurementEvidence`] version 1.
pub const RUNTIME_MEASUREMENT_EVIDENCE_SIZE: u32 = 256;

/// Evidence collection is active in this image.
pub const RUNTIME_MEASUREMENT_FLAG_ACTIVE: u32 = 1 << 0;
/// At least one aggregate stack scan has initialized the stack fields.
pub const RUNTIME_MEASUREMENT_FLAG_STACK_INITIALIZED: u32 = 1 << 1;
/// At least one heap snapshot has initialized the heap minimum fields.
pub const RUNTIME_MEASUREMENT_FLAG_HEAP_REGISTERED: u32 = 1 << 2;
/// Product composition reached its ready boundary.
pub const RUNTIME_MEASUREMENT_FLAG_COMPOSITION_READY: u32 = 1 << 3;
/// Every aggregate stack scan observed so far was valid.
pub const RUNTIME_MEASUREMENT_FLAG_SCAN_VALID: u32 = 1 << 4;
/// Every aggregate stack scan observed so far found an intact guard.
pub const RUNTIME_MEASUREMENT_FLAG_GUARD_INTACT: u32 = 1 << 5;
/// At least one input or counter could not be represented exactly in 32 bits.
pub const RUNTIME_MEASUREMENT_FLAG_SATURATED: u32 = 1 << 6;

/// Return whether one dump contains a complete, stable snapshot.
///
/// Matching even leading and trailing markers mean no observation was in
/// progress while the ordered 256-byte evidence object was copied.
pub const fn runtime_measurement_snapshot_is_stable(begin: u32, end: u32) -> bool {
    begin == end && begin & 1 == 0
}

/// Stable boot-phase identifiers for the eight ABI timing pairs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum BootPhase {
    /// Credential-store mount and recovery.
    CredentialBoot = 0,
    /// Identity inspection before any identity mutation.
    IdentityPreflight = 1,
    /// Journal provision or erased-media validation.
    JournalProvision = 2,
    /// Durable announce-epoch reservation.
    AnnounceEpoch = 3,
    /// Identity load or first provisioning.
    IdentityBoot = 4,
    /// Strict journal mount and boot recovery.
    JournalMount = 5,
    /// Durable inbound-store mount.
    InboxMount = 6,
    /// SX1262 construction and initialization.
    RadioInit = 7,
}

/// Stable operation identifiers for the ABI operation aggregates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum OperationKind {
    /// Durable inbound inbox admission.
    Inbound = 0,
    /// Authorized-frame projection and durable handling.
    AuthorizedFrame = 1,
    /// Submission-runtime physical drive.
    Submission = 2,
    /// Authenticated API dispatch.
    ApiDispatch = 3,
    /// One bounded receive operation.
    Receive = 4,
    /// One channel-activity-detection operation.
    Cad = 5,
    /// One transmit operation.
    Transmit = 6,
}

/// One allocator/heap observation supplied by the target allocation hook.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeapSnapshot {
    /// Total registered heap bytes.
    pub total_bytes: u64,
    /// Current bytes allocated across all registered regions.
    pub current_bytes: u64,
    /// Maximum bytes allocated across all registered regions.
    pub maximum_bytes: u64,
    /// Current free bytes across all registered regions.
    pub free_bytes: u64,
    /// Current bytes allocated from internal RAM.
    pub internal_current_bytes: u64,
    /// Current free bytes in internal RAM.
    pub internal_free_bytes: u64,
    /// Current bytes allocated from external RAM.
    pub external_current_bytes: u64,
    /// Current free bytes in external RAM.
    pub external_free_bytes: u64,
}

/// One aggregate shared-executor stack observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StackSnapshot {
    /// Bytes reserved by the linker for the measured stack region.
    pub reserved_bytes: u64,
    /// Bytes available after guard and scanner exclusions.
    pub usable_bytes: u64,
    /// Bytes initially painted by the measurement fixture.
    pub painted_bytes: u64,
    /// Largest observed use of the painted region.
    pub high_water_bytes: u64,
    /// Remaining unused bytes in the current scan.
    pub remaining_bytes: u64,
    /// Guard offset from the start of the linked stack reservation.
    pub guard_offset_bytes: u64,
    /// Whether this scan produced a structurally valid result.
    pub scan_valid: bool,
    /// Whether this scan found the stack guard intact.
    pub guard_intact: bool,
}

/// Exact 256-byte debugger-visible runtime measurement ABI.
///
/// The field order is the ABI. Word 0 is the leading sequence marker, words 1
/// through 5 are the header and initialization state, word 6 is uptime, words
/// 7 through 22 are memory and composition snapshots, words 23 through 38 are
/// eight boot last/max pairs, words 39 through 59 are operation and scheduler
/// aggregates, words 60 through 62 are counters, and word 63 is the trailing
/// sequence marker. Minima use `u32::MAX` as the not-yet-observed sentinel.
#[repr(C)]
pub struct RuntimeMeasurementEvidence {
    snapshot_seq_begin: AtomicU32,
    magic: u32,
    version: u32,
    size: u32,
    flags: AtomicU32,
    init_error: AtomicU32,
    uptime_ms: AtomicU32,
    psram_bytes: AtomicU32,
    heap_total_bytes: AtomicU32,
    heap_current_bytes: AtomicU32,
    heap_maximum_bytes: AtomicU32,
    heap_minimum_free_bytes: AtomicU32,
    internal_heap_current_bytes: AtomicU32,
    internal_heap_minimum_free_bytes: AtomicU32,
    external_heap_current_bytes: AtomicU32,
    external_heap_minimum_free_bytes: AtomicU32,
    stack_reserved_bytes: AtomicU32,
    stack_usable_bytes: AtomicU32,
    stack_painted_bytes: AtomicU32,
    stack_high_water_bytes: AtomicU32,
    stack_minimum_remaining_bytes: AtomicU32,
    stack_guard_offset_bytes: AtomicU32,
    composition_ready_us: AtomicU32,
    credential_boot_last_us: AtomicU32,
    credential_boot_max_us: AtomicU32,
    identity_preflight_last_us: AtomicU32,
    identity_preflight_max_us: AtomicU32,
    journal_provision_last_us: AtomicU32,
    journal_provision_max_us: AtomicU32,
    announce_epoch_last_us: AtomicU32,
    announce_epoch_max_us: AtomicU32,
    identity_boot_last_us: AtomicU32,
    identity_boot_max_us: AtomicU32,
    journal_mount_last_us: AtomicU32,
    journal_mount_max_us: AtomicU32,
    inbox_mount_last_us: AtomicU32,
    inbox_mount_max_us: AtomicU32,
    radio_init_last_us: AtomicU32,
    radio_init_max_us: AtomicU32,
    inbound_count: AtomicU32,
    inbound_max_us: AtomicU32,
    authorized_frame_count: AtomicU32,
    authorized_frame_max_us: AtomicU32,
    submission_count: AtomicU32,
    submission_max_us: AtomicU32,
    api_dispatch_count: AtomicU32,
    api_dispatch_max_us: AtomicU32,
    rx_count: AtomicU32,
    rx_max_us: AtomicU32,
    rx_timeout_count: AtomicU32,
    cad_count: AtomicU32,
    cad_max_us: AtomicU32,
    cad_timeout_count: AtomicU32,
    tx_count: AtomicU32,
    tx_max_us: AtomicU32,
    tx_timeout_count: AtomicU32,
    node_loop_gap_max_us: AtomicU32,
    radio_loop_gap_max_us: AtomicU32,
    measurement_lateness_max_us: AtomicU32,
    measurement_work_max_us: AtomicU32,
    unexpected_error_count: AtomicU32,
    allocation_count: AtomicU32,
    failed_allocation_count: AtomicU32,
    snapshot_seq_end: AtomicU32,
}

impl RuntimeMeasurementEvidence {
    /// Construct zeroed version-1 evidence with active collection and empty minima.
    pub const fn new() -> Self {
        Self {
            snapshot_seq_begin: AtomicU32::new(0),
            magic: RUNTIME_MEASUREMENT_EVIDENCE_MAGIC,
            version: RUNTIME_MEASUREMENT_EVIDENCE_VERSION,
            size: RUNTIME_MEASUREMENT_EVIDENCE_SIZE,
            flags: AtomicU32::new(RUNTIME_MEASUREMENT_FLAG_ACTIVE),
            init_error: AtomicU32::new(0),
            uptime_ms: AtomicU32::new(0),
            psram_bytes: AtomicU32::new(0),
            heap_total_bytes: AtomicU32::new(0),
            heap_current_bytes: AtomicU32::new(0),
            heap_maximum_bytes: AtomicU32::new(0),
            heap_minimum_free_bytes: AtomicU32::new(u32::MAX),
            internal_heap_current_bytes: AtomicU32::new(0),
            internal_heap_minimum_free_bytes: AtomicU32::new(u32::MAX),
            external_heap_current_bytes: AtomicU32::new(0),
            external_heap_minimum_free_bytes: AtomicU32::new(u32::MAX),
            stack_reserved_bytes: AtomicU32::new(0),
            stack_usable_bytes: AtomicU32::new(0),
            stack_painted_bytes: AtomicU32::new(0),
            stack_high_water_bytes: AtomicU32::new(0),
            stack_minimum_remaining_bytes: AtomicU32::new(u32::MAX),
            stack_guard_offset_bytes: AtomicU32::new(0),
            composition_ready_us: AtomicU32::new(0),
            credential_boot_last_us: AtomicU32::new(0),
            credential_boot_max_us: AtomicU32::new(0),
            identity_preflight_last_us: AtomicU32::new(0),
            identity_preflight_max_us: AtomicU32::new(0),
            journal_provision_last_us: AtomicU32::new(0),
            journal_provision_max_us: AtomicU32::new(0),
            announce_epoch_last_us: AtomicU32::new(0),
            announce_epoch_max_us: AtomicU32::new(0),
            identity_boot_last_us: AtomicU32::new(0),
            identity_boot_max_us: AtomicU32::new(0),
            journal_mount_last_us: AtomicU32::new(0),
            journal_mount_max_us: AtomicU32::new(0),
            inbox_mount_last_us: AtomicU32::new(0),
            inbox_mount_max_us: AtomicU32::new(0),
            radio_init_last_us: AtomicU32::new(0),
            radio_init_max_us: AtomicU32::new(0),
            inbound_count: AtomicU32::new(0),
            inbound_max_us: AtomicU32::new(0),
            authorized_frame_count: AtomicU32::new(0),
            authorized_frame_max_us: AtomicU32::new(0),
            submission_count: AtomicU32::new(0),
            submission_max_us: AtomicU32::new(0),
            api_dispatch_count: AtomicU32::new(0),
            api_dispatch_max_us: AtomicU32::new(0),
            rx_count: AtomicU32::new(0),
            rx_max_us: AtomicU32::new(0),
            rx_timeout_count: AtomicU32::new(0),
            cad_count: AtomicU32::new(0),
            cad_max_us: AtomicU32::new(0),
            cad_timeout_count: AtomicU32::new(0),
            tx_count: AtomicU32::new(0),
            tx_max_us: AtomicU32::new(0),
            tx_timeout_count: AtomicU32::new(0),
            node_loop_gap_max_us: AtomicU32::new(0),
            radio_loop_gap_max_us: AtomicU32::new(0),
            measurement_lateness_max_us: AtomicU32::new(0),
            measurement_work_max_us: AtomicU32::new(0),
            unexpected_error_count: AtomicU32::new(0),
            allocation_count: AtomicU32::new(0),
            failed_allocation_count: AtomicU32::new(0),
            snapshot_seq_end: AtomicU32::new(0),
        }
    }

    /// Record the first nonzero product-defined initialization error code.
    pub fn record_initialization_error(&self, error_code: u32) {
        if error_code != 0 {
            self.begin_sample();
            let _ = self.init_error.compare_exchange(
                0,
                error_code,
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
            self.finish_sample();
        }
    }

    /// Publish the latest measurement uptime in milliseconds.
    pub fn record_uptime_ms(&self, uptime_ms: u64) {
        self.begin_sample();
        let uptime_ms = self.clamp(uptime_ms);
        update_max(&self.uptime_ms, uptime_ms);
        self.finish_sample();
    }

    /// Publish the detected external PSRAM capacity.
    pub fn record_psram_bytes(&self, bytes: u64) {
        self.begin_sample();
        let bytes = self.clamp(bytes);
        self.psram_bytes.store(bytes, Ordering::Relaxed);
        self.finish_sample();
    }

    /// Publish current heap state and update all minimum-free observations.
    pub fn record_heap_snapshot(&self, snapshot: HeapSnapshot) {
        self.begin_sample();
        self.heap_total_bytes
            .store(self.clamp(snapshot.total_bytes), Ordering::Relaxed);
        self.heap_current_bytes
            .store(self.clamp(snapshot.current_bytes), Ordering::Relaxed);
        update_max(&self.heap_maximum_bytes, self.clamp(snapshot.maximum_bytes));
        update_min(
            &self.heap_minimum_free_bytes,
            self.clamp(snapshot.free_bytes),
        );
        self.internal_heap_current_bytes.store(
            self.clamp(snapshot.internal_current_bytes),
            Ordering::Relaxed,
        );
        update_min(
            &self.internal_heap_minimum_free_bytes,
            self.clamp(snapshot.internal_free_bytes),
        );
        self.external_heap_current_bytes.store(
            self.clamp(snapshot.external_current_bytes),
            Ordering::Relaxed,
        );
        update_min(
            &self.external_heap_minimum_free_bytes,
            self.clamp(snapshot.external_free_bytes),
        );
        self.flags
            .fetch_or(RUNTIME_MEASUREMENT_FLAG_HEAP_REGISTERED, Ordering::Relaxed);
        self.finish_sample();
    }

    /// Publish a stack scan and update sticky validity and minimum margin.
    pub fn record_stack_snapshot(&self, snapshot: StackSnapshot) {
        self.begin_sample();
        self.stack_reserved_bytes
            .store(self.clamp(snapshot.reserved_bytes), Ordering::Relaxed);
        self.stack_usable_bytes
            .store(self.clamp(snapshot.usable_bytes), Ordering::Relaxed);
        self.stack_painted_bytes
            .store(self.clamp(snapshot.painted_bytes), Ordering::Relaxed);
        update_max(
            &self.stack_high_water_bytes,
            self.clamp(snapshot.high_water_bytes),
        );
        update_min(
            &self.stack_minimum_remaining_bytes,
            self.clamp(snapshot.remaining_bytes),
        );
        self.stack_guard_offset_bytes
            .store(self.clamp(snapshot.guard_offset_bytes), Ordering::Relaxed);

        let previous = self.flags.fetch_or(
            RUNTIME_MEASUREMENT_FLAG_STACK_INITIALIZED,
            Ordering::Relaxed,
        );
        let first = previous & RUNTIME_MEASUREMENT_FLAG_STACK_INITIALIZED == 0;
        self.update_sticky_truth_flag(
            RUNTIME_MEASUREMENT_FLAG_SCAN_VALID,
            snapshot.scan_valid,
            first,
        );
        self.update_sticky_truth_flag(
            RUNTIME_MEASUREMENT_FLAG_GUARD_INTACT,
            snapshot.guard_intact,
            first,
        );
        self.finish_sample();
    }

    /// Record elapsed composition time and publish the ready flag.
    pub fn record_composition_ready(&self, elapsed_us: u64) {
        self.begin_sample();
        let elapsed_us = self.clamp(elapsed_us);
        self.composition_ready_us
            .store(elapsed_us, Ordering::Relaxed);
        self.flags.fetch_or(
            RUNTIME_MEASUREMENT_FLAG_COMPOSITION_READY,
            Ordering::Relaxed,
        );
        self.finish_sample();
    }

    /// Record the last and maximum duration for one stable boot phase.
    pub fn record_boot_phase(&self, phase: BootPhase, elapsed_us: u64) {
        self.begin_sample();
        let elapsed_us = self.clamp(elapsed_us);
        let (last, maximum) = self.boot_phase_fields(phase);
        last.store(elapsed_us, Ordering::Relaxed);
        update_max(maximum, elapsed_us);
        self.finish_sample();
    }

    /// Record one completed operation and update its saturating aggregates.
    pub fn record_operation(&self, operation: OperationKind, elapsed_us: u64) {
        self.begin_sample();
        let elapsed_us = self.clamp(elapsed_us);
        let (count, maximum) = self.operation_fields(operation);
        self.increment(count);
        update_max(maximum, elapsed_us);
        self.finish_sample();
    }

    /// Record one RX, CAD, or TX timeout.
    ///
    /// Passing a non-radio operation records an unexpected evidence error
    /// instead of silently assigning the timeout to an unrelated word.
    pub fn record_radio_timeout(&self, operation: OperationKind) {
        self.begin_sample();
        match operation {
            OperationKind::Receive => self.increment(&self.rx_timeout_count),
            OperationKind::Cad => self.increment(&self.cad_timeout_count),
            OperationKind::Transmit => self.increment(&self.tx_timeout_count),
            OperationKind::Inbound
            | OperationKind::AuthorizedFrame
            | OperationKind::Submission
            | OperationKind::ApiDispatch => self.increment(&self.unexpected_error_count),
        }
        self.finish_sample();
    }

    /// Update the maximum observed node-task loop gap.
    pub fn record_node_loop_gap(&self, elapsed_us: u64) {
        if elapsed_us <= u64::from(self.node_loop_gap_max_us.load(Ordering::Relaxed)) {
            return;
        }
        self.begin_sample();
        let elapsed_us = self.clamp(elapsed_us);
        update_max(&self.node_loop_gap_max_us, elapsed_us);
        self.finish_sample();
    }

    /// Update the maximum observed radio-task loop gap.
    pub fn record_radio_loop_gap(&self, elapsed_us: u64) {
        if elapsed_us <= u64::from(self.radio_loop_gap_max_us.load(Ordering::Relaxed)) {
            return;
        }
        self.begin_sample();
        let elapsed_us = self.clamp(elapsed_us);
        update_max(&self.radio_loop_gap_max_us, elapsed_us);
        self.finish_sample();
    }

    /// Update the maximum lateness of a scheduled measurement or wake.
    pub fn record_measurement_lateness(&self, elapsed_us: u64) {
        if elapsed_us <= u64::from(self.measurement_lateness_max_us.load(Ordering::Relaxed)) {
            return;
        }
        self.begin_sample();
        let elapsed_us = self.clamp(elapsed_us);
        update_max(&self.measurement_lateness_max_us, elapsed_us);
        self.finish_sample();
    }

    /// Update the maximum time spent collecting one measurement snapshot.
    pub fn record_measurement_work(&self, elapsed_us: u64) {
        if elapsed_us <= u64::from(self.measurement_work_max_us.load(Ordering::Relaxed)) {
            return;
        }
        self.begin_sample();
        let elapsed_us = self.clamp(elapsed_us);
        update_max(&self.measurement_work_max_us, elapsed_us);
        self.finish_sample();
    }

    /// Increment the saturating unexpected runtime/evidence error counter.
    pub fn record_unexpected_error(&self) {
        self.begin_sample();
        self.increment(&self.unexpected_error_count);
        self.finish_sample();
    }

    /// Record one allocation attempt and whether it succeeded.
    ///
    /// This path performs no allocation and is suitable for an allocator hook.
    pub fn record_allocation(&self, success: bool) {
        self.begin_sample();
        self.increment(&self.allocation_count);
        if !success {
            self.increment(&self.failed_allocation_count);
        }
        self.finish_sample();
    }

    fn boot_phase_fields(&self, phase: BootPhase) -> (&AtomicU32, &AtomicU32) {
        match phase {
            BootPhase::CredentialBoot => {
                (&self.credential_boot_last_us, &self.credential_boot_max_us)
            }
            BootPhase::IdentityPreflight => (
                &self.identity_preflight_last_us,
                &self.identity_preflight_max_us,
            ),
            BootPhase::JournalProvision => (
                &self.journal_provision_last_us,
                &self.journal_provision_max_us,
            ),
            BootPhase::AnnounceEpoch => (&self.announce_epoch_last_us, &self.announce_epoch_max_us),
            BootPhase::IdentityBoot => (&self.identity_boot_last_us, &self.identity_boot_max_us),
            BootPhase::JournalMount => (&self.journal_mount_last_us, &self.journal_mount_max_us),
            BootPhase::InboxMount => (&self.inbox_mount_last_us, &self.inbox_mount_max_us),
            BootPhase::RadioInit => (&self.radio_init_last_us, &self.radio_init_max_us),
        }
    }

    fn operation_fields(&self, operation: OperationKind) -> (&AtomicU32, &AtomicU32) {
        match operation {
            OperationKind::Inbound => (&self.inbound_count, &self.inbound_max_us),
            OperationKind::AuthorizedFrame => {
                (&self.authorized_frame_count, &self.authorized_frame_max_us)
            }
            OperationKind::Submission => (&self.submission_count, &self.submission_max_us),
            OperationKind::ApiDispatch => (&self.api_dispatch_count, &self.api_dispatch_max_us),
            OperationKind::Receive => (&self.rx_count, &self.rx_max_us),
            OperationKind::Cad => (&self.cad_count, &self.cad_max_us),
            OperationKind::Transmit => (&self.tx_count, &self.tx_max_us),
        }
    }

    fn update_sticky_truth_flag(&self, flag: u32, observed: bool, first: bool) {
        if first && observed {
            self.flags.fetch_or(flag, Ordering::Relaxed);
        } else if !observed {
            self.flags.fetch_and(!flag, Ordering::Relaxed);
        }
    }

    fn clamp(&self, value: u64) -> u32 {
        match u32::try_from(value) {
            Ok(value) => value,
            Err(_) => {
                self.mark_saturated();
                u32::MAX
            }
        }
    }

    fn increment(&self, counter: &AtomicU32) {
        if !saturating_increment(counter) {
            self.mark_saturated();
        }
    }

    fn begin_sample(&self) {
        let odd = self
            .snapshot_seq_begin
            .load(Ordering::Relaxed)
            .wrapping_add(1);
        self.snapshot_seq_begin.store(odd, Ordering::SeqCst);
        self.snapshot_seq_end.store(odd, Ordering::SeqCst);
    }

    fn finish_sample(&self) {
        let even = self
            .snapshot_seq_end
            .load(Ordering::Relaxed)
            .wrapping_add(1);
        self.snapshot_seq_end.store(even, Ordering::SeqCst);
        self.snapshot_seq_begin.store(even, Ordering::SeqCst);
    }

    fn mark_saturated(&self) {
        self.flags
            .fetch_or(RUNTIME_MEASUREMENT_FLAG_SATURATED, Ordering::Relaxed);
    }
}

impl Default for RuntimeMeasurementEvidence {
    fn default() -> Self {
        Self::new()
    }
}

/// Unique debugger-locatable runtime measurement evidence retained in HIL images.
#[used]
pub static RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE: RuntimeMeasurementEvidence =
    RuntimeMeasurementEvidence::new();

/// Four-byte proof-trace marker, stored in memory as the ASCII bytes `RPTE`.
pub const RUNTIME_PROOF_TRACE_EVIDENCE_MAGIC: u32 = u32::from_le_bytes(*b"RPTE");

/// Version of the debugger-visible proof-trace ABI.
pub const RUNTIME_PROOF_TRACE_EVIDENCE_VERSION: u32 = 1;

/// Exact number of 32-bit words in [`RuntimeProofTraceEvidence`] version 1.
pub const RUNTIME_PROOF_TRACE_EVIDENCE_WORDS: u32 = 48;

/// Exact byte size of [`RuntimeProofTraceEvidence`] version 1.
pub const RUNTIME_PROOF_TRACE_EVIDENCE_SIZE: u32 = 192;

/// Proof-trace collection is active in this image.
pub const RUNTIME_PROOF_TRACE_FLAG_ACTIVE: u32 = 1 << 0;
/// At least one trace counter, timestamp, or metadata scalar saturated.
pub const RUNTIME_PROOF_TRACE_FLAG_SATURATED: u32 = 1 << 1;
/// A compact tag is available for the last ingress that generated a proof.
pub const RUNTIME_PROOF_TRACE_FLAG_GENERATED_TAG_PRESENT: u32 = 1 << 2;
/// All generated proof tags in that ingress observation agreed.
pub const RUNTIME_PROOF_TRACE_FLAG_GENERATED_TAGS_CONSISTENT: u32 = 1 << 3;
/// A compact tag is available for the last delivered receipt terminal.
pub const RUNTIME_PROOF_TRACE_FLAG_DELIVERED_TAG_PRESENT: u32 = 1 << 4;
/// All delivered receipt tags in that ingress observation agreed.
pub const RUNTIME_PROOF_TRACE_FLAG_DELIVERED_TAGS_CONSISTENT: u32 = 1 << 5;
/// A compact tag is available for the last receipt-timeout pass.
pub const RUNTIME_PROOF_TRACE_FLAG_TIMEOUT_TAG_PRESENT: u32 = 1 << 6;
/// All timeout tags in that maintenance observation agreed.
pub const RUNTIME_PROOF_TRACE_FLAG_TIMEOUT_TAGS_CONSISTENT: u32 = 1 << 7;
/// An inbox commit has started but has not yet recorded its end boundary.
pub const RUNTIME_PROOF_TRACE_FLAG_INBOX_COMMIT_IN_PROGRESS: u32 = 1 << 8;
/// Inbox commit start/end calls have remained properly paired.
pub const RUNTIME_PROOF_TRACE_FLAG_INBOX_COMMIT_ORDER_CONSISTENT: u32 = 1 << 9;
/// A record call supplied internally inconsistent scalar metadata.
pub const RUNTIME_PROOF_TRACE_FLAG_INPUT_INCONSISTENT: u32 = 1 << 10;

/// Return whether one proof-trace dump contains a complete, stable snapshot.
pub const fn runtime_proof_trace_snapshot_is_stable(begin: u32, end: u32) -> bool {
    begin == end && begin & 1 == 0
}

/// Stable Reticulum ingress result encoded by the proof-trace ABI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum RuntimeProofTraceIngressDisposition {
    /// Native processing produced an event, action, or receipt terminal.
    Processed = 1,
    /// Native deduplication rejected an already-seen packet.
    NativeDuplicate = 2,
    /// Native validation rejected the packet.
    NativeInvalid = 3,
    /// Native processing produced no observable result.
    NoObservableOutcome = 4,
    /// Product-owned preflight rejected the packet.
    Rejected = 5,
}

/// Stable Reticulum wire packet type encoded by the proof-trace ABI.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u32)]
pub enum RuntimeProofTracePacketType {
    /// Parsing did not reach the common Reticulum header.
    #[default]
    Unparsed = 0,
    /// Reticulum DATA packet.
    Data = 1,
    /// Reticulum ANNOUNCE packet.
    Announce = 2,
    /// Reticulum LINKREQUEST packet.
    LinkRequest = 3,
    /// Reticulum PROOF packet.
    Proof = 4,
}

/// Allocation-free scalar metadata for one RNS ingress trace observation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RuntimeProofTraceIngressMetadata {
    /// Parsed wire packet type, or [`RuntimeProofTracePacketType::Unparsed`].
    pub wire_packet_type: RuntimeProofTracePacketType,
    /// Number of outbound packet actions emitted by the ingress call.
    pub emitted_packets: u64,
    /// Number of explicit proof actions generated by the ingress call.
    pub generated_proof_actions: u64,
    /// Number of delivered receipt terminals committed by the ingress call.
    pub delivered_receipt_terminals: u64,
    /// Number of timeout terminals unexpectedly committed during ingress.
    pub timed_out_receipt_terminals: u64,
    /// First compact generated-proof tag, when one was observable.
    pub generated_proof_tag: Option<u64>,
    /// First compact delivered-receipt tag, when one was observable.
    pub delivered_receipt_tag: Option<u64>,
    /// Whether every generated proof tag in this call agreed.
    pub generated_proof_tags_consistent: bool,
    /// Whether every delivered receipt tag in this call agreed.
    pub delivered_receipt_tags_consistent: bool,
    /// Whether the source metadata had already saturated a scalar count.
    pub counts_saturated: bool,
}

/// Exact 192-byte debugger-visible proof and durable-inbox trace ABI.
///
/// Words 0 through 4 are the leading sequence and fixed header. Words 5
/// through 27 are saturating event counts and millisecond timestamps. Words 28
/// through 32 are separate ingress-disposition counters, words 33 through 38
/// retain the last ingress classification metadata, words 39 through 44 hold
/// three split little-endian `u64` correlation tags, words 45 and 46 are the
/// saturating confirmed-success and not-confirmed-success radio-TX wrapper
/// counts, and word 47 is the trailing sequence marker.
///
/// This sequence protocol is deliberately not a lock. The E290 product runs
/// all current writers on one core, and every record method must run to
/// completion without yielding, awaiting, or otherwise allowing another trace
/// writer to enter. Multiple tasks may call these methods only under that
/// single-core, non-yielding writer assumption. A matching even sequence lets a
/// debugger reject a torn read; it does not serialize concurrent writers.
#[repr(C)]
pub struct RuntimeProofTraceEvidence {
    snapshot_seq_begin: AtomicU32,
    magic: u32,
    version: u32,
    size: u32,
    flags: AtomicU32,
    logical_rx_completed_count: AtomicU32,
    logical_rx_completed_last_ms: AtomicU32,
    ingress_enqueued_count: AtomicU32,
    ingress_enqueued_last_ms: AtomicU32,
    ingress_deferred_count: AtomicU32,
    ingress_deferred_last_ms: AtomicU32,
    ingress_failed_count: AtomicU32,
    ingress_failed_last_ms: AtomicU32,
    rns_ingress_count: AtomicU32,
    rns_ingress_last_ms: AtomicU32,
    proof_generated_count: AtomicU32,
    proof_generated_last_ms: AtomicU32,
    delivered_terminal_count: AtomicU32,
    delivered_terminal_last_ms: AtomicU32,
    receipt_timeout_count: AtomicU32,
    receipt_timeout_last_ms: AtomicU32,
    action_pressure_count: AtomicU32,
    action_pressure_last_ms: AtomicU32,
    correlation_fault_count: AtomicU32,
    correlation_fault_last_ms: AtomicU32,
    inbox_commit_count: AtomicU32,
    inbox_commit_start_last_ms: AtomicU32,
    inbox_commit_end_last_ms: AtomicU32,
    disposition_processed_count: AtomicU32,
    disposition_native_duplicate_count: AtomicU32,
    disposition_native_invalid_count: AtomicU32,
    disposition_no_observable_outcome_count: AtomicU32,
    disposition_rejected_count: AtomicU32,
    ingress_last_disposition: AtomicU32,
    ingress_last_wire_packet_type: AtomicU32,
    ingress_last_emitted_packets: AtomicU32,
    ingress_last_generated_proof_actions: AtomicU32,
    ingress_last_delivered_receipt_terminals: AtomicU32,
    ingress_last_timed_out_receipt_terminals: AtomicU32,
    generated_proof_tag_low: AtomicU32,
    generated_proof_tag_high: AtomicU32,
    delivered_receipt_tag_low: AtomicU32,
    delivered_receipt_tag_high: AtomicU32,
    timed_out_receipt_tag_low: AtomicU32,
    timed_out_receipt_tag_high: AtomicU32,
    radio_tx_confirmed_success_count: AtomicU32,
    radio_tx_not_confirmed_success_count: AtomicU32,
    snapshot_seq_end: AtomicU32,
}

impl RuntimeProofTraceEvidence {
    /// Construct empty version-1 proof-trace evidence.
    pub const fn new() -> Self {
        Self {
            snapshot_seq_begin: AtomicU32::new(0),
            magic: RUNTIME_PROOF_TRACE_EVIDENCE_MAGIC,
            version: RUNTIME_PROOF_TRACE_EVIDENCE_VERSION,
            size: RUNTIME_PROOF_TRACE_EVIDENCE_SIZE,
            flags: AtomicU32::new(
                RUNTIME_PROOF_TRACE_FLAG_ACTIVE
                    | RUNTIME_PROOF_TRACE_FLAG_INBOX_COMMIT_ORDER_CONSISTENT,
            ),
            logical_rx_completed_count: AtomicU32::new(0),
            logical_rx_completed_last_ms: AtomicU32::new(0),
            ingress_enqueued_count: AtomicU32::new(0),
            ingress_enqueued_last_ms: AtomicU32::new(0),
            ingress_deferred_count: AtomicU32::new(0),
            ingress_deferred_last_ms: AtomicU32::new(0),
            ingress_failed_count: AtomicU32::new(0),
            ingress_failed_last_ms: AtomicU32::new(0),
            rns_ingress_count: AtomicU32::new(0),
            rns_ingress_last_ms: AtomicU32::new(0),
            proof_generated_count: AtomicU32::new(0),
            proof_generated_last_ms: AtomicU32::new(0),
            delivered_terminal_count: AtomicU32::new(0),
            delivered_terminal_last_ms: AtomicU32::new(0),
            receipt_timeout_count: AtomicU32::new(0),
            receipt_timeout_last_ms: AtomicU32::new(0),
            action_pressure_count: AtomicU32::new(0),
            action_pressure_last_ms: AtomicU32::new(0),
            correlation_fault_count: AtomicU32::new(0),
            correlation_fault_last_ms: AtomicU32::new(0),
            inbox_commit_count: AtomicU32::new(0),
            inbox_commit_start_last_ms: AtomicU32::new(0),
            inbox_commit_end_last_ms: AtomicU32::new(0),
            disposition_processed_count: AtomicU32::new(0),
            disposition_native_duplicate_count: AtomicU32::new(0),
            disposition_native_invalid_count: AtomicU32::new(0),
            disposition_no_observable_outcome_count: AtomicU32::new(0),
            disposition_rejected_count: AtomicU32::new(0),
            ingress_last_disposition: AtomicU32::new(0),
            ingress_last_wire_packet_type: AtomicU32::new(0),
            ingress_last_emitted_packets: AtomicU32::new(0),
            ingress_last_generated_proof_actions: AtomicU32::new(0),
            ingress_last_delivered_receipt_terminals: AtomicU32::new(0),
            ingress_last_timed_out_receipt_terminals: AtomicU32::new(0),
            generated_proof_tag_low: AtomicU32::new(0),
            generated_proof_tag_high: AtomicU32::new(0),
            delivered_receipt_tag_low: AtomicU32::new(0),
            delivered_receipt_tag_high: AtomicU32::new(0),
            timed_out_receipt_tag_low: AtomicU32::new(0),
            timed_out_receipt_tag_high: AtomicU32::new(0),
            radio_tx_confirmed_success_count: AtomicU32::new(0),
            radio_tx_not_confirmed_success_count: AtomicU32::new(0),
            snapshot_seq_end: AtomicU32::new(0),
        }
    }

    /// Record completion of one reassembled logical radio packet.
    pub fn record_logical_rx_completed(&self, now_ms: u64) {
        self.record_occurrence(
            &self.logical_rx_completed_count,
            &self.logical_rx_completed_last_ms,
            now_ms,
        );
    }

    /// Record successful transfer of one logical packet into ingress ownership.
    pub fn record_ingress_enqueued(&self, now_ms: u64) {
        self.record_occurrence(
            &self.ingress_enqueued_count,
            &self.ingress_enqueued_last_ms,
            now_ms,
        );
    }

    /// Record one logical packet retained for a later ingress enqueue retry.
    pub fn record_ingress_deferred(&self, now_ms: u64) {
        self.record_occurrence(
            &self.ingress_deferred_count,
            &self.ingress_deferred_last_ms,
            now_ms,
        );
    }

    /// Record a terminal logical-packet ingress handoff failure.
    pub fn record_ingress_failed(&self, now_ms: u64) {
        self.record_occurrence(
            &self.ingress_failed_count,
            &self.ingress_failed_last_ms,
            now_ms,
        );
    }

    /// Record one RNS ingress result and its proof/receipt correlation metadata.
    pub fn record_rns_ingress(
        &self,
        now_ms: u64,
        disposition: RuntimeProofTraceIngressDisposition,
        metadata: RuntimeProofTraceIngressMetadata,
    ) {
        self.begin_sample();
        let now_ms = self.clamp(now_ms);
        self.add(&self.rns_ingress_count, 1);
        self.rns_ingress_last_ms.store(now_ms, Ordering::Relaxed);
        self.add(self.disposition_counter(disposition), 1);
        self.ingress_last_disposition
            .store(disposition as u32, Ordering::Relaxed);
        self.ingress_last_wire_packet_type
            .store(metadata.wire_packet_type as u32, Ordering::Relaxed);
        self.store_clamped(&self.ingress_last_emitted_packets, metadata.emitted_packets);
        self.store_clamped(
            &self.ingress_last_generated_proof_actions,
            metadata.generated_proof_actions,
        );
        self.store_clamped(
            &self.ingress_last_delivered_receipt_terminals,
            metadata.delivered_receipt_terminals,
        );
        self.store_clamped(
            &self.ingress_last_timed_out_receipt_terminals,
            metadata.timed_out_receipt_terminals,
        );
        if metadata.counts_saturated {
            self.mark_saturated();
        }

        if metadata.generated_proof_actions != 0 {
            self.add(
                &self.proof_generated_count,
                metadata.generated_proof_actions,
            );
            self.proof_generated_last_ms
                .store(now_ms, Ordering::Relaxed);
            self.store_tag(
                &self.generated_proof_tag_low,
                &self.generated_proof_tag_high,
                RUNTIME_PROOF_TRACE_FLAG_GENERATED_TAG_PRESENT,
                RUNTIME_PROOF_TRACE_FLAG_GENERATED_TAGS_CONSISTENT,
                metadata.generated_proof_tag,
                metadata.generated_proof_tags_consistent,
            );
        } else if metadata.generated_proof_tag.is_some() {
            self.mark_input_inconsistent();
        }

        if metadata.delivered_receipt_terminals != 0 {
            self.add(
                &self.delivered_terminal_count,
                metadata.delivered_receipt_terminals,
            );
            self.delivered_terminal_last_ms
                .store(now_ms, Ordering::Relaxed);
            self.store_tag(
                &self.delivered_receipt_tag_low,
                &self.delivered_receipt_tag_high,
                RUNTIME_PROOF_TRACE_FLAG_DELIVERED_TAG_PRESENT,
                RUNTIME_PROOF_TRACE_FLAG_DELIVERED_TAGS_CONSISTENT,
                metadata.delivered_receipt_tag,
                metadata.delivered_receipt_tags_consistent,
            );
        } else if metadata.delivered_receipt_tag.is_some() {
            self.mark_input_inconsistent();
        }
        self.finish_sample();
    }

    /// Record one maintenance pass that committed one or more receipt timeouts.
    pub fn record_receipt_timeouts(
        &self,
        now_ms: u64,
        count: u64,
        tag: Option<u64>,
        tags_consistent: bool,
    ) {
        if count == 0 && tag.is_none() {
            return;
        }
        self.begin_sample();
        if count == 0 {
            self.mark_input_inconsistent();
        } else {
            let now_ms = self.clamp(now_ms);
            self.add(&self.receipt_timeout_count, count);
            self.receipt_timeout_last_ms
                .store(now_ms, Ordering::Relaxed);
            self.store_tag(
                &self.timed_out_receipt_tag_low,
                &self.timed_out_receipt_tag_high,
                RUNTIME_PROOF_TRACE_FLAG_TIMEOUT_TAG_PRESENT,
                RUNTIME_PROOF_TRACE_FLAG_TIMEOUT_TAGS_CONSISTENT,
                tag,
                tags_consistent,
            );
        }
        self.finish_sample();
    }

    /// Record backpressure while admitting or retrying outbound node actions.
    pub fn record_action_pressure(&self, now_ms: u64) {
        self.record_occurrence(
            &self.action_pressure_count,
            &self.action_pressure_last_ms,
            now_ms,
        );
    }

    /// Record a terminal action/receipt correlation ownership fault.
    pub fn record_correlation_fault(&self, now_ms: u64) {
        self.record_occurrence(
            &self.correlation_fault_count,
            &self.correlation_fault_last_ms,
            now_ms,
        );
    }

    /// Record one terminal radio-TX report whose outcome was transmitted.
    pub fn record_radio_tx_confirmed_success(&self) {
        self.record_count(&self.radio_tx_confirmed_success_count);
    }

    /// Record one radio-TX wrapper invocation without confirmed transmission.
    pub fn record_radio_tx_not_confirmed_success(&self) {
        self.record_count(&self.radio_tx_not_confirmed_success_count);
    }

    /// Record the start of one durable inbox commit attempt.
    pub fn record_inbox_commit_started(&self, now_ms: u64) {
        self.begin_sample();
        let now_ms = self.clamp(now_ms);
        if self.flags.load(Ordering::Relaxed) & RUNTIME_PROOF_TRACE_FLAG_INBOX_COMMIT_IN_PROGRESS
            != 0
        {
            self.flags.fetch_and(
                !RUNTIME_PROOF_TRACE_FLAG_INBOX_COMMIT_ORDER_CONSISTENT,
                Ordering::Relaxed,
            );
        }
        self.flags.fetch_or(
            RUNTIME_PROOF_TRACE_FLAG_INBOX_COMMIT_IN_PROGRESS,
            Ordering::Relaxed,
        );
        self.add(&self.inbox_commit_count, 1);
        self.inbox_commit_start_last_ms
            .store(now_ms, Ordering::Relaxed);
        self.finish_sample();
    }

    /// Record the end of the active durable inbox commit attempt.
    pub fn record_inbox_commit_finished(&self, now_ms: u64) {
        self.begin_sample();
        let now_ms = self.clamp(now_ms);
        if self.flags.load(Ordering::Relaxed) & RUNTIME_PROOF_TRACE_FLAG_INBOX_COMMIT_IN_PROGRESS
            == 0
        {
            self.flags.fetch_and(
                !RUNTIME_PROOF_TRACE_FLAG_INBOX_COMMIT_ORDER_CONSISTENT,
                Ordering::Relaxed,
            );
        }
        self.flags.fetch_and(
            !RUNTIME_PROOF_TRACE_FLAG_INBOX_COMMIT_IN_PROGRESS,
            Ordering::Relaxed,
        );
        self.inbox_commit_end_last_ms
            .store(now_ms, Ordering::Relaxed);
        self.finish_sample();
    }

    fn record_occurrence(&self, count: &AtomicU32, last_ms: &AtomicU32, now_ms: u64) {
        self.begin_sample();
        let now_ms = self.clamp(now_ms);
        self.add(count, 1);
        last_ms.store(now_ms, Ordering::Relaxed);
        self.finish_sample();
    }

    fn record_count(&self, count: &AtomicU32) {
        self.begin_sample();
        self.add(count, 1);
        self.finish_sample();
    }

    fn disposition_counter(&self, disposition: RuntimeProofTraceIngressDisposition) -> &AtomicU32 {
        match disposition {
            RuntimeProofTraceIngressDisposition::Processed => &self.disposition_processed_count,
            RuntimeProofTraceIngressDisposition::NativeDuplicate => {
                &self.disposition_native_duplicate_count
            }
            RuntimeProofTraceIngressDisposition::NativeInvalid => {
                &self.disposition_native_invalid_count
            }
            RuntimeProofTraceIngressDisposition::NoObservableOutcome => {
                &self.disposition_no_observable_outcome_count
            }
            RuntimeProofTraceIngressDisposition::Rejected => &self.disposition_rejected_count,
        }
    }

    fn store_clamped(&self, target: &AtomicU32, value: u64) {
        target.store(self.clamp(value), Ordering::Relaxed);
    }

    fn store_tag(
        &self,
        low: &AtomicU32,
        high: &AtomicU32,
        present_flag: u32,
        consistency_flag: u32,
        tag: Option<u64>,
        consistent: bool,
    ) {
        let mut flags = self.flags.load(Ordering::Relaxed);
        flags &= !(present_flag | consistency_flag);
        if let Some(tag) = tag {
            low.store(tag as u32, Ordering::Relaxed);
            high.store((tag >> 32) as u32, Ordering::Relaxed);
            flags |= present_flag;
        } else {
            low.store(0, Ordering::Relaxed);
            high.store(0, Ordering::Relaxed);
        }
        if consistent {
            flags |= consistency_flag;
        }
        self.flags.store(flags, Ordering::Relaxed);
    }

    fn clamp(&self, value: u64) -> u32 {
        match u32::try_from(value) {
            Ok(value) => value,
            Err(_) => {
                self.mark_saturated();
                u32::MAX
            }
        }
    }

    fn add(&self, counter: &AtomicU32, amount: u64) {
        if !saturating_add(counter, amount) {
            self.mark_saturated();
        }
    }

    fn begin_sample(&self) {
        let odd = self
            .snapshot_seq_begin
            .load(Ordering::Relaxed)
            .wrapping_add(1);
        self.snapshot_seq_begin.store(odd, Ordering::SeqCst);
        self.snapshot_seq_end.store(odd, Ordering::SeqCst);
    }

    fn finish_sample(&self) {
        let even = self
            .snapshot_seq_end
            .load(Ordering::Relaxed)
            .wrapping_add(1);
        self.snapshot_seq_end.store(even, Ordering::SeqCst);
        self.snapshot_seq_begin.store(even, Ordering::SeqCst);
    }

    fn mark_saturated(&self) {
        self.flags
            .fetch_or(RUNTIME_PROOF_TRACE_FLAG_SATURATED, Ordering::Relaxed);
    }

    fn mark_input_inconsistent(&self) {
        self.flags.fetch_or(
            RUNTIME_PROOF_TRACE_FLAG_INPUT_INCONSISTENT,
            Ordering::Relaxed,
        );
    }
}

impl Default for RuntimeProofTraceEvidence {
    fn default() -> Self {
        Self::new()
    }
}

/// Unique debugger-locatable proof-trace evidence retained in HIL images.
#[used]
pub static RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE: RuntimeProofTraceEvidence =
    RuntimeProofTraceEvidence::new();

fn saturating_increment(counter: &AtomicU32) -> bool {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        if current == u32::MAX {
            return false;
        }
        match counter.compare_exchange_weak(
            current,
            current + 1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return true,
            Err(observed) => current = observed,
        }
    }
}

fn saturating_add(counter: &AtomicU32, amount: u64) -> bool {
    let amount_was_representable = u32::try_from(amount).is_ok();
    let amount = u32::try_from(amount).unwrap_or(u32::MAX);
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        let (next, overflowed) = current.overflowing_add(amount);
        let next = if overflowed { u32::MAX } else { next };
        match counter.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return amount_was_representable && !overflowed,
            Err(observed) => current = observed,
        }
    }
}

fn update_max(value: &AtomicU32, candidate: u32) {
    let _ = value.fetch_max(candidate, Ordering::Relaxed);
}

fn update_min(value: &AtomicU32, candidate: u32) {
    let _ = value.fetch_min(candidate, Ordering::Relaxed);
}

macro_rules! assert_word_offset {
    ($field:ident, $word:expr) => {
        assert!(
            core::mem::offset_of!(RuntimeMeasurementEvidence, $field)
                == $word * core::mem::size_of::<u32>()
        );
    };
}

const _: () = {
    assert!(core::mem::align_of::<RuntimeMeasurementEvidence>() == 4);
    assert!(
        core::mem::size_of::<RuntimeMeasurementEvidence>()
            == RUNTIME_MEASUREMENT_EVIDENCE_SIZE as usize
    );
    assert!(
        core::mem::size_of::<RuntimeMeasurementEvidence>()
            == RUNTIME_MEASUREMENT_EVIDENCE_WORDS as usize * core::mem::size_of::<u32>()
    );
    assert_word_offset!(snapshot_seq_begin, 0);
    assert_word_offset!(magic, 1);
    assert_word_offset!(version, 2);
    assert_word_offset!(size, 3);
    assert_word_offset!(flags, 4);
    assert_word_offset!(init_error, 5);
    assert_word_offset!(uptime_ms, 6);
    assert_word_offset!(psram_bytes, 7);
    assert_word_offset!(heap_total_bytes, 8);
    assert_word_offset!(heap_current_bytes, 9);
    assert_word_offset!(heap_maximum_bytes, 10);
    assert_word_offset!(heap_minimum_free_bytes, 11);
    assert_word_offset!(internal_heap_current_bytes, 12);
    assert_word_offset!(internal_heap_minimum_free_bytes, 13);
    assert_word_offset!(external_heap_current_bytes, 14);
    assert_word_offset!(external_heap_minimum_free_bytes, 15);
    assert_word_offset!(stack_reserved_bytes, 16);
    assert_word_offset!(stack_usable_bytes, 17);
    assert_word_offset!(stack_painted_bytes, 18);
    assert_word_offset!(stack_high_water_bytes, 19);
    assert_word_offset!(stack_minimum_remaining_bytes, 20);
    assert_word_offset!(stack_guard_offset_bytes, 21);
    assert_word_offset!(composition_ready_us, 22);
    assert_word_offset!(credential_boot_last_us, 23);
    assert_word_offset!(credential_boot_max_us, 24);
    assert_word_offset!(identity_preflight_last_us, 25);
    assert_word_offset!(identity_preflight_max_us, 26);
    assert_word_offset!(journal_provision_last_us, 27);
    assert_word_offset!(journal_provision_max_us, 28);
    assert_word_offset!(announce_epoch_last_us, 29);
    assert_word_offset!(announce_epoch_max_us, 30);
    assert_word_offset!(identity_boot_last_us, 31);
    assert_word_offset!(identity_boot_max_us, 32);
    assert_word_offset!(journal_mount_last_us, 33);
    assert_word_offset!(journal_mount_max_us, 34);
    assert_word_offset!(inbox_mount_last_us, 35);
    assert_word_offset!(inbox_mount_max_us, 36);
    assert_word_offset!(radio_init_last_us, 37);
    assert_word_offset!(radio_init_max_us, 38);
    assert_word_offset!(inbound_count, 39);
    assert_word_offset!(inbound_max_us, 40);
    assert_word_offset!(authorized_frame_count, 41);
    assert_word_offset!(authorized_frame_max_us, 42);
    assert_word_offset!(submission_count, 43);
    assert_word_offset!(submission_max_us, 44);
    assert_word_offset!(api_dispatch_count, 45);
    assert_word_offset!(api_dispatch_max_us, 46);
    assert_word_offset!(rx_count, 47);
    assert_word_offset!(rx_max_us, 48);
    assert_word_offset!(rx_timeout_count, 49);
    assert_word_offset!(cad_count, 50);
    assert_word_offset!(cad_max_us, 51);
    assert_word_offset!(cad_timeout_count, 52);
    assert_word_offset!(tx_count, 53);
    assert_word_offset!(tx_max_us, 54);
    assert_word_offset!(tx_timeout_count, 55);
    assert_word_offset!(node_loop_gap_max_us, 56);
    assert_word_offset!(radio_loop_gap_max_us, 57);
    assert_word_offset!(measurement_lateness_max_us, 58);
    assert_word_offset!(measurement_work_max_us, 59);
    assert_word_offset!(unexpected_error_count, 60);
    assert_word_offset!(allocation_count, 61);
    assert_word_offset!(failed_allocation_count, 62);
    assert_word_offset!(snapshot_seq_end, 63);
};

macro_rules! assert_proof_trace_word_offset {
    ($field:ident, $word:expr) => {
        assert!(
            core::mem::offset_of!(RuntimeProofTraceEvidence, $field)
                == $word * core::mem::size_of::<u32>()
        );
    };
}

const _: () = {
    assert!(core::mem::align_of::<RuntimeProofTraceEvidence>() == 4);
    assert!(
        core::mem::size_of::<RuntimeProofTraceEvidence>()
            == RUNTIME_PROOF_TRACE_EVIDENCE_SIZE as usize
    );
    assert!(
        core::mem::size_of::<RuntimeProofTraceEvidence>()
            == RUNTIME_PROOF_TRACE_EVIDENCE_WORDS as usize * core::mem::size_of::<u32>()
    );
    assert_proof_trace_word_offset!(snapshot_seq_begin, 0);
    assert_proof_trace_word_offset!(magic, 1);
    assert_proof_trace_word_offset!(version, 2);
    assert_proof_trace_word_offset!(size, 3);
    assert_proof_trace_word_offset!(flags, 4);
    assert_proof_trace_word_offset!(logical_rx_completed_count, 5);
    assert_proof_trace_word_offset!(logical_rx_completed_last_ms, 6);
    assert_proof_trace_word_offset!(ingress_enqueued_count, 7);
    assert_proof_trace_word_offset!(ingress_enqueued_last_ms, 8);
    assert_proof_trace_word_offset!(ingress_deferred_count, 9);
    assert_proof_trace_word_offset!(ingress_deferred_last_ms, 10);
    assert_proof_trace_word_offset!(ingress_failed_count, 11);
    assert_proof_trace_word_offset!(ingress_failed_last_ms, 12);
    assert_proof_trace_word_offset!(rns_ingress_count, 13);
    assert_proof_trace_word_offset!(rns_ingress_last_ms, 14);
    assert_proof_trace_word_offset!(proof_generated_count, 15);
    assert_proof_trace_word_offset!(proof_generated_last_ms, 16);
    assert_proof_trace_word_offset!(delivered_terminal_count, 17);
    assert_proof_trace_word_offset!(delivered_terminal_last_ms, 18);
    assert_proof_trace_word_offset!(receipt_timeout_count, 19);
    assert_proof_trace_word_offset!(receipt_timeout_last_ms, 20);
    assert_proof_trace_word_offset!(action_pressure_count, 21);
    assert_proof_trace_word_offset!(action_pressure_last_ms, 22);
    assert_proof_trace_word_offset!(correlation_fault_count, 23);
    assert_proof_trace_word_offset!(correlation_fault_last_ms, 24);
    assert_proof_trace_word_offset!(inbox_commit_count, 25);
    assert_proof_trace_word_offset!(inbox_commit_start_last_ms, 26);
    assert_proof_trace_word_offset!(inbox_commit_end_last_ms, 27);
    assert_proof_trace_word_offset!(disposition_processed_count, 28);
    assert_proof_trace_word_offset!(disposition_native_duplicate_count, 29);
    assert_proof_trace_word_offset!(disposition_native_invalid_count, 30);
    assert_proof_trace_word_offset!(disposition_no_observable_outcome_count, 31);
    assert_proof_trace_word_offset!(disposition_rejected_count, 32);
    assert_proof_trace_word_offset!(ingress_last_disposition, 33);
    assert_proof_trace_word_offset!(ingress_last_wire_packet_type, 34);
    assert_proof_trace_word_offset!(ingress_last_emitted_packets, 35);
    assert_proof_trace_word_offset!(ingress_last_generated_proof_actions, 36);
    assert_proof_trace_word_offset!(ingress_last_delivered_receipt_terminals, 37);
    assert_proof_trace_word_offset!(ingress_last_timed_out_receipt_terminals, 38);
    assert_proof_trace_word_offset!(generated_proof_tag_low, 39);
    assert_proof_trace_word_offset!(generated_proof_tag_high, 40);
    assert_proof_trace_word_offset!(delivered_receipt_tag_low, 41);
    assert_proof_trace_word_offset!(delivered_receipt_tag_high, 42);
    assert_proof_trace_word_offset!(timed_out_receipt_tag_low, 43);
    assert_proof_trace_word_offset!(timed_out_receipt_tag_high, 44);
    assert_proof_trace_word_offset!(radio_tx_confirmed_success_count, 45);
    assert_proof_trace_word_offset!(radio_tx_not_confirmed_success_count, 46);
    assert_proof_trace_word_offset!(snapshot_seq_end, 47);
};

#[cfg(test)]
mod tests {
    use super::*;

    fn load(value: &AtomicU32) -> u32 {
        value.load(Ordering::Relaxed)
    }

    fn heap_snapshot(
        current: u64,
        maximum: u64,
        free: u64,
        internal_free: u64,
        external_free: u64,
    ) -> HeapSnapshot {
        HeapSnapshot {
            total_bytes: 9_000,
            current_bytes: current,
            maximum_bytes: maximum,
            free_bytes: free,
            internal_current_bytes: 400,
            internal_free_bytes: internal_free,
            external_current_bytes: 600,
            external_free_bytes: external_free,
        }
    }

    fn stack_snapshot(
        high_water: u64,
        remaining: u64,
        scan_valid: bool,
        guard_intact: bool,
    ) -> StackSnapshot {
        StackSnapshot {
            reserved_bytes: 32_768,
            usable_bytes: 32_000,
            painted_bytes: 31_744,
            high_water_bytes: high_water,
            remaining_bytes: remaining,
            guard_offset_bytes: 764,
            scan_valid,
            guard_intact,
        }
    }

    #[test]
    fn abi_is_exactly_64_four_byte_words_with_stable_boundaries() {
        assert_eq!(RUNTIME_MEASUREMENT_EVIDENCE_MAGIC.to_le_bytes(), *b"RTME");
        assert_eq!(RUNTIME_MEASUREMENT_EVIDENCE_VERSION, 1);
        assert_eq!(RUNTIME_MEASUREMENT_EVIDENCE_WORDS, 64);
        assert_eq!(RUNTIME_MEASUREMENT_EVIDENCE_SIZE, 256);
        assert_eq!(core::mem::size_of::<RuntimeMeasurementEvidence>(), 256);
        assert_eq!(core::mem::align_of::<RuntimeMeasurementEvidence>(), 4);
        assert_eq!(
            core::mem::offset_of!(RuntimeMeasurementEvidence, snapshot_seq_begin),
            0
        );
        assert_eq!(core::mem::offset_of!(RuntimeMeasurementEvidence, magic), 4);
        assert_eq!(
            core::mem::offset_of!(RuntimeMeasurementEvidence, flags),
            4 * 4
        );
        assert_eq!(
            core::mem::offset_of!(RuntimeMeasurementEvidence, init_error),
            5 * 4
        );
        assert_eq!(
            core::mem::offset_of!(RuntimeMeasurementEvidence, composition_ready_us),
            22 * 4
        );
        assert_eq!(
            core::mem::offset_of!(RuntimeMeasurementEvidence, credential_boot_last_us),
            23 * 4
        );
        assert_eq!(
            core::mem::offset_of!(RuntimeMeasurementEvidence, inbound_count),
            39 * 4
        );
        assert_eq!(
            core::mem::offset_of!(RuntimeMeasurementEvidence, unexpected_error_count),
            60 * 4
        );
        assert_eq!(
            core::mem::offset_of!(RuntimeMeasurementEvidence, failed_allocation_count),
            62 * 4
        );
        assert_eq!(
            core::mem::offset_of!(RuntimeMeasurementEvidence, snapshot_seq_end),
            63 * 4
        );
    }

    #[test]
    fn fresh_evidence_has_exact_header_sentinels_and_stable_sequence() {
        let evidence = RuntimeMeasurementEvidence::new();
        assert_eq!(evidence.magic, RUNTIME_MEASUREMENT_EVIDENCE_MAGIC);
        assert_eq!(evidence.version, RUNTIME_MEASUREMENT_EVIDENCE_VERSION);
        assert_eq!(evidence.size, RUNTIME_MEASUREMENT_EVIDENCE_SIZE);
        assert_eq!(load(&evidence.flags), RUNTIME_MEASUREMENT_FLAG_ACTIVE);
        assert_eq!(load(&evidence.snapshot_seq_begin), 0);
        assert_eq!(load(&evidence.snapshot_seq_end), 0);
        assert_eq!(load(&evidence.heap_minimum_free_bytes), u32::MAX);
        assert_eq!(load(&evidence.internal_heap_minimum_free_bytes), u32::MAX);
        assert_eq!(load(&evidence.external_heap_minimum_free_bytes), u32::MAX);
        assert_eq!(load(&evidence.stack_minimum_remaining_bytes), u32::MAX);
        assert!(runtime_measurement_snapshot_is_stable(0, 0));
        assert!(!runtime_measurement_snapshot_is_stable(1, 1));
        assert!(!runtime_measurement_snapshot_is_stable(2, 4));
    }

    #[test]
    fn heap_snapshots_initialize_minima_and_preserve_extrema() {
        let evidence = RuntimeMeasurementEvidence::new();
        evidence.record_heap_snapshot(heap_snapshot(2_000, 2_500, 7_000, 1_500, 5_500));
        evidence.record_heap_snapshot(heap_snapshot(1_000, 2_000, 8_000, 2_000, 6_000));

        assert_eq!(load(&evidence.heap_total_bytes), 9_000);
        assert_eq!(load(&evidence.heap_current_bytes), 1_000);
        assert_eq!(load(&evidence.heap_maximum_bytes), 2_500);
        assert_eq!(load(&evidence.heap_minimum_free_bytes), 7_000);
        assert_eq!(load(&evidence.internal_heap_current_bytes), 400);
        assert_eq!(load(&evidence.internal_heap_minimum_free_bytes), 1_500);
        assert_eq!(load(&evidence.external_heap_current_bytes), 600);
        assert_eq!(load(&evidence.external_heap_minimum_free_bytes), 5_500);
        assert_ne!(
            load(&evidence.flags) & RUNTIME_MEASUREMENT_FLAG_HEAP_REGISTERED,
            0
        );
        assert_eq!(load(&evidence.snapshot_seq_begin), 4);
        assert_eq!(load(&evidence.snapshot_seq_end), 4);
    }

    #[test]
    fn stack_extrema_and_validity_fail_sticky() {
        let evidence = RuntimeMeasurementEvidence::new();
        evidence.record_stack_snapshot(stack_snapshot(4_000, 28_000, true, true));
        evidence.record_stack_snapshot(stack_snapshot(5_000, 27_000, false, false));
        evidence.record_stack_snapshot(stack_snapshot(4_500, 27_500, true, true));

        assert_eq!(load(&evidence.stack_reserved_bytes), 32_768);
        assert_eq!(load(&evidence.stack_usable_bytes), 32_000);
        assert_eq!(load(&evidence.stack_painted_bytes), 31_744);
        assert_eq!(load(&evidence.stack_high_water_bytes), 5_000);
        assert_eq!(load(&evidence.stack_minimum_remaining_bytes), 27_000);
        assert_eq!(load(&evidence.stack_guard_offset_bytes), 764);
        let flags = load(&evidence.flags);
        assert_ne!(flags & RUNTIME_MEASUREMENT_FLAG_STACK_INITIALIZED, 0);
        assert_eq!(flags & RUNTIME_MEASUREMENT_FLAG_SCAN_VALID, 0);
        assert_eq!(flags & RUNTIME_MEASUREMENT_FLAG_GUARD_INTACT, 0);
    }

    #[test]
    fn all_boot_pairs_keep_last_and_maximum() {
        let evidence = RuntimeMeasurementEvidence::new();
        let phases = [
            BootPhase::CredentialBoot,
            BootPhase::IdentityPreflight,
            BootPhase::JournalProvision,
            BootPhase::AnnounceEpoch,
            BootPhase::IdentityBoot,
            BootPhase::JournalMount,
            BootPhase::InboxMount,
            BootPhase::RadioInit,
        ];
        for (index, phase) in phases.into_iter().enumerate() {
            evidence.record_boot_phase(phase, 100 + index as u64);
            evidence.record_boot_phase(phase, 10 + index as u64);
            let (last, maximum) = evidence.boot_phase_fields(phase);
            assert_eq!(load(last), 10 + index as u32);
            assert_eq!(load(maximum), 100 + index as u32);
        }
        assert_eq!(load(&evidence.snapshot_seq_begin), 32);
        assert_eq!(load(&evidence.snapshot_seq_end), 32);
    }

    #[test]
    fn operations_update_counts_maxima_and_radio_timeouts() {
        let evidence = RuntimeMeasurementEvidence::new();
        let operations = [
            OperationKind::Inbound,
            OperationKind::AuthorizedFrame,
            OperationKind::Submission,
            OperationKind::ApiDispatch,
            OperationKind::Receive,
            OperationKind::Cad,
            OperationKind::Transmit,
        ];
        for operation in operations {
            evidence.record_operation(operation, 90);
            evidence.record_operation(operation, 20);
            let (count, maximum) = evidence.operation_fields(operation);
            assert_eq!(load(count), 2);
            assert_eq!(load(maximum), 90);
        }
        evidence.record_radio_timeout(OperationKind::Receive);
        evidence.record_radio_timeout(OperationKind::Cad);
        evidence.record_radio_timeout(OperationKind::Transmit);
        assert_eq!(load(&evidence.rx_timeout_count), 1);
        assert_eq!(load(&evidence.cad_timeout_count), 1);
        assert_eq!(load(&evidence.tx_timeout_count), 1);

        evidence.record_radio_timeout(OperationKind::Submission);
        assert_eq!(load(&evidence.unexpected_error_count), 1);
        assert_eq!(load(&evidence.snapshot_seq_begin), 36);
        assert_eq!(load(&evidence.snapshot_seq_end), 36);
        assert!(runtime_measurement_snapshot_is_stable(36, 36));
    }

    #[test]
    fn scheduler_maxima_composition_and_uptime_are_monotonic() {
        let evidence = RuntimeMeasurementEvidence::new();
        evidence.record_node_loop_gap(25);
        evidence.record_node_loop_gap(10);
        evidence.record_radio_loop_gap(30);
        evidence.record_radio_loop_gap(15);
        evidence.record_measurement_lateness(7);
        evidence.record_measurement_lateness(3);
        evidence.record_measurement_work(11);
        evidence.record_measurement_work(5);
        evidence.record_uptime_ms(400);
        evidence.record_uptime_ms(300);
        evidence.record_composition_ready(12_345);

        assert_eq!(load(&evidence.node_loop_gap_max_us), 25);
        assert_eq!(load(&evidence.radio_loop_gap_max_us), 30);
        assert_eq!(load(&evidence.measurement_lateness_max_us), 7);
        assert_eq!(load(&evidence.measurement_work_max_us), 11);
        assert_eq!(load(&evidence.uptime_ms), 400);
        assert_eq!(load(&evidence.composition_ready_us), 12_345);
        assert_ne!(
            load(&evidence.flags) & RUNTIME_MEASUREMENT_FLAG_COMPOSITION_READY,
            0
        );
    }

    #[test]
    fn unchanged_scheduler_maxima_do_not_advance_snapshot() {
        let evidence = RuntimeMeasurementEvidence::new();
        evidence.record_node_loop_gap(25);
        evidence.record_radio_loop_gap(30);
        evidence.record_measurement_lateness(7);
        evidence.record_measurement_work(11);
        let sequence = load(&evidence.snapshot_seq_begin);
        assert_eq!(load(&evidence.snapshot_seq_end), sequence);

        evidence.record_node_loop_gap(25);
        evidence.record_node_loop_gap(10);
        evidence.record_radio_loop_gap(30);
        evidence.record_radio_loop_gap(15);
        evidence.record_measurement_lateness(7);
        evidence.record_measurement_lateness(3);
        evidence.record_measurement_work(11);
        evidence.record_measurement_work(5);

        assert_eq!(load(&evidence.snapshot_seq_begin), sequence);
        assert_eq!(load(&evidence.snapshot_seq_end), sequence);
    }

    #[test]
    fn initialization_error_is_first_nonzero_and_zero_does_not_advance_snapshot() {
        let evidence = RuntimeMeasurementEvidence::new();
        evidence.record_initialization_error(0);
        assert_eq!(load(&evidence.snapshot_seq_begin), 0);
        assert_eq!(load(&evidence.snapshot_seq_end), 0);

        evidence.record_initialization_error(7);
        evidence.record_initialization_error(9);
        assert_eq!(load(&evidence.init_error), 7);
        assert_eq!(load(&evidence.snapshot_seq_begin), 4);
        assert_eq!(load(&evidence.snapshot_seq_end), 4);
    }

    #[test]
    fn allocation_hook_counters_are_saturating_and_allocation_free() {
        let evidence = RuntimeMeasurementEvidence::new();
        evidence.record_allocation(true);
        evidence.record_allocation(false);
        assert_eq!(load(&evidence.allocation_count), 2);
        assert_eq!(load(&evidence.failed_allocation_count), 1);

        evidence.allocation_count.store(u32::MAX, Ordering::Relaxed);
        evidence
            .failed_allocation_count
            .store(u32::MAX, Ordering::Relaxed);
        evidence.record_allocation(false);
        assert_eq!(load(&evidence.allocation_count), u32::MAX);
        assert_eq!(load(&evidence.failed_allocation_count), u32::MAX);
        assert_ne!(
            load(&evidence.flags) & RUNTIME_MEASUREMENT_FLAG_SATURATED,
            0
        );
    }

    #[test]
    fn oversized_values_saturate_without_wrapping_and_sequence_wraps_even() {
        let evidence = RuntimeMeasurementEvidence::new();
        evidence
            .snapshot_seq_begin
            .store(u32::MAX - 1, Ordering::Relaxed);
        evidence
            .snapshot_seq_end
            .store(u32::MAX - 1, Ordering::Relaxed);
        evidence.record_psram_bytes(u64::from(u32::MAX) + 1);
        assert_eq!(load(&evidence.psram_bytes), u32::MAX);
        assert_eq!(load(&evidence.snapshot_seq_begin), 0);
        assert_eq!(load(&evidence.snapshot_seq_end), 0);
        assert!(runtime_measurement_snapshot_is_stable(0, 0));
        assert_ne!(
            load(&evidence.flags) & RUNTIME_MEASUREMENT_FLAG_SATURATED,
            0
        );
    }

    #[test]
    fn a_halted_mid_update_sequence_is_rejected_by_decoder_rule() {
        let evidence = RuntimeMeasurementEvidence::new();
        evidence.begin_sample();
        let odd = load(&evidence.snapshot_seq_begin);
        assert_eq!(load(&evidence.snapshot_seq_end), odd);
        assert_eq!(odd & 1, 1);
        assert!(!runtime_measurement_snapshot_is_stable(odd, odd));

        let even = odd.wrapping_add(1);
        evidence.snapshot_seq_end.store(even, Ordering::SeqCst);
        assert!(!runtime_measurement_snapshot_is_stable(
            load(&evidence.snapshot_seq_begin),
            load(&evidence.snapshot_seq_end)
        ));
        evidence.snapshot_seq_begin.store(even, Ordering::SeqCst);
        assert_eq!(even & 1, 0);
        assert!(runtime_measurement_snapshot_is_stable(even, even));

        let completed = RuntimeMeasurementEvidence::new();
        completed.begin_sample();
        completed.finish_sample();
        assert_eq!(load(&completed.snapshot_seq_begin), 2);
        assert_eq!(load(&completed.snapshot_seq_end), 2);
        assert!(runtime_measurement_snapshot_is_stable(2, 2));
    }

    #[test]
    fn proof_trace_abi_is_exactly_48_words_with_stable_boundaries() {
        assert_eq!(RUNTIME_PROOF_TRACE_EVIDENCE_MAGIC.to_le_bytes(), *b"RPTE");
        assert_eq!(RUNTIME_PROOF_TRACE_EVIDENCE_VERSION, 1);
        assert_eq!(RUNTIME_PROOF_TRACE_EVIDENCE_WORDS, 48);
        assert_eq!(RUNTIME_PROOF_TRACE_EVIDENCE_SIZE, 192);
        assert_eq!(core::mem::size_of::<RuntimeProofTraceEvidence>(), 192);
        assert_eq!(core::mem::align_of::<RuntimeProofTraceEvidence>(), 4);
        assert_eq!(
            core::mem::offset_of!(RuntimeProofTraceEvidence, snapshot_seq_begin),
            0
        );
        assert_eq!(
            core::mem::offset_of!(RuntimeProofTraceEvidence, flags),
            4 * 4
        );
        assert_eq!(
            core::mem::offset_of!(RuntimeProofTraceEvidence, rns_ingress_count),
            13 * 4
        );
        assert_eq!(
            core::mem::offset_of!(RuntimeProofTraceEvidence, generated_proof_tag_low),
            39 * 4
        );
        assert_eq!(
            core::mem::offset_of!(RuntimeProofTraceEvidence, radio_tx_confirmed_success_count),
            45 * 4
        );
        assert_eq!(
            core::mem::offset_of!(
                RuntimeProofTraceEvidence,
                radio_tx_not_confirmed_success_count
            ),
            46 * 4
        );
        assert_eq!(
            core::mem::offset_of!(RuntimeProofTraceEvidence, snapshot_seq_end),
            47 * 4
        );
    }

    #[test]
    fn proof_trace_records_all_stages_dispositions_metadata_and_tags() {
        let evidence = RuntimeProofTraceEvidence::new();
        evidence.record_logical_rx_completed(10);
        evidence.record_ingress_enqueued(11);
        evidence.record_ingress_deferred(12);
        evidence.record_ingress_failed(13);
        evidence.record_rns_ingress(
            14,
            RuntimeProofTraceIngressDisposition::Processed,
            RuntimeProofTraceIngressMetadata {
                wire_packet_type: RuntimeProofTracePacketType::Proof,
                emitted_packets: 2,
                generated_proof_actions: 3,
                delivered_receipt_terminals: 4,
                timed_out_receipt_terminals: 0,
                generated_proof_tag: Some(0x1122_3344_5566_7788),
                delivered_receipt_tag: Some(0x8877_6655_4433_2211),
                generated_proof_tags_consistent: true,
                delivered_receipt_tags_consistent: false,
                counts_saturated: false,
            },
        );
        evidence.record_rns_ingress(
            15,
            RuntimeProofTraceIngressDisposition::NativeDuplicate,
            RuntimeProofTraceIngressMetadata::default(),
        );
        evidence.record_rns_ingress(
            16,
            RuntimeProofTraceIngressDisposition::NativeInvalid,
            RuntimeProofTraceIngressMetadata::default(),
        );
        evidence.record_rns_ingress(
            17,
            RuntimeProofTraceIngressDisposition::NoObservableOutcome,
            RuntimeProofTraceIngressMetadata::default(),
        );
        evidence.record_rns_ingress(
            18,
            RuntimeProofTraceIngressDisposition::Rejected,
            RuntimeProofTraceIngressMetadata::default(),
        );
        evidence.record_receipt_timeouts(19, 2, Some(0xaabb_ccdd_eeff_0011), true);
        evidence.record_action_pressure(20);
        evidence.record_correlation_fault(21);
        evidence.record_inbox_commit_started(22);
        evidence.record_inbox_commit_finished(23);
        evidence.record_radio_tx_confirmed_success();
        evidence.record_radio_tx_not_confirmed_success();

        assert_eq!(load(&evidence.logical_rx_completed_count), 1);
        assert_eq!(load(&evidence.logical_rx_completed_last_ms), 10);
        assert_eq!(load(&evidence.ingress_enqueued_count), 1);
        assert_eq!(load(&evidence.ingress_enqueued_last_ms), 11);
        assert_eq!(load(&evidence.ingress_deferred_count), 1);
        assert_eq!(load(&evidence.ingress_deferred_last_ms), 12);
        assert_eq!(load(&evidence.ingress_failed_count), 1);
        assert_eq!(load(&evidence.ingress_failed_last_ms), 13);
        assert_eq!(load(&evidence.rns_ingress_count), 5);
        assert_eq!(load(&evidence.rns_ingress_last_ms), 18);
        assert_eq!(load(&evidence.proof_generated_count), 3);
        assert_eq!(load(&evidence.proof_generated_last_ms), 14);
        assert_eq!(load(&evidence.delivered_terminal_count), 4);
        assert_eq!(load(&evidence.delivered_terminal_last_ms), 14);
        assert_eq!(load(&evidence.receipt_timeout_count), 2);
        assert_eq!(load(&evidence.receipt_timeout_last_ms), 19);
        assert_eq!(load(&evidence.action_pressure_count), 1);
        assert_eq!(load(&evidence.action_pressure_last_ms), 20);
        assert_eq!(load(&evidence.correlation_fault_count), 1);
        assert_eq!(load(&evidence.correlation_fault_last_ms), 21);
        assert_eq!(load(&evidence.inbox_commit_count), 1);
        assert_eq!(load(&evidence.inbox_commit_start_last_ms), 22);
        assert_eq!(load(&evidence.inbox_commit_end_last_ms), 23);
        assert_eq!(load(&evidence.radio_tx_confirmed_success_count), 1);
        assert_eq!(load(&evidence.radio_tx_not_confirmed_success_count), 1);
        for counter in [
            &evidence.disposition_processed_count,
            &evidence.disposition_native_duplicate_count,
            &evidence.disposition_native_invalid_count,
            &evidence.disposition_no_observable_outcome_count,
            &evidence.disposition_rejected_count,
        ] {
            assert_eq!(load(counter), 1);
        }
        assert_eq!(
            load(&evidence.ingress_last_disposition),
            RuntimeProofTraceIngressDisposition::Rejected as u32
        );
        assert_eq!(load(&evidence.ingress_last_wire_packet_type), 0);
        assert_eq!(load(&evidence.generated_proof_tag_low), 0x5566_7788);
        assert_eq!(load(&evidence.generated_proof_tag_high), 0x1122_3344);
        assert_eq!(load(&evidence.delivered_receipt_tag_low), 0x4433_2211);
        assert_eq!(load(&evidence.delivered_receipt_tag_high), 0x8877_6655);
        assert_eq!(load(&evidence.timed_out_receipt_tag_low), 0xeeff_0011);
        assert_eq!(load(&evidence.timed_out_receipt_tag_high), 0xaabb_ccdd);
        let flags = load(&evidence.flags);
        assert_ne!(flags & RUNTIME_PROOF_TRACE_FLAG_GENERATED_TAG_PRESENT, 0);
        assert_ne!(
            flags & RUNTIME_PROOF_TRACE_FLAG_GENERATED_TAGS_CONSISTENT,
            0
        );
        assert_ne!(flags & RUNTIME_PROOF_TRACE_FLAG_DELIVERED_TAG_PRESENT, 0);
        assert_eq!(
            flags & RUNTIME_PROOF_TRACE_FLAG_DELIVERED_TAGS_CONSISTENT,
            0
        );
        assert_ne!(flags & RUNTIME_PROOF_TRACE_FLAG_TIMEOUT_TAG_PRESENT, 0);
        assert_ne!(flags & RUNTIME_PROOF_TRACE_FLAG_TIMEOUT_TAGS_CONSISTENT, 0);
        assert_eq!(flags & RUNTIME_PROOF_TRACE_FLAG_INBOX_COMMIT_IN_PROGRESS, 0);
        assert_ne!(
            flags & RUNTIME_PROOF_TRACE_FLAG_INBOX_COMMIT_ORDER_CONSISTENT,
            0
        );
        assert_eq!(load(&evidence.snapshot_seq_begin), 32);
        assert_eq!(load(&evidence.snapshot_seq_end), 32);
        assert!(runtime_proof_trace_snapshot_is_stable(32, 32));
    }

    #[test]
    fn proof_trace_saturates_counts_timestamps_and_marks_bad_input() {
        let evidence = RuntimeProofTraceEvidence::new();
        evidence
            .logical_rx_completed_count
            .store(u32::MAX, Ordering::Relaxed);
        evidence.record_logical_rx_completed(u64::from(u32::MAX) + 1);
        evidence.record_rns_ingress(
            2,
            RuntimeProofTraceIngressDisposition::Processed,
            RuntimeProofTraceIngressMetadata {
                generated_proof_actions: u64::from(u32::MAX) + 1,
                generated_proof_tag: None,
                counts_saturated: true,
                ..RuntimeProofTraceIngressMetadata::default()
            },
        );
        evidence.record_receipt_timeouts(3, 0, Some(7), true);
        evidence
            .radio_tx_not_confirmed_success_count
            .store(u32::MAX, Ordering::Relaxed);
        evidence
            .radio_tx_confirmed_success_count
            .store(u32::MAX, Ordering::Relaxed);
        evidence.record_radio_tx_confirmed_success();
        evidence.record_radio_tx_not_confirmed_success();

        assert_eq!(load(&evidence.logical_rx_completed_count), u32::MAX);
        assert_eq!(load(&evidence.logical_rx_completed_last_ms), u32::MAX);
        assert_eq!(load(&evidence.proof_generated_count), u32::MAX);
        assert_eq!(load(&evidence.radio_tx_confirmed_success_count), u32::MAX);
        assert_eq!(
            load(&evidence.radio_tx_not_confirmed_success_count),
            u32::MAX
        );
        let flags = load(&evidence.flags);
        assert_ne!(flags & RUNTIME_PROOF_TRACE_FLAG_SATURATED, 0);
        assert_ne!(flags & RUNTIME_PROOF_TRACE_FLAG_INPUT_INCONSISTENT, 0);
        assert_eq!(load(&evidence.receipt_timeout_count), 0);
    }

    #[test]
    fn proof_trace_commit_pairing_and_sequence_fail_closed() {
        let evidence = RuntimeProofTraceEvidence::new();
        assert!(runtime_proof_trace_snapshot_is_stable(0, 0));
        evidence.record_inbox_commit_finished(1);
        assert_eq!(
            load(&evidence.flags) & RUNTIME_PROOF_TRACE_FLAG_INBOX_COMMIT_ORDER_CONSISTENT,
            0
        );
        evidence.record_inbox_commit_started(2);
        evidence.record_inbox_commit_started(3);
        assert_ne!(
            load(&evidence.flags) & RUNTIME_PROOF_TRACE_FLAG_INBOX_COMMIT_IN_PROGRESS,
            0
        );

        evidence.begin_sample();
        let odd = load(&evidence.snapshot_seq_begin);
        assert_eq!(odd, load(&evidence.snapshot_seq_end));
        assert!(!runtime_proof_trace_snapshot_is_stable(odd, odd));
        evidence.finish_sample();
        assert!(runtime_proof_trace_snapshot_is_stable(
            load(&evidence.snapshot_seq_begin),
            load(&evidence.snapshot_seq_end)
        ));
    }
}
