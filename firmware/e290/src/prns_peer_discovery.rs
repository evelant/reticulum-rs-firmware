//! Boot-scoped product projection of authenticated LXMF announces.
//!
//! PRNS remains the sole route and announce-validation authority. The accepted
//! announce observer copies only `lxmf.delivery` evidence into this bounded
//! table so authenticated Device API clients can present contacts observed by
//! the appliance. Nothing in this module feeds back into PRNS routing state.

use core::cell::RefCell;

use allocator_api2::{alloc::Allocator, vec::Vec};
use embassy_sync::blocking_mutex::{Mutex, raw::CriticalSectionRawMutex};
use personal_rns::routing::announce::{AnnounceObservation, derive_destination_hash, expand_name};
use reticulum_device_api::{
    DestinationHash as ApiDestinationHash, IdentityHash as ApiIdentityHash, LxmfDiscoveredPeer,
    LxmfPeerDiscoveryCursor, LxmfPeerDiscoveryIncarnation, LxmfPeerDiscoveryPage,
    LxmfPeerGeneration, MAX_LXMF_PEER_APP_DATA_BYTES, ReticulumInterfaceId,
};
use reticulum_peer_discovery::{
    AuthenticatedAnnounceObservation, DestinationHash, DiscoveredPeer, DiscoveredPeers,
    DiscoveryCursor, IdentityHash, MonotonicMillis, ObservationMetadata, ObserveError,
    ObservedInterfaceId, SignalObservations,
};

/// Maximum number of latest LXMF destination observations retained per boot.
pub const LXMF_PEER_CAPACITY: usize = 64;

type PeerTable<'storage> = DiscoveredPeers<'storage, MAX_LXMF_PEER_APP_DATA_BYTES>;

struct PeerProjection<'storage> {
    incarnation: LxmfPeerDiscoveryIncarnation,
    peers: PeerTable<'storage>,
}

impl<'storage> PeerProjection<'storage> {
    const fn new(incarnation: [u8; 8], peers: PeerTable<'storage>) -> Self {
        Self {
            incarnation: LxmfPeerDiscoveryIncarnation::new(incarnation),
            peers,
        }
    }

    fn observe(&mut self, observation: AnnounceObservation<'_>) -> Result<bool, ObserveError> {
        let name = expand_name("lxmf", &["delivery"])
            .expect("the static LXMF delivery destination name is valid");
        if derive_destination_hash(&observation.announced_identity, &name)
            != observation.destination
        {
            return Ok(false);
        }

        self.peers.observe(AuthenticatedAnnounceObservation::new(
            DestinationHash::new(*observation.destination.as_bytes()),
            IdentityHash::new(*observation.announced_identity.as_bytes()),
            observation.app_data,
            ObservationMetadata::new(
                observation.hops.0,
                ObservedInterfaceId::new(*observation.source_interface.as_bytes()),
                SignalObservations::UNKNOWN,
                MonotonicMillis::new(observation.arrived_at.0),
            ),
        ))?;
        Ok(true)
    }

    fn page(
        &self,
        requested: Option<LxmfPeerDiscoveryCursor>,
        now_millis: u64,
    ) -> LxmfPeerDiscoveryPage {
        let (cursor, reset_history) = match requested {
            Some(requested) if requested.incarnation() == self.incarnation => {
                (DiscoveryCursor::new(requested.after_generation()), false)
            }
            Some(_) => (DiscoveryCursor::START, true),
            None => (DiscoveryCursor::START, false),
        };
        let (page, reset_history) = match self.peers.page_after(cursor) {
            Ok(page) => (page, reset_history),
            Err(_) => (
                self.peers
                    .page_after(DiscoveryCursor::START)
                    .expect("the start cursor cannot be ahead of discovery history"),
                true,
            ),
        };
        let peer = page.peer().map(|peer| {
            let signal = peer.signal();
            LxmfDiscoveredPeer::new(
                ApiDestinationHash(*peer.destination().as_bytes()),
                ApiIdentityHash::new(*peer.identity_hash().as_bytes()),
                peer.app_data(),
                peer.hops(),
                ReticulumInterfaceId::new(*peer.interface().as_bytes()),
                signal.rssi_dbm(),
                signal.snr_db(),
                now_millis.saturating_sub(peer.observed_at().get()),
                generation(peer.generation().get()),
            )
            .expect("peer discovery and Device API use the same app-data bound")
        });
        LxmfPeerDiscoveryPage::new(
            LxmfPeerDiscoveryCursor::new(self.incarnation, page.next_cursor().get()),
            page.observed_through().map(|value| generation(value.get())),
            page.oldest_retained().map(|value| generation(value.get())),
            reset_history || page.history_gap(),
            peer,
        )
    }
}

const fn generation(value: u64) -> LxmfPeerGeneration {
    match LxmfPeerGeneration::new(value) {
        Ok(generation) => generation,
        Err(_) => panic!("peer discovery generations are always nonzero"),
    }
}

static PEERS: Mutex<CriticalSectionRawMutex, RefCell<Option<PeerProjection<'static>>>> =
    Mutex::new(RefCell::new(None));

