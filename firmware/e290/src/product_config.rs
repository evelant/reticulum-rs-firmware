//! Small board/product constants used by the PRNS composition.

/// Minimum qualified mapped PSRAM capacity.
pub const MINIMUM_PSRAM_BYTES: usize = 8 * 1024 * 1024;
/// Largest supported mapped PSRAM capacity.
pub const MAXIMUM_PSRAM_BYTES: usize = 16 * 1024 * 1024;
/// Strict internal heap retained for the firmware and Bluetooth runtime.
pub const INTERNAL_HEAP_BYTES: usize = 72 * 1024;
/// Additional strict internal heap retained by the gateway Wi-Fi profile.
#[cfg(feature = "gateway")]
pub const WIFI_INTERNAL_HEAP_BYTES: usize = 48 * 1024;
/// No Wi-Fi heap is reserved by the LoRa/Bluetooth-only profile.
#[cfg(not(feature = "gateway"))]
pub const WIFI_INTERNAL_HEAP_BYTES: usize = 0;
/// Conservative SPI clock used for the supported LoRa board.
pub const SPI_FREQUENCY_HZ: u32 = 1_000_000;
/// Maximum SX1262 BUSY wait before the radio operation fails.
pub const BUSY_PIN_WATCHDOG_MS: u64 = 100;

/// Caller-owned LXMF index rows retained in mapped PSRAM.
pub const LXMF_INDEX_SLOTS: usize =
    crate::partition_contract::LXMF_STORE_LEN as usize / reticulum_lxmf_store::EXTENT_SIZE;
/// Exact initialized bytes occupied by the LXMF index.
pub const LXMF_INDEX_STORAGE_BYTES: usize =
    core::mem::size_of::<reticulum_lxmf_store::LxmfStoreIndexSlot>() * LXMF_INDEX_SLOTS;
/// Caller-owned outbound-intent index rows retained in mapped PSRAM.
pub const LXMF_OUTBOX_INDEX_SLOTS: usize = crate::product_outbox::OUTBOX_RECORD_CAPACITY;
/// Exact initialized bytes occupied by the outbound-intent index.
pub const LXMF_OUTBOX_INDEX_STORAGE_BYTES: usize =
    core::mem::size_of::<crate::product_outbox::OutboxIndexSlot>() * LXMF_OUTBOX_INDEX_SLOTS;

/// Maximum normalized LXMF wire bytes admitted by this product.
pub const LXMF_MAX_WIRE_BYTES: usize = 4_096;
/// Maximum one LXMF MessagePack value body.
pub const LXMF_MAX_VALUE_BYTES: usize = 2_048;
/// Maximum items in one LXMF MessagePack container.
pub const LXMF_MAX_CONTAINER_ITEMS: usize = 256;
/// Maximum aggregate LXMF MessagePack values.
pub const LXMF_MAX_TOTAL_VALUES: usize = 2_048;
/// Maximum bounded LXMF parser scan steps.
pub const LXMF_MAX_SCAN_STEPS: usize = 65_536;
/// Maximum LXMF MessagePack nesting depth.
pub const LXMF_MAX_NESTING_DEPTH: usize = 16;

/// Maximum MessagePack bytes for `[name, nil, []]` announce app data.
pub const MAX_LXMF_DELIVERY_APP_NAME_BYTES: usize = 32;
/// Maximum MessagePack bytes for `[name, nil, []]` announce app data.
pub const MAX_LXMF_DELIVERY_ANNOUNCE_APP_DATA_BYTES: usize =
    1 + 2 + MAX_LXMF_DELIVERY_APP_NAME_BYTES + 1 + 1;

/// Encode Python-compatible LXMF delivery announce app data `[name, nil, []]`.
pub fn encode_lxmf_delivery_announce_app_data(
    name: &str,
    output: &mut [u8; MAX_LXMF_DELIVERY_ANNOUNCE_APP_DATA_BYTES],
) -> usize {
    let bytes = name.as_bytes();
    assert!(bytes.len() <= MAX_LXMF_DELIVERY_APP_NAME_BYTES);
    let mut cursor = 0;
    output[cursor] = 0x93;
    cursor += 1;
    if bytes.len() <= 31 {
        output[cursor] = 0xa0 | bytes.len() as u8;
        cursor += 1;
    } else {
        output[cursor] = 0xd9;
        output[cursor + 1] = bytes.len() as u8;
        cursor += 2;
    }
    output[cursor..cursor + bytes.len()].copy_from_slice(bytes);
    cursor += bytes.len();
    output[cursor] = 0xc0;
    cursor += 1;
    output[cursor] = 0x90;
    cursor + 1
}

const _: () = assert!(LXMF_INDEX_SLOTS == 1_024);
const _: () = assert!(LXMF_INDEX_STORAGE_BYTES > 0);
const _: () = assert!(LXMF_OUTBOX_INDEX_SLOTS == 64);
const _: () = assert!(LXMF_OUTBOX_INDEX_STORAGE_BYTES > 0);
