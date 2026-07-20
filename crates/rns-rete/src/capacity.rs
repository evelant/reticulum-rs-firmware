//! Observable capacity and defensive admission control for the Phase-0 Rete node.
//!
//! Rete's heapless transport uses independent fixed-capacity collections. Some
//! have recoverable overflow policies (path LRU eviction, dedup FIFO eviction,
//! and an announce-queue `false` result). The pinned integration revision also
//! makes owned- and relayed-Link insertion transactional. The owning
//! `EmbeddedNode` keeps the crate-private outbound preflight here to enforce
//! product quotas and stable local diagnostics before native packet
//! construction.

use rand_core::{CryptoRng, RngCore};
use rete_core::{DestHash, LinkId};
use rete_stack::{NodeCore, OutboundPacket};
use rete_transport::{HeaplessStorage, SendError};

#[cfg(test)]
use crate::ProbeNode;

/// Occupancy of one fixed-capacity table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityUse {
    /// Entries currently visible through Rete's public API.
    pub used: usize,
    /// Configured maximum entries.
    pub limit: usize,
}

impl CapacityUse {
    /// Slots that can still be admitted without eviction or rejection.
    pub const fn available(self) -> usize {
        self.limit.saturating_sub(self.used)
    }

    /// Whether the collection has reached its configured limit.
    pub const fn is_full(self) -> bool {
        self.used >= self.limit
    }
}

/// Publicly observable occupancy for a heapless Rete node.
///
/// Rete does not currently expose occupancy for its identity, resource,
/// announce-replay, announce-rate, path-request-throttle, or packet-dedup
/// tables. Those collections therefore cannot be represented here without an
/// upstream API change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaplessCapacitySnapshot {
    /// Learned destination paths. Full insertion evicts the LRU path.
    pub paths: CapacityUse,
    /// Pending announces. Full insertion is rejected.
    pub announces: CapacityUse,
    /// Locally owned link sessions. Full insertion must be rejected before send.
    pub links: CapacityUse,
    /// Link routes retained on behalf of two remote endpoints.
    pub relay_links: CapacityUse,
    /// Reverse-routing entries. Transported H2 insertion is transactional;
    /// the temporary H1 local-origin seam is guarded at the owning boundary.
    pub reverse_entries: CapacityUse,
    /// Pending DATA delivery-proof receipts. The pinned DATA path rejects full
    /// admission transactionally.
    pub receipts: CapacityUse,
    /// Pending channel receipts. Rete currently drops insertion failures.
    pub channel_receipts: CapacityUse,
    /// Configured rolling packet-deduplication window; occupancy is not exposed.
    pub deduplication_limit: usize,
}

/// Snapshot all table occupancies that the pinned Rete API exposes.
pub(crate) fn heapless_capacity_snapshot<
    const P: usize,
    const A: usize,
    const D: usize,
    const L: usize,
>(
    node: &NodeCore<HeaplessStorage<P, A, D, L>>,
) -> HeaplessCapacitySnapshot {
    HeaplessCapacitySnapshot {
        paths: CapacityUse {
            used: node.transport.path_count(),
            limit: P,
        },
        announces: CapacityUse {
            used: node.transport.announce_count(),
            limit: A,
        },
        links: CapacityUse {
            used: node.transport.link_count(),
            limit: L,
        },
        relay_links: CapacityUse {
            used: node.transport.relay_link_count(),
            limit: L,
        },
        reverse_entries: CapacityUse {
            used: node.transport.reverse_count(),
            limit: P,
        },
        receipts: CapacityUse {
            used: node.transport.receipt_count(),
            limit: P,
        },
        channel_receipts: CapacityUse {
            used: node.transport.channel_receipt_count(),
            limit: L,
        },
        deduplication_limit: D,
    }
}

/// Capacity snapshot for the initial Phase-0 profile.
#[cfg(test)]
pub(crate) type ProbeCapacitySnapshot = HeaplessCapacitySnapshot;

