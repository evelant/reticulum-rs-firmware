//! Product profile for the first permanent E290 node image.

use reticulum_board_heltec_vision_master_e290_radio::E290_NA915_DEV_PROFILE;
use reticulum_interface_router::{
    AdvertisedBitrate, InterfaceConfigId, InterfaceCost, InterfaceProperties, LogicalMtu,
};
use reticulum_node_core::{
    MonotonicMillis, OrdinaryActionAdmissionError, TxCompletionCode, TxLeaseDeadline,
};
use reticulum_radio_interface::LogicalPacketAccessConfig;
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
/// Canonical LXMF delivery-announce data for an unnamed, stamp-free service
/// with no advertised optional functionality.
///
/// This is MessagePack `[nil, nil, []]`. The explicit empty functionality list
/// avoids the legacy interpretation that a missing third field implies LXMF
/// compression support.
pub const LXMF_DELIVERY_ANNOUNCE_APP_DATA: [u8; 4] = [0x93, 0xc0, 0xc0, 0x90];

/// Fixed native path-table capacity.
pub const PATHS: usize = 16;
/// Pending local announce capacity.
pub const ANNOUNCES: usize = 4;
/// Native packet deduplication capacity.
pub const DEDUPLICATION: usize = 32;
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
const _: () = assert!(LXMF_INDEX_SLOTS == 512);
const _: () = assert!(LXMF_INDEX_STORAGE_BYTES > 0);
/// Concrete interface actors in the first LoRa-only executable profile.
pub const INTERFACE_SLOTS: usize = 1;
/// Jobs, completions and ingress buffers available per concrete actor.
pub const INTERFACE_QUEUE_DEPTH: usize = 2;

/// Submission records retained by the first USB-usable PSRAM product profile.
///
/// One hundred twenty-eight supports a useful multi-message client trial while
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
// Xtensa's 32-bit field layout is 24 bytes smaller than the 64-bit host layout.
#[cfg(target_arch = "xtensa")]
const REVIEWED_DURABLE_RUNTIME_BYTES: usize = 389_368;
#[cfg(not(target_arch = "xtensa"))]
const REVIEWED_DURABLE_RUNTIME_BYTES: usize = 389_392;
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
/// Accepted submissions permitted before the non-reclaiming POC journal
/// reports explicit capacity exhaustion.
pub const DURABLE_ACCEPTED_SUBMISSION_LIMIT: usize = DURABLE_SUBMISSIONS;
/// First durable submission identifier in an empty product journal.
pub const FIRST_SUBMISSION_ID: u64 = 1;

/// Immutable product configuration identity for E290 NA915 LoRa.
pub const LORA_INTERFACE_CONFIG_ID: InterfaceConfigId = InterfaceConfigId::new(0xe290_0001);
/// Initial relative route cost for LoRa.
pub const LORA_INTERFACE_COST: InterfaceCost = InterfaceCost::new(10);
/// Nominal SF7/BW125/CR4/5 PHY bitrate advertised for diagnostics.
pub const LORA_ADVERTISED_BITRATE: AdvertisedBitrate = match AdvertisedBitrate::try_new(5_468) {
    Ok(value) => value,
    Err(_) => panic!("the E290 advertised bitrate must be non-zero"),
};

/// Reclaimed internal SRAM assigned to a BLE-capable global allocator.
///
/// Ownership machines and Embassy task state remain in internal static RAM.
/// This region is registered before PSRAM, so ordinary global allocations use
/// it first and spill into external RAM only when no internal hole fits. An
/// explicit placement policy is still required before large protocol/client
/// allocations are enabled. Seventy-two KiB is the largest whole-KiB
/// allocation that fits the ESP32-S3's separate 73,744-byte reclaimed DRAM2
/// segment. The final 8 KiB is available to esp-radio's 8,192-byte
/// strict-internal controller-task stack and controller allocations without
/// shrinking the product executor stack in ordinary DRAM. The pinned
/// esp-radio documentation recommends more total heap (64 KiB reclaimed plus
/// 36 KiB ordinary), so this profile still requires powered heap
/// qualification rather than treating 72 KiB as a general BLE guarantee.
#[cfg(feature = "ble-api-proof")]
pub const INTERNAL_HEAP_BYTES: usize = 72 * 1024;
/// Reclaimed internal SRAM assigned to the ordinary and Wi-Fi allocators.
///
/// These profiles retain their measured 64 KiB reservation. BLE alone claims
/// the otherwise-unused final 8 KiB of the separate reclaimed DRAM2 segment.
#[cfg(not(feature = "ble-api-proof"))]
pub const INTERNAL_HEAP_BYTES: usize = 64 * 1024;
/// Qualified minimum external RAM required by this product profile.
pub const MINIMUM_PSRAM_BYTES: usize = 8 * 1024 * 1024;
/// Largest E290 datasheet PSRAM claim accepted by this image.
pub const MAXIMUM_PSRAM_BYTES: usize = 16 * 1024 * 1024;