/// Allocate the boot-scoped projection in caller-selected product memory.
///
/// The E290 supplies its mapped-PSRAM allocator. The allocation is intentionally
/// leaked because this boot-lifetime table is shared by a synchronous PRNS
/// callback and authenticated Device API reads.
pub fn initialize_in<A>(
    incarnation: [u8; 8],
    allocator: A,
) -> Result<(), allocator_api2::alloc::AllocError>
where
    A: Allocator + 'static,
{
    let mut slots: Vec<Option<DiscoveredPeer<MAX_LXMF_PEER_APP_DATA_BYTES>>, A> =
        Vec::new_in(allocator);
    slots
        .try_reserve_exact(LXMF_PEER_CAPACITY)
        .map_err(|_| allocator_api2::alloc::AllocError)?;
    for _ in 0..LXMF_PEER_CAPACITY {
        slots.push(None);
    }
    let table = DiscoveredPeers::try_new(slots.leak())
        .expect("the E290 LXMF peer capacity must be nonzero");
    PEERS.lock(|state| {
        let previous = state
            .borrow_mut()
            .replace(PeerProjection::new(incarnation, table));
        assert!(
            previous.is_none(),
            "peer discovery initializes only once per boot"
        );
    });
    Ok(())
}

/// Copy one PRNS-validated `lxmf.delivery` announce into product discovery.
///
/// PRNS calls this synchronously before releasing the borrowed announce bytes.
pub fn observe_accepted_announce(observation: AnnounceObservation<'_>) {
    PEERS.lock(|peers| {
        let mut peers = peers.borrow_mut();
        let peers = peers
            .as_mut()
            .expect("peer discovery must initialize before PRNS starts");
        if let Err(error) = peers.observe(observation) {
            #[cfg(target_arch = "xtensa")]
            log::warn!("e290-prns lxmf-peer-observation status=DROPPED reason={error:?}");
            #[cfg(not(target_arch = "xtensa"))]
            let _ = error;
        }
    });
}

/// Return one authenticated discovery record after a boot-scoped cursor.
pub fn page(after: Option<LxmfPeerDiscoveryCursor>, now_millis: u64) -> LxmfPeerDiscoveryPage {
    PEERS.lock(|peers| {
        peers
            .borrow()
            .as_ref()
            .expect("peer discovery must initialize before PRNS starts")
            .page(after, now_millis)
    })
}

#[cfg(test)]
mod tests {
    use personal_rns::{
        identity::IdentityHash as PrnsIdentityHash,
        interfaces::InterfaceId,
        routing::announce::{AnnounceObservation, derive_single_destination_hash},
        units::{HopCount, InstantMillis},
        wire::DestinationHash as PrnsDestinationHash,
    };

    use super::*;

    fn lxmf_observation<'a>(identity: u8, app_data: &'a [u8]) -> AnnounceObservation<'a> {
        let identity = PrnsIdentityHash::new([identity; 16]);
        AnnounceObservation {
            destination: derive_single_destination_hash(&identity, "lxmf", &["delivery"]).unwrap(),
            announced_identity: identity,
            hops: HopCount(2),
            source_interface: InterfaceId::new([3, 4, 5, 6, 7, 8, 9, 10]),
            arrived_at: InstantMillis(1_000),
            app_data,
            is_path_response: false,
        }
    }

    #[test]
    fn only_authenticated_lxmf_delivery_destinations_enter_the_projection() {
        let mut slots = [None; 2];
        let peers = DiscoveredPeers::try_new(&mut slots).unwrap();
        let mut projection = PeerProjection::new(*b"boot-one", peers);
        let mut unrelated = lxmf_observation(7, b"unrelated");
        unrelated.destination = PrnsDestinationHash::new([9; 16]);

        assert!(!projection.observe(unrelated).unwrap());
        assert!(projection.observe(lxmf_observation(7, b"Alice")).unwrap());

        let page = projection.page(None, 1_250);
        let peer = page.peer().unwrap();
        assert_eq!(peer.app_data(), b"Alice");
        assert_eq!(peer.hops(), 2);
        assert_eq!(peer.interface_id().as_bytes(), &[3, 4, 5, 6, 7, 8, 9, 10]);
        assert_eq!(peer.observed_age_ms(), 250);
    }

    #[test]
    fn incarnation_changes_and_ahead_cursors_restart_with_a_history_gap() {
        let mut slots = [None; 2];
        let peers = DiscoveredPeers::try_new(&mut slots).unwrap();
        let mut projection = PeerProjection::new(*b"boot-two", peers);
        projection.observe(lxmf_observation(8, b"Bob")).unwrap();

        let stale =
            LxmfPeerDiscoveryCursor::new(LxmfPeerDiscoveryIncarnation::new(*b"old-boot"), 1);
        let stale_page = projection.page(Some(stale), 1_000);
        assert!(stale_page.history_gap());
        assert!(stale_page.peer().is_some());

        let ahead =
            LxmfPeerDiscoveryCursor::new(LxmfPeerDiscoveryIncarnation::new(*b"boot-two"), 99);
        let ahead_page = projection.page(Some(ahead), 1_000);
        assert!(ahead_page.history_gap());
        assert!(ahead_page.peer().is_some());
    }
}