/// Snapshot the initial Phase-0 profile without spelling its const generics.
#[cfg(test)]
pub(crate) fn probe_capacity_snapshot(node: &ProbeNode) -> ProbeCapacitySnapshot {
    heapless_capacity_snapshot(node)
}

/// Failure to admit a new locally owned link session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkAdmissionError {
    /// The heapless link map is already at capacity.
    LinkTableFull { limit: usize },
    /// Fresh Link construction reproduced an already-owned Link ID.
    ///
    /// This normally indicates repeated cryptographic RNG output. Existing
    /// state is retained and no request is released.
    LinkIdCollision,
    /// Rete rejected link construction for a native protocol reason.
    Rete(SendError),
    /// Rete returned success but did not retain the new link in its map.
    ///
    /// The capacity preflight should make this unreachable in a single-threaded
    /// node. Keeping the check prevents a future upstream behavior change from
    /// causing the caller to transmit an unusable link request.
    LinkStateNotRetained,
}

impl core::fmt::Display for LinkAdmissionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::LinkTableFull { limit } => {
                write!(f, "local link table is full (limit {limit})")
            }
            Self::LinkIdCollision => write!(f, "generated link ID already exists"),
            Self::Rete(error) => write!(f, "Rete link initiation failed: {error}"),
            Self::LinkStateNotRetained => {
                write!(f, "Rete built a link request but did not retain its state")
            }
        }
    }
}

/// Initiate a link only when its heapless session state can be retained.
///
/// The product preflight reports the configured limit before native packet
/// construction. The pinned Rete revision independently returns
/// `SendError::LinkTableFull`, and the retained-state check remains a defensive
/// invariant. This guard is safe because a `NodeCore` is mutably borrowed for
/// the entire operation.
pub(crate) fn try_initiate_heapless_link<
    R: RngCore + CryptoRng,
    const P: usize,
    const A: usize,
    const D: usize,
    const L: usize,
>(
    node: &mut NodeCore<HeaplessStorage<P, A, D, L>>,
    destination: DestHash,
    now: u64,
    rng: &mut R,
) -> Result<(OutboundPacket, LinkId), LinkAdmissionError> {
    if node.transport.link_count() >= L {
        return Err(LinkAdmissionError::LinkTableFull { limit: L });
    }

    let result = match node.initiate_link(destination, now, rng) {
        Ok(result) => result,
        Err(SendError::LinkTableFull) => {
            return Err(LinkAdmissionError::LinkTableFull { limit: L });
        }
        Err(SendError::LinkAlreadyExists) => return Err(LinkAdmissionError::LinkIdCollision),
        Err(error) => return Err(LinkAdmissionError::Rete(error)),
    };
    if node.transport.get_link(&result.1).is_none() {
        return Err(LinkAdmissionError::LinkStateNotRetained);
    }

    Ok(result)
}

/// Link admission error for the initial Phase-0 profile.
#[cfg(test)]
pub(crate) type ProbeLinkError = LinkAdmissionError;

