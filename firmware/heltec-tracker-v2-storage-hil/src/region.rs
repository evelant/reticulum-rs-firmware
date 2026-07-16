//! Checked partition-relative access to raw NOR flash.
//!
//! This deliberately does not use `esp_bootloader_esp_idf::partitions::FlashRegion`:
//! that adapter rejects an erase whose exclusive end is the partition end in
//! the version pinned by this workspace. It also deliberately implements only
//! raw NOR traits, so the sector-rewriting `embedded_storage::Storage::write`
//! path cannot be selected accidentally.

use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegionError<E> {
    Backend(E),
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

/// A checked relative view over one raw partition.
pub struct PartitionNorFlash<'a, F> {
    flash: &'a mut F,
    start: u32,
    len: u32,
    write_calls: u32,
    erase_calls: u32,
}

impl<'a, F> PartitionNorFlash<'a, F> {
    pub const fn new(flash: &'a mut F, start: u32, len: u32) -> Self {
        Self {
            flash,
            start,
            len,
            write_calls: 0,
            erase_calls: 0,
        }
    }

    /// Number of raw program calls attempted through this view this boot.
    pub const fn write_calls(&self) -> u32 {
        self.write_calls
    }

    /// Number of raw erase calls attempted through this view this boot.
    pub const fn erase_calls(&self) -> u32 {
        self.erase_calls
    }

    fn absolute_offset<E>(&self, offset: u32, len: usize) -> Result<u32, RegionError<E>> {
        let len = u32::try_from(len).map_err(|_| RegionError::OutOfBounds)?;
        let relative_end = offset.checked_add(len).ok_or(RegionError::OutOfBounds)?;
        if relative_end > self.len {
            return Err(RegionError::OutOfBounds);
        }
        self.start
            .checked_add(offset)
            .ok_or(RegionError::OutOfBounds)
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
