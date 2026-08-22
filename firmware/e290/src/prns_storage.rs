//! Product-owned PRNS storage profile for the PSRAM-equipped E290.
//!
//! PRNS's [`personal_rns::storage::Esp32S3`] is the Personal Hopspot reference
//! recipe. The public [`StorageLayout`] contract exists so a board can choose
//! its own bounded capacities without changing PRNS or creating a storage
//! layout for each application combination. This profile keeps the reviewed
//! ESP32-S3 routing and Link bounds while reserving one stable registry for
//! current and future Reticulum applications.

use core::marker::PhantomData;

use allocator_api2::alloc::{Allocator, Global};
use personal_rns::crypto::ratchets::FixedSelfRatchetTable;
use personal_rns::identity::destination_identity::{
    NoDestinationIdentityAppData, NoDestinationIdentityTable,
};
use personal_rns::identity::held::FixedHeldIdentityTable;
use personal_rns::interfaces::EMBEDDED_MAX_LINK_MTU;
use personal_rns::persistence::{
    flash_journal_record_storage_len, maximum_route_upsert_payload_len, self_ratchets_snapshot_len,
};
use personal_rns::routing::announce::defaults::MAX_ANNOUNCE_IDS_PER_DESTINATION;
use personal_rns::routing::announce::destination_announce_limit::{
    FixedHeapDestinationAnnounceLimitTable, destination_announce_limit_index_buckets,
};
use personal_rns::routing::announce::held::FixedHeapHeldAnnounceTable;
use personal_rns::routing::announce::interface_announce_limit::FixedInterfaceAnnounceLimitTable;
use personal_rns::routing::announce::schedule::FixedHeapScheduledAnnounceQueue;
use personal_rns::routing::announce::stored::{
    FixedHeapAnnounceIdHistory, FixedHeapAnnounceRecordTable, FixedHeapPackedAppDataArena,
};
use personal_rns::routing::blackhole::{FixedHeapBlackholeTable, blackhole_index_buckets};
use personal_rns::routing::dedup::FixedPacketHashHistory;
use personal_rns::routing::delivery::receipts::FixedReceiptTable;
use personal_rns::routing::group_keys::FixedGroupKeyTable;
use personal_rns::routing::links::channel::channel_mdu;
use personal_rns::routing::links::channel::receive::WINDOW_MAX_MESSAGES;
use personal_rns::routing::links::channel::table::impls::FixedHeapChannelTable;
use personal_rns::routing::links::resources::assembly::{
    FixedIncomingAssemblyTable, FixedStaticOutgoingAssemblyTable,
};
use personal_rns::routing::links::resources::table::{
    FixedHeapResourceTable, IncomingResourceState, OutgoingResourceState,
};
use personal_rns::routing::links::resources::{
    max_outgoing_resource_reaction_frames, max_part_count, sealed_transfer_bytes,
};
use personal_rns::routing::links::table::FixedHeapLinkTable;
use personal_rns::routing::links::transported::FixedHeapTransportedLinkTable;
use personal_rns::routing::path_requests::interface_path_request_limit::FixedInterfacePathRequestLimitTable;
use personal_rns::routing::path_requests::pending::FixedPendingPathRequestTable;
use personal_rns::routing::path_requests::recent::FixedRecentPathRequestTable;
use personal_rns::routing::path_requests::recursive::FixedRecursivePathRequestTable;
use personal_rns::routing::path_requests::seen::FixedSeenPathRequestTable;
use personal_rns::routing::request_handlers::FixedRequestHandlerTable;
use personal_rns::routing::reverse_routes::FixedReverseRouteTable;
use personal_rns::routing::route_expiry::LinearRouteExpiryIndex;
use personal_rns::routing::routes::{FixedHeapRouteTable, route_index_buckets};
use personal_rns::routing::tunnel::FixedTunnelTable;
use personal_rns::routing::upstream_app_destinations::FixedUpstreamAppDestinationTable;
use personal_rns::routing::warmth::FixedDepartedInterfaceTable;
use personal_rns::storage::{DisplayedStorageLimits, StorageCapacity, StorageLayout};

/// Local application destinations available to any mix of firmware apps.
///
/// The initial product uses fewer than half of these rows. The remaining rows
/// are general application headroom, not assignments to named apps.
pub const APPLICATION_DESTINATION_CAPACITY: usize = 16;

