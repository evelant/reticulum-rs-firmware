//! Exact permanent-image flash partition contract.

/// Physical flash capacity required by the E290 product image.
pub const REQUIRED_FLASH_BYTES: usize = 16 * 1024 * 1024;

/// ESP-IDF raw data-partition type.
pub const DATA_PARTITION_TYPE: u8 = 0x01;
/// ESP-IDF undefined raw data-partition subtype.
pub const UNDEFINED_DATA_SUBTYPE: u8 = 0x06;

/// Dedicated immutable Reticulum identity partition label.
pub const NODE_IDENTITY_LABEL: &str = "node_identity";
/// Padded partition-table bytes for [`NODE_IDENTITY_LABEL`].
pub const NODE_IDENTITY_LABEL_BYTES: [u8; 16] = [
    b'n', b'o', b'd', b'e', b'_', b'i', b'd', b'e', b'n', b't', b'i', b't', b'y', 0, 0, 0,
];
/// Identity partition absolute flash offset.
pub const NODE_IDENTITY_OFFSET: u32 = 0x0061_0000;
/// Identity partition length: exactly two 4 KiB erase sectors.
pub const NODE_IDENTITY_LEN: u32 = 0x0000_2000;

/// Durable announce-emission boot-clock partition label.
pub const ANNOUNCE_CLOCK_LABEL: &str = "announce_clock";
/// Padded partition-table bytes for [`ANNOUNCE_CLOCK_LABEL`].
pub const ANNOUNCE_CLOCK_LABEL_BYTES: [u8; 16] = [
    b'a', b'n', b'n', b'o', b'u', b'n', b'c', b'e', b'_', b'c', b'l', b'o', b'c', b'k', 0, 0,
];
/// Announce-clock partition absolute flash offset.
pub const ANNOUNCE_CLOCK_OFFSET: u32 = 0x0061_2000;
/// Announce-clock partition length: exactly two 4 KiB erase sectors.
pub const ANNOUNCE_CLOCK_LEN: u32 = 0x0000_2000;

/// Reserved future configuration partition label.
pub const DEVICE_CONFIG_LABEL: &str = "device_config";
/// Reserved future configuration partition absolute flash offset.
pub const DEVICE_CONFIG_OFFSET: u32 = 0x0061_4000;
/// Reserved future configuration partition length.
pub const DEVICE_CONFIG_LEN: u32 = 0x0001_c000;

/// Durable submission-journal partition label.
pub const NODE_JOURNAL_LABEL: &str = "node_journal";
/// Padded partition-table bytes for [`NODE_JOURNAL_LABEL`].
pub const NODE_JOURNAL_LABEL_BYTES: [u8; 16] = [
    b'n', b'o', b'd', b'e', b'_', b'j', b'o', b'u', b'r', b'n', b'a', b'l', 0, 0, 0, 0,
];
/// Submission-journal partition absolute flash offset.
pub const NODE_JOURNAL_OFFSET: u32 = 0x0063_0000;
/// Submission-journal partition length: exactly one journal format partition.
pub const NODE_JOURNAL_LEN: u32 = 0x0010_0000;

const _: () = assert!(NODE_IDENTITY_OFFSET + NODE_IDENTITY_LEN == ANNOUNCE_CLOCK_OFFSET);
const _: () = assert!(ANNOUNCE_CLOCK_OFFSET + ANNOUNCE_CLOCK_LEN == DEVICE_CONFIG_OFFSET);
const _: () = assert!(DEVICE_CONFIG_OFFSET + DEVICE_CONFIG_LEN == NODE_JOURNAL_OFFSET);
const _: () = assert!(NODE_JOURNAL_OFFSET + NODE_JOURNAL_LEN == 0x0073_0000);
