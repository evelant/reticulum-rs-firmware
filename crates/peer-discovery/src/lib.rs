//! Allocation-free application history of authenticated Reticulum announces.
//!
//! The sole RNS owner supplies observations only after validating an announce
//! and resolving its ingress metadata. This crate deliberately performs no
//! cryptography and does not derive a destination from an identity. It copies
//! that trusted, already-validated public evidence into a fixed table for
//! application discovery.
//!
//! [`DiscoveredPeers`] is not route authority. Insertion, update, eviction, or
//! absence here must never add, remove, refresh, or override RNS path state.
//! A consumer may present a retained record for explicit contact selection,
//! but must return to the RNS owner for reachability and path decisions.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::fmt;

/// Length of a complete Reticulum destination hash.
pub const DESTINATION_HASH_LENGTH: usize = 16;
/// Length of a public Reticulum identity hash.
pub const IDENTITY_HASH_LENGTH: usize = 16;
/// Length of the combined public X25519-plus-Ed25519 Reticulum key.
pub const IDENTITY_PUBLIC_KEY_LENGTH: usize = 64;

/// Complete destination hash from an authenticated announce.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DestinationHash([u8; DESTINATION_HASH_LENGTH]);

impl DestinationHash {
    /// Construct a destination hash from all wire bytes.
    pub const fn new(bytes: [u8; DESTINATION_HASH_LENGTH]) -> Self {
        Self(bytes)
    }

    /// Borrow all destination-hash bytes.
    pub const fn as_bytes(&self) -> &[u8; DESTINATION_HASH_LENGTH] {
        &self.0
    }
}

/// Public hash of the identity that authenticated an announce.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct IdentityHash([u8; IDENTITY_HASH_LENGTH]);

impl IdentityHash {
    /// Construct an identity hash from all public bytes.
    pub const fn new(bytes: [u8; IDENTITY_HASH_LENGTH]) -> Self {
        Self(bytes)
    }

    /// Borrow all identity-hash bytes.
    pub const fn as_bytes(&self) -> &[u8; IDENTITY_HASH_LENGTH] {
        &self.0
    }
}

/// Combined public X25519-plus-Ed25519 Reticulum identity key.
///
/// The bytes are public protocol material but are redacted from `Debug` so
/// diagnostic output cannot accidentally become a bulk identity export.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct IdentityPublicKey([u8; IDENTITY_PUBLIC_KEY_LENGTH]);

impl IdentityPublicKey {
    /// Construct a public identity key from all bytes.
    pub const fn new(bytes: [u8; IDENTITY_PUBLIC_KEY_LENGTH]) -> Self {
        Self(bytes)
    }

    /// Borrow all public identity-key bytes.
    pub const fn as_bytes(&self) -> &[u8; IDENTITY_PUBLIC_KEY_LENGTH] {
        &self.0
    }
}

impl fmt::Debug for IdentityPublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("IdentityPublicKey(<redacted>)")
    }
}

/// Stable product-owned scalar for the interface that observed an announce.
///
/// This has the same complete one-byte range as an RNS interface identifier,
/// but is local to this dependency-free projection crate. It is descriptive
/// observation metadata, not permission to route on that interface.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ObservedInterfaceId(u8);

impl ObservedInterfaceId {
    /// Construct an observed-interface scalar from its complete value.
    pub const fn new(value: u8) -> Self {
        Self(value)
    }

    /// Return the raw observed-interface value.
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// Executor-independent monotonic time in whole milliseconds.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MonotonicMillis(u64);

impl MonotonicMillis {
    /// Construct a monotonic-millisecond sample.
    pub const fn new(milliseconds: u64) -> Self {
        Self(milliseconds)
    }

    /// Return the raw monotonic-millisecond value.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Optional physical-link signal observations.
///
/// Either value may be absent independently. Values use the same whole-dB
/// integer units as the current radio ingress boundary, while non-radio
/// interfaces can supply [`SignalObservations::UNKNOWN`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignalObservations {
    rssi_dbm: Option<i16>,
    snr_db: Option<i16>,
}

impl SignalObservations {
    /// No signal metadata was available at the owning ingress boundary.
    pub const UNKNOWN: Self = Self {
        rssi_dbm: None,
        snr_db: None,
    };

    /// Construct independently optional RSSI and SNR observations.
    pub const fn new(rssi_dbm: Option<i16>, snr_db: Option<i16>) -> Self {
        Self { rssi_dbm, snr_db }
    }

