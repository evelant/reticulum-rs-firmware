//! Checked partition-relative access to raw NOR flash.
//!
//! This crate deliberately implements only the raw NOR traits. In particular,
//! it does not implement [`embedded_storage::Storage`], whose byte-storage
//! write contract can select a sector-rewriting path on some backends.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
};

/// Failure while translating or forwarding a partition-relative operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegionError<E> {
    /// The underlying NOR flash backend rejected an otherwise in-range request.
    Backend(E),
    /// The relative request or its translated absolute range is not representable
    /// inside the configured partition.
    OutOfBounds,
}

impl<E> NorFlashError for RegionError<E>
where
    E: NorFlashError,
{
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::Backend(error) => error.kind(),
            Self::OutOfBounds => NorFlashErrorKind::OutOfBounds,
        }
    }
}

/// A checked relative view over one raw NOR-flash partition.
///
/// The view borrows the sole backend owner. Every operation is translated from
/// `[0, capacity())` into the absolute range beginning at `start`; arithmetic
/// overflow and crossing the exclusive partition end are rejected locally.
pub struct PartitionNorFlash<'a, F> {
    flash: &'a mut F,
    start: u32,
    len: u32,
    write_calls: u32,
    erase_calls: u32,
}

impl<'a, F> PartitionNorFlash<'a, F> {
    /// Borrow `flash` as a partition beginning at `start` with length `len`.
    ///
    /// Construction does not access the backend. A caller that obtains these
    /// values from a partition table remains responsible for validating that
    /// table and the containing device capacity. Each later request is still
    /// checked against `len` and for absolute-address overflow.
    pub const fn new(flash: &'a mut F, start: u32, len: u32) -> Self {
        Self {
            flash,
            start,
            len,
            write_calls: 0,
            erase_calls: 0,
        }
    }

    /// Number of in-range raw program calls attempted against the backend.
    ///
    /// A backend error still counts as an attempt; a request rejected by this
    /// view before reaching the backend does not.
    pub const fn write_calls(&self) -> u32 {
        self.write_calls
    }

    /// Number of in-range raw erase calls attempted against the backend.
    ///
    /// A backend error still counts as an attempt; a request rejected by this
    /// view before reaching the backend does not.
    pub const fn erase_calls(&self) -> u32 {
        self.erase_calls
    }

    fn absolute_offset<E>(&self, offset: u32, len: usize) -> Result<u32, RegionError<E>> {
        let len = u32::try_from(len).map_err(|_| RegionError::OutOfBounds)?;
        let relative_end = offset.checked_add(len).ok_or(RegionError::OutOfBounds)?;
        if relative_end > self.len {
            return Err(RegionError::OutOfBounds);
        }
        let absolute = self
            .start
            .checked_add(offset)
            .ok_or(RegionError::OutOfBounds)?;
        self.start
            .checked_add(relative_end)
            .ok_or(RegionError::OutOfBounds)?;
        Ok(absolute)
    }

    fn absolute_erase_range<E>(&self, from: u32, to: u32) -> Result<(u32, u32), RegionError<E>> {
        if from > to || to > self.len {
            return Err(RegionError::OutOfBounds);
        }
        let absolute_from = self
            .start
            .checked_add(from)
            .ok_or(RegionError::OutOfBounds)?;
        let absolute_to = self.start.checked_add(to).ok_or(RegionError::OutOfBounds)?;
        Ok((absolute_from, absolute_to))
    }
}

impl<F> ErrorType for PartitionNorFlash<'_, F>
where
    F: ErrorType,
    F::Error: NorFlashError,
{
    type Error = RegionError<F::Error>;
}

impl<F> ReadNorFlash for PartitionNorFlash<'_, F>
where
    F: ReadNorFlash,
    F::Error: NorFlashError,
{
    const READ_SIZE: usize = F::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let absolute = self.absolute_offset(offset, bytes.len())?;
        self.flash
            .read(absolute, bytes)
            .map_err(RegionError::Backend)
    }

    fn capacity(&self) -> usize {
        self.len as usize
    }
}

impl<F> NorFlash for PartitionNorFlash<'_, F>
where
    F: NorFlash,
    F::Error: NorFlashError,
{
    const WRITE_SIZE: usize = F::WRITE_SIZE;
    const ERASE_SIZE: usize = F::ERASE_SIZE;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        let absolute = self.absolute_offset(offset, bytes.len())?;
        self.write_calls = self.write_calls.saturating_add(1);
        self.flash
            .write(absolute, bytes)
            .map_err(RegionError::Backend)
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let (absolute_from, absolute_to) = self.absolute_erase_range(from, to)?;
        self.erase_calls = self.erase_calls.saturating_add(1);
        self.flash
            .erase(absolute_from, absolute_to)
            .map_err(RegionError::Backend)
    }
}

