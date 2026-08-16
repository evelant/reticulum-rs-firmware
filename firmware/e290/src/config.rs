//! Product profile for the E290 appliance firmware.

use core::num::NonZeroU64;

#[cfg(test)]
use reticulum_board_e290_radio::E290_NA915_DEFAULT_CONFIGURATION;
use reticulum_board_e290_radio::{E290_NA915_DEFAULT_PROFILE, E290RadioConfiguration};
#[cfg(feature = "gateway")]
use reticulum_interface_router::InterfaceTopology;
use reticulum_interface_router::{
    AdvertisedBitrate, AnnouncePropagationMode, InterfaceConfigId, InterfaceCost,
    InterfaceProperties, LogicalMtu, RecursivePathSearchMode,
};
use reticulum_node_core::{
    MonotonicMillis, OrdinaryActionAdmissionError, PacketInterfaceId, TxCompletionCode,
    TxLeaseDeadline,
};
use reticulum_radio_interface::{LoRaProfile, LogicalPacketAccessConfig};
use reticulum_radio_tx_dispatch::{RadioTxCompletionCodes, RadioTxDispatcherConfig};
use reticulum_tx_supervisor::{
    DataPermitServerStep, DataRouterConfig, DataRouterStep, NodeInterfaceOrdinaryOfferError,
    NodeInterfaceSupervisorTransition, OrdinaryPermitServerStep, OrdinaryRouterAdmission,
    OrdinaryRouterConfig, OrdinaryRouterOfferError, OrdinaryRouterStep,
};

/// Application component of the permanent node's Reticulum destination name.
pub const RNS_APPLICATION_NAME: &str = "reticulum";
/// Aspect component of the permanent node's primary Reticulum destination name.
pub const RNS_PRIMARY_ASPECT: &str = "embedded-node";
/// Complete aspect list of the permanent node's primary destination.
pub const RNS_PRIMARY_ASPECTS: [&str; 1] = [RNS_PRIMARY_ASPECT];
/// Application component of the inbound LXMF delivery destination.
pub const RNS_LXMF_APPLICATION_NAME: &str = "lxmf";
/// Complete aspect list of the inbound LXMF delivery destination.
pub const RNS_LXMF_DELIVERY_ASPECTS: [&str; 1] = ["delivery"];
/// Maximum encoded bytes for one named LXMF delivery announce app data.
///
/// The app data is MessagePack `[name, nil, []]`. A 32-byte UTF-8 name uses a
/// two-byte `str8` header, giving `1 + 2 + 32 + 1 + 1 = 37` bytes. Shorter
/// names use a one-byte `fixstr` header and stay below this ceiling.
pub const MAX_LXMF_DELIVERY_ANNOUNCE_APP_DATA_BYTES: usize =
    1 + 2 + reticulum_device_api::MAX_DEVICE_NAME_BYTES + 1 + 1;

/// Encode the LXMF delivery announce app data `[name, nil, []]`.
///
/// The first array item is the human-readable display name decoded by nearby
/// peers. The explicit empty functionality list avoids the legacy interpretation
/// that a missing third field implies LXMF compression support.
///
/// Returns the number of encoded bytes written to `output`.
pub fn encode_lxmf_delivery_announce_app_data(
    name: &str,
    output: &mut [u8; MAX_LXMF_DELIVERY_ANNOUNCE_APP_DATA_BYTES],
) -> usize {
    let bytes = name.as_bytes();
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
    cursor += 1;
    cursor
}

/// Fixed native path-table capacity.
///
/// The production E290 supervisor is allocated in external PSRAM. Retain enough
/// routes for the local LoRa mesh plus the larger announce surface seen through
/// an enabled public TCP border interface without letting border churn evict
/// the local routes the operator is actually using. Each path caches its raw
/// announce packet inline in PSRAM (not in the strict internal heap), so the
/// table can grow without starving the Wi-Fi controller's receive buffers.
pub const PATHS: usize = 256;
/// Pending local announce capacity.
pub const ANNOUNCES: usize = 4;
/// Native packet deduplication capacity.
///
/// Retain two packet hashes per path so a four-node shared LoRa mesh and a
/// public TCP interface cannot immediately evict the duplicate evidence needed
/// to suppress delayed shared-medium copies.
pub const DEDUPLICATION: usize = 512;
/// Fixed Rete link-state capacity.
pub const LINKS: usize = 4;
/// Statically owned destination-DATA packet buffers.
pub const DATA_BUFFERS: usize = 4;
/// Statically owned ordinary-action packet buffers.
pub const ORDINARY_BUFFERS: usize = 8;
const _: () = assert!(ANNOUNCES <= ORDINARY_BUFFERS);
/// Transport-neutral application events retained outside Rete at once.
///
/// This is the first E290 outer-owner profile, not a protocol or no-PSRAM
/// ceiling. Packet-sized payload allocations already created by Rete move into
/// these slots without cloning; future RNS Resource bodies use durable blob
/// handles instead of consuming an assembled event slot.
pub const APPLICATION_EVENT_SLOTS: usize = 16;
/// Internal static RAM occupied by the fixed application-event slot array.
pub const APPLICATION_EVENT_STORAGE_BYTES: usize =
    core::mem::size_of::<[reticulum_node_core::ApplicationEventSlot; APPLICATION_EVENT_SLOTS]>();
/// Delayed proofs retained after durable LXMF commit and before ordinary TX handoff.
///
/// One slot per application-event slot lets independent events defer without
/// making a single retry or proof owner a global admission ceiling.
pub const LXMF_DELAYED_PROOF_SLOTS: usize = APPLICATION_EVENT_SLOTS;
/// Exact initialized byte span occupied by the external delayed-proof slice.
pub const LXMF_DELAYED_PROOF_STORAGE_BYTES: usize =
    core::mem::size_of::<[reticulum_node_core::DelayedProofSlot; LXMF_DELAYED_PROOF_SLOTS]>();
const _: () = assert!(LXMF_DELAYED_PROOF_SLOTS == 16);
/// Latest authenticated remote `lxmf.delivery` destinations retained for the
/// app's nearby picker.
///
/// This is a product profile rather than a protocol ceiling. The table lives
/// in external PSRAM with the other LXMF volatile owners.
pub const LXMF_DISCOVERED_PEERS: usize = 32;
/// Maximum authenticated announce application data retained per discovered peer.
pub const LXMF_DISCOVERED_PEER_APP_DATA_BYTES: usize =
    reticulum_device_api::MAX_LXMF_PEER_APP_DATA_BYTES;
/// External PSRAM occupied by the bounded nearby-peer projection.
pub const LXMF_DISCOVERED_PEER_STORAGE_BYTES: usize = core::mem::size_of::<
    reticulum_peer_discovery::DiscoveredPeers<
        LXMF_DISCOVERED_PEERS,
        LXMF_DISCOVERED_PEER_APP_DATA_BYTES,
    >,
>();
const _: () = assert!(LXMF_DISCOVERED_PEERS > 0);
const _: () = assert!(
    LXMF_DISCOVERED_PEER_APP_DATA_BYTES <= reticulum_device_api::MAX_LXMF_PEER_APP_DATA_BYTES
);
/// Caller-owned LXMF index slots retained in external PSRAM for the full boot.
pub const LXMF_INDEX_SLOTS: usize =
    crate::partition_contract::LXMF_STORE_LEN as usize / reticulum_lxmf_store::EXTENT_SIZE;
/// Exact initialized byte span occupied by the external LXMF index slice.
pub const LXMF_INDEX_STORAGE_BYTES: usize =
    core::mem::size_of::<reticulum_lxmf_store::LxmfStoreIndexSlot>() * LXMF_INDEX_SLOTS;
const _: () = assert!(
    (crate::partition_contract::LXMF_STORE_LEN as usize)
        .is_multiple_of(reticulum_lxmf_store::EXTENT_SIZE)
);
const _: () = assert!(LXMF_INDEX_SLOTS == 1_024);
const _: () = assert!(LXMF_INDEX_STORAGE_BYTES > 0);
/// Concrete interface actors in the ordinary and station-only profiles.
#[cfg(not(feature = "gateway"))]
pub const INTERFACE_SLOTS: usize = 1;
/// Concrete LoRa and outbound TCP actors in the border-node profile.
#[cfg(feature = "gateway")]
pub const INTERFACE_SLOTS: usize = 2;
/// Jobs, completions and ingress buffers available per concrete actor.
pub const INTERFACE_QUEUE_DEPTH: usize = 2;

