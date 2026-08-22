//! One physical ESP flash owner shared by disjoint product and PRNS regions.
//!
//! The current product stores use blocking `embedded-storage` traits, while
//! PRNS persistence uses `embedded-storage-async`. Both clients below execute
//! each physical operation through the same ESP-aware non-reentrant lock. The
//! async client yields only after the blocking flash operation has released
//! that lock.

use embassy_futures::yield_now;
use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash as BlockingMultiwriteNorFlash, NorFlash as BlockingNorFlash,
    ReadNorFlash as BlockingReadNorFlash,
};
use embedded_storage::{ReadStorage, Storage};
use embedded_storage_async::nor_flash::{
    MultiwriteNorFlash as AsyncMultiwriteNorFlash, NorFlash as AsyncNorFlash,
    ReadNorFlash as AsyncReadNorFlash,
};
use esp_bootloader_esp_idf::ota::OtaImageState;
use esp_bootloader_esp_idf::ota_updater::OtaUpdater;
use esp_bootloader_esp_idf::partitions::{
    AppPartitionSubType, Error as PartitionTableError, PARTITION_TABLE_MAX_LEN, PartitionTable,
    read_partition_table,
};
use esp_storage::{FlashStorage, FlashStorageError};
use esp_sync::NonReentrantMutex;
use static_cell::StaticCell;

struct PhysicalFlash {
    flash: NonReentrantMutex<FlashStorage<'static>>,
}

impl PhysicalFlash {
    const fn new(flash: FlashStorage<'static>) -> Self {
        Self {
            flash: NonReentrantMutex::new(flash),
        }
    }

    fn with<R>(&self, operation: impl FnOnce(&mut FlashStorage<'static>) -> R) -> R {
        self.flash.with(operation)
    }
}

static PHYSICAL_FLASH: StaticCell<PhysicalFlash> = StaticCell::new();

/// Blocking product-store view of the sole physical flash owner.
#[derive(Clone, Copy)]
pub(crate) struct E290ProductFlash {
    physical: &'static PhysicalFlash,
}

/// Async PRNS-persistence view of the sole physical flash owner.
#[derive(Clone, Copy)]
pub(crate) struct E290PrnsFlash {
    physical: &'static PhysicalFlash,
}

/// The two trait views created while moving the only `FlashStorage` value.
pub(crate) struct E290FlashClients {
    pub(crate) product: E290ProductFlash,
    pub(crate) prns: E290PrnsFlash,
}

/// Move the sole ESP flash peripheral into its permanent serialized owner.
pub(crate) fn share_flash(flash: FlashStorage<'static>) -> E290FlashClients {
    let physical = PHYSICAL_FLASH.init(PhysicalFlash::new(flash));
    E290FlashClients {
        product: E290ProductFlash { physical },
        prns: E290PrnsFlash { physical },
    }
}

impl E290ProductFlash {
    /// Read and validate the bootloader partition table through the same owner.
    pub(crate) fn read_partition_table<'a>(
        &mut self,
        storage: &'a mut [u8],
    ) -> Result<PartitionTable<'a>, PartitionTableError> {
        self.physical
            .with(|flash| read_partition_table(flash, storage))
    }

    /// Read the bootloader's state for the currently selected OTA image.
    pub(crate) fn current_ota_state(&mut self) -> Result<OtaImageState, PartitionTableError> {
        self.physical.with(|flash| {
            let mut storage = [0_u8; PARTITION_TABLE_MAX_LEN];
            OtaUpdater::new(flash, &mut storage)?.current_ota_state()
        })
    }

    /// Update the bootloader state for the currently selected OTA image.
    pub(crate) fn set_current_ota_state(
        &mut self,
        state: OtaImageState,
    ) -> Result<(), PartitionTableError> {
        self.physical.with(|flash| {
            let mut storage = [0_u8; PARTITION_TABLE_MAX_LEN];
            OtaUpdater::new(flash, &mut storage)?.set_current_ota_state(state)
        })
    }

    /// Return the app partition that the bootloader would select next.
    pub(crate) fn next_ota_partition(
        &mut self,
    ) -> Result<AppPartitionSubType, PartitionTableError> {
        self.physical.with(|flash| {
            let mut storage = [0_u8; PARTITION_TABLE_MAX_LEN];
            let (_, partition) = OtaUpdater::new(flash, &mut storage)?.next_partition()?;
            Ok(partition)
        })
    }

    /// Select the bootloader's next OTA partition.
    pub(crate) fn activate_next_ota_partition(&mut self) -> Result<(), PartitionTableError> {
        self.physical.with(|flash| {
            let mut storage = [0_u8; PARTITION_TABLE_MAX_LEN];
            OtaUpdater::new(flash, &mut storage)?.activate_next_partition()
        })
    }
}

impl ReadStorage for E290ProductFlash {
    type Error = FlashStorageError;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.physical
            .with(|flash| ReadStorage::read(flash, offset, bytes))
    }

    fn capacity(&self) -> usize {
        self.physical.with(|flash| ReadStorage::capacity(flash))
    }
}

impl Storage for E290ProductFlash {
    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.physical
            .with(|flash| Storage::write(flash, offset, bytes))
    }
}

impl ErrorType for E290ProductFlash {
    type Error = FlashStorageError;
}

impl BlockingReadNorFlash for E290ProductFlash {
    const READ_SIZE: usize = <FlashStorage<'static> as BlockingReadNorFlash>::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.physical
            .with(|flash| BlockingReadNorFlash::read(flash, offset, bytes))
    }

    fn capacity(&self) -> usize {
        self.physical
            .with(|flash| BlockingReadNorFlash::capacity(flash))
    }
}

impl BlockingNorFlash for E290ProductFlash {
    const WRITE_SIZE: usize = <FlashStorage<'static> as BlockingNorFlash>::WRITE_SIZE;
    const ERASE_SIZE: usize = <FlashStorage<'static> as BlockingNorFlash>::ERASE_SIZE;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.physical
            .with(|flash| BlockingNorFlash::erase(flash, from, to))
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.physical
            .with(|flash| BlockingNorFlash::write(flash, offset, bytes))
    }
}

impl BlockingMultiwriteNorFlash for E290ProductFlash {}

impl ErrorType for E290PrnsFlash {
    type Error = FlashStorageError;
}

impl AsyncReadNorFlash for E290PrnsFlash {
    const READ_SIZE: usize = <FlashStorage<'static> as BlockingReadNorFlash>::READ_SIZE;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let result = self
            .physical
            .with(|flash| BlockingReadNorFlash::read(flash, offset, bytes));
        yield_now().await;
        result
    }

    fn capacity(&self) -> usize {
        self.physical
            .with(|flash| BlockingReadNorFlash::capacity(flash))
    }
}

impl AsyncNorFlash for E290PrnsFlash {
    const WRITE_SIZE: usize = <FlashStorage<'static> as BlockingNorFlash>::WRITE_SIZE;
    const ERASE_SIZE: usize = <FlashStorage<'static> as BlockingNorFlash>::ERASE_SIZE;

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let result = self
            .physical
            .with(|flash| BlockingNorFlash::erase(flash, from, to));
        yield_now().await;
        result
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        let result = self
            .physical
            .with(|flash| BlockingNorFlash::write(flash, offset, bytes));
        yield_now().await;
        result
    }
}

impl AsyncMultiwriteNorFlash for E290PrnsFlash {}