    /// Observed received signal strength in whole dBm, when available.
    pub const fn rssi_dbm(self) -> Option<i16> {
        self.rssi_dbm
    }

    /// Observed signal-to-noise ratio in whole dB, when available.
    pub const fn snr_db(self) -> Option<i16> {
        self.snr_db
    }
}

/// Non-identity metadata attached to one authenticated announce observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObservationMetadata {
    hops: u8,
    interface: ObservedInterfaceId,
    signal: SignalObservations,
    observed_at: MonotonicMillis,
}

impl ObservationMetadata {
    /// Construct complete observation metadata.
    pub const fn new(
        hops: u8,
        interface: ObservedInterfaceId,
        signal: SignalObservations,
        observed_at: MonotonicMillis,
    ) -> Self {
        Self {
            hops,
            interface,
            signal,
            observed_at,
        }
    }

    /// Hop count reported by the sole RNS owner.
    pub const fn hops(self) -> u8 {
        self.hops
    }

    /// Stable scalar of the interface that observed the announce.
    pub const fn interface(self) -> ObservedInterfaceId {
        self.interface
    }

    /// Optional physical-link signal observations.
    pub const fn signal(self) -> SignalObservations {
        self.signal
    }

    /// Monotonic time at which the announce was observed.
    pub const fn observed_at(self) -> MonotonicMillis {
        self.observed_at
    }
}

/// Borrowed, authenticated announce evidence supplied by the sole RNS owner.
///
/// Construction does not validate a signature, recompute either hash, or
/// establish a route. The caller must do those things at the protocol-owned
/// boundary before constructing this value.
#[derive(Clone, Copy)]
pub struct AuthenticatedAnnounceObservation<'a> {
    destination: DestinationHash,
    identity_hash: IdentityHash,
    public_key: IdentityPublicKey,
    app_data: &'a [u8],
    metadata: ObservationMetadata,
}

impl<'a> AuthenticatedAnnounceObservation<'a> {
    /// Assemble already-validated announce evidence for bounded retention.
    pub const fn new(
        destination: DestinationHash,
        identity_hash: IdentityHash,
        public_key: IdentityPublicKey,
        app_data: &'a [u8],
        metadata: ObservationMetadata,
    ) -> Self {
        Self {
            destination,
            identity_hash,
            public_key,
            app_data,
            metadata,
        }
    }

    /// Complete announced destination hash.
    pub const fn destination(self) -> DestinationHash {
        self.destination
    }

    /// Hash of the identity that authenticated the announce.
    pub const fn identity_hash(self) -> IdentityHash {
        self.identity_hash
    }

    /// Combined public identity key that authenticated the announce.
    pub const fn public_key(self) -> IdentityPublicKey {
        self.public_key
    }

    /// Borrow bounded application data exactly as authenticated.
    pub const fn app_data(self) -> &'a [u8] {
        self.app_data
    }

    /// Non-identity ingress and timing metadata.
    pub const fn metadata(self) -> ObservationMetadata {
        self.metadata
    }
}

impl fmt::Debug for AuthenticatedAnnounceObservation<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthenticatedAnnounceObservation")
            .field("destination", &self.destination)
            .field("identity_hash", &self.identity_hash)
            .field("public_key", &"<redacted>")
            .field("app_data_len", &self.app_data.len())
            .field("app_data", &"<redacted>")
            .field("metadata", &self.metadata)
            .finish()
    }
}

/// Nonzero table-local generation assigned to an accepted observation.
///
/// Generations order accepted observations even when multiple announces share
/// the same monotonic timestamp. They reset with a newly constructed volatile
/// table and are not durable protocol sequence numbers.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ObservationGeneration(u64);

impl ObservationGeneration {
    /// Return the nonzero generation value.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Exclusive generation cursor for one-record-at-a-time table reads.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DiscoveryCursor(u64);

impl DiscoveryCursor {
    /// Cursor before the first accepted observation.
    pub const START: Self = Self(0);

    /// Reconstruct a cursor received from a bounded device API.
    pub const fn new(exclusive_generation: u64) -> Self {
        Self(exclusive_generation)
    }