/// Request routes available across all registered applications.
///
/// Applications may multiplex typed operations behind one route or register
/// several protocol-defined paths; neither choice changes the board layout.
pub const REQUEST_HANDLER_CAPACITY: usize = 64;

const MAX_TRACKED_DESTINATIONS: usize = 512;
const MAX_HELD_IDENTITIES: usize = 1;
const MAX_LINK_SESSIONS: usize = 512;
const MAX_TRANSPORTED_LINKS: usize = 32;
const MAX_OUTSTANDING_RECEIPTS: usize = 8;
const MAX_PACKET_HASHES: usize = 48;
const MAX_BLACKHOLED_IDENTITIES: usize = 32;
const BLACKHOLE_REASON_BYTES: usize = 64;
const MAX_REVERSE_ROUTES: usize = 32;
const MAX_PENDING_PATH_REQUESTS: usize = 8;
const MAX_HELD_ANNOUNCES: usize = 512;
const RETAINED_RATCHETS_PER_DESTINATION: usize = 8;
const MAX_CONCURRENT_CHANNELS: usize = 8;
const MAX_RESOURCE_ASSEMBLIES: usize = 1;
const CHANNEL_WINDOW_POOL: usize = 192;
const MAX_INCOMING_RESOURCE_TRANSFER_BYTES: usize = 8192;
const MAX_OUTGOING_RESOURCE_PLAINTEXT_BYTES: usize = 8192;
const MAX_OUTGOING_RESOURCE_TRANSFER_BYTES: usize =
    sealed_transfer_bytes(MAX_OUTGOING_RESOURCE_PLAINTEXT_BYTES);
const RETAINED_ANNOUNCE_APP_DATA_BYTES: usize = 40 * 1024;
const ROUTE_INDEX_BUCKETS: usize = route_index_buckets(MAX_TRACKED_DESTINATIONS);
const DESTINATION_ANNOUNCE_LIMIT_INDEX_BUCKETS: usize =
    destination_announce_limit_index_buckets(MAX_TRACKED_DESTINATIONS);
const MAX_OUTGOING_RESOURCE_PARTS: usize = max_part_count(MAX_OUTGOING_RESOURCE_TRANSFER_BYTES);
const MAX_INCOMING_RESOURCE_PARTS: usize = max_part_count(MAX_INCOMING_RESOURCE_TRANSFER_BYTES);
const CHANNEL_REORDER_DEPTH: usize = WINDOW_MAX_MESSAGES as usize;
const CHANNEL_MESSAGE_BYTES: usize = channel_mdu(EMBEDDED_MAX_LINK_MTU);

/// One stable E290 engine profile for every supported application set.
pub struct E290PrnsStorage<A: Allocator = Global>(PhantomData<A>);

impl<A: Allocator> E290PrnsStorage<A> {
    /// Route and announce destinations retained by the shared engine profile.
    pub const TRACKED_DESTINATIONS: usize = MAX_TRACKED_DESTINATIONS;

    /// Maximum frame burst synchronously produced by one Resource reaction.
    pub const MAX_OUTGOING_RESOURCE_REACTION_FRAMES: usize =
        max_outgoing_resource_reaction_frames(MAX_OUTGOING_RESOURCE_TRANSFER_BYTES);

    /// Flash arena bytes needed for the non-discardable self-ratchet journal.
    pub const MAX_CRITICAL_FLASH_JOURNAL_BYTES: usize = APPLICATION_DESTINATION_CAPACITY
        * flash_journal_record_storage_len(
            personal_rns::wire::TRUNCATED_HASH_BYTE_LEN
                + self_ratchets_snapshot_len(RETAINED_RATCHETS_PER_DESTINATION),
            4,
        );

    /// Worst-case compacted PRNS journal for the declared profile.
    pub const MAX_COMPACTED_FLASH_JOURNAL_BYTES: usize = MAX_TRACKED_DESTINATIONS
        * (flash_journal_record_storage_len(maximum_route_upsert_payload_len(0, 0), 4) + 3)
        + RETAINED_ANNOUNCE_APP_DATA_BYTES
        + APPLICATION_DESTINATION_CAPACITY
            * flash_journal_record_storage_len(
                personal_rns::wire::TRUNCATED_HASH_BYTE_LEN
                    + self_ratchets_snapshot_len(RETAINED_RATCHETS_PER_DESTINATION),
                4,
            )
        + flash_journal_record_storage_len(0, 4);
}

