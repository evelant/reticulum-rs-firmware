//! Exact E290 flash partition and product-store quota contract.
//!
//! The bootloader sees two OTA slots and two state arenas. Application formats
//! live behind typed quotas inside `product_state`; enabling another Reticulum
//! application never changes the physical partition table.

/// Physical flash capacity required by the E290 product image.
pub const REQUIRED_FLASH_BYTES: usize = 16 * 1024 * 1024;

/// ESP-IDF application partition type.
pub const APP_PARTITION_TYPE: u8 = 0x00;
/// ESP-IDF raw data-partition type.
pub const DATA_PARTITION_TYPE: u8 = 0x01;
/// ESP-IDF OTA data-partition subtype.
pub const OTA_DATA_SUBTYPE: u8 = 0x00;
/// ESP-IDF undefined raw data-partition subtype.
pub const UNDEFINED_DATA_SUBTYPE: u8 = 0x06;
/// First ESP-IDF OTA application subtype.
pub const OTA_0_SUBTYPE: u8 = 0x10;
/// Second ESP-IDF OTA application subtype.
pub const OTA_1_SUBTYPE: u8 = 0x11;

/// First OTA slot label.
pub const OTA_0_LABEL: &str = "ota_0";
/// Padded partition-table bytes for [`OTA_0_LABEL`].
pub const OTA_0_LABEL_BYTES: [u8; 16] = [
    b'o', b't', b'a', b'_', b'0', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];
/// First OTA slot offset.
pub const OTA_0_OFFSET: u32 = 0x0001_0000;
/// First OTA slot length.
pub const OTA_0_LEN: u32 = 0x0050_0000;

/// Second OTA slot label.
pub const OTA_1_LABEL: &str = "ota_1";
/// Padded partition-table bytes for [`OTA_1_LABEL`].
pub const OTA_1_LABEL_BYTES: [u8; 16] = [
    b'o', b't', b'a', b'_', b'1', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];
/// Second OTA slot offset.
pub const OTA_1_OFFSET: u32 = 0x0051_0000;
/// Second OTA slot length.
pub const OTA_1_LEN: u32 = 0x0050_0000;

/// OTA selection record label.
pub const OTA_DATA_LABEL: &str = "otadata";
/// Padded partition-table bytes for [`OTA_DATA_LABEL`].
pub const OTA_DATA_LABEL_BYTES: [u8; 16] = [
    b'o', b't', b'a', b'd', b'a', b't', b'a', 0, 0, 0, 0, 0, 0, 0, 0, 0,
];
/// OTA selection record offset.
pub const OTA_DATA_OFFSET: u32 = 0x00a1_0000;
/// OTA selection record length.
pub const OTA_DATA_LEN: u32 = 0x0000_2000;

/// Application-independent product-state arena label.
pub const PRODUCT_STATE_LABEL: &str = "product_state";
/// Padded partition-table bytes for [`PRODUCT_STATE_LABEL`].
pub const PRODUCT_STATE_LABEL_BYTES: [u8; 16] = [
    b'p', b'r', b'o', b'd', b'u', b'c', b't', b'_', b's', b't', b'a', b't', b'e', 0, 0, 0,
];
/// Product-state arena offset.
pub const PRODUCT_STATE_OFFSET: u32 = 0x00a1_2000;
/// Product-state arena length.
pub const PRODUCT_STATE_LEN: u32 = 0x0046_e000;

/// Application-independent PRNS route, ratchet, and timebase arena label.
pub const PRNS_STATE_LABEL: &str = "prns_state";
/// Padded partition-table bytes for [`PRNS_STATE_LABEL`].
pub const PRNS_STATE_LABEL_BYTES: [u8; 16] = [
    b'p', b'r', b'n', b's', b'_', b's', b't', b'a', b't', b'e', 0, 0, 0, 0, 0, 0,
];
/// PRNS state offset.
pub const PRNS_STATE_OFFSET: u32 = 0x00e8_0000;
/// PRNS state length.
pub const PRNS_STATE_LEN: u32 = 0x0018_0000;

/// Mirrored Reticulum identity quota within `product_state`.
pub const NODE_IDENTITY_OFFSET: u32 = PRODUCT_STATE_OFFSET;
/// Identity quota length.
pub const NODE_IDENTITY_LEN: u32 = 0x0000_2000;
/// Network configuration quota within `product_state`.
pub const NETWORK_CONFIG_OFFSET: u32 = NODE_IDENTITY_OFFSET + NODE_IDENTITY_LEN;
/// Network configuration quota length.
pub const NETWORK_CONFIG_LEN: u32 = 0x0000_2000;
/// Reserved product metadata and application registry quota.
pub const PRODUCT_METADATA_OFFSET: u32 = NETWORK_CONFIG_OFFSET + NETWORK_CONFIG_LEN;
/// Reserved product metadata and application registry quota length.
pub const PRODUCT_METADATA_LEN: u32 = 0x0006_a000;
/// Durable management-identity allow-list quota inside product metadata.
///
/// This is a product-store allocation, not a physical flash partition. It is
/// present regardless of which Reticulum applications are enabled.
pub const MANAGEMENT_AUTHORIZATION_OFFSET: u32 = PRODUCT_METADATA_OFFSET;
/// Two-sector mirrored management-identity allow-list quota length.
pub const MANAGEMENT_AUTHORIZATION_LEN: u32 = 0x0000_2000;
/// Remaining product metadata available to the app registry and future typed
/// product-store allocations.
pub const PRODUCT_REGISTRY_OFFSET: u32 =
    MANAGEMENT_AUTHORIZATION_OFFSET + MANAGEMENT_AUTHORIZATION_LEN;