    /// Return the exclusive observation-generation value.
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl From<ObservationGeneration> for DiscoveryCursor {
    fn from(generation: ObservationGeneration) -> Self {
        Self(generation.0)
    }
}

/// One retained peer's latest authenticated announce observation.
///
/// This is an application discovery record only. In particular,
/// [`DiscoveredPeer::interface`] and [`DiscoveredPeer::hops`] are displayable
/// historical evidence rather than live routing instructions.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct DiscoveredPeer<const APP_DATA_CAPACITY: usize> {
    destination: DestinationHash,
    identity_hash: IdentityHash,
    public_key: IdentityPublicKey,
    app_data: [u8; APP_DATA_CAPACITY],
    app_data_len: usize,
    hops: u8,
    interface: ObservedInterfaceId,
    signal: SignalObservations,
    observed_at: MonotonicMillis,
    generation: ObservationGeneration,
}

impl<const APP_DATA_CAPACITY: usize> DiscoveredPeer<APP_DATA_CAPACITY> {
    fn from_observation(
        observation: AuthenticatedAnnounceObservation<'_>,
        generation: ObservationGeneration,
    ) -> Self {
        let mut app_data = [0; APP_DATA_CAPACITY];
        let app_data_len = observation.app_data.len();
        app_data[..app_data_len].copy_from_slice(observation.app_data);
        let metadata = observation.metadata;
        Self {
            destination: observation.destination,
            identity_hash: observation.identity_hash,
            public_key: observation.public_key,
            app_data,
            app_data_len,
            hops: metadata.hops,
            interface: metadata.interface,
            signal: metadata.signal,
            observed_at: metadata.observed_at,
            generation,
        }
    }

    /// Complete announced destination hash.
    pub const fn destination(&self) -> DestinationHash {
        self.destination
    }

    /// Hash of the identity that authenticated the announce.
    pub const fn identity_hash(&self) -> IdentityHash {
        self.identity_hash
    }

    /// Combined public identity key that authenticated the announce.
    pub const fn public_key(&self) -> IdentityPublicKey {
        self.public_key
    }

    /// Exact authenticated application data without zero padding.
    pub fn app_data(&self) -> &[u8] {
        &self.app_data[..self.app_data_len]
    }

    /// Hop count reported for the latest retained observation.
    pub const fn hops(&self) -> u8 {
        self.hops
    }

    /// Stable scalar for the interface that saw the latest observation.
    pub const fn interface(&self) -> ObservedInterfaceId {
        self.interface
    }

    /// Optional signal values attached to the latest observation.
    pub const fn signal(&self) -> SignalObservations {
        self.signal
    }

    /// Monotonic time of the latest retained observation.
    pub const fn observed_at(&self) -> MonotonicMillis {
        self.observed_at
    }

    /// Generation assigned to the latest retained observation.
    pub const fn generation(&self) -> ObservationGeneration {
        self.generation
    }
}

impl<const APP_DATA_CAPACITY: usize> fmt::Debug for DiscoveredPeer<APP_DATA_CAPACITY> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DiscoveredPeer")
            .field("destination", &self.destination)
            .field("identity_hash", &self.identity_hash)
            .field("public_key", &"<redacted>")
            .field("app_data_len", &self.app_data_len)
            .field("app_data", &"<redacted>")
            .field("hops", &self.hops)
            .field("interface", &self.interface)
            .field("signal", &self.signal)
            .field("observed_at", &self.observed_at)
            .field("generation", &self.generation)
            .finish()
    }
}

/// Invalid fixed table capacity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryCapacityError {
    /// A zero-slot table cannot retain any authenticated observation.
    ZeroPeerCapacity,
}

/// Monotonic-time failure while accepting an observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationTimeError {
    /// The supplied table-wide observation time preceded the last accepted one.
    Regressed {
        /// Last monotonic time accepted by this table.
        previous: MonotonicMillis,
        /// Regressing time supplied with the rejected observation.
        observed: MonotonicMillis,
    },
}

/// Failure to retain a new authenticated announce observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObserveError {
    /// Authenticated application data does not fit the configured record bound.
    AppDataTooLarge {
        /// Exact supplied application-data length.
        supplied: usize,
        /// Fixed application-data capacity of each record.
        capacity: usize,
    },
    /// The caller's monotonic observation clock regressed.
    Time(ObservationTimeError),
    /// Every nonzero 64-bit observation generation has been consumed.
    GenerationExhausted,
}

/// How an accepted observation changed the bounded projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObserveDisposition {
    /// A free slot retained a previously unseen destination.
    Inserted,
    /// The existing record for this destination was replaced in place.
    Updated,
    /// The oldest retained destination was deterministically replaced.
    Evicted {
        /// Destination removed from this discovery history.
        destination: DestinationHash,
    },
}