impl<A: Allocator> Default for E290PrnsStorage<A> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<A: Allocator + Default> StorageLayout for E290PrnsStorage<A> {
    const LIMITS: DisplayedStorageLimits = DisplayedStorageLimits {
        tracked_destinations: StorageCapacity::Fixed(MAX_TRACKED_DESTINATIONS),
        destination_identities: StorageCapacity::Fixed(0),
        announce_records: StorageCapacity::Fixed(MAX_TRACKED_DESTINATIONS),
        upstream_app_destinations: StorageCapacity::Fixed(APPLICATION_DESTINATION_CAPACITY),
        held_identities: StorageCapacity::Fixed(MAX_HELD_IDENTITIES),
        links: StorageCapacity::Fixed(MAX_LINK_SESSIONS),
        channels: StorageCapacity::Fixed(MAX_CONCURRENT_CHANNELS),
        channel_window_pool: Some(CHANNEL_WINDOW_POOL),
        channel_reorder_depth: StorageCapacity::Fixed(CHANNEL_REORDER_DEPTH),
        link_mtu: StorageCapacity::Fixed(EMBEDDED_MAX_LINK_MTU),
        resource_transfer_bytes: StorageCapacity::Fixed(MAX_INCOMING_RESOURCE_TRANSFER_BYTES),
        receipts: StorageCapacity::Fixed(MAX_OUTSTANDING_RECEIPTS),
        packet_hashes: StorageCapacity::Fixed(MAX_PACKET_HASHES),
        blackholed_identities: StorageCapacity::Fixed(MAX_BLACKHOLED_IDENTITIES),
        blackhole_reason_bytes: StorageCapacity::Fixed(BLACKHOLE_REASON_BYTES),
        reverse_routes: StorageCapacity::Fixed(MAX_REVERSE_ROUTES),
        pending_path_requests: StorageCapacity::Fixed(MAX_PENDING_PATH_REQUESTS),
        held_announces: StorageCapacity::Fixed(MAX_HELD_ANNOUNCES),
        ratchets_per_destination: StorageCapacity::Fixed(RETAINED_RATCHETS_PER_DESTINATION),
    };