/// Submission records retained by the PSRAM-backed product profile.
///
/// One hundred twenty-eight supports a useful multi-message workload while
/// remaining below the append-only journal's explicit 154-submission lifetime.
/// This runtime is deliberately allocated in external PSRAM and is not a
/// non-PSRAM profile. Reclamation is still required for an indefinitely running
/// product.
pub const DURABLE_SUBMISSIONS: usize = 128;
/// Volatile lifecycle correlations retained by the resident runtime.
pub const DURABLE_PROJECTED_SUBMISSIONS: usize = 128;
/// Boot-lifetime PSRAM occupied by the backend-independent durable runtime.
pub const DURABLE_RUNTIME_BYTES: usize = core::mem::size_of::<
    reticulum_submission_runtime::SubmissionRuntime<
        DURABLE_SUBMISSIONS,
        DURABLE_PROJECTED_SUBMISSIONS,
        LINKS,
    >,
>();
// Keep both reviewed layouts explicit so an otherwise source-compatible field
// or alignment change cannot silently consume target PSRAM or host-test RAM.
// Xtensa's 32-bit field layout is 32 bytes smaller than the 64-bit host layout.
#[cfg(target_arch = "xtensa")]
const REVIEWED_DURABLE_RUNTIME_BYTES: usize = 396_560;
#[cfg(not(target_arch = "xtensa"))]
const REVIEWED_DURABLE_RUNTIME_BYTES: usize = 396_592;
const _: [(); REVIEWED_DURABLE_RUNTIME_BYTES] = [(); DURABLE_RUNTIME_BYTES];
/// Guard against silently growing the PSRAM-backed runtime and its independent
/// journal-replay scratch index.
///
/// The scratch index keeps all retry and compaction validation off the CPU
/// stack while preserving the live index until a durable outcome. The
/// remaining margin covers small metadata additions without allowing a
/// multi-fold regression to hide behind the board's large PSRAM.
pub const MAXIMUM_DURABLE_RUNTIME_BYTES: usize = 512 * 1024;
const _: () = assert!(DURABLE_RUNTIME_BYTES <= MAXIMUM_DURABLE_RUNTIME_BYTES);
/// Accepted submissions permitted before the non-reclaiming append-only journal
/// reports explicit capacity exhaustion.
pub const DURABLE_ACCEPTED_SUBMISSION_LIMIT: usize = DURABLE_SUBMISSIONS;
/// First durable submission identifier in an empty product journal.
pub const FIRST_SUBMISSION_ID: u64 = 1;

/// Immutable product configuration identity for E290 NA915 LoRa.
pub const LORA_INTERFACE_CONFIG_ID: InterfaceConfigId = InterfaceConfigId::new(0xe290_0001);
/// Initial relative route cost for LoRa.
pub const LORA_INTERFACE_COST: InterfaceCost = InterfaceCost::new(10);
/// Nominal SF7/BW125/CR4/5 PHY bitrate advertised for diagnostics.
pub const LORA_ADVERTISED_BITRATE: AdvertisedBitrate =
    lora_advertised_bitrate(E290_NA915_DEFAULT_PROFILE);
/// Immutable product configuration identity for the first outbound TCP peer.
pub const TCP_INTERFACE_CONFIG_ID: InterfaceConfigId = InterfaceConfigId::new(0xe290_0002);
/// Initial relative route cost for a reachable IP peer.
pub const TCP_INTERFACE_COST: InterfaceCost = InterfaceCost::new(1);

/// Bitmask of RNS interface indices treated as shared-medium for route-table
/// reservation.
///
/// The LoRa interface (index 1, see `main::LORA_INTERFACE`) is the product's
/// only shared-medium bearer; the TCP uplink (index 2) is point-to-point.
/// Marking LoRa shared-medium reserves native path-table space for
/// LoRa-learned routes so sustained public-TCP announce churn cannot evict
/// them.
pub const SHARED_MEDIUM_INTERFACES: u64 = 1 << 1;

/// Reclaimed internal SRAM assigned to a BLE-capable global allocator.
///
/// Channels, packet buffers, permit stores, task pools, and IRQ/DMA-visible
/// state remain in internal static RAM. The permanent transport-neutral
/// supervisor and its RNS node are explicitly placed in PSRAM after
/// construction. This region is registered before PSRAM, so other ordinary
/// global allocations use it first and spill into external RAM only when no
/// internal hole fits. An explicit placement policy is still required before
/// additional large protocol/client allocations are enabled. Seventy-two KiB
/// is the largest whole-KiB allocation that fits the ESP32-S3's separate
/// 73,744-byte reclaimed DRAM2 segment. The final 8 KiB is available to
/// esp-radio's 8,192-byte strict-internal controller-task stack and controller
/// allocations without shrinking the product executor stack in ordinary
/// DRAM. The pinned esp-radio documentation recommends more total heap (64 KiB
/// reclaimed plus 36 KiB ordinary), so this profile still requires powered
/// coexistence testing rather than treating 72 KiB as a general BLE guarantee.
#[cfg(feature = "appliance")]
pub const INTERNAL_HEAP_BYTES: usize = 72 * 1024;
/// Additional ordinary DRAM reserved when Wi-Fi and BLE coexist.
///
/// The upstream esp-radio coexistence example provisions 128 KiB of internal
/// heap. The product's reclaimed DRAM2 region is capped at 72 KiB, so this
/// second region brings the combined Wi-Fi/BLE allocator to 120 KiB while the
/// linked startup-stack audit still retains a large policy margin. Wi-Fi's
/// driver callbacks and dynamic RX buffers require strict internal memory;
/// PSRAM cannot substitute for this region.
#[cfg(feature = "gateway")]
pub const WIFI_INTERNAL_HEAP_BYTES: usize = 48 * 1024;
/// Ordinary internal-heap supplement is absent outside the station profile.
#[cfg(not(feature = "gateway"))]
pub const WIFI_INTERNAL_HEAP_BYTES: usize = 0;
/// Reclaimed internal SRAM assigned to the ordinary and Wi-Fi allocators.
///
/// These profiles retain their measured 64 KiB reservation. BLE alone claims
/// the otherwise-unused final 8 KiB of the separate reclaimed DRAM2 segment.
#[cfg(not(feature = "appliance"))]
pub const INTERNAL_HEAP_BYTES: usize = 64 * 1024;
/// Minimum external RAM required by this product profile.
pub const MINIMUM_PSRAM_BYTES: usize = 8 * 1024 * 1024;
/// Largest E290 datasheet PSRAM claim accepted by this image.
pub const MAXIMUM_PSRAM_BYTES: usize = 16 * 1024 * 1024;

/// Conservative initial SX1262 SPI clock.
pub const SPI_FREQUENCY_HZ: u32 = 1_000_000;
/// Per-edge upper bound for an asserted SX1262 BUSY signal.
pub const BUSY_PIN_WATCHDOG_MS: u64 = 100;

/// Maximum idle continuous-RX wait before the LoRa actor checks queued TX.
///
/// The 253.952 ms yield matches 248 SF7/BW125 symbols without using SX1262
/// single-shot receive mode: `248 * 1.024 ms = 253.952 ms`.
/// The modem remains continuously armed across this software-only yield.
pub const RX_SCHEDULER_YIELD_US: u64 = 253_952;

/// Driver/executor allowance after receive progress before a false-preamble rearm.
pub const RX_PROGRESS_TIMEOUT_MARGIN_US: u64 = 100_000;
/// Bounded dispatcher/driver setup before first predicted RF.
pub const TX_PRE_FIRST_RF_SETUP_US: u64 = 50_000;
/// Bounded non-RF gap between split packet frames.
pub const TX_INTER_FRAME_TURNAROUND_US: u64 = 25_000;
/// IRQ, SPI cleanup and scheduler latency allowance inside the watchdog.
pub const TX_DRIVER_AND_SCHEDULER_MARGIN_US: u64 = 500_000;
/// Additional outer-watchdog headroom beyond the named TX operation bounds.
pub const TX_OPERATION_WATCHDOG_HEADROOM_US: u64 = 100_000;
/// SX1262 CAD symbol count pinned by the current `lora-phy` driver.
pub const CAD_SYMBOLS: u64 = 8;
/// SPI, IRQ cleanup and executor allowance after CAD symbol time.
pub const CAD_DRIVER_AND_SCHEDULER_MARGIN_US: u64 = 250_000;
/// Compatibility floor for the proven SF7/BW125 whole-CAD watchdog.
pub const CAD_OPERATION_WATCHDOG_MINIMUM_US: u64 = 500_000;
/// Compatibility floor for the proven SF7/BW125 whole-packet TX watchdog.
pub const TX_OPERATION_WATCHDOG_MINIMUM_US: u64 = 1_500_000;