impl<F> MultiwriteNorFlash for PartitionNorFlash<'_, F>
where
    F: MultiwriteNorFlash,
    F::Error: NorFlashError,
{
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLASH_BYTES: usize = 64;
    const PARTITION_START: u32 = 16;
    const PARTITION_LEN: u32 = 24;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Operation {
        Read,
        Write,
        Erase,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum FakeError {
        Injected(Operation),
        OutOfBounds,
    }

    impl NorFlashError for FakeError {
        fn kind(&self) -> NorFlashErrorKind {
            match self {
                Self::Injected(_) => NorFlashErrorKind::Other,
                Self::OutOfBounds => NorFlashErrorKind::OutOfBounds,
            }
        }
    }

    struct FakeFlash {
        bytes: [u8; FLASH_BYTES],
        fail_next: Option<Operation>,
        reads: u32,
        writes: u32,
        erases: u32,
        last_read: Option<(u32, usize)>,
        last_write: Option<(u32, usize)>,
        last_erase: Option<(u32, u32)>,
    }

    impl FakeFlash {
        fn sequenced() -> Self {
            let mut bytes = [0_u8; FLASH_BYTES];
            for (index, byte) in bytes.iter_mut().enumerate() {
                *byte = index as u8;
            }
            Self {
                bytes,
                fail_next: None,
                reads: 0,
                writes: 0,
                erases: 0,
                last_read: None,
                last_write: None,
                last_erase: None,
            }
        }

        fn fail_once(&mut self, operation: Operation) {
            self.fail_next = Some(operation);
        }

        fn check_range(&self, offset: u32, len: usize) -> Result<usize, FakeError> {
            let start = usize::try_from(offset).map_err(|_| FakeError::OutOfBounds)?;
            let end = start.checked_add(len).ok_or(FakeError::OutOfBounds)?;
            if end > self.bytes.len() {
                return Err(FakeError::OutOfBounds);
            }
            Ok(start)
        }

        fn take_failure(&mut self, operation: Operation) -> Result<(), FakeError> {
            if self.fail_next == Some(operation) {
                self.fail_next = None;
                Err(FakeError::Injected(operation))
            } else {
                Ok(())
            }
        }
    }

    impl ErrorType for FakeFlash {
        type Error = FakeError;
    }

    impl ReadNorFlash for FakeFlash {
        const READ_SIZE: usize = 1;

        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
            self.reads = self.reads.saturating_add(1);
            self.last_read = Some((offset, bytes.len()));
            self.take_failure(Operation::Read)?;
            let start = self.check_range(offset, bytes.len())?;
            bytes.copy_from_slice(&self.bytes[start..start + bytes.len()]);
            Ok(())
        }

        fn capacity(&self) -> usize {
            self.bytes.len()
        }
    }

    impl NorFlash for FakeFlash {
        const WRITE_SIZE: usize = 1;
        const ERASE_SIZE: usize = 4;

        fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
            self.writes = self.writes.saturating_add(1);
            self.last_write = Some((offset, bytes.len()));
            self.take_failure(Operation::Write)?;
            let start = self.check_range(offset, bytes.len())?;
            self.bytes[start..start + bytes.len()].copy_from_slice(bytes);
            Ok(())
        }

        fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            self.erases = self.erases.saturating_add(1);
            self.last_erase = Some((from, to));
            self.take_failure(Operation::Erase)?;
            let len = to.checked_sub(from).ok_or(FakeError::OutOfBounds)?;
            let start = self.check_range(from, len as usize)?;
            self.bytes[start..start + len as usize].fill(0xff);
            Ok(())
        }
    }

    impl MultiwriteNorFlash for FakeFlash {}

    #[test]
    fn reads_translate_both_exact_partition_boundaries() {
        let mut flash = FakeFlash::sequenced();
        {
            let mut region = PartitionNorFlash::new(&mut flash, PARTITION_START, PARTITION_LEN);
            let mut first = [0_u8; 4];
            let mut last = [0_u8; 4];
            region.read(0, &mut first).unwrap();
            region.read(PARTITION_LEN - 4, &mut last).unwrap();
            assert_eq!(first, [16, 17, 18, 19]);
            assert_eq!(last, [36, 37, 38, 39]);
            assert_eq!(region.capacity(), PARTITION_LEN as usize);
        }
        assert_eq!(flash.reads, 2);
        assert_eq!(
            flash.last_read,
            Some((PARTITION_START + PARTITION_LEN - 4, 4))
        );
    }

    #[test]
    fn writes_translate_both_exact_partition_boundaries_and_count_attempts() {
        let mut flash = FakeFlash::sequenced();
        {
            let mut region = PartitionNorFlash::new(&mut flash, PARTITION_START, PARTITION_LEN);
            region.write(0, &[0xa0, 0xa1]).unwrap();
            region.write(PARTITION_LEN - 2, &[0xbe, 0xbf]).unwrap();
            assert_eq!(region.write_calls(), 2);
            assert_eq!(region.erase_calls(), 0);
        }
        assert_eq!(&flash.bytes[16..18], &[0xa0, 0xa1]);
        assert_eq!(&flash.bytes[38..40], &[0xbe, 0xbf]);
        assert_eq!(flash.bytes[15], 15);
        assert_eq!(flash.bytes[40], 40);
        assert_eq!(flash.writes, 2);
        assert_eq!(flash.last_write, Some((38, 2)));
    }

    #[test]
    fn erase_accepts_the_exclusive_partition_end_without_touching_neighbors() {
        let mut flash = FakeFlash::sequenced();
        {
            let mut region = PartitionNorFlash::new(&mut flash, PARTITION_START, PARTITION_LEN);
            region.erase(0, PARTITION_LEN).unwrap();
            assert_eq!(region.erase_calls(), 1);
            assert_eq!(region.write_calls(), 0);
        }
        assert!(flash.bytes[16..40].iter().all(|byte| *byte == 0xff));
        assert_eq!(flash.bytes[15], 15);
        assert_eq!(flash.bytes[40], 40);
        assert_eq!(flash.erases, 1);
        assert_eq!(flash.last_erase, Some((16, 40)));
    }

    #[test]
    fn relative_out_of_bounds_requests_never_reach_the_backend_or_increment_counters() {
        let mut flash = FakeFlash::sequenced();
        {
            let mut region = PartitionNorFlash::new(&mut flash, PARTITION_START, PARTITION_LEN);
            assert_eq!(
                region.read(PARTITION_LEN, &mut [0_u8; 1]),
                Err(RegionError::OutOfBounds)
            );
            assert_eq!(
                region.write(PARTITION_LEN - 1, &[1, 2]),
                Err(RegionError::OutOfBounds)
            );
            assert_eq!(region.erase(8, 4), Err(RegionError::OutOfBounds));
            assert_eq!(
                region.erase(0, PARTITION_LEN + 1),
                Err(RegionError::OutOfBounds)
            );
            assert_eq!(region.write_calls(), 0);
            assert_eq!(region.erase_calls(), 0);
        }
        assert_eq!((flash.reads, flash.writes, flash.erases), (0, 0, 0));
    }

    #[test]
    fn relative_and_absolute_address_overflow_are_rejected_locally() {
        let mut flash = FakeFlash::sequenced();
        {
            let mut relative = PartitionNorFlash::new(&mut flash, 0, u32::MAX);
            assert_eq!(
                relative.read(u32::MAX, &mut [0_u8; 1]),
                Err(RegionError::OutOfBounds)
            );
        }
        {
            let mut absolute = PartitionNorFlash::new(&mut flash, u32::MAX - 4, 8);
            assert_eq!(absolute.write(2, &[1, 2, 3]), Err(RegionError::OutOfBounds));
            assert_eq!(absolute.write(6, &[1]), Err(RegionError::OutOfBounds));
            assert_eq!(absolute.erase(0, 8), Err(RegionError::OutOfBounds));
            assert_eq!(absolute.write_calls(), 0);
            assert_eq!(absolute.erase_calls(), 0);
        }
        assert_eq!((flash.reads, flash.writes, flash.erases), (0, 0, 0));
    }

    #[test]
    fn backend_errors_preserve_their_error_kind_and_attempt_accounting() {
        let mut flash = FakeFlash::sequenced();
        {
            let mut region = PartitionNorFlash::new(&mut flash, PARTITION_START, PARTITION_LEN);

            region.flash.fail_once(Operation::Read);
            let read = region.read(0, &mut [0_u8; 1]).unwrap_err();
            assert_eq!(
                read,
                RegionError::Backend(FakeError::Injected(Operation::Read))
            );
            assert_eq!(read.kind(), NorFlashErrorKind::Other);

            region.flash.fail_once(Operation::Write);
            let write = region.write(0, &[0]).unwrap_err();
            assert_eq!(
                write,
                RegionError::Backend(FakeError::Injected(Operation::Write))
            );
            assert_eq!(region.write_calls(), 1);

            region.flash.fail_once(Operation::Erase);
            let erase = region.erase(0, 4).unwrap_err();
            assert_eq!(
                erase,
                RegionError::Backend(FakeError::Injected(Operation::Erase))
            );
            assert_eq!(region.erase_calls(), 1);
        }
        assert_eq!((flash.reads, flash.writes, flash.erases), (1, 1, 1));
    }

    #[test]
    fn raw_nor_geometry_and_multiwrite_capability_are_forwarded() {
        fn require_multiwrite<F: MultiwriteNorFlash>(_flash: &mut F) {}

        let mut flash = FakeFlash::sequenced();
        let mut region = PartitionNorFlash::new(&mut flash, PARTITION_START, PARTITION_LEN);
        assert_eq!(
            <PartitionNorFlash<'_, FakeFlash> as ReadNorFlash>::READ_SIZE,
            1
        );
        assert_eq!(
            <PartitionNorFlash<'_, FakeFlash> as NorFlash>::WRITE_SIZE,
            1
        );
        assert_eq!(
            <PartitionNorFlash<'_, FakeFlash> as NorFlash>::ERASE_SIZE,
            4
        );
        require_multiwrite(&mut region);
    }
}