    type Routes = FixedHeapRouteTable<MAX_TRACKED_DESTINATIONS, ROUTE_INDEX_BUCKETS, A>;
    type RouteExpiries = LinearRouteExpiryIndex;
    type DestinationIdentities = NoDestinationIdentityTable;
    type DestinationIdentityAppData = NoDestinationIdentityAppData;
    type Announces = FixedHeapAnnounceRecordTable<MAX_TRACKED_DESTINATIONS, A>;
    type History =
        FixedHeapAnnounceIdHistory<MAX_TRACKED_DESTINATIONS, MAX_ANNOUNCE_IDS_PER_DESTINATION, A>;
    type AppData =
        FixedHeapPackedAppDataArena<RETAINED_ANNOUNCE_APP_DATA_BYTES, MAX_TRACKED_DESTINATIONS, A>;
    type ScheduledAnnounces = FixedHeapScheduledAnnounceQueue<MAX_TRACKED_DESTINATIONS, A>;
    type UpstreamAppDestinations =
        FixedUpstreamAppDestinationTable<APPLICATION_DESTINATION_CAPACITY>;
    type HeldIdentities = FixedHeldIdentityTable<MAX_HELD_IDENTITIES>;
    type SelfRatchets =
        FixedSelfRatchetTable<APPLICATION_DESTINATION_CAPACITY, RETAINED_RATCHETS_PER_DESTINATION>;
    type Receipts = FixedReceiptTable<MAX_OUTSTANDING_RECEIPTS>;
    type PacketHashes = FixedPacketHashHistory<MAX_PACKET_HASHES>;
    type Blackholes = FixedHeapBlackholeTable<
        MAX_BLACKHOLED_IDENTITIES,
        { blackhole_index_buckets(MAX_BLACKHOLED_IDENTITIES) },
        BLACKHOLE_REASON_BYTES,
        A,
    >;
    type ReverseRoutes = FixedReverseRouteTable<MAX_REVERSE_ROUTES>;
    type DepartedInterfaces = FixedDepartedInterfaceTable<16>;
    type PendingPathRequests = FixedPendingPathRequestTable<MAX_PENDING_PATH_REQUESTS>;
    type RecentPathRequests = FixedRecentPathRequestTable<8>;
    type SeenPathRequests = FixedSeenPathRequestTable<8>;
    type Tunnels = FixedTunnelTable<0>;
    type RecursivePathRequests = FixedRecursivePathRequestTable<8>;
    type InterfacePathRequestLimits = FixedInterfacePathRequestLimitTable<8>;
    type InterfaceAnnounceLimits = FixedInterfaceAnnounceLimitTable<8>;
    type DirtyInterfaces = heapless::Vec<personal_rns::interfaces::InterfaceId, 8>;
    type HeldAnnounces = FixedHeapHeldAnnounceTable<MAX_HELD_ANNOUNCES, A>;
    type HeldAnnounceAppData = FixedHeapPackedAppDataArena<65536, MAX_HELD_ANNOUNCES, A>;
    type DestinationAnnounceLimits = FixedHeapDestinationAnnounceLimitTable<
        MAX_TRACKED_DESTINATIONS,
        DESTINATION_ANNOUNCE_LIMIT_INDEX_BUCKETS,
        A,
    >;
    type GroupKeys = FixedGroupKeyTable<APPLICATION_DESTINATION_CAPACITY>;
    type RequestHandlers = FixedRequestHandlerTable<REQUEST_HANDLER_CAPACITY>;
    type TransportedLinks = FixedHeapTransportedLinkTable<MAX_TRANSPORTED_LINKS, A>;
    type Links = FixedHeapLinkTable<MAX_LINK_SESSIONS, A>;
    type OutgoingResources = FixedHeapResourceTable<
        OutgoingResourceState,
        1,
        MAX_OUTGOING_RESOURCE_TRANSFER_BYTES,
        MAX_OUTGOING_RESOURCE_PARTS,
        A,
    >;
    type IncomingResources = FixedHeapResourceTable<
        IncomingResourceState,
        1,
        MAX_INCOMING_RESOURCE_TRANSFER_BYTES,
        MAX_INCOMING_RESOURCE_PARTS,
        A,
    >;
    type IncomingAssemblies = FixedIncomingAssemblyTable<MAX_RESOURCE_ASSEMBLIES>;
    type OutgoingAssemblies = FixedStaticOutgoingAssemblyTable<MAX_RESOURCE_ASSEMBLIES>;
    type Channels = FixedHeapChannelTable<
        MAX_CONCURRENT_CHANNELS,
        CHANNEL_REORDER_DEPTH,
        CHANNEL_MESSAGE_BYTES,
        CHANNEL_WINDOW_POOL,
        A,
    >;
}

#[cfg(test)]
mod tests {
    use super::*;
    use personal_rns::engine::EngineState;
    use personal_rns::routing::request_handlers::RequestHandlerTable;
    use personal_rns::routing::upstream_app_destinations::UpstreamAppDestinationTable;

    #[test]
    fn one_profile_reserves_shared_application_registries() {
        type Layout = E290PrnsStorage;
        let destinations = <Layout as StorageLayout>::UpstreamAppDestinations::default();
        let handlers = <Layout as StorageLayout>::RequestHandlers::default();
        assert_eq!(destinations.capacity(), APPLICATION_DESTINATION_CAPACITY);
        assert_eq!(handlers.capacity(), REQUEST_HANDLER_CAPACITY);
        assert_eq!(
            Layout::LIMITS.destination_identities,
            StorageCapacity::Fixed(0)
        );
    }

    #[test]
    fn product_profile_keeps_the_reviewed_resource_reaction_bound() {
        assert_eq!(
            E290PrnsStorage::<Global>::MAX_OUTGOING_RESOURCE_REACTION_FRAMES,
            19
        );
    }

    #[test]
    fn application_headroom_has_a_bounded_psram_footprint() {
        assert!(core::mem::size_of::<EngineState<E290PrnsStorage>>() <= 40 * 1024);
    }
}