/// Complete profile-derived timing values consumed by the permanent LoRa task.
///
/// Construction is tied to the same board configuration used to initialize the
/// dispatcher. This prevents a selectable slow profile from inheriting SF7
/// receive, CAD, fragmentation or transmit deadlines.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTaskTiming {
    fragment_timeout_us: NonZeroU64,
    receive_operation_watchdog_us: NonZeroU64,
    rx_scheduler_yield_us: u64,
    rx_progress_timeout_us: u64,
    cad_operation_watchdog_us: u64,
    tx_operation_watchdog_us: u64,
    maximum_logical_packet_airtime_us: u64,
    maximum_tx_operation_required_us: u64,
}

impl RadioTaskTiming {
    /// Derive all LoRa actor timing from one board-validated configuration.
    pub const fn for_configuration(configuration: E290RadioConfiguration) -> Self {
        let profile = configuration.profile();
        let fragment_timeout_us = nonzero_timing(profile.fragment_timeout_us());
        let rx_progress_timeout_us = profile
            .maximum_frame_time_on_air_us()
            .saturating_add(RX_PROGRESS_TIMEOUT_MARGIN_US);
        let cad_required_us = profile
            .symbol_time_us_ceil()
            .saturating_mul(CAD_SYMBOLS)
            .saturating_add(CAD_DRIVER_AND_SCHEDULER_MARGIN_US);
        let cad_operation_watchdog_us = if cad_required_us > CAD_OPERATION_WATCHDOG_MINIMUM_US {
            cad_required_us
        } else {
            CAD_OPERATION_WATCHDOG_MINIMUM_US
        };
        let maximum_logical_packet_airtime_us = match profile.rnode_packet_airtime(500) {
            Ok(airtime) => airtime.aggregate_time_on_air_us(),
            Err(_) => u64::MAX,
        };
        let maximum_tx_operation_required_us = maximum_logical_packet_airtime_us
            .saturating_add(TX_PRE_FIRST_RF_SETUP_US)
            .saturating_add(TX_INTER_FRAME_TURNAROUND_US)
            .saturating_add(TX_DRIVER_AND_SCHEDULER_MARGIN_US);
        let profile_tx_watchdog_us =
            maximum_tx_operation_required_us.saturating_add(TX_OPERATION_WATCHDOG_HEADROOM_US);
        let tx_operation_watchdog_us = if profile_tx_watchdog_us > TX_OPERATION_WATCHDOG_MINIMUM_US
        {
            profile_tx_watchdog_us
        } else {
            TX_OPERATION_WATCHDOG_MINIMUM_US
        };

        Self {
            fragment_timeout_us,
            receive_operation_watchdog_us: configuration.maximum_receive_operation_us(),
            rx_scheduler_yield_us: RX_SCHEDULER_YIELD_US,
            rx_progress_timeout_us,
            cad_operation_watchdog_us,
            tx_operation_watchdog_us,
            maximum_logical_packet_airtime_us,
            maximum_tx_operation_required_us,
        }
    }

    /// Deadline for a matching second physical RNode frame.
    pub const fn fragment_timeout_us(self) -> NonZeroU64 {
        self.fragment_timeout_us
    }

    /// Destructive whole-receive-operation watchdog.
    pub const fn receive_operation_watchdog_us(self) -> NonZeroU64 {
        self.receive_operation_watchdog_us
    }

    /// Software-only continuous-RX fairness yield.
    pub const fn rx_scheduler_yield_us(self) -> u64 {
        self.rx_scheduler_yield_us
    }

    /// Recoverable deadline after receive progress is first observed.
    pub const fn rx_progress_timeout_us(self) -> u64 {
        self.rx_progress_timeout_us
    }

    /// Destructive whole-CAD-operation watchdog.
    pub const fn cad_operation_watchdog_us(self) -> u64 {
        self.cad_operation_watchdog_us
    }

    /// Destructive whole-logical-packet TX watchdog.
    pub const fn tx_operation_watchdog_us(self) -> u64 {
        self.tx_operation_watchdog_us
    }

    /// Exact two-frame airtime ceiling for one 500-byte logical packet.
    pub const fn maximum_logical_packet_airtime_us(self) -> u64 {
        self.maximum_logical_packet_airtime_us
    }

    /// Named airtime, setup, turnaround, driver and scheduler TX coverage.
    pub const fn maximum_tx_operation_required_us(self) -> u64 {
        self.maximum_tx_operation_required_us
    }
}

const fn nonzero_timing(value: u64) -> NonZeroU64 {
    match NonZeroU64::new(value) {
        Some(value) => value,
        None => panic!("validated radio timing must be non-zero"),
    }
}

/// Quiescent poll delay used when no node lane makes progress.
pub const NODE_POLL_INTERVAL_MS: u64 = 1;
/// Fair synchronous lane passes before the node task yields.
pub const NODE_MAX_IMMEDIATE_PASSES: usize = 16;
/// Fair node-task lanes: ingress, supervisor, maintenance, announce,
/// authenticated local API, outbound Nomad, volatile proof probe, and storage.
pub const NODE_FAIR_LANES: u8 = 8;

/// Apply one already-armed reservation for an admitted Nomad fetch.
///
/// An exact externally owned Nomad packet already represents that opportunity,
/// while fail-closed drain forbids every fresh command. The scheduler arms this
/// only after a Nomad lane was blocked by another owner and releases it after
/// the next empty-owner attempt, successful or not.
pub const fn reserve_fresh_nomad_turn(
    reservation_armed: bool,
    exact_nomad_packet_owned: bool,
    fail_closed_draining: bool,
) -> bool {
    reservation_armed && !exact_nomad_packet_owned && !fail_closed_draining
}

/// Decide whether queued native application evidence must outrank Nomad timeout cleanup.
///
/// An event already projected into the product-owned FIFO remains
/// authoritative even after upstream intake has failed closed. Events retained
/// farther upstream count only while that drain path can still make progress.
pub const fn application_events_preempt_nomad_timeout(
    ready_product_event: bool,
    upstream_drain_healthy: bool,
    upstream_event_pending: bool,
) -> bool {
    ready_product_event || (upstream_drain_healthy && upstream_event_pending)
}

/// Arm one future protected Nomad turn only when this lane was owner-blocked.
///
/// An empty-owner lane consumes the grant regardless of whether the subsequent
/// native preparation succeeds, preventing persistent native capacity pressure
/// from suppressing every other fresh producer.
pub const fn next_fresh_nomad_turn_armed(
    fresh_command_pending: bool,
    exact_nomad_packet_owned: bool,
    fail_closed_draining: bool,
    ordinary_owners_quiescent: bool,
) -> bool {
    fresh_command_pending
        && !exact_nomad_packet_owned
        && !fail_closed_draining
        && !ordinary_owners_quiescent
}

/// Whether a durable inbound proof still needs ordinary-coordinator admission.
///
/// A proof in the dedicated holder has already left the delayed queue but is
/// not actor-owned. A ready delayed proof has completed durable application
/// ownership but has not yet reached that holder. Either state blocks fresh
/// ordinary producers; an already actor-owned packet continues normally until
/// the coordinator can accept the proof unchanged.
pub const fn lxmf_proof_admission_pending(
    proof_holder_occupied: bool,
    ready_delayed_proofs: usize,
) -> bool {
    proof_holder_occupied || ready_delayed_proofs != 0
}

/// Whether a proof may displace one unadmitted ordinary envelope.
///
/// The caller needs one exact retry slot for the displaced action owner. An
/// externally retained protocol-dispatch owner (Path, Nomad, or probe work)
/// also forbids displacement because its scalar correlation cannot be moved
/// independently from the envelope.
pub const fn lxmf_proof_displacement_allowed(
    retry_slot_available: bool,
    protocol_dispatch_pending: bool,
) -> bool {
    retry_slot_available && !protocol_dispatch_pending
}

/// Whether receiver-side RF proof tracing applies to this ingress interface.
///
/// TCP proofs still use the same durable and priority admission path, but they
/// never arm a LoRa physical-TX correlation that only the radio dispatcher can
/// complete.
pub const fn inbound_proof_uses_lora_trace(
    proof_interface: u8,
    lora_interface: PacketInterfaceId,
) -> bool {
    proof_interface == lora_interface.get()
}

/// Reviewed upper bound for the PSRAM-resident outbound Nomad owner.
pub const MAXIMUM_NOMAD_RUNTIME_BYTES: usize = 1_024;
/// Fixed-RAM ceiling for the static authenticated request/reply channels.
pub const MAXIMUM_AUTHENTICATED_API_HANDOFF_BYTES: usize = 2_048;
/// Fixed-RAM ceiling for node-retained authenticated request/reply/quarantine state.
pub const MAXIMUM_AUTHENTICATED_API_NODE_STATE_BYTES: usize = 1_024;
/// Native RNS maintenance cadence.
pub const PROTOCOL_TICK_INTERVAL_SECONDS: u64 = 1;
/// Number of short post-boot announce retries before the steady cadence.
pub const ANNOUNCE_BOOTSTRAP_RETRIES: u8 = 2;
/// Pinned Rete delay before the one native retransmission of an announce.
pub const ANNOUNCE_NATIVE_RETRANSMIT_SECONDS: u64 =
    reticulum_node_core::RNS_ANNOUNCE_RETRANSMIT_SECONDS;