/// Guarded link initiation for the initial Phase-0 profile.
#[cfg(test)]
pub(crate) fn try_initiate_probe_link<R: RngCore + CryptoRng>(
    node: &mut ProbeNode,
    destination: DestHash,
    now: u64,
    rng: &mut R,
) -> Result<(OutboundPacket, LinkId), ProbeLinkError> {
    try_initiate_heapless_link(node, destination, now, rng)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe_capacity;
    use rete_core::{DestType, MTU, PacketBuilder, PacketType};
    use rete_transport::{
        IngestResult, Link, LinkTableKind, Path, PendingAnnounce, Transport, compute_link_id,
    };

    #[derive(Default)]
    struct CounterRng(u8);

    impl RngCore for CounterRng {
        fn next_u32(&mut self) -> u32 {
            let mut bytes = [0u8; 4];
            self.fill_bytes(&mut bytes);
            u32::from_le_bytes(bytes)
        }

        fn next_u64(&mut self) -> u64 {
            let mut bytes = [0u8; 8];
            self.fill_bytes(&mut bytes);
            u64::from_le_bytes(bytes)
        }

        fn fill_bytes(&mut self, dest: &mut [u8]) {
            for byte in dest {
                self.0 = self.0.wrapping_add(1);
                *byte = self.0;
            }
        }

        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
            self.fill_bytes(dest);
            Ok(())
        }
    }

    impl CryptoRng for CounterRng {}

    fn node() -> ProbeNode {
        let identity = crate::Identity::from_seed(&[0x43; 32]).unwrap();
        ProbeNode::new(identity, "reticulum", &["phase0"]).unwrap()
    }

    fn pending_announce(tag: u8) -> PendingAnnounce {
        PendingAnnounce {
            dest_hash: DestHash::from([tag; rete_core::TRUNCATED_HASH_LEN]),
            raw: alloc::vec![tag],
            tx_count: 0,
            retransmit_timeout: 0,
            local: false,
            local_rebroadcasts: 0,
            block_rebroadcasts: false,
            received_hops: 0,
        }
    }

    type TinyResponder = Transport<HeaplessStorage<4, 2, 8, 2>>;

    fn tiny_responder() -> (TinyResponder, crate::Identity, DestHash) {
        let identity = crate::Identity::from_seed(b"project inbound capacity responder").unwrap();
        let mut name_buf = [0u8; 128];
        let expanded = rete_core::expand_name("reticulum", &["capacity"], &mut name_buf).unwrap();
        let destination = rete_core::destination_hash(expanded, Some(&identity.hash()));

        let mut transport = TinyResponder::new();
        transport.add_local_destination(destination);
        (transport, identity, destination)
    }

    fn native_link_request<R: RngCore + CryptoRng>(
        destination: DestHash,
        initiator: &crate::Identity,
        rng: &mut R,
    ) -> alloc::vec::Vec<u8> {
        let (_, payload) = Link::new_initiator(destination, initiator.ed25519_pub(), rng, 100);
        let mut buffer = [0u8; MTU];
        let len = PacketBuilder::new(&mut buffer)
            .packet_type(PacketType::LinkRequest)
            .dest_type(DestType::Single)
            .destination_hash(destination.as_ref())
            .context(0)
            .payload(&payload)
            .build()
            .unwrap();
        buffer[..len].to_vec()
    }

    #[test]
    fn snapshot_reports_only_publicly_observable_probe_tables() {
        let node = node();
        let snapshot = probe_capacity_snapshot(&node);

        assert_eq!(snapshot.paths, CapacityUse { used: 0, limit: 64 });
        assert_eq!(snapshot.announces, CapacityUse { used: 0, limit: 16 });
        assert_eq!(snapshot.links, CapacityUse { used: 0, limit: 4 });
        assert_eq!(snapshot.relay_links, CapacityUse { used: 0, limit: 4 });
        assert_eq!(snapshot.reverse_entries, CapacityUse { used: 0, limit: 64 });
        assert_eq!(snapshot.receipts, CapacityUse { used: 0, limit: 64 });
        assert_eq!(snapshot.channel_receipts, CapacityUse { used: 0, limit: 4 });
        assert_eq!(snapshot.deduplication_limit, 128);
        assert_eq!(snapshot.links.available(), 4);
        assert!(!snapshot.links.is_full());

        type TinyNode = NodeCore<HeaplessStorage<2, 3, 5, 2>>;
        let tiny = TinyNode::new(
            crate::Identity::from_seed(b"tiny capacity profile").unwrap(),
            "reticulum",
            &["tiny"],
        )
        .unwrap();
        let tiny_snapshot = heapless_capacity_snapshot(&tiny);
        assert_eq!(tiny_snapshot.paths.limit, 2);
        assert_eq!(tiny_snapshot.announces.limit, 3);
        assert_eq!(tiny_snapshot.deduplication_limit, 5);
        assert_eq!(tiny_snapshot.links.limit, 2);
        assert_eq!(tiny_snapshot.relay_links.limit, 2);
    }

    #[test]
    fn path_capacity_evicts_the_least_recently_used_entry() {
        let mut node = node();

        for tag in 0..probe_capacity::PATHS as u8 {
            assert!(node.transport.insert_path(
                DestHash::from([tag; rete_core::TRUNCATED_HASH_LEN]),
                Path::direct(u64::from(tag)),
            ));
        }

        let full = probe_capacity_snapshot(&node).paths;
        assert!(full.is_full());
        assert_eq!(full.available(), 0);

        let replacement = DestHash::from([0xFE; rete_core::TRUNCATED_HASH_LEN]);
        assert!(node.transport.insert_path(replacement, Path::direct(1_000)));
        assert_eq!(node.transport.path_count(), probe_capacity::PATHS);
        assert!(node.transport.get_path(&replacement).is_some());
        assert!(
            node.transport
                .get_path(&DestHash::from([0; rete_core::TRUNCATED_HASH_LEN]))
                .is_none(),
            "oldest path must be evicted"
        );
    }

    #[test]
    fn dedup_capacity_is_a_rolling_fifo_window() {
        let mut node = node();

        for tag in 0..probe_capacity::DEDUPLICATION_ENTRIES as u8 {
            assert!(!node.transport.is_duplicate(&[tag; 32]));
        }
        assert!(node.transport.is_duplicate(&[0; 32]));

        assert!(!node.transport.is_duplicate(&[0xFE; 32]));
        assert!(
            !node.transport.is_duplicate(&[0; 32]),
            "inserting past capacity must evict the oldest hash"
        );
    }

    #[test]
    fn announce_capacity_rejects_without_growing() {
        let mut node = node();

        for tag in 0..probe_capacity::ANNOUNCES as u8 {
            assert!(node.transport.queue_announce(pending_announce(tag)));
        }
        assert!(probe_capacity_snapshot(&node).announces.is_full());

        assert!(!node.transport.queue_announce(pending_announce(0xFE)));
        assert_eq!(node.transport.announce_count(), probe_capacity::ANNOUNCES);
    }

    #[test]
    fn guarded_link_initiation_fails_before_emitting_an_untracked_request() {
        let mut node = node();
        let mut rng = CounterRng::default();

        for tag in 0..probe_capacity::LINKS as u8 {
            let (packet, link_id) = try_initiate_probe_link(
                &mut node,
                DestHash::from([tag.wrapping_add(1); rete_core::TRUNCATED_HASH_LEN]),
                u64::from(tag),
                &mut rng,
            )
            .unwrap();
            assert!(!packet.data.is_empty());
            assert!(node.transport.get_link(&link_id).is_some());
        }

        assert!(probe_capacity_snapshot(&node).links.is_full());
        assert!(matches!(
            try_initiate_probe_link(
                &mut node,
                DestHash::from([0xFE; rete_core::TRUNCATED_HASH_LEN]),
                100,
                &mut rng,
            ),
            Err(ProbeLinkError::LinkTableFull {
                limit: probe_capacity::LINKS
            })
        ));
        assert_eq!(node.transport.link_count(), probe_capacity::LINKS);
    }

    #[test]
    fn pinned_rete_native_link_initiation_reports_table_full() {
        let mut node = node();
        let mut rng = CounterRng::default();

        for tag in 0..probe_capacity::LINKS as u8 {
            node.initiate_link(
                DestHash::from([tag.wrapping_add(1); rete_core::TRUNCATED_HASH_LEN]),
                u64::from(tag),
                &mut rng,
            )
            .unwrap();
        }

        assert!(matches!(
            node.initiate_link(
                DestHash::from([0xFE; rete_core::TRUNCATED_HASH_LEN]),
                100,
                &mut rng,
            ),
            Err(SendError::LinkTableFull)
        ));
        assert_eq!(node.transport.link_count(), probe_capacity::LINKS);
    }

    #[test]
    fn pinned_rete_native_inbound_owned_link_capacity_is_transactional() {
        let (mut transport, responder, destination) = tiny_responder();
        let mut rng = CounterRng::default();
        let mut accepted_ids = alloc::vec::Vec::new();

        for tag in [0x11, 0x22] {
            let initiator = crate::Identity::from_seed(&[tag; 32]).unwrap();
            let mut request = native_link_request(destination, &initiator, &mut rng);
            match transport.ingest(&mut request, 100 + u64::from(tag), &mut rng, &responder) {
                IngestResult::LinkRequestReceived { link_id, proof_raw } => {
                    assert!(!proof_raw.is_empty());
                    accepted_ids.push(link_id);
                }
                other => panic!("expected retained LINKREQUEST, got {other:?}"),
            }
        }
        assert_eq!(transport.link_count(), 2);

        let candidate = crate::Identity::from_seed(&[0x33; 32]).unwrap();
        let rejected_request = native_link_request(destination, &candidate, &mut rng);
        let rejected_id = compute_link_id(&rejected_request).unwrap();
        let stats_before_rejection = transport.stats().clone();
        let mut rejected_attempt = rejected_request.clone();

        // The typed result cannot carry an LRPROOF. Capacity rejection must
        // retain neither candidate state nor link-success/error counters.
        assert!(matches!(
            transport.ingest(&mut rejected_attempt, 200, &mut rng, &responder),
            IngestResult::LinkTableFull {
                link_id,
                table: LinkTableKind::Owned,
            } if link_id == rejected_id
        ));
        assert_eq!(transport.link_count(), 2);
        assert!(
            accepted_ids
                .iter()
                .all(|link_id| transport.get_link(link_id).is_some())
        );
        assert!(transport.get_link(&rejected_id).is_none());

        let stats_after_rejection = transport.stats();
        assert_eq!(
            stats_after_rejection.packets_received,
            stats_before_rejection.packets_received + 1
        );
        assert_eq!(
            stats_after_rejection.link_requests_received,
            stats_before_rejection.link_requests_received
        );
        assert_eq!(
            stats_after_rejection.packets_dropped_invalid,
            stats_before_rejection.packets_dropped_invalid
        );
        assert_eq!(
            stats_after_rejection.packets_dropped_dedup,
            stats_before_rejection.packets_dropped_dedup
        );
        assert_eq!(
            stats_after_rejection.links_established,
            stats_before_rejection.links_established
        );
        assert_eq!(
            stats_after_rejection.links_failed,
            stats_before_rejection.links_failed
        );
        assert_eq!(
            stats_after_rejection.crypto_failures,
            stats_before_rejection.crypto_failures
        );

        let close_packet = transport
            .build_linkclose_packet(&accepted_ids[0], &mut rng)
            .unwrap();
        assert!(!close_packet.is_empty());
        assert_eq!(transport.link_count(), 1);
        assert!(transport.get_link(&accepted_ids[0]).is_none());

        // Releasing capacity does not make the rejected packet reusable: its
        // hash remains in Rete's packet-level deduplication window.
        let mut exact_replay = rejected_request.clone();
        assert!(matches!(
            transport.ingest(&mut exact_replay, 201, &mut rng, &responder),
            IngestResult::Duplicate
        ));
        assert_eq!(transport.link_count(), 1);
        assert!(transport.get_link(&rejected_id).is_none());

        // A legitimate retry has fresh ephemeral Link material and can claim
        // the released slot.
        let mut fresh_request = native_link_request(destination, &candidate, &mut rng);
        let fresh_id = compute_link_id(&fresh_request).unwrap();
        assert_ne!(fresh_id, rejected_id);
        assert!(matches!(
            transport.ingest(&mut fresh_request, 202, &mut rng, &responder),
            IngestResult::LinkRequestReceived { link_id, proof_raw }
                if link_id == fresh_id && !proof_raw.is_empty()
        ));
        assert_eq!(transport.link_count(), 2);
        assert!(transport.get_link(&fresh_id).is_some());
        assert!(transport.get_link(&accepted_ids[1]).is_some());

        let final_stats = transport.stats();
        assert_eq!(final_stats.packets_received, 5);
        assert_eq!(final_stats.link_requests_received, 3);
        assert_eq!(final_stats.packets_dropped_dedup, 1);
        assert_eq!(final_stats.packets_dropped_invalid, 0);
        assert_eq!(final_stats.links_failed, 0);
        assert_eq!(final_stats.crypto_failures, 0);
    }
}
