//! E290 composition of PRNS's public embedded flash persistence.
//!
//! PRNS owns the records, restore rules, compaction policy, and journal
//! protocol. The product owns only one fixed raw-NOR range and the capacity of
//! its route-snapshot scratch index. This state is independent of which
//! Reticulum applications are enabled.

use allocator_api2::alloc::{Allocator, Global};
use allocator_api2::vec::Vec;
use embedded_storage_async::nor_flash::NorFlash;
use personal_rns::persistence::{FlashArenaRange, FlashJournalLayout};
use personal_rns::runtime::{
    EmbeddedCompactionPolicy, EmbeddedFlashPersistence, EmbeddedPersistenceDiagnostic,
    EmbeddedPersistencePolicy, RouteSnapshotKeyError, RouteSnapshotKeys,
};
use personal_rns::wire::DestinationHash;

use crate::prns_node::EngineStorage;

/// Physical erase sector used by the E290 flash and PRNS journal.
pub const E290_FLASH_PAGE_BYTES: u32 = 4 * 1024;
/// One application-independent PRNS persistence partition.
pub const PRNS_STATE_OFFSET: u32 = 0x00e8_0000;
/// PRNS persistence partition length through the end of 16 MiB flash.
pub const PRNS_STATE_LEN: u32 = 0x0018_0000;
/// One of the two equal PRNS compaction arenas.
pub const PRNS_ARENA_BYTES: usize = 191 * E290_FLASH_PAGE_BYTES as usize;
/// Pending persistence intents retained by the embedded worker.
pub const PRNS_PERSISTENCE_PENDING_CAPACITY: usize = 64;

/// PRNS journal layout within the product's single `prns_state` partition.
pub const E290_PRNS_FLASH_LAYOUT: FlashJournalLayout = FlashJournalLayout::new(
    [PRNS_STATE_OFFSET, PRNS_STATE_OFFSET + E290_FLASH_PAGE_BYTES],
    [
        FlashArenaRange::new(0x00e8_2000, 0x00f4_1000),
        FlashArenaRange::new(0x00f4_1000, 0x0100_0000),
    ],
);

/// PSRAM-backed scratch keys used while PRNS snapshots its route table.
pub struct E290RouteSnapshotKeys<A: Allocator = Global> {
    keys: Vec<DestinationHash, A>,
}

impl<A: Allocator + Default> Default for E290RouteSnapshotKeys<A> {
    fn default() -> Self {
        Self::new()
    }
}

impl<A: Allocator + Default> E290RouteSnapshotKeys<A> {
    /// Reserve exactly the product storage profile's route capacity.
    pub fn new() -> Self {
        Self {
            keys: Vec::with_capacity_in(EngineStorage::<A>::TRACKED_DESTINATIONS, A::default()),
        }
    }
}

impl<A: Allocator> RouteSnapshotKeys for E290RouteSnapshotKeys<A> {
    fn clear(&mut self) {
        self.keys.clear();
    }

    fn push(&mut self, destination: DestinationHash) -> Result<(), RouteSnapshotKeyError> {
        if self.keys.len() == self.keys.capacity() {
            return Err(RouteSnapshotKeyError::Capacity);
        }
        self.keys.push(destination);
        Ok(())
    }

    fn get(&self, index: usize) -> Option<DestinationHash> {
        self.keys.get(index).copied()
    }
}

/// PRNS persistence instance over the product's shared physical flash owner.
pub type E290PrnsPersistence<F, A = Global> = EmbeddedFlashPersistence<
    F,
    E290RouteSnapshotKeys<A>,
    fn(EmbeddedPersistenceDiagnostic),
    PRNS_PERSISTENCE_PENDING_CAPACITY,
>;

/// Compose PRNS's ordinary persistence worker with the E290 range and bounds.
pub fn e290_prns_persistence<F, A>(
    flash: F,
    observe: fn(EmbeddedPersistenceDiagnostic),
) -> E290PrnsPersistence<F, A>
where
    A: Allocator + Default,
    F: NorFlash,
{
    EmbeddedFlashPersistence::new(
        flash,
        E290_PRNS_FLASH_LAYOUT,
        EmbeddedPersistencePolicy::hopspot_default(EmbeddedCompactionPolicy::hopspot(
            EngineStorage::<A>::MAX_CRITICAL_FLASH_JOURNAL_BYTES,
        )),
        E290RouteSnapshotKeys::new(),
        observe,
    )
}

const _: () = assert!(PRNS_STATE_OFFSET.is_multiple_of(E290_FLASH_PAGE_BYTES));
const _: () = assert!(PRNS_STATE_LEN.is_multiple_of(E290_FLASH_PAGE_BYTES));
const _: () = assert!(PRNS_STATE_OFFSET + PRNS_STATE_LEN == 16 * 1024 * 1024);
const _: () = assert!(E290_PRNS_FLASH_LAYOUT.arenas[0].len() as usize == PRNS_ARENA_BYTES);
const _: () = assert!(E290_PRNS_FLASH_LAYOUT.arenas[1].len() as usize == PRNS_ARENA_BYTES);
const _: () =
    assert!(EngineStorage::<Global>::MAX_COMPACTED_FLASH_JOURNAL_BYTES <= PRNS_ARENA_BYTES);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_partition_contains_the_complete_prns_layout() {
        let state_end = PRNS_STATE_OFFSET + PRNS_STATE_LEN;
        assert_eq!(
            E290_PRNS_FLASH_LAYOUT.timebase_regions[0],
            PRNS_STATE_OFFSET
        );
        assert_eq!(E290_PRNS_FLASH_LAYOUT.timebase_regions[1], 0x00e8_1000);
        assert_eq!(E290_PRNS_FLASH_LAYOUT.arenas[0].start, 0x00e8_2000);
        assert_eq!(E290_PRNS_FLASH_LAYOUT.arenas[1].end, state_end);
    }

    #[test]
    fn compaction_arena_covers_every_configured_application_mix() {
        assert!(EngineStorage::<Global>::MAX_COMPACTED_FLASH_JOURNAL_BYTES <= PRNS_ARENA_BYTES);
        assert_eq!(EngineStorage::<Global>::TRACKED_DESTINATIONS, 512);
    }

    #[test]
    fn route_snapshot_keys_have_the_engine_route_capacity() {
        let keys = E290RouteSnapshotKeys::<Global>::new();
        assert_eq!(
            keys.keys.capacity(),
            EngineStorage::<Global>::TRACKED_DESTINATIONS
        );
    }
}