/// Conservative initial SX1262 SPI clock.
pub const SPI_FREQUENCY_HZ: u32 = 1_000_000;
/// Per-edge upper bound for an asserted SX1262 BUSY signal.
pub const BUSY_PIN_WATCHDOG_MS: u64 = 100;

/// Maximum idle continuous-RX wait before the LoRa actor checks queued TX.
///
/// This preserves the former 248-symbol SF7/BW125 scheduler cadence without
/// using SX1262 single-shot receive mode: `248 * 1.024 ms = 253.952 ms`.
/// The modem remains continuously armed across this software-only yield.
pub const RX_SCHEDULER_YIELD_US: u64 = 253_952;

/// Driver/executor allowance after receive progress before a false-preamble rearm.
pub const RX_PROGRESS_TIMEOUT_MARGIN_US: u64 = 100_000;
/// Recoverable deadline from first-polled receive progress to a terminal frame IRQ.
///
/// The maximum-frame airtime already includes the complete configured preamble,
/// so starting this bound at the progress IRQ is conservative for every
/// admissible 255-byte physical frame. Expiry rearms continuous RX; it does not
/// authorize TX or fail-stop the actor.
pub const RX_PROGRESS_TIMEOUT_US: u64 = E290_NA915_DEV_PROFILE
    .maximum_frame_time_on_air_us()
    .saturating_add(RX_PROGRESS_TIMEOUT_MARGIN_US);

/// Whole-operation CAD watchdog.
///
/// A single SF7/BW125 CAD is only a few symbols. Five hundred milliseconds
/// deliberately leaves more than two orders of magnitude for SPI, IRQ and
/// executor latency while remaining finite. Expiry is terminal cancellation,
/// never a synthetic busy/clear result.
pub const CAD_OPERATION_WATCHDOG_US: u64 = 500_000;

/// Whole logical-packet TX watchdog, including both maximum-size RNode frames.
///
/// The fixed profile needs well under one second of RF time for two 255-byte
/// frames. This 1.5-second development bound also covers preparation,
/// inter-frame turnaround, IRQ processing and cleanup. Expiry invokes the
/// dispatcher's explicit cancellation recovery and permanently fail-stops the
/// LoRa actor, so it schedules no further radio operations.
pub const TX_OPERATION_WATCHDOG_US: u64 = 1_500_000;
/// Exact two-frame airtime ceiling for a 500-byte packet in this profile.
pub const MAXIMUM_LOGICAL_PACKET_AIRTIME_US: u64 =
    match E290_NA915_DEV_PROFILE.rnode_packet_airtime(500) {
        Ok(airtime) => airtime.aggregate_time_on_air_us(),
        Err(_) => panic!("the fixed E290 profile must represent a base-MTU packet"),
    };
/// Bounded dispatcher/driver setup before first predicted RF.
pub const TX_PRE_FIRST_RF_SETUP_US: u64 = 50_000;
/// Bounded non-RF gap between split packet frames.
pub const TX_INTER_FRAME_TURNAROUND_US: u64 = 25_000;
/// IRQ, SPI cleanup and scheduler latency allowance inside the watchdog.
pub const TX_DRIVER_AND_SCHEDULER_MARGIN_US: u64 = 500_000;
/// Minimum justified whole-operation TX watchdog coverage.
pub const MAXIMUM_TX_OPERATION_REQUIRED_US: u64 = MAXIMUM_LOGICAL_PACKET_AIRTIME_US
    .saturating_add(TX_PRE_FIRST_RF_SETUP_US)
    .saturating_add(TX_INTER_FRAME_TURNAROUND_US)
    .saturating_add(TX_DRIVER_AND_SCHEDULER_MARGIN_US);

