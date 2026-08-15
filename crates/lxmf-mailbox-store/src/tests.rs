use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
};

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FakeError;

impl NorFlashError for FakeError {
    fn kind(&self) -> NorFlashErrorKind {
        NorFlashErrorKind::Other
    }
}

struct FakeFlash {
    bytes: [u8; PARTITION_SIZE],
    fail_write_after: Option<usize>,
    writes: usize,
}

impl FakeFlash {
    fn erased() -> Self {
        Self {
            bytes: [0xff; PARTITION_SIZE],
            fail_write_after: None,
            writes: 0,
        }
    }

    fn bound(&mut self) -> BoundMailboxStore<&mut Self> {
        BoundMailboxStore::new(self, binding())
    }
}

impl ErrorType for FakeFlash {
    type Error = FakeError;
}

impl ReadNorFlash for FakeFlash {
    const READ_SIZE: usize = 1;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let start = offset as usize;
        bytes.copy_from_slice(&self.bytes[start..start + bytes.len()]);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.bytes.len()
    }
}

impl NorFlash for FakeFlash {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = SECTOR_SIZE;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        if self.fail_write_after == Some(self.writes) {
            return Err(FakeError);
        }
        self.writes += 1;
        let start = offset as usize;
        for (target, source) in self.bytes[start..start + bytes.len()].iter_mut().zip(bytes) {
            if *target & *source != *source {
                return Err(FakeError);
            }
            *target &= *source;
        }
        Ok(())
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.bytes[from as usize..to as usize].fill(0xff);
        Ok(())
    }
}

impl MultiwriteNorFlash for FakeFlash {}
impl MultiwriteNorFlash for &mut FakeFlash {}

fn binding() -> MailboxStoreBinding {
    MailboxStoreBinding::new(
        MailboxStoreDeviceId::new([0x31; 16]),
        0x61_a000,
        PARTITION_SIZE,
        PHYSICAL_FORMAT_VERSION,
    )
}

fn cursor(handle: u64, identity: u8) -> MailboxStoreCursor {
    MailboxStoreCursor::new(
        NonZeroU64::new(handle).unwrap(),
        [identity; 32],
        [identity.wrapping_add(1); 32],
    )
}

#[test]
fn erased_media_provisions_a_migration_baseline_and_remounts() {
    let mut flash = FakeFlash::erased();
    let baseline = cursor(17, 0x41);
    {
        let mut access = flash.bound();
        assert_eq!(
            mount(&mut access).unwrap(),
            MailboxStoreMount::Unprovisioned
        );
        let mounted = provision(&mut access, Some(baseline)).unwrap();
        assert_eq!(mounted.generation(), NonZeroU64::MIN);
        assert_eq!(mounted.acknowledged_through(), 17);
        assert_eq!(mounted.acknowledged_cursor(), Some(baseline));
    }
    let mut access = flash.bound();
    let MailboxStoreMount::Mounted(remounted) = mount(&mut access).unwrap() else {
        panic!("provisioned watermark must remount");
    };
    assert_eq!(remounted.acknowledged_through(), 17);
    assert_eq!(remounted.acknowledged_cursor(), Some(baseline));
}

#[test]
fn acknowledgements_advance_monotonically_and_regressions_are_io_free() {
    let mut flash = FakeFlash::erased();
    let first = {
        let mut access = flash.bound();
        provision(&mut access, None).unwrap()
    };
    let advanced = cursor(3, 0x51);
    let second = {
        let mut access = flash.bound();
        acknowledge_through(first, &mut access, advanced).unwrap()
    };
    assert_eq!(second.acknowledged_through(), 3);
    assert_eq!(second.acknowledged_cursor(), Some(advanced));
    assert_eq!(second.generation().get(), 2);
    let writes = flash.writes;
    let unchanged = {
        let mut access = flash.bound();
        acknowledge_through(second, &mut access, cursor(2, 0x61)).unwrap()
    };
    assert_eq!(unchanged, second);
    assert_eq!(flash.writes, writes);

    let mut access = flash.bound();
    let MailboxStoreMount::Mounted(remounted) = mount(&mut access).unwrap() else {
        panic!("successor must remount");
    };
    assert_eq!(remounted.acknowledged_through(), 3);
}

#[test]
fn interrupted_successor_preserves_the_prior_committed_watermark() {
    let mut flash = FakeFlash::erased();
    let first = {
        let mut access = flash.bound();
        provision(&mut access, Some(cursor(4, 0x71))).unwrap()
    };
    flash.fail_write_after = Some(flash.writes + 1);
    {
        let mut access = flash.bound();
        assert!(matches!(
            acknowledge_through(first, &mut access, cursor(9, 0x72)),
            Err(MailboxStoreError::Backend(FakeError))
        ));
    }
    flash.fail_write_after = None;

    let mut access = flash.bound();
    let MailboxStoreMount::Mounted(remounted) = mount(&mut access).unwrap() else {
        panic!("prior snapshot must survive a torn successor");
    };
    assert_eq!(remounted.acknowledged_through(), 4);
}

#[test]
fn erase_and_handle_reuse_can_rebaseline_to_a_new_receipt_identity() {
    let mut flash = FakeFlash::erased();
    let old_incarnation = cursor(7, 0x81);
    let first = {
        let mut access = flash.bound();
        provision(&mut access, Some(old_incarnation)).unwrap()
    };

    // A reformatted LXMF store can reuse handle 7 for a different receipt.
    // The product detects that mismatch and durably rebinds this store before
    // admitting future ingress.
    let new_incarnation = cursor(7, 0x91);
    let rebound = {
        let mut access = flash.bound();
        rebaseline(first, &mut access, Some(new_incarnation)).unwrap()
    };
    assert_eq!(rebound.generation().get(), 2);
    assert_eq!(rebound.acknowledged_cursor(), Some(new_incarnation));

    let mut access = flash.bound();
    let MailboxStoreMount::Mounted(remounted) = mount(&mut access).unwrap() else {
        panic!("rebaselined incarnation must remount");
    };
    assert_eq!(remounted.acknowledged_cursor(), Some(new_incarnation));
}

#[test]
fn equal_handle_with_conflicting_identity_is_not_an_idempotent_acknowledgement() {
    let mut flash = FakeFlash::erased();
    let first = {
        let mut access = flash.bound();
        provision(&mut access, Some(cursor(5, 0xa1))).unwrap()
    };
    let writes = flash.writes;
    {
        let mut access = flash.bound();
        assert_eq!(
            acknowledge_through(first, &mut access, cursor(5, 0xa2)),
            Err(MailboxStoreError::Fault(
                MailboxStoreFault::CursorIdentityConflict
            ))
        );
    }
    assert_eq!(flash.writes, writes);
}