/// Minimum nominal separation between scheduled or native announce emissions.
pub const ANNOUNCE_MINIMUM_EMISSION_SEPARATION_SECONDS: u64 = 3;
/// Largest current local bootstrap announce packet in complete RNS bytes.
///
/// This is the 167-byte fixed signed announce plus the 37-byte named LXMF
/// delivery announce app data. The ten-byte UTF-8 Nomad application payload and
/// the primary destination are smaller. Timing code feeds this exact maximum
/// through the selected [`LoRaProfile`] rather than classifying profiles by
/// spreading factor.
pub const ANNOUNCE_BOOTSTRAP_MAXIMUM_PACKET_BYTES: usize =
    167 + MAX_LXMF_DELIVERY_ANNOUNCE_APP_DATA_BYTES;
/// Identity phase buckets before a node's first boot announce cycle.
pub const ANNOUNCE_BOOTSTRAP_INITIAL_PHASE_SLOTS: u64 = 8;
/// Primary-destination-derived phase buckets for the first bootstrap retry.
///
/// The prime bucket count makes the full little-endian identity seed
/// participate in the modulus and spreads peers across retry opportunities.
pub const ANNOUNCE_BOOTSTRAP_PHASE_SLOTS: u64 = 43;
/// Bounded delay before retrying the same destination after protocol rejection.
pub const ANNOUNCE_ADMISSION_RETRY_SECONDS: u64 = 1;
/// Periodic local transport announce cadence after bootstrap discovery.
pub const ANNOUNCE_INTERVAL_SECONDS: u64 = 30 * 60;
const _: () = assert!(ANNOUNCE_BOOTSTRAP_RETRIES > 0);
const _: () =
    assert!(ANNOUNCE_BOOTSTRAP_MAXIMUM_PACKET_BYTES <= reticulum_node_core::PACKET_CAPACITY);
const _: () = assert!(ANNOUNCE_BOOTSTRAP_INITIAL_PHASE_SLOTS > 0);
const _: () = assert!(ANNOUNCE_BOOTSTRAP_PHASE_SLOTS > 0);
const _: () = assert!(ANNOUNCE_ADMISSION_RETRY_SECONDS > 0);
/// Deadline assigned to one admitted ordinary-action envelope.
pub const ORDINARY_OWNER_LEASE_MS: u64 = 30_000;
/// Deadline assigned to one durable DATA packet-owner attempt.
pub const SUBMISSION_OWNER_LEASE_MS: u64 = 30_000;
/// Delay before retrying an ambiguous or temporarily busy journal operation.
pub const STORAGE_RETRY_BACKOFF_MS: u64 = 1_000;
/// First periodic retry for an LXMF event awaiting an admission dependency.
pub const LXMF_ADMISSION_RETRY_INITIAL_MS: u64 = 5_000;
/// Hard upper bound for one admission-deferred retry interval, including jitter.
pub const LXMF_ADMISSION_RETRY_MAX_MS: u64 = 5 * 60 * 1_000;
const _: () = assert!(LXMF_ADMISSION_RETRY_INITIAL_MS > STORAGE_RETRY_BACKOFF_MS);
const _: () = assert!(LXMF_ADMISSION_RETRY_MAX_MS >= LXMF_ADMISSION_RETRY_INITIAL_MS);
const _: () = assert!(
    LXMF_ADMISSION_RETRY_MAX_MS - LXMF_ADMISSION_RETRY_MAX_MS / 5
        >= LXMF_ADMISSION_RETRY_INITIAL_MS
);
/// Product-selected maximum normalized opportunistic LXMF wire length.
pub const LXMF_MAX_WIRE_BYTES: usize = 4_096;
/// Product-selected maximum bytes in one LXMF MessagePack scalar value.
pub const LXMF_MAX_VALUE_BYTES: usize = 2_048;
/// Product-selected maximum entries in one LXMF MessagePack container.
pub const LXMF_MAX_CONTAINER_ITEMS: usize = 256;
/// Product-selected maximum values visited by LXMF validation.
pub const LXMF_MAX_TOTAL_VALUES: usize = 2_048;
/// Product-selected maximum scanner work for one LXMF validation.
pub const LXMF_MAX_SCAN_STEPS: usize = 65_536;
/// Product-selected maximum LXMF MessagePack nesting depth.
pub const LXMF_MAX_NESTING_DEPTH: usize = 16;
/// Repeated debounced button-observation cadence supplied to pairing policy.
pub const PAIRING_BUTTON_OBSERVATION_INTERVAL_MS: u64 = 20;

/// Product action for an envelope surfaced by the coordinator's explicit
/// rejected-actions output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RejectedActionDisposition {
    /// Preserve the exact actions but replace their elapsed owner deadline.
    RefreshDeadline,
    /// A semantic packet/profile error is terminal for this owner.
    FailStop,
}

/// Whether an ordinary input-envelope transition can own the pending
/// path-discovery protocol correlation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PendingPathProtocolDisposition {
    /// The path packet has not staged yet, so the current input envelope owns
    /// its still-undispatched protocol correlation.
    TransferAwaitingStage,
    /// The exact path packet has staged and remains independently in flight;
    /// a later input-envelope transition belongs to other ordinary work.
    RetainInFlight,
}

/// Preserve a staged exact path owner across rejected or terminal unrelated
/// ordinary input-envelope transitions.
pub fn pending_path_protocol_disposition<T>(
    staged_token: Option<T>,
) -> PendingPathProtocolDisposition {
    if staged_token.is_some() {
        PendingPathProtocolDisposition::RetainInFlight
    } else {
        PendingPathProtocolDisposition::TransferAwaitingStage
    }
}

/// Product handling for a terminal ingress action or correlation result
/// observed in the same step as retryable exact RX-buffer recycle pressure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalIngressDisposition {
    /// Keep stepping until the aggregate returns the exact sealed RX buffer.
    DeferUntilBufferRecycled,
    /// Handle the terminal condition now that no RX buffer remains retained.
    HandleTerminal,
}

/// Return a retryably pressured exact RX buffer before handling the terminal
/// ingress result. Non-retryable return failures are separately quarantined.
pub const fn terminal_ingress_disposition(
    retryable_recycle_pending: bool,
) -> TerminalIngressDisposition {
    if retryable_recycle_pending {
        TerminalIngressDisposition::DeferUntilBufferRecycled
    } else {
        TerminalIngressDisposition::HandleTerminal
    }
}

/// Product handling for an unchanged local protocol-action offer failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryOfferDisposition {
    /// The coordinator is temporarily occupied; retain and retry unchanged.
    RetryBusy,
    /// Retain separately as terminal quarantine and stop accepting fresh work.
    QuarantineAndDrain,
}

/// Only coordinator `Busy` is a retryable action-offer outcome.
pub const fn ordinary_offer_disposition(
    reason: NodeInterfaceOrdinaryOfferError,
) -> OrdinaryOfferDisposition {
    match reason {
        NodeInterfaceOrdinaryOfferError::Coordinator(OrdinaryRouterOfferError::Busy(_)) => {
            OrdinaryOfferDisposition::RetryBusy
        }
        NodeInterfaceOrdinaryOfferError::Fault(_)
        | NodeInterfaceOrdinaryOfferError::Coordinator(
            OrdinaryRouterOfferError::Disabled(_)
            | OrdinaryRouterOfferError::EnvelopeExceedsPool { .. },
        ) => OrdinaryOfferDisposition::QuarantineAndDrain,
    }
}

/// One independently retained ordinary-action retry owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionRetrySlot {
    /// Neither retry owner is populated, so protocol maintenance may run.
    None,
    /// First fungible retained-action slot.
    First,
    /// Second fungible retained-action slot.
    Second,
}

/// Select at most one retry owner without allowing either populated slot to
/// hide the other.
///
/// The caller flips the preference after each selected populated slot. This
/// makes persistent coordinator pressure alternate fairly while preserving
/// each unchanged action owner in its own slot.
pub const fn action_retry_slot(first: bool, second: bool, prefer_second: bool) -> ActionRetrySlot {
    match (first, second, prefer_second) {
        (false, false, _) => ActionRetrySlot::None,
        (true, false, _) => ActionRetrySlot::First,
        (false, true, _) => ActionRetrySlot::Second,
        (true, true, true) => ActionRetrySlot::Second,
        (true, true, false) => ActionRetrySlot::First,
    }
}