/// Remaining app-neutral product metadata length.
pub const PRODUCT_REGISTRY_LEN: u32 = PRODUCT_METADATA_LEN - MANAGEMENT_AUTHORIZATION_LEN;
/// Durable LXMF mailbox watermark quota inside the product registry.
///
/// This is a logical application-store allocation. It is deliberately not a
/// bootloader partition and does not vary with the enabled application set.
pub const LXMF_MAILBOX_STATE_OFFSET: u32 = PRODUCT_REGISTRY_OFFSET;
/// Two-sector mirrored LXMF mailbox watermark quota length.
pub const LXMF_MAILBOX_STATE_LEN: u32 = 0x0000_2000;
/// Durable LXMF outbound-intent quota inside the product registry.
pub const LXMF_OUTBOX_OFFSET: u32 = LXMF_MAILBOX_STATE_OFFSET + LXMF_MAILBOX_STATE_LEN;
/// Initial 64-record LXMF outbound-intent quota length.
pub const LXMF_OUTBOX_LEN: u32 = 0x0004_0000;
/// Product-owned appliance-settings quota inside the generic registry.
///
/// The label stored here identifies the physical appliance in product UI. It
/// is not Reticulum destination announce data and is not owned by PRNS.
pub const APPLIANCE_SETTINGS_OFFSET: u32 = LXMF_OUTBOX_OFFSET + LXMF_OUTBOX_LEN;
/// Two-sector mirrored appliance-settings quota length.
pub const APPLIANCE_SETTINGS_LEN: u32 = 0x0000_2000;
/// Unassigned product-registry space available for additional typed stores.
pub const PRODUCT_REGISTRY_FREE_OFFSET: u32 = APPLIANCE_SETTINGS_OFFSET + APPLIANCE_SETTINGS_LEN;
/// Unassigned product-registry length.
pub const PRODUCT_REGISTRY_FREE_LEN: u32 =
    PRODUCT_REGISTRY_LEN - LXMF_MAILBOX_STATE_LEN - LXMF_OUTBOX_LEN - APPLIANCE_SETTINGS_LEN;
/// Initial LXMF payload-log quota within the stable product arena.
pub const LXMF_STORE_OFFSET: u32 = PRODUCT_METADATA_OFFSET + PRODUCT_METADATA_LEN;
/// Initial LXMF payload-log quota length.
pub const LXMF_STORE_LEN: u32 = 0x0040_0000;

const _: () = assert!(OTA_0_OFFSET + OTA_0_LEN == OTA_1_OFFSET);
const _: () = assert!(OTA_1_OFFSET + OTA_1_LEN == OTA_DATA_OFFSET);
const _: () = assert!(OTA_DATA_OFFSET + OTA_DATA_LEN == PRODUCT_STATE_OFFSET);
const _: () = assert!(PRODUCT_STATE_OFFSET + PRODUCT_STATE_LEN == PRNS_STATE_OFFSET);
const _: () = assert!(NODE_IDENTITY_OFFSET + NODE_IDENTITY_LEN == NETWORK_CONFIG_OFFSET);
const _: () = assert!(NETWORK_CONFIG_OFFSET + NETWORK_CONFIG_LEN == PRODUCT_METADATA_OFFSET);
const _: () = assert!(MANAGEMENT_AUTHORIZATION_OFFSET == PRODUCT_METADATA_OFFSET);
const _: () = assert!(MANAGEMENT_AUTHORIZATION_LEN == 2 * 4096);
const _: () = assert!(PRODUCT_REGISTRY_OFFSET + PRODUCT_REGISTRY_LEN == LXMF_STORE_OFFSET);
const _: () = assert!(LXMF_MAILBOX_STATE_OFFSET == PRODUCT_REGISTRY_OFFSET);
const _: () = assert!(LXMF_MAILBOX_STATE_LEN == 2 * 4096);
const _: () = assert!(LXMF_OUTBOX_OFFSET == LXMF_MAILBOX_STATE_OFFSET + LXMF_MAILBOX_STATE_LEN);
const _: () = assert!(LXMF_OUTBOX_LEN == 64 * 4096);
const _: () = assert!(APPLIANCE_SETTINGS_OFFSET == LXMF_OUTBOX_OFFSET + LXMF_OUTBOX_LEN);
const _: () = assert!(APPLIANCE_SETTINGS_LEN == 2 * 4096);
const _: () =
    assert!(PRODUCT_REGISTRY_FREE_OFFSET + PRODUCT_REGISTRY_FREE_LEN == LXMF_STORE_OFFSET);
const _: () = assert!(PRODUCT_METADATA_OFFSET + PRODUCT_METADATA_LEN == LXMF_STORE_OFFSET);
const _: () = assert!(LXMF_STORE_OFFSET + LXMF_STORE_LEN == PRNS_STATE_OFFSET);
const _: () = assert!(PRNS_STATE_OFFSET + PRNS_STATE_LEN == REQUIRED_FLASH_BYTES as u32);
