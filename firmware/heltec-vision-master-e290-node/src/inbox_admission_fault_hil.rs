//! Deliberate commit-write suppression for the inbound inbox admission HIL.
//!
//! This module is a narrowly scoped physical-fault fixture. It forwards the
//! first two NOR writes, acknowledges the third without touching the backend,
//! and forwards every later write. The inbox writer's three-stage ordering
//! therefore turns that third call into a missing terminal commit marker.

use core::{
    fmt,
    sync::atomic::{AtomicU32, Ordering},
};

use embedded_storage::nor_flash::{ErrorType, MultiwriteNorFlash, NorFlash, ReadNorFlash};
use reticulum_rns_inbox_store::{InboxAdmissionError, InboxProgramStage, InboxStoreFault};

/// Four-byte evidence marker, stored in memory as the ASCII bytes `RIAF`.
pub const INBOX_ADMISSION_FAULT_HIL_EVIDENCE_MAGIC: u32 = u32::from_le_bytes(*b"RIAF");

/// Version of the debugger-visible evidence ABI.
pub const INBOX_ADMISSION_FAULT_HIL_EVIDENCE_VERSION: u32 = 1;

/// Exact byte size of [`InboxAdmissionFaultHilEvidence`] version 1.
pub const INBOX_ADMISSION_FAULT_HIL_EVIDENCE_SIZE: u32 = 40;

/// Fixed, nonsecret evidence exported by the commit-suppression HIL image.
///
/// Every mutable word is atomic so a debugger cannot observe a partially
/// written scalar. `expected_commit_readback_mismatch` and
/// `unexpected_admission_failure` are published only after the quarantine
/// snapshot fields have been stored. The split dropped count is written once
/// by the product quarantine path and remains stable afterward.
#[repr(C)]
pub struct InboxAdmissionFaultHilEvidence {
    /// [`INBOX_ADMISSION_FAULT_HIL_EVIDENCE_MAGIC`].
    magic: u32,
    /// [`INBOX_ADMISSION_FAULT_HIL_EVIDENCE_VERSION`].
    version: u32,
    /// [`INBOX_ADMISSION_FAULT_HIL_EVIDENCE_SIZE`].
    size: u32,
    /// Number of wrapper write calls, saturated at `u32::MAX`.
    write_calls: AtomicU32,
    /// Number of deliberately acknowledged, suppressed third writes.
    commit_suppressed: AtomicU32,
    /// Expected terminal-commit readback mismatches, saturated at `u32::MAX`.
    expected_commit_readback_mismatch: AtomicU32,
    /// Every other admission failure, saturated at `u32::MAX`.
    unexpected_admission_failure: AtomicU32,
    /// Post-quarantine service state: one means disabled and zero means enabled.
    service_disabled: AtomicU32,
    /// Low 32 bits of the post-quarantine dropped-since-boot scalar.
    dropped_since_boot_low: AtomicU32,
    /// High 32 bits of the post-quarantine dropped-since-boot scalar.
    dropped_since_boot_high: AtomicU32,
}

impl InboxAdmissionFaultHilEvidence {
    const fn new() -> Self {
        Self {
            magic: INBOX_ADMISSION_FAULT_HIL_EVIDENCE_MAGIC,
            version: INBOX_ADMISSION_FAULT_HIL_EVIDENCE_VERSION,
            size: INBOX_ADMISSION_FAULT_HIL_EVIDENCE_SIZE,
            write_calls: AtomicU32::new(0),
            commit_suppressed: AtomicU32::new(0),
            expected_commit_readback_mismatch: AtomicU32::new(0),
            unexpected_admission_failure: AtomicU32::new(0),
            service_disabled: AtomicU32::new(0),
            dropped_since_boot_low: AtomicU32::new(0),
            dropped_since_boot_high: AtomicU32::new(0),
        }
    }
}

/// Distinctive debugger-locatable evidence retained in HIL images.
///
/// The verifier resolves the unique mangled ELF symbol containing this Rust
/// identifier and binds its actual address to the exact firmware image hash.
#[used]
pub static RETICULUM_INBOX_ADMISSION_FAULT_HIL_EVIDENCE: InboxAdmissionFaultHilEvidence =
    InboxAdmissionFaultHilEvidence::new();

