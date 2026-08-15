extern crate std;

use std::format;

use super::*;

const fn destination(tag: u8) -> DestinationHash {
    DestinationHash::new([tag; DESTINATION_HASH_LENGTH])
}

const fn identity_hash(tag: u8) -> IdentityHash {
    IdentityHash::new([tag; IDENTITY_HASH_LENGTH])
}

const fn public_key(tag: u8) -> IdentityPublicKey {
    IdentityPublicKey::new([tag; IDENTITY_PUBLIC_KEY_LENGTH])
}

fn observation(tag: u8, app_data: &[u8], observed_at: u64) -> AuthenticatedAnnounceObservation<'_> {
    AuthenticatedAnnounceObservation::new(
        destination(tag),
        identity_hash(tag.wrapping_add(1)),
        public_key(tag.wrapping_add(2)),
        app_data,
        ObservationMetadata::new(
            tag,
            ObservedInterfaceId::new(tag.wrapping_add(3)),
            SignalObservations::new(Some(-100 + i16::from(tag)), Some(i16::from(tag))),
            MonotonicMillis::new(observed_at),
        ),
    )
}

#[test]
fn zero_peer_capacity_is_an_explicit_construction_error() {
    assert!(matches!(
        DiscoveredPeers::<0, 8>::try_new(),
        Err(DiscoveryCapacityError::ZeroPeerCapacity)
    ));
}

#[test]
fn insertion_copies_bounded_authenticated_evidence() {
    let mut table = DiscoveredPeers::<2, 8>::try_new().unwrap();
    let mut source = *b"peer";
    let outcome = table.observe(observation(1, &source, 10)).unwrap();
    source.fill(0);

    assert_eq!(outcome.generation().get(), 1);
    assert_eq!(outcome.disposition(), ObserveDisposition::Inserted);
    assert_eq!(table.len(), 1);
    assert_eq!(table.capacity(), 2);
    assert!(!table.is_empty());
    let peer = table.get(destination(1)).unwrap();
    assert_eq!(peer.app_data(), b"peer");
    assert_eq!(peer.identity_hash(), identity_hash(2));
    assert_eq!(peer.public_key(), public_key(3));
    assert_eq!(peer.hops(), 1);
    assert_eq!(peer.interface(), ObservedInterfaceId::new(4));
    assert_eq!(peer.signal().rssi_dbm(), Some(-99));
    assert_eq!(peer.signal().snr_db(), Some(1));
    assert_eq!(peer.observed_at(), MonotonicMillis::new(10));
    assert_eq!(peer.generation().get(), 1);
}

#[test]
fn matching_destination_updates_in_place_with_a_new_generation() {
    let mut table = DiscoveredPeers::<2, 8>::try_new().unwrap();
    table.observe(observation(1, b"old", 10)).unwrap();
    let replacement = AuthenticatedAnnounceObservation::new(
        destination(1),
        identity_hash(9),
        public_key(10),
        b"new-data",
        ObservationMetadata::new(
            7,
            ObservedInterfaceId::new(8),
            SignalObservations::UNKNOWN,
            MonotonicMillis::new(11),
        ),
    );
    let outcome = table.observe(replacement).unwrap();

    assert_eq!(outcome.generation().get(), 2);
    assert_eq!(outcome.disposition(), ObserveDisposition::Updated);
    assert_eq!(table.len(), 1);
    let peer = table.get(destination(1)).unwrap();
    assert_eq!(peer.identity_hash(), identity_hash(9));
    assert_eq!(peer.public_key(), public_key(10));
    assert_eq!(peer.app_data(), b"new-data");
    assert_eq!(peer.hops(), 7);
    assert_eq!(peer.interface(), ObservedInterfaceId::new(8));
    assert_eq!(peer.signal(), SignalObservations::UNKNOWN);
}

#[test]
fn full_table_evicts_oldest_latest_observation_deterministically() {
    let mut table = DiscoveredPeers::<2, 1>::try_new().unwrap();
    table.observe(observation(1, b"a", 10)).unwrap();
    table.observe(observation(2, b"b", 10)).unwrap();
    table.observe(observation(1, b"c", 10)).unwrap();
    let outcome = table.observe(observation(3, b"d", 10)).unwrap();

    assert_eq!(
        outcome.disposition(),
        ObserveDisposition::Evicted {
            destination: destination(2)
        }
    );
    assert_eq!(outcome.generation().get(), 4);
    assert!(table.get(destination(1)).is_some());
    assert!(table.get(destination(2)).is_none());
    assert!(table.get(destination(3)).is_some());
    assert_eq!(table.len(), 2);
}