/// Product meaning of one portable aggregate transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SupervisorTransitionDisposition {
    /// No useful ownership work occurred.
    Idle,
    /// Normal exact-owner progress occurred.
    Progress,
    /// A local machine entered fail-closed mode; keep stepping to drain owners.
    TerminalFailClosedDrain,
}

/// Product handling for one token-bearing router-dispatch confirmation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtocolDispatchConfirmationDisposition {
    /// RNS accepted this interface and timing interval.
    Confirmed,
    /// RNS had already accepted an earlier serialized fan-out interface.
    EarlierFanoutConfirmed,
    /// The first router acceptance could not confirm its required RNS edge.
    TerminalFailClosedDrain,
}

/// Relationship between one observed ordinary packet generation and the exact
/// packet generation retained by a product protocol operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExactPacketCorrelation {
    /// No exact generation has been observed yet; capture this first token.
    Capture,
    /// The observation belongs to the retained exact packet generation.
    Exact,
    /// The observation belongs to independent ordinary work in another slot or
    /// reuse generation.
    Unrelated,
}

/// Correlate an ordinary-router observation without treating legal concurrent
/// packet generations as corruption of the retained protocol operation.
pub fn exact_packet_correlation<T: Eq>(expected: Option<T>, observed: T) -> ExactPacketCorrelation {
    match expected {
        None => ExactPacketCorrelation::Capture,
        Some(expected) if expected == observed => ExactPacketCorrelation::Exact,
        Some(_) => ExactPacketCorrelation::Unrelated,
    }
}

/// Relationship between a routed or completed ordinary packet and an exact
/// protocol packet whose staging token may not have been observed yet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetainedPacketCorrelation {
    /// The observation belongs to the retained exact packet generation.
    Exact,
    /// The observation predates exact-token capture or belongs to another live
    /// ordinary packet generation.
    Unrelated,
}

/// Correlate a routed or completed packet with an already captured exact token.
///
/// Before the exact packet's `PacketStaged` transition, every routed or
/// completed generation is necessarily unrelated: the exact packet cannot
/// reach either transition before the coordinator reports staging it.
pub fn retained_packet_correlation<T: Eq>(
    expected: Option<T>,
    observed: T,
) -> RetainedPacketCorrelation {
    if expected.is_some_and(|expected| expected == observed) {
        RetainedPacketCorrelation::Exact
    } else {
        RetainedPacketCorrelation::Unrelated
    }
}

/// Distinguish expected fan-out idempotence from a rejected first dispatch.
pub const fn protocol_dispatch_confirmation_disposition(
    first_dispatch: bool,
    confirmed: bool,
) -> ProtocolDispatchConfirmationDisposition {
    match (first_dispatch, confirmed) {
        (_, true) => ProtocolDispatchConfirmationDisposition::Confirmed,
        (false, false) => ProtocolDispatchConfirmationDisposition::EarlierFanoutConfirmed,
        (true, false) => ProtocolDispatchConfirmationDisposition::TerminalFailClosedDrain,
    }
}

/// Classify terminal local faults without stopping forced denial/completion
/// drainage for owners already in motion.
pub const fn supervisor_transition_disposition(
    transition: NodeInterfaceSupervisorTransition,
) -> SupervisorTransitionDisposition {
    match transition {
        NodeInterfaceSupervisorTransition::Idle => SupervisorTransitionDisposition::Idle,
        NodeInterfaceSupervisorTransition::Ordinary(
            OrdinaryRouterStep::RouteBackpressured { .. }
            | OrdinaryRouterStep::WaitingForInterfaces
            | OrdinaryRouterStep::AdmissionBackpressured { .. }
            | OrdinaryRouterStep::OutputBackpressured
            | OrdinaryRouterStep::Idle,
        ) => SupervisorTransitionDisposition::Idle,
        NodeInterfaceSupervisorTransition::Fault(_)
        | NodeInterfaceSupervisorTransition::Data(
            DataRouterStep::OwnerMismatch | DataRouterStep::Disabled(_),
        )
        | NodeInterfaceSupervisorTransition::Ordinary(OrdinaryRouterStep::Disabled(_))
        | NodeInterfaceSupervisorTransition::DataPermit {
            step: DataPermitServerStep::Disabled(_) | DataPermitServerStep::InternalInvariant,
            ..
        }
        | NodeInterfaceSupervisorTransition::OrdinaryPermit {
            step:
                OrdinaryPermitServerStep::Disabled(_) | OrdinaryPermitServerStep::InternalInvariant,
            ..
        } => SupervisorTransitionDisposition::TerminalFailClosedDrain,
        _ => SupervisorTransitionDisposition::Progress,
    }
}

/// Classify the only recoverable output reasons emitted by the coordinator.
pub const fn rejected_action_disposition(
    reason: OrdinaryActionAdmissionError,
) -> RejectedActionDisposition {
    match reason {
        OrdinaryActionAdmissionError::DeadlineExpired { .. } => {
            RejectedActionDisposition::RefreshDeadline
        }
        _ => RejectedActionDisposition::FailStop,
    }
}

/// Calculate the nominal coded LoRa bitrate for one validated profile.
///
/// The integer result is `BW * SF * 4 / CR / 2^SF`, rounded down to match the
/// previous SF7/BW125 value. The one-bit floor preserves the registry's
/// nonzero contract for any future extremely slow validated profile.
pub const fn lora_advertised_bitrate(profile: LoRaProfile) -> AdvertisedBitrate {
    let spreading_factor = profile.spreading_factor().factor();
    let symbols_per_chirp = match 1_u64.checked_shl(spreading_factor) {
        Some(symbols) => symbols,
        None => u64::MAX,
    };
    let denominator = symbols_per_chirp.saturating_mul(profile.coding_rate().denom() as u64);
    let numerator = (profile.bandwidth().hz() as u64)
        .saturating_mul(spreading_factor as u64)
        .saturating_mul(4);
    let calculated = match numerator.checked_div(denominator) {
        Some(calculated) => calculated,
        None => 0,
    };
    let nonzero = if calculated == 0 { 1 } else { calculated };
    let bounded = if nonzero > u32::MAX as u64 {
        u32::MAX
    } else {
        nonzero as u32
    };
    match AdvertisedBitrate::try_new(bounded) {
        Ok(bitrate) => bitrate,
        Err(_) => panic!("the profile-derived LoRa bitrate must be non-zero"),
    }
}

/// Construct the immutable registry properties for LoRa slot zero.
pub const fn interface_properties(profile: LoRaProfile) -> InterfaceProperties {
    InterfaceProperties::new(
        match LogicalMtu::try_new(500) {
            Ok(mtu) => mtu,
            Err(_) => panic!("the base Reticulum MTU must be non-zero"),
        },
        LORA_INTERFACE_CONFIG_ID,
        Some(lora_advertised_bitrate(profile)),
        LORA_INTERFACE_COST,
    )
    .with_announce_mode(AnnouncePropagationMode::Internal)
    .with_recursive_path_search_mode(RecursivePathSearchMode::Unrestricted)
}

/// Construct immutable registry properties for outbound TCP slot one.
#[cfg(feature = "gateway")]
pub const fn tcp_interface_properties() -> InterfaceProperties {
    InterfaceProperties::new(
        match LogicalMtu::try_new(500) {
            Ok(mtu) => mtu,
            Err(_) => panic!("the base Reticulum MTU must be non-zero"),
        },
        TCP_INTERFACE_CONFIG_ID,
        None,
        TCP_INTERFACE_COST,
    )
    .with_topology(InterfaceTopology::PointToPoint)
    .with_announce_mode(AnnouncePropagationMode::Boundary)
    .with_recursive_path_search_mode(RecursivePathSearchMode::Boundary)
}

/// Product diagnostic completion codes for the DATA coordinator.
pub const fn data_router_config() -> DataRouterConfig {
    DataRouterConfig::new(
        TxCompletionCode::new(0xe201),
        TxCompletionCode::new(0xe202),
        TxCompletionCode::new(0xe203),
    )
}

/// Product diagnostic completion codes for the ordinary coordinator.
pub const fn ordinary_router_config() -> OrdinaryRouterConfig {
    OrdinaryRouterConfig::new(
        TxCompletionCode::new(0xe211),
        TxCompletionCode::new(0xe212),
        TxCompletionCode::new(0xe213),
    )
}

/// Construct a fresh ordinary envelope admission deadline.
pub const fn ordinary_admission(now_ms: u64) -> OrdinaryRouterAdmission {
    OrdinaryRouterAdmission::new(TxLeaseDeadline::new(MonotonicMillis::new(
        now_ms.saturating_add(ORDINARY_OWNER_LEASE_MS),
    )))
}