const _: () = {
    assert!(core::mem::align_of::<InboxAdmissionFaultHilEvidence>() == 4);
    assert!(
        core::mem::size_of::<InboxAdmissionFaultHilEvidence>()
            == INBOX_ADMISSION_FAULT_HIL_EVIDENCE_SIZE as usize
    );
    assert!(core::mem::offset_of!(InboxAdmissionFaultHilEvidence, magic) == 0);
    assert!(core::mem::offset_of!(InboxAdmissionFaultHilEvidence, version) == 4);
    assert!(core::mem::offset_of!(InboxAdmissionFaultHilEvidence, size) == 8);
    assert!(core::mem::offset_of!(InboxAdmissionFaultHilEvidence, write_calls) == 12);
    assert!(core::mem::offset_of!(InboxAdmissionFaultHilEvidence, commit_suppressed) == 16);
    assert!(
        core::mem::offset_of!(
            InboxAdmissionFaultHilEvidence,
            expected_commit_readback_mismatch
        ) == 20
    );
    assert!(
        core::mem::offset_of!(InboxAdmissionFaultHilEvidence, unexpected_admission_failure) == 24
    );
    assert!(core::mem::offset_of!(InboxAdmissionFaultHilEvidence, service_disabled) == 28);
    assert!(core::mem::offset_of!(InboxAdmissionFaultHilEvidence, dropped_since_boot_low) == 32);
    assert!(core::mem::offset_of!(InboxAdmissionFaultHilEvidence, dropped_since_boot_high) == 36);
};

/// Allocation-free NOR wrapper that acknowledges but suppresses write call three.
///
/// The backend is deliberately private: the fixture offers no getter or
/// ownership-recovery method that could leak raw flash ownership into product
/// code. Dropping the wrapper releases any borrow held by `F` normally.
#[must_use = "the fault-injection wrapper must remain around the complete admission attempt"]
pub struct SuppressThirdWrite<F> {
    inner: F,
    write_calls: u32,
}

impl<F> SuppressThirdWrite<F> {
    /// Wrap a NOR backend for one deliberate admission attempt.
    pub const fn new(inner: F) -> Self {
        Self {
            inner,
            write_calls: 0,
        }
    }

    /// Number of write calls observed by this wrapper instance.
    pub const fn write_calls(&self) -> u32 {
        self.write_calls
    }
}

impl<F> fmt::Debug for SuppressThirdWrite<F> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SuppressThirdWrite")
            .field("write_calls", &self.write_calls)
            .finish_non_exhaustive()
    }
}

impl<F: ErrorType> ErrorType for SuppressThirdWrite<F> {
    type Error = F::Error;
}

impl<F: ReadNorFlash> ReadNorFlash for SuppressThirdWrite<F> {
    const READ_SIZE: usize = F::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.inner.read(offset, bytes)
    }

    fn capacity(&self) -> usize {
        self.inner.capacity()
    }
}

impl<F: NorFlash> NorFlash for SuppressThirdWrite<F> {
    const WRITE_SIZE: usize = F::WRITE_SIZE;
    const ERASE_SIZE: usize = F::ERASE_SIZE;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.inner.erase(from, to)
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.write_calls = self.write_calls.saturating_add(1);
        saturating_increment(&RETICULUM_INBOX_ADMISSION_FAULT_HIL_EVIDENCE.write_calls);
        if self.write_calls == 3 {
            saturating_increment(&RETICULUM_INBOX_ADMISSION_FAULT_HIL_EVIDENCE.commit_suppressed);
            Ok(())
        } else {
            self.inner.write(offset, bytes)
        }
    }
}

impl<F: MultiwriteNorFlash> MultiwriteNorFlash for SuppressThirdWrite<F> {}