const _: () = assert!(TX_OPERATION_WATCHDOG_US >= MAXIMUM_TX_OPERATION_REQUIRED_US);

/// Temporary quiescent poll delay pending a combined aggregate wait surface.
pub const NODE_POLL_INTERVAL_MS: u64 = 1;
/// Fair synchronous lane passes before the node task yields.
pub const NODE_MAX_IMMEDIATE_PASSES: usize = 16;
/// Fair node-task lanes: ingress, supervisor, maintenance, announce,
/// authenticated local API, outbound Nomad, and storage.
pub const NODE_FAIR_LANES: u8 = 7;

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
/// Quiet time between distinct local destinations on the half-duplex radio.
///
/// A transport peer may immediately rebroadcast the first announce it accepts;
/// the native retransmission delay plus this product guard keeps the following
/// service announce out of the same nominal emission opportunity.
pub const ANNOUNCE_DESTINATION_SPACING_SECONDS: u64 =
    ANNOUNCE_NATIVE_RETRANSMIT_SECONDS + ANNOUNCE_MINIMUM_EMISSION_SEPARATION_SECONDS;
/// Earliest delay before the first post-boot announce retry.
pub const ANNOUNCE_BOOTSTRAP_BASE_SECONDS: u64 = 13;
/// Primary-destination-derived phase buckets for the first bootstrap retry.
///
/// The prime bucket count makes the full little-endian seed participate in the
/// modulus. The qualified E290 A/B identities occupy phases 34 and 5, leaving
/// 29 seconds between their corresponding three-destination retry bursts.
pub const ANNOUNCE_BOOTSTRAP_PHASE_SLOTS: u64 = 43;
/// Bounded delay before retrying the same destination after protocol rejection.
pub const ANNOUNCE_ADMISSION_RETRY_SECONDS: u64 = 1;
/// Delay from the first bootstrap retry's final destination to the second.
///
/// Together with the 16-second three-destination burst and the qualified
/// identities' 29-second phase separation, 38 seconds leaves at least the
/// three-second product guard between both boards' native retransmissions.
pub const ANNOUNCE_BOOTSTRAP_RETRY_SPACING_SECONDS: u64 = 38;
/// Periodic local transport announce cadence after bootstrap discovery.
pub const ANNOUNCE_INTERVAL_SECONDS: u64 = 30 * 60;
const _: () = assert!(ANNOUNCE_BOOTSTRAP_RETRIES > 0);
const _: () = assert!(ANNOUNCE_DESTINATION_SPACING_SECONDS > ANNOUNCE_NATIVE_RETRANSMIT_SECONDS);
const _: () = assert!(ANNOUNCE_BOOTSTRAP_PHASE_SLOTS > 0);
const _: () = assert!(ANNOUNCE_ADMISSION_RETRY_SECONDS > 0);
const _: () = assert!(ANNOUNCE_BOOTSTRAP_RETRY_SPACING_SECONDS > 0);
const _: () = assert!(ANNOUNCE_INTERVAL_SECONDS > ANNOUNCE_BOOTSTRAP_RETRY_SPACING_SECONDS);
/// Deadline assigned to one admitted ordinary-action envelope.
pub const ORDINARY_OWNER_LEASE_MS: u64 = 30_000;
/// Deadline assigned to one durable DATA packet-owner attempt.
pub const SUBMISSION_OWNER_LEASE_MS: u64 = 30_000;
/// Delay before retrying an ambiguous or temporarily busy journal operation.
pub const STORAGE_RETRY_BACKOFF_MS: u64 = 1_000;
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
/// Bounded cadence for the USB Serial/JTAG owner and raw GPIO sampler.
pub const USB_PAIRING_POLL_INTERVAL_MS: u64 = 1;
/// Repeated debounced button-observation cadence supplied to pairing policy.
pub const PAIRING_BUTTON_OBSERVATION_INTERVAL_MS: u64 = 20;
/// Maximum RX or TX bytes touched by one USB task poll.
pub const USB_PAIRING_MAX_BYTES_PER_POLL: usize = 64;

/// Product action for an envelope surfaced by the coordinator's explicit
/// rejected-actions output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RejectedActionDisposition {
    /// Preserve the exact actions but replace their elapsed owner deadline.
    RefreshDeadline,
    /// A semantic packet/profile error is not retryable in this milestone.
    FailStop,
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