/// Decide whether the node task may advance one durable submission step.
///
/// Fresh submission scheduling waits for every ordinary owner to become
/// quiescent. An already transmitted DATA frame is different: the sole radio
/// dispatcher retains its completion until the frame observation is durable,
/// so ordinary work queued behind that frame cannot become quiescent first.
/// That exact active owner therefore bypasses scheduler quiescence, including
/// a later unrelated fail-closed drain; storage retry timing remains
/// authoritative. Once its observation becomes durable, the owner is
/// acknowledged and the ordinary gates become authoritative again.
pub const fn submission_storage_step_admitted(
    storage_step_attempted: bool,
    storage_step_due: bool,
    retained_frame_pending: bool,
    ordinary_owners_quiescent: bool,
    fail_closed_draining: bool,
    ordinary_control_step_pending: bool,
) -> bool {
    !storage_step_attempted
        && storage_step_due
        && (retained_frame_pending || (ordinary_owners_quiescent && !fail_closed_draining))
        && (!ordinary_control_step_pending || (ordinary_owners_quiescent && !fail_closed_draining))
}

/// Validated profile-aware RNode-compatible randomized backoff and CAD policy.
///
/// The continuous random interval preserves the reference 24 ms minimum slot
/// and complete 15-slot contention envelope before initial CAD. After a busy
/// observation, one maximum physical-frame interval is added before that same
/// randomized envelope. A contender therefore cannot consume all of its CAD
/// retries while the frame that made the channel busy is still on air. Busy
/// exhaustion still rejects without a permit; it never forces transmission.
pub const fn logical_packet_access(
    configuration: E290RadioConfiguration,
) -> LogicalPacketAccessConfig {
    match LogicalPacketAccessConfig::try_new_with_busy_retry_holdoff(
        3,       // initial CAD plus two bounded busy retries
        24_000,  // one reference RNode contention slot
        360_000, // full 15-slot contention envelope, including the first slot
        configuration.profile().maximum_frame_time_on_air_us(),
        250_000, // clear CAD must remain fresh through permit and setup
        TX_PRE_FIRST_RF_SETUP_US,
        TX_INTER_FRAME_TURNAROUND_US,
        100_000, // owner-reconciliation guard after predicted RF completion
    ) {
        Ok(config) => config,
        Err(_) => panic!("the E290 logical packet access policy must be valid"),
    }
}