/// Publish one post-quarantine admission result into debugger-visible evidence.
///
/// Only `Fault(ReadbackMismatch { stage: Commit })` is expected. Every binding
/// error, backend error, other stable fault, or mismatch at another program
/// stage increments `unexpected_admission_failure`. `service_disabled` and
/// `dropped_since_boot` must be the product scalars after it has disabled the
/// local inbox service and recorded the dropped candidate. The product calls
/// this once because quarantine makes subsequent admissions unavailable.
pub fn observe_product_quarantine<E>(
    error: &InboxAdmissionError<E>,
    service_disabled: bool,
    dropped_since_boot: u64,
) {
    let evidence = &RETICULUM_INBOX_ADMISSION_FAULT_HIL_EVIDENCE;
    evidence
        .service_disabled
        .store(u32::from(service_disabled), Ordering::SeqCst);
    evidence
        .dropped_since_boot_low
        .store(dropped_since_boot as u32, Ordering::SeqCst);
    evidence
        .dropped_since_boot_high
        .store((dropped_since_boot >> 32) as u32, Ordering::SeqCst);

    match error {
        InboxAdmissionError::Fault(InboxStoreFault::ReadbackMismatch {
            stage: InboxProgramStage::Commit,
        }) => saturating_increment(&evidence.expected_commit_readback_mismatch),
        _ => saturating_increment(&evidence.unexpected_admission_failure),
    }
}

