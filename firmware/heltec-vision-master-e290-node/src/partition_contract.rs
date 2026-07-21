//! Exact permanent-image flash partition contract.

/// Physical flash capacity required by the E290 product image.
pub const REQUIRED_FLASH_BYTES: usize = 16 * 1024 * 1024;

/// ESP-IDF raw data-partition type.
pub const DATA_PARTITION_TYPE: u8 = 0x01;
/// ESP-IDF undefined raw data-partition subtype.
pub const UNDEFINED_DATA_SUBTYPE: u8 = 0x06;
/// ESP-IDF standard NVS data-partition subtype.
pub const NVS_DATA_SUBTYPE: u8 = 0x02;

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

/// Dedicated device-API credential-snapshot partition label.
pub const API_CREDENTIALS_LABEL: &str = "api_credentials";
/// Padded partition-table bytes for [`API_CREDENTIALS_LABEL`].
pub const API_CREDENTIALS_LABEL_BYTES: [u8; 16] = [
    b'a', b'p', b'i', b'_', b'c', b'r', b'e', b'd', b'e', b'n', b't', b'i', b'a', b'l', b's', 0,
];
/// Credential-snapshot partition absolute flash offset.
pub const API_CREDENTIALS_OFFSET: u32 = 0x0061_4000;
/// Credential-snapshot partition length: exactly two 4 KiB erase sectors.
pub const API_CREDENTIALS_LEN: u32 = 0x0000_2000;

/// Reserved future configuration partition label.
pub const DEVICE_CONFIG_LABEL: &str = "device_config";
/// Padded partition-table bytes for [`DEVICE_CONFIG_LABEL`].
pub const DEVICE_CONFIG_LABEL_BYTES: [u8; 16] = [
    b'd', b'e', b'v', b'i', b'c', b'e', b'_', b'c', b'o', b'n', b'f', b'i', b'g', 0, 0, 0,
];
/// Reserved future configuration partition absolute flash offset.
pub const DEVICE_CONFIG_OFFSET: u32 = 0x0061_6000;
/// Reserved future configuration partition length.
pub const DEVICE_CONFIG_LEN: u32 = 0x0001_a000;

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

/// Durable inbound-message qualification partition label.
pub const MESSAGE_STORE_LABEL: &str = "message_store";
/// Padded partition-table bytes for [`MESSAGE_STORE_LABEL`].
pub const MESSAGE_STORE_LABEL_BYTES: [u8; 16] = [
    b'm', b'e', b's', b's', b'a', b'g', b'e', b'_', b's', b't', b'o', b'r', b'e', 0, 0, 0,
];
/// Inbound-message store partition absolute flash offset.
pub const MESSAGE_STORE_OFFSET: u32 = 0x0073_0000;
/// Inbound-message store partition length: exactly 2 MiB.
pub const MESSAGE_STORE_LEN: u32 = 0x0020_0000;

/// Permanent append-only LXMF message-store partition label.
pub const LXMF_STORE_LABEL: &str = "lxmf_store";
/// Padded partition-table bytes for [`LXMF_STORE_LABEL`].
pub const LXMF_STORE_LABEL_BYTES: [u8; 16] = [
    b'l', b'x', b'm', b'f', b'_', b's', b't', b'o', b'r', b'e', 0, 0, 0, 0, 0, 0,
];
/// LXMF message-store partition absolute flash offset.
pub const LXMF_STORE_OFFSET: u32 = 0x0093_0000;
/// LXMF message-store partition length: exactly 2 MiB.
pub const LXMF_STORE_LEN: u32 = 0x0020_0000;

const _: () = assert!(NODE_IDENTITY_OFFSET + NODE_IDENTITY_LEN == ANNOUNCE_CLOCK_OFFSET);
const _: () = assert!(ANNOUNCE_CLOCK_OFFSET + ANNOUNCE_CLOCK_LEN == API_CREDENTIALS_OFFSET);
const _: () = assert!(API_CREDENTIALS_OFFSET.is_multiple_of(0x1000));
const _: () = assert!(API_CREDENTIALS_LEN == 2 * 0x1000);
const _: () = assert!(API_CREDENTIALS_OFFSET + API_CREDENTIALS_LEN == DEVICE_CONFIG_OFFSET);
const _: () = assert!(DEVICE_CONFIG_OFFSET + DEVICE_CONFIG_LEN == NODE_JOURNAL_OFFSET);
const _: () = assert!(NODE_JOURNAL_OFFSET + NODE_JOURNAL_LEN == MESSAGE_STORE_OFFSET);
const _: () = assert!(MESSAGE_STORE_OFFSET.is_multiple_of(0x1000));
const _: () = assert!(MESSAGE_STORE_LEN.is_multiple_of(0x1000));
const _: () = assert!(MESSAGE_STORE_OFFSET + MESSAGE_STORE_LEN == LXMF_STORE_OFFSET);
const _: () = assert!(LXMF_STORE_OFFSET.is_multiple_of(0x1000));
const _: () = assert!(LXMF_STORE_LEN.is_multiple_of(0x1000));
const _: () = assert!(LXMF_STORE_OFFSET + LXMF_STORE_LEN == 0x00b3_0000);