/// Complete immutable sole-radio dispatcher policy.
pub const fn dispatcher_config(configuration: E290RadioConfiguration) -> RadioTxDispatcherConfig {
    RadioTxDispatcherConfig::new(
        LORA_INTERFACE_CONFIG_ID,
        logical_packet_access(configuration),
        1_000_000,
        0,
        RadioTxCompletionCodes::new(
            TxCompletionCode::new(0xe220),
            TxCompletionCode::new(0xe221),
            TxCompletionCode::new(0xe222),
            TxCompletionCode::new(0xe223),
            TxCompletionCode::new(0xe224),
            TxCompletionCode::new(0xe225),
            TxCompletionCode::new(0xe226),
            TxCompletionCode::new(0xe227),
            TxCompletionCode::new(0xe228),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn psram_product_retains_border_mesh_scale_routing_state() {
        assert_eq!(PATHS, 256);
        assert_eq!(DEDUPLICATION, 512);
        assert!(DEDUPLICATION >= PATHS * 2);
    }

    #[test]
    #[cfg(feature = "appliance")]
    fn internal_heap_uses_the_bounded_esp32s3_reclaimed_dram2_capacity() {
        assert_eq!(INTERNAL_HEAP_BYTES, 72 * 1024);
        assert!(INTERNAL_HEAP_BYTES <= 73_744);
        assert_eq!(73_744 - INTERNAL_HEAP_BYTES, 16);
    }

    #[test]
    #[cfg(feature = "gateway")]
    fn coexistence_profile_adds_ordinary_internal_heap_for_wifi_rx() {
        assert_eq!(WIFI_INTERNAL_HEAP_BYTES, 48 * 1024);
        assert_eq!(INTERNAL_HEAP_BYTES + WIFI_INTERNAL_HEAP_BYTES, 120 * 1024);
    }

    #[test]
    #[cfg(not(feature = "appliance"))]
    fn non_ble_internal_heap_preserves_the_measured_baseline() {
        assert_eq!(INTERNAL_HEAP_BYTES, 64 * 1024);
    }

    #[test]
    fn lxmf_delivery_announce_encodes_a_name_and_keeps_an_empty_functionality_list() {
        let mut short = [0_u8; MAX_LXMF_DELIVERY_ANNOUNCE_APP_DATA_BYTES];
        let len = encode_lxmf_delivery_announce_app_data("Field node", &mut short);
        assert_eq!(
            &short[..len],
            [
                0x93, 0xaa, b'F', b'i', b'e', b'l', b'd', b' ', b'n', b'o', b'd', b'e', 0xc0, 0x90
            ]
        );

        let name = "x".repeat(reticulum_device_api::MAX_DEVICE_NAME_BYTES);
        let mut maximum = [0_u8; MAX_LXMF_DELIVERY_ANNOUNCE_APP_DATA_BYTES];
        let len = encode_lxmf_delivery_announce_app_data(&name, &mut maximum);
        assert_eq!(len, MAX_LXMF_DELIVERY_ANNOUNCE_APP_DATA_BYTES);
        assert_eq!(maximum[0], 0x93);
        assert_eq!(maximum[1], 0xd9);
        assert_eq!(maximum[2] as usize, name.len());
        assert_eq!(&maximum[3..3 + name.len()], name.as_bytes());
        assert_eq!(maximum[len - 2], 0xc0);
        assert_eq!(maximum[len - 1], 0x90);
    }

    #[test]
    fn bootstrap_announce_bound_tracks_the_named_lxmf_announce() {
        assert_eq!(MAX_LXMF_DELIVERY_ANNOUNCE_APP_DATA_BYTES, 37);
        assert_eq!(ANNOUNCE_BOOTSTRAP_MAXIMUM_PACKET_BYTES, 204);
    }

    #[test]
    fn tcp_proof_skips_rf_trace_and_following_lora_proof_remains_traceable() {
        let lora = PacketInterfaceId::new(1);
        assert!(!inbound_proof_uses_lora_trace(2, lora));
        assert!(inbound_proof_uses_lora_trace(1, lora));
        // RF diagnostics are not an input to durable proof admission. A TCP
        // proof therefore neither arms LoRa correlation nor blocks the next
        // LoRa proof from the priority lane.
        assert!(lxmf_proof_displacement_allowed(true, false));
    }

    #[test]
    #[cfg(feature = "gateway")]
    fn border_interfaces_distinguish_shared_lora_from_point_to_point_tcp() {
        assert_eq!(
            interface_properties(E290_NA915_DEFAULT_PROFILE).topology(),
            InterfaceTopology::SharedMedium
        );
        assert_eq!(
            tcp_interface_properties().topology(),
            InterfaceTopology::PointToPoint
        );
        assert_eq!(
            interface_properties(E290_NA915_DEFAULT_PROFILE).announce_mode(),
            AnnouncePropagationMode::Internal
        );
        assert_eq!(
            tcp_interface_properties().announce_mode(),
            AnnouncePropagationMode::Boundary
        );
        assert_eq!(
            interface_properties(E290_NA915_DEFAULT_PROFILE).recursive_path_search_mode(),
            RecursivePathSearchMode::Unrestricted
        );
        assert_eq!(
            tcp_interface_properties().recursive_path_search_mode(),
            RecursivePathSearchMode::Boundary
        );
    }

    #[test]
    fn advertised_lora_bitrate_tracks_the_selected_modulation() {
        use reticulum_board_e290_radio::{E290Na915TxPower, E290RadioConfiguration};

        assert_eq!(LORA_ADVERTISED_BITRATE.get(), 5_468);
        assert_eq!(
            interface_properties(E290_NA915_DEFAULT_PROFILE)
                .advertised_bitrate()
                .map(AdvertisedBitrate::get),
            Some(5_468)
        );

        let slow = E290RadioConfiguration::try_from_profile(
            915_000_000,
            125_000,
            12,
            8,
            E290Na915TxPower::Dbm14,
        )
        .unwrap();
        assert_eq!(lora_advertised_bitrate(slow.profile()).get(), 183);
        assert_eq!(
            interface_properties(slow.profile())
                .advertised_bitrate()
                .map(AdvertisedBitrate::get),
            Some(183)
        );
    }

    #[test]
    fn appliance_profile_owns_128_bounded_submissions_in_psram() {
        assert_eq!(DURABLE_SUBMISSIONS, 128);
        assert_eq!(DURABLE_PROJECTED_SUBMISSIONS, 128);
        assert_eq!(DURABLE_ACCEPTED_SUBMISSION_LIMIT, 128);
        assert_eq!(DURABLE_RUNTIME_BYTES, 396_592);
        assert_eq!(MAXIMUM_DURABLE_RUNTIME_BYTES, 512 * 1024);
        const { assert!(DURABLE_ACCEPTED_SUBMISSION_LIMIT <= DURABLE_SUBMISSIONS) };
    }

    #[test]
    fn channel_access_preserves_the_reference_rnode_contention_envelope() {
        let access = logical_packet_access(E290_NA915_DEFAULT_CONFIGURATION);
        assert_eq!(access.maximum_cad_attempts(), 3);
        assert_eq!(access.minimum_backoff_us(), 24_000);
        assert_eq!(access.maximum_backoff_us(), 360_000);
        assert_eq!(
            access.busy_retry_holdoff_us(),
            E290_NA915_DEFAULT_PROFILE.maximum_frame_time_on_air_us()
        );
        assert!(
            access.minimum_busy_retry_interval_us()
                > E290_NA915_DEFAULT_PROFILE.maximum_frame_time_on_air_us()
        );
        assert_eq!(access.maximum_pre_first_rf_setup_us(), 50_000);
        assert_eq!(access.maximum_inter_frame_turnaround_us(), 25_000);
    }

    #[test]
    fn receive_progress_deadline_is_recoverable_and_covers_a_maximum_frame() {
        let default = RadioTaskTiming::for_configuration(E290_NA915_DEFAULT_CONFIGURATION);
        assert_eq!(RX_PROGRESS_TIMEOUT_MARGIN_US, 100_000);
        assert_eq!(
            default.rx_progress_timeout_us(),
            E290_NA915_DEFAULT_PROFILE
                .maximum_frame_time_on_air_us()
                .saturating_add(RX_PROGRESS_TIMEOUT_MARGIN_US)
        );
        assert!(
            default.rx_progress_timeout_us()
                < reticulum_board_e290_radio::E290_MAXIMUM_RECEIVE_OPERATION_US.get()
        );
    }

    #[test]
    fn tx_watchdog_covers_exact_maximum_airtime_and_named_margins() {
        let default = RadioTaskTiming::for_configuration(E290_NA915_DEFAULT_CONFIGURATION);
        assert_eq!(default.maximum_logical_packet_airtime_us(), 821_760);
        assert_eq!(default.maximum_tx_operation_required_us(), 1_396_760);
        assert!(default.tx_operation_watchdog_us() >= default.maximum_tx_operation_required_us());
    }

    #[test]
    fn slow_profile_scales_every_airtime_sensitive_actor_deadline() {
        use reticulum_board_e290_radio::{E290Na915TxPower, E290RadioConfiguration};

        let slow_configuration = E290RadioConfiguration::try_from_profile(
            915_000_000,
            125_000,
            12,
            8,
            E290Na915TxPower::Dbm22,
        )
        .unwrap();
        let slow = RadioTaskTiming::for_configuration(slow_configuration);
        let default = RadioTaskTiming::for_configuration(E290_NA915_DEFAULT_CONFIGURATION);
        assert!(slow.fragment_timeout_us() > default.fragment_timeout_us());
        assert!(slow.receive_operation_watchdog_us() > default.receive_operation_watchdog_us());
        assert!(slow.rx_progress_timeout_us() > default.rx_progress_timeout_us());
        assert!(slow.cad_operation_watchdog_us() > default.cad_operation_watchdog_us());
        assert!(slow.tx_operation_watchdog_us() > default.tx_operation_watchdog_us());
        assert!(
            slow.maximum_logical_packet_airtime_us() > default.maximum_logical_packet_airtime_us()
        );
    }

    #[test]
    fn every_board_admitted_modulation_receives_conservative_runtime_timing() {
        use reticulum_board_e290_radio::{E290Na915TxPower, E290RadioConfiguration};

        let mut admitted = 0_u32;
        for bandwidth_hz in [
            7_810, 10_420, 15_630, 20_830, 31_250, 41_670, 62_500, 125_000, 250_000, 500_000,
        ] {
            for spreading_factor in 7..=12 {
                for coding_rate_denominator in 5..=8 {
                    let Ok(configuration) = E290RadioConfiguration::try_from_profile(
                        915_000_000,
                        bandwidth_hz,
                        spreading_factor,
                        coding_rate_denominator,
                        E290Na915TxPower::Dbm14,
                    ) else {
                        continue;
                    };
                    admitted += 1;
                    let profile = configuration.profile();
                    let timing = RadioTaskTiming::for_configuration(configuration);
                    let access = logical_packet_access(configuration);

                    assert_eq!(
                        timing.fragment_timeout_us().get(),
                        profile.fragment_timeout_us()
                    );
                    assert!(
                        timing.receive_operation_watchdog_us().get()
                            >= profile
                                .symbol_time_us_ceil()
                                .saturating_mul(u64::from(configuration.rx_symbol_timeout()))
                                .saturating_add(profile.maximum_frame_time_on_air_us())
                                .saturating_add(
                                    reticulum_board_e290_radio::E290_RX_WATCHDOG_MINIMUM_MARGIN_US,
                                )
                    );
                    assert!(
                        timing.rx_progress_timeout_us() >= profile.maximum_frame_time_on_air_us()
                    );
                    assert!(
                        timing.cad_operation_watchdog_us()
                            >= profile
                                .symbol_time_us_ceil()
                                .saturating_mul(CAD_SYMBOLS)
                                .saturating_add(CAD_DRIVER_AND_SCHEDULER_MARGIN_US)
                    );
                    assert!(
                        timing.tx_operation_watchdog_us()
                            >= timing
                                .maximum_tx_operation_required_us()
                                .saturating_add(TX_OPERATION_WATCHDOG_HEADROOM_US)
                    );
                    assert_eq!(
                        access.busy_retry_holdoff_us(),
                        profile.maximum_frame_time_on_air_us()
                    );
                    assert!(
                        access.minimum_busy_retry_interval_us()
                            > profile.maximum_frame_time_on_air_us()
                    );
                }
            }
        }
        assert!(admitted > 0);
    }

    #[test]
    fn only_an_elapsed_owner_deadline_is_retried() {
        assert_eq!(
            rejected_action_disposition(OrdinaryActionAdmissionError::DeadlineExpired {
                now: MonotonicMillis::new(31),
                deadline: TxLeaseDeadline::new(MonotonicMillis::new(30)),
            }),
            RejectedActionDisposition::RefreshDeadline
        );
        assert_eq!(
            rejected_action_disposition(OrdinaryActionAdmissionError::PacketTooLarge {
                packet_index: 0,
                actual: 501,
                maximum: 500,
            }),
            RejectedActionDisposition::FailStop
        );
        assert_eq!(
            rejected_action_disposition(OrdinaryActionAdmissionError::InterfaceOutsideProfile {
                packet_index: 0,
                interface: reticulum_node_core::PacketInterfaceId::new(17),
            }),
            RejectedActionDisposition::FailStop
        );
    }

    #[test]
    fn terminal_ingress_actions_wait_for_exact_buffer_recycle() {
        assert_eq!(
            terminal_ingress_disposition(true),
            TerminalIngressDisposition::DeferUntilBufferRecycled
        );
        assert_eq!(
            terminal_ingress_disposition(false),
            TerminalIngressDisposition::HandleTerminal
        );
    }

    #[test]
    fn only_coordinator_busy_retries_a_local_action_offer() {
        assert_eq!(
            ordinary_offer_disposition(NodeInterfaceOrdinaryOfferError::Coordinator(
                OrdinaryRouterOfferError::Busy(
                    reticulum_tx_supervisor::OrdinaryRouterBusyReason::PendingActions,
                ),
            )),
            OrdinaryOfferDisposition::RetryBusy
        );
        assert_eq!(
            ordinary_offer_disposition(NodeInterfaceOrdinaryOfferError::Fault(
                reticulum_tx_supervisor::NodeInterfaceSupervisorFault::DataOwnerMismatch,
            )),
            OrdinaryOfferDisposition::QuarantineAndDrain
        );
        assert_eq!(
            ordinary_offer_disposition(NodeInterfaceOrdinaryOfferError::Coordinator(
                OrdinaryRouterOfferError::EnvelopeExceedsPool {
                    packet_count: 9,
                    limit: 8,
                },
            )),
            OrdinaryOfferDisposition::QuarantineAndDrain
        );
    }

    #[test]
    fn proof_displacement_requires_an_owner_slot_and_no_path_or_nomad_correlation() {
        assert!(lxmf_proof_displacement_allowed(true, false));
        assert!(
            !lxmf_proof_displacement_allowed(false, false),
            "without an exact retry slot the pending envelope stays coordinator-owned"
        );
        assert!(
            !lxmf_proof_displacement_allowed(true, true),
            "a pending Path or Nomad protocol owner must remain bound to its envelope"
        );
    }

    #[test]
    fn retry_slot_selection_preserves_independent_owners() {
        assert_eq!(action_retry_slot(false, false, true), ActionRetrySlot::None);
        assert_eq!(action_retry_slot(true, false, true), ActionRetrySlot::First);
        assert_eq!(
            action_retry_slot(true, false, false),
            ActionRetrySlot::First
        );
        assert_eq!(
            action_retry_slot(false, true, true),
            ActionRetrySlot::Second
        );
        assert_eq!(
            action_retry_slot(false, true, false),
            ActionRetrySlot::Second
        );
    }

    #[test]
    fn retry_slot_selection_honors_the_fairness_cursor_when_both_are_live() {
        assert_eq!(action_retry_slot(true, true, true), ActionRetrySlot::Second);
        assert_eq!(action_retry_slot(true, true, false), ActionRetrySlot::First);
    }

    #[test]
    fn armed_nomad_turn_reservation_is_exact_and_drain_safe() {
        assert!(reserve_fresh_nomad_turn(true, false, false));
        assert!(
            !reserve_fresh_nomad_turn(true, true, false),
            "an already-owned Nomad packet is the reserved opportunity"
        );
        assert!(
            !reserve_fresh_nomad_turn(true, false, true),
            "fail-closed drain never reserves fresh work"
        );
        assert!(!reserve_fresh_nomad_turn(false, false, false));
        assert!(next_fresh_nomad_turn_armed(true, false, false, false));
        assert!(
            !next_fresh_nomad_turn_armed(true, false, false, true),
            "one empty-owner attempt consumes the bounded grant even if preparation rejects"
        );
        assert!(!next_fresh_nomad_turn_armed(true, true, false, false));
        assert!(!next_fresh_nomad_turn_armed(true, false, true, false));
    }

    #[test]
    fn ready_product_event_remains_authoritative_after_upstream_fail_closed() {
        assert!(application_events_preempt_nomad_timeout(true, false, false));
    }

    #[test]
    fn upstream_event_preempts_timeout_only_while_its_drain_path_is_healthy() {
        assert!(application_events_preempt_nomad_timeout(false, true, true));
        assert!(!application_events_preempt_nomad_timeout(
            false, false, true
        ));
        assert!(!application_events_preempt_nomad_timeout(
            false, true, false
        ));
    }

    #[test]
    fn retained_data_frame_bypasses_ordinary_quiescence_for_durability() {
        assert!(submission_storage_step_admitted(
            false, true, true, false, false, false,
        ));
        assert!(
            !submission_storage_step_admitted(false, true, false, false, false, false),
            "fresh scheduling still waits for ordinary owners"
        );
        assert!(
            !submission_storage_step_admitted(false, false, true, false, false, false),
            "a retained frame does not bypass storage retry timing"
        );
        assert!(
            submission_storage_step_admitted(false, true, true, false, true, false),
            "a later unrelated fault cannot strand an active DATA completion"
        );
        assert!(
            !submission_storage_step_admitted(false, true, false, true, true, false),
            "fail-closed drain still forbids fresh submission scheduling"
        );
        assert!(
            !submission_storage_step_admitted(false, true, true, false, false, true),
            "a retained frame cannot bypass action-owner capacity for a ready control step"
        );
        assert!(submission_storage_step_admitted(
            false, true, false, true, false, true,
        ));
    }

    #[test]
    fn supervisor_terminal_faults_enter_fail_closed_drain() {
        assert_eq!(
            supervisor_transition_disposition(NodeInterfaceSupervisorTransition::Idle),
            SupervisorTransitionDisposition::Idle
        );
        assert_eq!(
            supervisor_transition_disposition(NodeInterfaceSupervisorTransition::DataPermit {
                actor: 0,
                step: DataPermitServerStep::Advanced,
            }),
            SupervisorTransitionDisposition::Progress
        );
        assert_eq!(
            supervisor_transition_disposition(NodeInterfaceSupervisorTransition::Ordinary(
                OrdinaryRouterStep::WaitingForInterfaces,
            )),
            SupervisorTransitionDisposition::Idle
        );
        assert_eq!(
            supervisor_transition_disposition(NodeInterfaceSupervisorTransition::Ordinary(
                OrdinaryRouterStep::AdmissionBackpressured {
                    needed: 2,
                    available: 1,
                },
            )),
            SupervisorTransitionDisposition::Idle
        );
        assert_eq!(
            supervisor_transition_disposition(NodeInterfaceSupervisorTransition::Ordinary(
                OrdinaryRouterStep::OutputBackpressured,
            )),
            SupervisorTransitionDisposition::Idle
        );
        assert_eq!(
            supervisor_transition_disposition(NodeInterfaceSupervisorTransition::Data(
                DataRouterStep::OwnerMismatch,
            )),
            SupervisorTransitionDisposition::TerminalFailClosedDrain
        );
        assert_eq!(
            supervisor_transition_disposition(NodeInterfaceSupervisorTransition::OrdinaryPermit {
                actor: 0,
                step: OrdinaryPermitServerStep::InternalInvariant,
            }),
            SupervisorTransitionDisposition::TerminalFailClosedDrain
        );
    }

    #[test]
    fn protocol_dispatch_confirmation_fails_closed_only_on_the_first_hop() {
        assert_eq!(
            protocol_dispatch_confirmation_disposition(true, true),
            ProtocolDispatchConfirmationDisposition::Confirmed
        );
        assert_eq!(
            protocol_dispatch_confirmation_disposition(false, true),
            ProtocolDispatchConfirmationDisposition::Confirmed
        );
        assert_eq!(
            protocol_dispatch_confirmation_disposition(false, false),
            ProtocolDispatchConfirmationDisposition::EarlierFanoutConfirmed
        );
        assert_eq!(
            protocol_dispatch_confirmation_disposition(true, false),
            ProtocolDispatchConfirmationDisposition::TerminalFailClosedDrain
        );
    }

    #[test]
    fn exact_path_packet_ignores_other_generations_before_and_after_token_capture() {
        let path = (0_u16, 6_u64);
        let interleaved = (1_u16, 7_u64);
        let mut expected = None;

        assert_eq!(
            retained_packet_correlation(expected, interleaved),
            RetainedPacketCorrelation::Unrelated,
            "a routed or completed packet before path staging cannot be the path packet",
        );
        assert_eq!(
            exact_packet_correlation(expected, path),
            ExactPacketCorrelation::Capture,
        );
        expected = Some(path);
        assert_eq!(
            retained_packet_correlation(expected, path),
            RetainedPacketCorrelation::Exact,
        );

        assert_eq!(
            exact_packet_correlation(expected, interleaved),
            ExactPacketCorrelation::Unrelated,
            "another packet may stage while the exact path packet is in an actor",
        );
        assert_eq!(
            retained_packet_correlation(expected, interleaved),
            RetainedPacketCorrelation::Unrelated,
            "another packet may route and complete while the path completion remains pending",
        );
        assert_eq!(
            retained_packet_correlation(expected, path),
            RetainedPacketCorrelation::Exact,
        );
        assert_eq!(
            exact_packet_correlation(expected, path),
            ExactPacketCorrelation::Exact,
            "only a duplicate stage of the exact captured generation is an invariant violation",
        );
    }

    #[test]
    fn rejected_unrelated_envelope_cannot_steal_a_staged_exact_path_owner() {
        let path_token = (0_u16, 6_u64);

        assert_eq!(
            pending_path_protocol_disposition::<(u16, u64)>(None),
            PendingPathProtocolDisposition::TransferAwaitingStage,
            "a rejected path envelope still owns its correlation before staging",
        );
        assert_eq!(
            pending_path_protocol_disposition(Some(path_token)),
            PendingPathProtocolDisposition::RetainInFlight,
            "after staging, a rejected envelope is unrelated to the exact path generation",
        );
        assert_eq!(
            retained_packet_correlation(Some(path_token), path_token),
            RetainedPacketCorrelation::Exact,
            "retaining the correlation keeps the later exact path completion actionable",
        );
    }

    #[test]
    fn unrelated_terminal_input_fault_cannot_detach_a_staged_exact_path_owner() {
        let path_token = (0_u16, 6_u64);

        assert_eq!(
            pending_path_protocol_disposition(Some(path_token)),
            PendingPathProtocolDisposition::RetainInFlight,
        );
        assert_eq!(
            pending_path_protocol_disposition::<(u16, u64)>(None),
            PendingPathProtocolDisposition::TransferAwaitingStage,
            "only the unstaged path input itself can own terminal input residue",
        );
        assert_eq!(
            retained_packet_correlation(Some(path_token), path_token),
            RetainedPacketCorrelation::Exact,
            "retaining T leaves exact completion reconciliation live during fault drain",
        );
    }
}