fn saturating_increment(counter: &AtomicU32) {
    let mut current = counter.load(Ordering::SeqCst);
    while current != u32::MAX {
        match counter.compare_exchange_weak(
            current,
            current + 1,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    extern crate std;

    use embedded_storage::nor_flash::{
        NorFlashError, NorFlashErrorKind, check_erase, check_read, check_write,
    };
    use reticulum_rns_inbox_store::{
        BoundInboxStore, InboxCandidate, InboxDestination, InboxStoreBinding, InboxStoreDeviceId,
        InboxStoreMountError, PARTITION_SIZE, PHYSICAL_FORMAT_VERSION, mount,
    };
    use std::{format, string::String, sync::Mutex, vec, vec::Vec};

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum FakeError {
        Bounds,
        Alignment,
        Injected,
    }

    impl NorFlashError for FakeError {
        fn kind(&self) -> NorFlashErrorKind {
            match self {
                Self::Bounds => NorFlashErrorKind::OutOfBounds,
                Self::Alignment => NorFlashErrorKind::NotAligned,
                Self::Injected => NorFlashErrorKind::Other,
            }
        }
    }

    struct FakeNor {
        bytes: [u8; 32],
        reads: u32,
        writes: u32,
        erases: u32,
        fail_write_call: Option<u32>,
        debug_secret: &'static str,
    }

    impl FakeNor {
        fn erased() -> Self {
            Self {
                bytes: [0xff; 32],
                reads: 0,
                writes: 0,
                erases: 0,
                fail_write_call: None,
                debug_secret: "INNER_BACKEND_SECRET_MUST_NOT_ESCAPE",
            }
        }

        fn program(&mut self, offset: usize, bytes: &[u8]) {
            for (stored, supplied) in self.bytes[offset..offset + bytes.len()]
                .iter_mut()
                .zip(bytes)
            {
                *stored &= *supplied;
            }
        }
    }

    impl fmt::Debug for FakeNor {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("FakeNor")
                .field("debug_secret", &self.debug_secret)
                .finish_non_exhaustive()
        }
    }

    impl ErrorType for FakeNor {
        type Error = FakeError;
    }

    impl ReadNorFlash for FakeNor {
        const READ_SIZE: usize = 1;

        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
            check_read(self, offset, bytes.len()).map_err(map_check_error)?;
            self.reads = self.reads.saturating_add(1);
            let offset = offset as usize;
            bytes.copy_from_slice(&self.bytes[offset..offset + bytes.len()]);
            Ok(())
        }

        fn capacity(&self) -> usize {
            self.bytes.len()
        }
    }

    impl NorFlash for FakeNor {
        const WRITE_SIZE: usize = 1;
        const ERASE_SIZE: usize = 4;

        fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            check_erase(self, from, to).map_err(map_check_error)?;
            self.erases = self.erases.saturating_add(1);
            self.bytes[from as usize..to as usize].fill(0xff);
            Ok(())
        }

        fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
            check_write(self, offset, bytes.len()).map_err(map_check_error)?;
            self.writes = self.writes.saturating_add(1);
            if self.fail_write_call == Some(self.writes) {
                return Err(FakeError::Injected);
            }
            self.program(offset as usize, bytes);
            Ok(())
        }
    }

    impl MultiwriteNorFlash for FakeNor {}
    impl MultiwriteNorFlash for &mut FakeNor {}

    struct InboxFakeNor {
        bytes: Vec<u8>,
        writes: u32,
        write_offsets: [u32; 2],
        write_lengths: [usize; 2],
        erases: u32,
    }

    impl InboxFakeNor {
        fn erased() -> Self {
            Self {
                bytes: vec![0xff; PARTITION_SIZE],
                writes: 0,
                write_offsets: [u32::MAX; 2],
                write_lengths: [usize::MAX; 2],
                erases: 0,
            }
        }
    }

    impl ErrorType for InboxFakeNor {
        type Error = FakeError;
    }

    impl ReadNorFlash for InboxFakeNor {
        const READ_SIZE: usize = 4;

        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
            check_read(self, offset, bytes.len()).map_err(map_check_error)?;
            let offset = offset as usize;
            bytes.copy_from_slice(&self.bytes[offset..offset + bytes.len()]);
            Ok(())
        }

        fn capacity(&self) -> usize {
            self.bytes.len()
        }
    }

    impl NorFlash for InboxFakeNor {
        const WRITE_SIZE: usize = 4;
        const ERASE_SIZE: usize = 4096;

        fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            check_erase(self, from, to).map_err(map_check_error)?;
            self.erases = self.erases.saturating_add(1);
            self.bytes[from as usize..to as usize].fill(0xff);
            Ok(())
        }

        fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
            check_write(self, offset, bytes.len()).map_err(map_check_error)?;
            let observation = self.writes as usize;
            assert!(observation < self.write_offsets.len());
            self.write_offsets[observation] = offset;
            self.write_lengths[observation] = bytes.len();
            self.writes = self.writes.saturating_add(1);
            let offset = offset as usize;
            for (stored, supplied) in self.bytes[offset..offset + bytes.len()]
                .iter_mut()
                .zip(bytes)
            {
                *stored &= *supplied;
            }
            Ok(())
        }
    }

    impl MultiwriteNorFlash for InboxFakeNor {}
    impl MultiwriteNorFlash for &mut InboxFakeNor {}

    fn map_check_error(error: NorFlashErrorKind) -> FakeError {
        match error {
            NorFlashErrorKind::OutOfBounds => FakeError::Bounds,
            NorFlashErrorKind::NotAligned => FakeError::Alignment,
            _ => FakeError::Injected,
        }
    }

    fn lock_tests() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn reset_evidence() {
        let evidence = &RETICULUM_INBOX_ADMISSION_FAULT_HIL_EVIDENCE;
        evidence.write_calls.store(0, Ordering::SeqCst);
        evidence.commit_suppressed.store(0, Ordering::SeqCst);
        evidence
            .expected_commit_readback_mismatch
            .store(0, Ordering::SeqCst);
        evidence
            .unexpected_admission_failure
            .store(0, Ordering::SeqCst);
        evidence.service_disabled.store(0, Ordering::SeqCst);
        evidence.dropped_since_boot_low.store(0, Ordering::SeqCst);
        evidence.dropped_since_boot_high.store(0, Ordering::SeqCst);
    }

    fn assert_same_error_type<T: ErrorType<Error = FakeError>>(_value: &T) {}

    fn assert_multiwrite<T: MultiwriteNorFlash>(_value: &T) {}

    #[test]
    fn third_write_only_is_suppressed_and_other_operations_are_exactly_forwarded() {
        let _guard = lock_tests();
        reset_evidence();
        let mut backend = FakeNor::erased();
        let wrapper_debug: String;
        {
            let mut wrapper = SuppressThirdWrite::new(&mut backend);
            assert_same_error_type(&wrapper);
            assert_multiwrite(&wrapper);
            assert_eq!(wrapper.capacity(), 32);

            wrapper.write(0, &[0xf0, 0x0f, 0xaa, 0x55]).unwrap();
            wrapper.write(4, &[0xcc, 0x33, 0x00, 0xff]).unwrap();
            wrapper.write(8, &[0x00, 0x00, 0x00, 0x00]).unwrap();
            wrapper.write(12, &[0x12, 0x34, 0x56, 0x78]).unwrap();
            assert_eq!(wrapper.write_calls(), 4);

            let mut readback = [0; 16];
            wrapper.read(0, &mut readback).unwrap();
            assert_eq!(
                readback,
                [
                    0xf0, 0x0f, 0xaa, 0x55, 0xcc, 0x33, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0x12,
                    0x34, 0x56, 0x78,
                ]
            );
            wrapper.erase(4, 8).unwrap();
            wrapper_debug = format!("{wrapper:?}");
        }

        assert_eq!(backend.writes, 3);
        assert_eq!(backend.reads, 1);
        assert_eq!(backend.erases, 1);
        assert_eq!(
            backend.bytes,
            [
                0xf0, 0x0f, 0xaa, 0x55, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x12, 0x34,
                0x56, 0x78, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0xff, 0xff, 0xff,
            ]
        );
        assert_eq!(
            RETICULUM_INBOX_ADMISSION_FAULT_HIL_EVIDENCE
                .write_calls
                .load(Ordering::SeqCst),
            4
        );
        assert_eq!(
            RETICULUM_INBOX_ADMISSION_FAULT_HIL_EVIDENCE
                .commit_suppressed
                .load(Ordering::SeqCst),
            1
        );
        assert!(wrapper_debug.contains("write_calls: 4"));
        assert!(!wrapper_debug.contains(backend.debug_secret));
        assert!(format!("{backend:?}").contains(backend.debug_secret));
    }

    #[test]
    fn forwarded_write_errors_keep_the_backend_error_type_and_call_position() {
        let _guard = lock_tests();
        reset_evidence();
        let mut backend = FakeNor::erased();
        backend.fail_write_call = Some(1);
        {
            let mut wrapper = SuppressThirdWrite::new(&mut backend);
            assert_eq!(wrapper.write(0, &[0x00]), Err(FakeError::Injected));
            wrapper.write(1, &[0x11]).unwrap();
            wrapper.write(2, &[0x22]).unwrap();
            wrapper.write(3, &[0x33]).unwrap();
        }

        assert_eq!(backend.writes, 3);
        assert_eq!(&backend.bytes[..4], &[0xff, 0x11, 0xff, 0x33]);
    }

    #[test]
    fn inbox_accept_suppresses_commit_then_bare_remount_reports_interrupted_record() {
        let _guard = lock_tests();
        reset_evidence();
        let binding = InboxStoreBinding::new(
            InboxStoreDeviceId::new([0x5a; 16]),
            0x73_0000,
            PARTITION_SIZE,
            PHYSICAL_FORMAT_VERSION,
        );
        let mut backend = InboxFakeNor::erased();
        let mut mounted = {
            let mut initial_access = BoundInboxStore::new(&mut backend, binding);
            match mount(&mut initial_access) {
                Ok(mounted) => mounted,
                Err(_) => panic!("erased inbox fixture must mount"),
            }
        };

        let candidate = InboxCandidate::new(InboxDestination::new([0xa5; 16]), b"commit fault HIL")
            .expect("bounded candidate");
        let failure = {
            let wrapper = SuppressThirdWrite::new(&mut backend);
            let mut fault_access = BoundInboxStore::new(wrapper, binding);
            match mounted.accept(&mut fault_access, candidate) {
                Ok(_) => panic!("suppressed commit must fail readback"),
                Err(failure) => failure,
            }
        };
        assert!(matches!(
            failure.error(),
            InboxAdmissionError::Fault(InboxStoreFault::ReadbackMismatch {
                stage: InboxProgramStage::Commit,
            })
        ));
        assert_eq!(backend.writes, 2);
        assert_eq!(backend.write_offsets, [0, 32]);
        assert_eq!(backend.write_lengths, [32, 512]);
        assert_eq!(backend.erases, 0);
        assert_eq!(
            RETICULUM_INBOX_ADMISSION_FAULT_HIL_EVIDENCE
                .write_calls
                .load(Ordering::SeqCst),
            3
        );
        assert_eq!(
            RETICULUM_INBOX_ADMISSION_FAULT_HIL_EVIDENCE
                .commit_suppressed
                .load(Ordering::SeqCst),
            1
        );

        let mut remount_access = BoundInboxStore::new(&mut backend, binding);
        let remount_error = match mount(&mut remount_access) {
            Ok(_) => panic!("missing commit must not remount"),
            Err(error) => error,
        };
        assert!(matches!(
            remount_error,
            InboxStoreMountError::Fault(InboxStoreFault::InterruptedRecord)
        ));
        assert_eq!(backend.erases, 0);
    }

    #[test]
    fn quarantine_classification_accepts_exactly_commit_readback_mismatch() {
        let _guard = lock_tests();
        reset_evidence();
        let wrong_stage =
            InboxAdmissionError::<FakeError>::Fault(InboxStoreFault::ReadbackMismatch {
                stage: InboxProgramStage::BodyAndDigest,
            });
        observe_product_quarantine(&wrong_stage, true, 9);
        let backend = InboxAdmissionError::Backend {
            stage: Some(InboxProgramStage::Commit),
            error: FakeError::Injected,
        };
        observe_product_quarantine(&backend, true, 10);
        let expected = InboxAdmissionError::<FakeError>::Fault(InboxStoreFault::ReadbackMismatch {
            stage: InboxProgramStage::Commit,
        });
        observe_product_quarantine(&expected, true, 0x0102_0304_0506_0708);

        let evidence = &RETICULUM_INBOX_ADMISSION_FAULT_HIL_EVIDENCE;
        assert_eq!(
            evidence
                .expected_commit_readback_mismatch
                .load(Ordering::SeqCst),
            1
        );
        assert_eq!(
            evidence.unexpected_admission_failure.load(Ordering::SeqCst),
            2
        );
        assert_eq!(evidence.service_disabled.load(Ordering::SeqCst), 1);
        assert_eq!(
            evidence.dropped_since_boot_low.load(Ordering::SeqCst),
            0x0506_0708
        );
        assert_eq!(
            evidence.dropped_since_boot_high.load(Ordering::SeqCst),
            0x0102_0304
        );
    }

    #[test]
    fn evidence_abi_offsets_size_and_magic_are_exact() {
        let _guard = lock_tests();
        let evidence = &RETICULUM_INBOX_ADMISSION_FAULT_HIL_EVIDENCE;
        assert_eq!(evidence.magic.to_le_bytes(), *b"RIAF");
        assert_eq!(evidence.version, 1);
        assert_eq!(evidence.size, 40);
        assert_eq!(core::mem::size_of::<InboxAdmissionFaultHilEvidence>(), 40);
        assert_eq!(core::mem::align_of::<InboxAdmissionFaultHilEvidence>(), 4);
        assert_eq!(
            core::mem::offset_of!(InboxAdmissionFaultHilEvidence, magic),
            0
        );
        assert_eq!(
            core::mem::offset_of!(InboxAdmissionFaultHilEvidence, version),
            4
        );
        assert_eq!(
            core::mem::offset_of!(InboxAdmissionFaultHilEvidence, size),
            8
        );
        assert_eq!(
            core::mem::offset_of!(InboxAdmissionFaultHilEvidence, write_calls),
            12
        );
        assert_eq!(
            core::mem::offset_of!(InboxAdmissionFaultHilEvidence, commit_suppressed),
            16
        );
        assert_eq!(
            core::mem::offset_of!(
                InboxAdmissionFaultHilEvidence,
                expected_commit_readback_mismatch
            ),
            20
        );
        assert_eq!(
            core::mem::offset_of!(InboxAdmissionFaultHilEvidence, unexpected_admission_failure),
            24
        );
        assert_eq!(
            core::mem::offset_of!(InboxAdmissionFaultHilEvidence, service_disabled),
            28
        );
        assert_eq!(
            core::mem::offset_of!(InboxAdmissionFaultHilEvidence, dropped_since_boot_low),
            32
        );
        assert_eq!(
            core::mem::offset_of!(InboxAdmissionFaultHilEvidence, dropped_since_boot_high),
            36
        );
    }

    #[test]
    fn counter_increment_saturates_without_wrapping() {
        let _guard = lock_tests();
        let counter = AtomicU32::new(u32::MAX - 1);
        saturating_increment(&counter);
        assert_eq!(counter.load(Ordering::SeqCst), u32::MAX);
        saturating_increment(&counter);
        assert_eq!(counter.load(Ordering::SeqCst), u32::MAX);
    }
}