/// Construct the immutable registry properties for LoRa slot zero.
pub const fn interface_properties() -> InterfaceProperties {
    InterfaceProperties::new(
        match LogicalMtu::try_new(500) {
            Ok(mtu) => mtu,
            Err(_) => panic!("the base Reticulum MTU must be non-zero"),
        },
        LORA_INTERFACE_CONFIG_ID,
        Some(LORA_ADVERTISED_BITRATE),
        LORA_INTERFACE_COST,
    )
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

/// Validated RNode-compatible randomized backoff and CAD policy.
///
/// The continuous random interval preserves the reference 24 ms minimum slot
/// and complete 15-slot contention envelope. Busy exhaustion rejects without
/// a permit; it never forces a transmission.
pub const fn logical_packet_access() -> LogicalPacketAccessConfig {
    match LogicalPacketAccessConfig::try_new(
        3,       // initial CAD plus two bounded busy retries
        24_000,  // one reference RNode contention slot
        360_000, // full 15-slot contention envelope, including the first slot
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
pub const fn dispatcher_config() -> RadioTxDispatcherConfig {
    RadioTxDispatcherConfig::new(
        LORA_INTERFACE_CONFIG_ID,
        logical_packet_access(),
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
    #[cfg(feature = "ble-api-proof")]
    fn internal_heap_uses_the_bounded_esp32s3_reclaimed_dram2_capacity() {
        assert_eq!(INTERNAL_HEAP_BYTES, 72 * 1024);
        assert!(INTERNAL_HEAP_BYTES <= 73_744);
        assert_eq!(73_744 - INTERNAL_HEAP_BYTES, 16);
    }

    #[test]
    #[cfg(not(feature = "ble-api-proof"))]
    fn non_ble_internal_heap_preserves_the_measured_baseline() {
        assert_eq!(INTERNAL_HEAP_BYTES, 64 * 1024);
    }

    #[test]
    fn lxmf_delivery_announce_explicitly_advertises_no_optional_functionality() {
        assert_eq!(LXMF_DELIVERY_ANNOUNCE_APP_DATA, [0x93, 0xc0, 0xc0, 0x90]);
    }

    #[test]
    fn usb_usable_profile_owns_128_bounded_submissions_in_psram() {
        assert_eq!(DURABLE_SUBMISSIONS, 128);
        assert_eq!(DURABLE_PROJECTED_SUBMISSIONS, 128);
        assert_eq!(DURABLE_ACCEPTED_SUBMISSION_LIMIT, 128);
        assert_eq!(DURABLE_RUNTIME_BYTES, 389_392);
        assert_eq!(MAXIMUM_DURABLE_RUNTIME_BYTES, 512 * 1024);
        const { assert!(DURABLE_ACCEPTED_SUBMISSION_LIMIT <= DURABLE_SUBMISSIONS) };
    }

    #[test]
    fn channel_access_preserves_the_reference_rnode_contention_envelope() {
        let access = logical_packet_access();
        assert_eq!(access.maximum_cad_attempts(), 3);
        assert_eq!(access.minimum_backoff_us(), 24_000);
        assert_eq!(access.maximum_backoff_us(), 360_000);
        assert_eq!(access.maximum_pre_first_rf_setup_us(), 50_000);
        assert_eq!(access.maximum_inter_frame_turnaround_us(), 25_000);
    }

    #[test]
    fn receive_progress_deadline_is_recoverable_and_covers_a_maximum_frame() {
        assert_eq!(RX_PROGRESS_TIMEOUT_MARGIN_US, 100_000);
        assert_eq!(
            RX_PROGRESS_TIMEOUT_US,
            E290_NA915_DEV_PROFILE
                .maximum_frame_time_on_air_us()
                .saturating_add(RX_PROGRESS_TIMEOUT_MARGIN_US)
        );
        assert!(
            RX_PROGRESS_TIMEOUT_US
                < reticulum_board_heltec_vision_master_e290_radio::E290_MAXIMUM_RECEIVE_OPERATION_US
                    .get()
        );
    }

    #[test]
    fn tx_watchdog_covers_exact_maximum_airtime_and_named_margins() {
        assert_eq!(MAXIMUM_LOGICAL_PACKET_AIRTIME_US, 821_760);
        assert_eq!(MAXIMUM_TX_OPERATION_REQUIRED_US, 1_396_760);
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
}