#[test]
fn oversize_rejection_is_explicit_and_atomic() {
    let mut table = DiscoveredPeers::<1, 3>::try_new().unwrap();
    table.observe(observation(1, b"ok", 10)).unwrap();

    assert_eq!(
        table.observe(observation(2, b"wide", 11)),
        Err(ObserveError::AppDataTooLarge {
            supplied: 4,
            capacity: 3
        })
    );
    assert_eq!(table.len(), 1);
    assert_eq!(table.latest_generation().unwrap().get(), 1);
    assert_eq!(table.last_observed_at(), Some(MonotonicMillis::new(10)));
    assert!(table.get(destination(1)).is_some());
}

#[test]
fn global_time_regression_is_explicit_and_atomic_but_equal_time_is_valid() {
    let mut table = DiscoveredPeers::<2, 1>::try_new().unwrap();
    table.observe(observation(1, b"a", 10)).unwrap();

    assert_eq!(
        table.observe(observation(2, b"b", 9)),
        Err(ObserveError::Time(ObservationTimeError::Regressed {
            previous: MonotonicMillis::new(10),
            observed: MonotonicMillis::new(9)
        }))
    );
    assert_eq!(table.latest_generation().unwrap().get(), 1);
    assert!(table.get(destination(2)).is_none());

    let accepted = table.observe(observation(2, b"b", 10)).unwrap();
    assert_eq!(accepted.generation().get(), 2);
}

#[test]
fn generation_exhaustion_is_explicit_and_atomic() {
    let mut table = DiscoveredPeers::<1, 1>::try_new().unwrap();
    table.latest_generation = u64::MAX;

    assert_eq!(
        table.observe(observation(1, b"a", 10)),
        Err(ObserveError::GenerationExhausted)
    );
    assert!(table.is_empty());
    assert_eq!(table.last_observed_at(), None);
}

#[test]
fn one_record_pages_follow_generation_and_report_projection_gaps() {
    let mut table = DiscoveredPeers::<2, 1>::try_new().unwrap();
    table.observe(observation(1, b"a", 10)).unwrap();
    table.observe(observation(2, b"b", 11)).unwrap();
    table.observe(observation(1, b"c", 12)).unwrap();

    let first = table.page_after(DiscoveryCursor::START).unwrap();
    assert_eq!(first.peer().unwrap().destination(), destination(2));
    assert_eq!(first.peer().unwrap().generation().get(), 2);
    assert_eq!(first.next_cursor().get(), 2);
    assert_eq!(first.observed_through().unwrap().get(), 3);
    assert_eq!(first.oldest_retained().unwrap().get(), 2);
    assert!(first.history_gap());

    let second = table.page_after(first.next_cursor()).unwrap();
    assert_eq!(second.peer().unwrap().destination(), destination(1));
    assert_eq!(second.peer().unwrap().generation().get(), 3);
    assert_eq!(second.next_cursor().get(), 3);
    assert!(!second.history_gap());

    let end = table.page_after(second.next_cursor()).unwrap();
    assert!(end.peer().is_none());
    assert_eq!(end.next_cursor().get(), 3);
    assert!(!end.history_gap());

    assert_eq!(
        table.page_after(DiscoveryCursor::new(4)),
        Err(DiscoveryCursorError::AheadOfHistory {
            cursor: DiscoveryCursor::new(4),
            latest: Some(ObservationGeneration(3))
        })
    );
}

#[test]
fn debug_output_redacts_public_key_and_application_data() {
    let observation = observation(171, &[177; 8], 10);
    let observation_debug = format!("{observation:?}");
    assert!(observation_debug.contains("<redacted>"));
    assert!(!observation_debug.contains("173, 173"));
    assert!(!observation_debug.contains("177, 177"));

    let mut table = DiscoveredPeers::<1, 8>::try_new().unwrap();
    table.observe(observation).unwrap();
    let peer = table.get(destination(171)).unwrap();
    let peer_debug = format!("{peer:?}");
    assert!(peer_debug.contains("<redacted>"));
    assert!(!peer_debug.contains("173, 173"));
    assert!(!peer_debug.contains("177, 177"));

    let table_debug = format!("{table:?}");
    assert!(table_debug.contains("<redacted>"));
    assert!(!table_debug.contains("173, 173"));
    assert!(!table_debug.contains("177, 177"));

    let page_debug = format!("{:?}", table.page_after(DiscoveryCursor::START).unwrap());
    assert!(page_debug.contains("<redacted>"));
    assert!(!page_debug.contains("173, 173"));
    assert!(!page_debug.contains("177, 177"));
}
