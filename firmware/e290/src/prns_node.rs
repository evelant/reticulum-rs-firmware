//! Capacity and ownership contract for the PRNS-native E290 node.
//!
//! These bounds make the product's queues explicit before hardware composition
//! replaces the legacy node. Exhaustion is diagnosed and dropped according to
//! PRNS's existing nonblocking runtime behavior; it does not delay proofs,
//! change command settlement, or manufacture a protocol retry.

use allocator_api2::alloc::Global;
use personal_rns::routing::dedup::dedup_index_buckets;
use personal_rns::runtime::{
    minimum_interface_store_capacity, minimum_manifold_notification_capacity,
};
use personal_rns::storage::{StorageCapacity, StorageLayout};
pub use reticulum_device_api::OTA_IMAGE_CHUNK_BYTES;

/// The E290's one application-independent PRNS storage profile.
///
/// Host checks use the global allocator. Target composition supplies its
/// default-able PSRAM allocator without changing any storage capacities.
pub type EngineStorage<A = Global> = crate::prns_storage::E290PrnsStorage<A>;

/// One dedicated LoRa lane, one optional TCP lane, and one Bluetooth Auto fleet lane.
pub const MANIFOLD_LANE_CAPACITY: usize = 3;

/// Every E290 manifold ingress lane is one frame deep.
pub const MANIFOLD_INGRESS_DEPTH: usize = 1;

/// ESP32-S3 Bluetooth Auto retains at most four live peers.
pub const BLUETOOTH_PEER_CAPACITY: usize = 4;

/// Dedicated interfaces plus every Bluetooth Auto child descriptor.
pub const INTERFACE_CAPACITY: usize = MANIFOLD_LANE_CAPACITY + BLUETOOTH_PEER_CAPACITY;

/// A coalesced doorbell still has capacity for every buffered ingress frame.
pub const MANIFOLD_NOTIFICATION_CAPACITY: usize =
    minimum_manifold_notification_capacity(MANIFOLD_LANE_CAPACITY, MANIFOLD_INGRESS_DEPTH);

/// Every lane can retain one complete synchronous Resource response reaction.
pub const MANIFOLD_OUTBOUND_DEPTH: usize =
    EngineStorage::<Global>::MAX_OUTGOING_RESOURCE_REACTION_FRAMES;

/// Concurrent product-issued PRNS commands admitted by the embedded node.
pub const COMMAND_CAPACITY: usize = 8;

/// Dynamic Bluetooth Auto member changes admitted ahead of the manifold.
pub const INTERFACE_LIFECYCLE_CAPACITY: usize = 8;

/// Product command callers that may await settlement concurrently.
pub const COMPLETION_CAPACITY: usize = 8;

/// Retained interface-count and packet-PHY rows use a power-of-two index.
pub const INTERFACE_INSPECTION_CAPACITY: usize =
    minimum_interface_store_capacity(INTERFACE_CAPACITY);

/// Recently seen packets for which interface PHY observations remain queryable.
pub const PACKET_PHY_RETENTION_CAPACITY: usize =
    match <EngineStorage<Global> as StorageLayout>::LIMITS.packet_hashes {
        StorageCapacity::Fixed(capacity) => capacity,
        StorageCapacity::Dynamic => panic!("the E290 packet history must be bounded"),
    };

/// Hash-index buckets required by the retained packet PHY table.
pub const PACKET_PHY_INDEX_BUCKETS: usize = dedup_index_buckets(PACKET_PHY_RETENTION_CAPACITY);

/// Owned application events waiting for the product storage coordinator.
pub const APPLICATION_EVENT_CAPACITY: usize = 16;

/// Product command settlements retained ahead of their bounded callers.
///
/// Every settlement originates from one of PRNS's concurrently admitted
/// commands, so a deeper application-event lane cannot accept additional
/// useful work.
pub const APPLICATION_SETTLEMENT_CAPACITY: usize = COMPLETION_CAPACITY;

/// PSRAM reserved for copies borrowed from synchronous PRNS application events.
///
/// This holds four maximum-size incoming Resources, or at least twenty-one
/// maximum embedded Link payloads. OTA requests one Resource at a time; the
/// extra headroom covers management and messaging events that arrive while a
/// flash write is in progress.
pub const APPLICATION_EVENT_PAYLOAD_POOL_BYTES: usize = 32 * 1024;

/// PRNS's maximum sealed incoming Resource transfer on this storage layout.
pub const INCOMING_RESOURCE_TRANSFER_BYTES: usize =
    match <EngineStorage<Global> as StorageLayout>::LIMITS.resource_transfer_bytes {
        StorageCapacity::Fixed(bytes) => bytes,
        StorageCapacity::Dynamic => panic!("the E290 PRNS Resource store must be bounded"),
    };

/// E290 application-lane budget for packed OTA Resource metadata.
pub const OTA_RESOURCE_METADATA_BYTES: usize = 512;

/// Resource stream bytes before PRNS sealing.
pub const OTA_RESOURCE_STREAM_BYTES: usize =
    OTA_IMAGE_CHUNK_BYTES + OTA_RESOURCE_METADATA_BYTES + 3;

/// PRNS sealed-transfer bytes for one OTA application chunk.
pub const OTA_RESOURCE_SEALED_BYTES: usize = 7_760;

const _: () = assert!(MANIFOLD_NOTIFICATION_CAPACITY == 3);
const _: () = assert!(MANIFOLD_OUTBOUND_DEPTH == 19);
const _: () = assert!(INTERFACE_INSPECTION_CAPACITY >= INTERFACE_CAPACITY);
const _: () = assert!(PACKET_PHY_RETENTION_CAPACITY == 48);
const _: () = assert!(APPLICATION_EVENT_PAYLOAD_POOL_BYTES >= INCOMING_RESOURCE_TRANSFER_BYTES);
const _: () = assert!(OTA_RESOURCE_SEALED_BYTES <= INCOMING_RESOURCE_TRANSFER_BYTES);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacities_cover_the_declared_e290_topology() {
        assert_eq!(MANIFOLD_LANE_CAPACITY, 3);
        assert_eq!(BLUETOOTH_PEER_CAPACITY, 4);
        assert_eq!(INTERFACE_CAPACITY, 7);
        assert_eq!(MANIFOLD_NOTIFICATION_CAPACITY, 3);
        assert_eq!(MANIFOLD_OUTBOUND_DEPTH, 19);
        assert!(INTERFACE_INSPECTION_CAPACITY >= INTERFACE_CAPACITY);
        assert_eq!(PACKET_PHY_RETENTION_CAPACITY, 48);
        assert_eq!(PACKET_PHY_INDEX_BUCKETS, 72);
        assert_eq!(APPLICATION_SETTLEMENT_CAPACITY, COMPLETION_CAPACITY);
    }

    #[test]
    fn ota_chunk_fits_the_prns_esp32s3_resource_store() {
        assert_eq!(OTA_RESOURCE_STREAM_BYTES, 7_683);
        assert_eq!(OTA_RESOURCE_SEALED_BYTES, 7_760);
        assert!(OTA_RESOURCE_SEALED_BYTES <= INCOMING_RESOURCE_TRANSFER_BYTES);
    }

    #[test]
    fn application_event_pool_covers_four_maximum_resources() {
        assert_eq!(
            APPLICATION_EVENT_PAYLOAD_POOL_BYTES,
            4 * INCOMING_RESOURCE_TRANSFER_BYTES
        );
    }
}