/// Successful bounded retention of one authenticated observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObserveOutcome {
    generation: ObservationGeneration,
    disposition: ObserveDisposition,
}

impl ObserveOutcome {
    /// Generation assigned to the accepted observation.
    pub const fn generation(self) -> ObservationGeneration {
        self.generation
    }

    /// Structural change made to the bounded table.
    pub const fn disposition(self) -> ObserveDisposition {
        self.disposition
    }
}

/// Invalid cursor supplied for a one-record discovery read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryCursorError {
    /// The cursor names a generation this table has not observed.
    AheadOfHistory {
        /// Supplied exclusive cursor.
        cursor: DiscoveryCursor,
        /// Latest generation in this table, or `None` before any observation.
        latest: Option<ObservationGeneration>,
    },
}

/// One bounded page from the live discovered-peer projection.
///
/// Records are returned in increasing latest-observation generation, not
/// physical slot order. Mutating the table between calls can make a refreshed
/// destination appear again at its new generation. This is intentionally not
/// an immutable event log.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiscoveryPage<const APP_DATA_CAPACITY: usize> {
    peer: Option<DiscoveredPeer<APP_DATA_CAPACITY>>,
    next_cursor: DiscoveryCursor,
    observed_through: Option<ObservationGeneration>,
    oldest_retained: Option<ObservationGeneration>,
    history_gap: bool,
}

impl<const APP_DATA_CAPACITY: usize> DiscoveryPage<APP_DATA_CAPACITY> {
    /// One retained record after the requested cursor, if present.
    pub const fn peer(&self) -> Option<&DiscoveredPeer<APP_DATA_CAPACITY>> {
        self.peer.as_ref()
    }

    /// Exclusive cursor for the next page request.
    pub const fn next_cursor(&self) -> DiscoveryCursor {
        self.next_cursor
    }

    /// Latest observation generation when this page was produced.
    pub const fn observed_through(&self) -> Option<ObservationGeneration> {
        self.observed_through
    }

    /// Oldest generation still represented by a current peer record.
    pub const fn oldest_retained(&self) -> Option<ObservationGeneration> {
        self.oldest_retained
    }

    /// Whether generations between the input cursor and returned peer are no
    /// longer represented in the live bounded projection.
    pub const fn history_gap(&self) -> bool {
        self.history_gap
    }
}

/// Fixed-capacity latest-observation history keyed by destination hash.
///
/// The two const parameters independently bound peer count and per-peer
/// application data. Full insertion evicts the record with the smallest
/// latest-observation generation; generations are unique, making eviction
/// independent of physical slot order or equal timestamps.
pub struct DiscoveredPeers<const PEER_CAPACITY: usize, const APP_DATA_CAPACITY: usize> {
    peers: [Option<DiscoveredPeer<APP_DATA_CAPACITY>>; PEER_CAPACITY],
    len: usize,
    latest_generation: u64,
    last_observed_at: Option<MonotonicMillis>,
}

