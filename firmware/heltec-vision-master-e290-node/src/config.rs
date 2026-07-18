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
/// Concrete interface actors in the first LoRa-only executable profile.
pub const INTERFACE_SLOTS: usize = 1;
/// Jobs, completions and ingress buffers available per concrete actor.
pub const INTERFACE_QUEUE_DEPTH: usize = 2;

/// Submission records retained by the current resident development profile.
///
/// This deliberately small fixed profile validates the complete coordinator
/// ownership path. It is not the eventual message-retention capacity or a
/// reason to constrain PSRAM-equipped product profiles.
pub const DURABLE_SUBMISSIONS: usize = 4;
/// Volatile lifecycle correlations retained by the resident runtime.
pub const DURABLE_PROJECTED_SUBMISSIONS: usize = 2;
/// Internal-static RAM occupied by the backend-independent durable runtime.
pub const DURABLE_RUNTIME_BYTES: usize = core::mem::size_of::<
    reticulum_submission_runtime::SubmissionRuntime<
        DURABLE_SUBMISSIONS,
        DURABLE_PROJECTED_SUBMISSIONS,
    >,
>();
/// Guard against silently growing the current internal-static profile.
pub const MAXIMUM_DURABLE_RUNTIME_BYTES: usize = 16 * 1024;
const _: () = assert!(DURABLE_RUNTIME_BYTES <= MAXIMUM_DURABLE_RUNTIME_BYTES);
/// Accepted submissions permitted by the first bounded local-admission profile.
///
/// One durable submission is enough to qualify the complete admission,
/// preparation, LoRa DATA, exact frame-echo, terminal, status, and remount
/// path without presenting the current four-slot development index as a
/// product retention policy. No external API bearer exists yet.
pub const DURABLE_ACCEPTED_SUBMISSION_LIMIT: usize = 1;
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

/// Reclaimed internal SRAM assigned to the global allocator.
///
/// Ownership machines and Embassy task state remain in internal static RAM;
/// the qualified external allocator carries growth-oriented protocol/client
/// allocations. Sixty-four KiB leaves the E290 link layout enough internal
/// space for the fixed 4-DATA/8-ordinary profile.
pub const INTERNAL_HEAP_BYTES: usize = 64 * 1024;
/// Qualified minimum external RAM required by this product profile.
pub const MINIMUM_PSRAM_BYTES: usize = 8 * 1024 * 1024;
/// Largest E290 datasheet PSRAM claim accepted by this image.
pub const MAXIMUM_PSRAM_BYTES: usize = 16 * 1024 * 1024;

/// Conservative initial SX1262 SPI clock.
pub const SPI_FREQUENCY_HZ: u32 = 1_000_000;
/// Per-edge upper bound for an asserted SX1262 BUSY signal.
pub const BUSY_PIN_WATCHDOG_MS: u64 = 100;

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
/// authenticated local API, and storage.
pub const NODE_FAIR_LANES: u8 = 6;
/// Fixed-RAM ceiling for the static authenticated request/reply channels.
pub const MAXIMUM_AUTHENTICATED_API_HANDOFF_BYTES: usize = 2_048;
/// Fixed-RAM ceiling for node-retained authenticated request/reply/quarantine state.
pub const MAXIMUM_AUTHENTICATED_API_NODE_STATE_BYTES: usize = 1_024;
/// Native RNS maintenance cadence.
pub const PROTOCOL_TICK_INTERVAL_SECONDS: u64 = 1;
/// Periodic local transport announce cadence for the first milestone.
pub const ANNOUNCE_INTERVAL_SECONDS: u64 = 30 * 60;
/// Deadline assigned to one admitted ordinary-action envelope.
pub const ORDINARY_OWNER_LEASE_MS: u64 = 30_000;
/// Deadline assigned to one durable DATA packet-owner attempt.
pub const SUBMISSION_OWNER_LEASE_MS: u64 = 30_000;
/// Delay before retrying an ambiguous or temporarily busy journal operation.
pub const STORAGE_RETRY_BACKOFF_MS: u64 = 1_000;
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

/// Classify terminal local faults without stopping forced denial/completion
/// drainage for owners already in motion.
pub const fn supervisor_transition_disposition(
    transition: NodeInterfaceSupervisorTransition,
) -> SupervisorTransitionDisposition {
    match transition {
        NodeInterfaceSupervisorTransition::Idle => SupervisorTransitionDisposition::Idle,
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
    fn first_live_admission_profile_owns_exactly_one_submission() {
        assert_eq!(DURABLE_ACCEPTED_SUBMISSION_LIMIT, 1);
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
}