impl<const PEER_CAPACITY: usize, const APP_DATA_CAPACITY: usize>
    DiscoveredPeers<PEER_CAPACITY, APP_DATA_CAPACITY>
{
    /// Construct an empty table or explicitly reject zero peer capacity.
    pub const fn try_new() -> Result<Self, DiscoveryCapacityError> {
        if PEER_CAPACITY == 0 {
            return Err(DiscoveryCapacityError::ZeroPeerCapacity);
        }
        Ok(Self {
            peers: [None; PEER_CAPACITY],
            len: 0,
            latest_generation: 0,
            last_observed_at: None,
        })
    }

    /// Number of currently retained peer destinations.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether no peer destination is currently retained.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Compile-time peer-record capacity.
    pub const fn capacity(&self) -> usize {
        PEER_CAPACITY
    }

    /// Latest accepted generation, or `None` before any observation.
    pub const fn latest_generation(&self) -> Option<ObservationGeneration> {
        if self.latest_generation == 0 {
            None
        } else {
            Some(ObservationGeneration(self.latest_generation))
        }
    }

    /// Last accepted table-wide monotonic observation time.
    pub const fn last_observed_at(&self) -> Option<MonotonicMillis> {
        self.last_observed_at
    }

    /// Return the latest retained observation for one destination.
    pub fn get(&self, destination: DestinationHash) -> Option<&DiscoveredPeer<APP_DATA_CAPACITY>> {
        self.peers
            .iter()
            .flatten()
            .find(|peer| peer.destination == destination)
    }

    /// Retain one trusted authenticated announce observation atomically.
    ///
    /// Equal timestamps are accepted because generations provide total
    /// observation order. Oversize data, a regressing clock, or generation
    /// exhaustion leaves every table field unchanged.
    pub fn observe(
        &mut self,
        observation: AuthenticatedAnnounceObservation<'_>,
    ) -> Result<ObserveOutcome, ObserveError> {
        if observation.app_data.len() > APP_DATA_CAPACITY {
            return Err(ObserveError::AppDataTooLarge {
                supplied: observation.app_data.len(),
                capacity: APP_DATA_CAPACITY,
            });
        }
        if let Some(previous) = self.last_observed_at
            && observation.metadata.observed_at < previous
        {
            return Err(ObserveError::Time(ObservationTimeError::Regressed {
                previous,
                observed: observation.metadata.observed_at,
            }));
        }
        let next_generation = self
            .latest_generation
            .checked_add(1)
            .ok_or(ObserveError::GenerationExhausted)?;
        let generation = ObservationGeneration(next_generation);

        let (slot, disposition) = if let Some(index) = self.index_of(observation.destination) {
            (index, ObserveDisposition::Updated)
        } else if let Some(index) = self.first_empty_slot() {
            (index, ObserveDisposition::Inserted)
        } else {
            let index = self.oldest_slot();
            let evicted = self.peers[index]
                .as_ref()
                .expect("nonzero full table has an oldest record")
                .destination;
            (
                index,
                ObserveDisposition::Evicted {
                    destination: evicted,
                },
            )
        };

        self.peers[slot] = Some(DiscoveredPeer::from_observation(observation, generation));
        if matches!(disposition, ObserveDisposition::Inserted) {
            self.len += 1;
        }
        self.latest_generation = next_generation;
        self.last_observed_at = Some(observation.metadata.observed_at);
        Ok(ObserveOutcome {
            generation,
            disposition,
        })
    }

    /// Return at most one current record after an exclusive generation cursor.
    ///
    /// An ahead-of-history cursor is explicit so an API client can reset after
    /// reconnecting to a newly booted volatile table. [`DiscoveryPage::history_gap`]
    /// reports generations no longer represented because records were updated
    /// or evicted.
    pub fn page_after(
        &self,
        cursor: DiscoveryCursor,
    ) -> Result<DiscoveryPage<APP_DATA_CAPACITY>, DiscoveryCursorError> {
        let latest = self.latest_generation();
        if cursor.0 > self.latest_generation {
            return Err(DiscoveryCursorError::AheadOfHistory { cursor, latest });
        }

        let oldest_retained = self
            .peers
            .iter()
            .flatten()
            .map(|peer| peer.generation)
            .min();
        let peer = self
            .peers
            .iter()
            .flatten()
            .filter(|peer| peer.generation.0 > cursor.0)
            .min_by_key(|peer| peer.generation)
            .copied();
        let next_cursor = peer
            .map(|peer| DiscoveryCursor::from(peer.generation))
            .unwrap_or(cursor);
        let history_gap = peer.is_some_and(|peer| peer.generation.0 - cursor.0 > 1);

        Ok(DiscoveryPage {
            peer,
            next_cursor,
            observed_through: latest,
            oldest_retained,
            history_gap,
        })
    }

    fn index_of(&self, destination: DestinationHash) -> Option<usize> {
        self.peers.iter().position(|peer| {
            peer.as_ref()
                .is_some_and(|peer| peer.destination == destination)
        })
    }

    fn first_empty_slot(&self) -> Option<usize> {
        self.peers.iter().position(Option::is_none)
    }

    fn oldest_slot(&self) -> usize {
        self.peers
            .iter()
            .enumerate()
            .filter_map(|(index, peer)| peer.as_ref().map(|peer| (index, peer.generation)))
            .min_by_key(|(_, generation)| *generation)
            .map(|(index, _)| index)
            .expect("nonzero full table has an oldest record")
    }
}

impl<const PEER_CAPACITY: usize, const APP_DATA_CAPACITY: usize> fmt::Debug
    for DiscoveredPeers<PEER_CAPACITY, APP_DATA_CAPACITY>
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DiscoveredPeers")
            .field("capacity", &PEER_CAPACITY)
            .field("app_data_capacity", &APP_DATA_CAPACITY)
            .field("len", &self.len)
            .field("latest_generation", &self.latest_generation())
            .field("last_observed_at", &self.last_observed_at)
            .field("peers", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
mod tests;
