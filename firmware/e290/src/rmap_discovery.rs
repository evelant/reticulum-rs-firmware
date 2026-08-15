//! Cooperative runtime policy for opt-in RMAP interface discovery.

use core::cell::RefCell;

use embassy_sync::{blocking_mutex::Mutex, blocking_mutex::raw::CriticalSectionRawMutex};
use reticulum_device_api::{
    RmapDeferredReason, RmapEgressConfirmation, RmapInitialTcpGateState, RmapQueueOutcome,
    RmapRuntimeStatus, RmapStampPhase,
};
use reticulum_node_core::{
    DestinationHash, InboundProofPolicy, LocalDestinationLinkPolicyError,
    LocalDestinationProofPolicyError, LocalDestinationRegistrationError, NodeCore,
    PacketInterfaceId,
};
use reticulum_rns_interface_discovery::{
    DISCOVERY_APPLICATION_NAME, DISCOVERY_ASPECTS, DiscoveryAppData, DiscoveryStampProgress,
    DiscoveryStampSearch, PackedDiscoveryInfo,
};

/// Current RNS default interval between interface-discovery announces.
pub const RMAP_DISCOVERY_INTERVAL_SECONDS: u64 = 6 * 60 * 60;
/// Delay after temporary Reticulum announce-queue pressure.
pub const RMAP_DISCOVERY_RETRY_SECONDS: u64 = 60;
/// Proof-of-work candidates tested per cooperative node turn.
pub const RMAP_STAMP_ATTEMPTS_PER_TURN: u32 = 8;

/// Transport policy for every RMAP discovery publication.
///
/// A node with only local transports broadcasts when its stamp is ready. A
/// node configured with a public uplink retains every due publication until
/// that exact interface is authoritative and online. The exact target also
/// applies to RNS's native announce retransmit, preventing discovery traffic
/// from spilling onto LoRa after the initial TCP send.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RmapPublicationPolicy {
    /// Publish the stamped payload on every ordinarily eligible transport.
    Immediate,
    /// Retain and then send only on this exact interface.
    RequireInterface(PacketInterfaceId),
}

impl RmapPublicationPolicy {
    /// Construct the LoRa-only/default policy with no initial interface gate.
    pub const fn immediate() -> Self {
        Self::Immediate
    }

    /// Gate every publication on one transport-neutral interface ID.
    pub const fn require_interface(interface: PacketInterfaceId) -> Self {
        Self::RequireInterface(interface)
    }

    /// Interface whose Ready lifecycle state gates the first publication.
    pub const fn initial_interface(self) -> Option<PacketInterfaceId> {
        match self {
            Self::Immediate => None,
            Self::RequireInterface(interface) => Some(interface),
        }
    }

    const fn permits_publication(self, interface_ready: bool) -> bool {
        matches!(self, Self::Immediate) || interface_ready
    }

    const fn gate_state(self, interface_ready: bool) -> RmapInitialTcpGateState {
        match self {
            Self::Immediate => RmapInitialTcpGateState::NotRequired,
            Self::RequireInterface(_) if interface_ready => RmapInitialTcpGateState::Open,
            Self::RequireInterface(_) => RmapInitialTcpGateState::Waiting,
        }
    }
}

/// Construction failure for the local interface-discovery destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RmapDiscoveryActivationError {
    /// RNS rejected or lacked capacity for the additional destination.
    Registration(LocalDestinationRegistrationError),
    /// RNS could not disable inbound Links on this announce-only destination.
    LinkPolicy(LocalDestinationLinkPolicyError),
    /// RNS rejected the explicit no-proof policy.
    ProofPolicy(LocalDestinationProofPolicyError),
}

/// Register the local `rnstransport.discovery.interface` announce source.
///
/// The destination exists only to sign opt-in interface-discovery announces.
/// It accepts neither Links nor ordinary application DATA.
pub fn activate_rmap_discovery_destination<
    const PATHS: usize,
    const ANNOUNCES: usize,
    const DEDUPLICATION: usize,
    const LINKS: usize,
    const PACKET_BUFFERS: usize,
>(
    node: &mut NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, PACKET_BUFFERS>,
) -> Result<DestinationHash, RmapDiscoveryActivationError> {
    let destination = node
        .register_inbound_single_destination(DISCOVERY_APPLICATION_NAME, &DISCOVERY_ASPECTS)
        .map_err(RmapDiscoveryActivationError::Registration)?;
    node.set_destination_accepts_links(&destination, false)
        .map_err(RmapDiscoveryActivationError::LinkPolicy)?;
    node.set_destination_inbound_proof_policy(&destination, InboundProofPolicy::Never)
        .map_err(RmapDiscoveryActivationError::ProofPolicy)?;
    Ok(destination)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RmapDiscoveryStatusState {
    stamp_phase: RmapStampPhase,
    stamp_attempts: u64,
    initial_tcp_gate: RmapInitialTcpGateState,
    queued_count: u32,
    last_queue_outcome: RmapQueueOutcome,
    last_queue_attempt_at_uptime_seconds: Option<u64>,
    egress_confirmation: RmapEgressConfirmation,
    next_due_at_uptime_seconds: Option<u64>,
    deferred_reason: Option<RmapDeferredReason>,
}

impl RmapDiscoveryStatusState {
    const DISABLED: Self = Self {
        stamp_phase: RmapStampPhase::Disabled,
        stamp_attempts: 0,
        initial_tcp_gate: RmapInitialTcpGateState::NotRequired,
        queued_count: 0,
        last_queue_outcome: RmapQueueOutcome::NotAttempted,
        last_queue_attempt_at_uptime_seconds: None,
        egress_confirmation: RmapEgressConfirmation::NotApplicable,
        next_due_at_uptime_seconds: None,
        deferred_reason: None,
    };

    const fn searching(policy: RmapPublicationPolicy) -> Self {
        Self {
            stamp_phase: RmapStampPhase::Searching,
            initial_tcp_gate: policy.gate_state(false),
            ..Self::DISABLED
        }
    }

    fn projection(self, now_seconds: u64, config_applied: bool) -> RmapRuntimeStatus {
        RmapRuntimeStatus::new(
            config_applied,
            self.stamp_phase,
            self.stamp_attempts,
            self.initial_tcp_gate,
            self.queued_count,
            self.last_queue_outcome,
            self.last_queue_attempt_at_uptime_seconds,
            self.egress_confirmation,
            self.next_due_at_uptime_seconds
                .map(|due| due.saturating_sub(now_seconds)),
            self.deferred_reason,
        )
    }
}

/// Blocking latest-value cell shared by the node and authenticated API owners.
pub struct RmapDiscoveryStatusCell {
    state: Mutex<CriticalSectionRawMutex, RefCell<RmapDiscoveryStatusState>>,
}

impl RmapDiscoveryStatusCell {
    /// Construct a disabled status cell before boot applies network configuration.
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(RefCell::new(RmapDiscoveryStatusState::DISABLED)),
        }
    }

    fn publish(&self, state: RmapDiscoveryStatusState) {
        self.state.lock(|cell| *cell.borrow_mut() = state);
    }

    fn update(&self, update: impl FnOnce(&mut RmapDiscoveryStatusState)) {
        self.state.lock(|cell| update(&mut cell.borrow_mut()));
    }

    /// Publish an activation failure that prevented a usable stamp search.
    pub fn publish_activation_failure(
        &self,
        policy: RmapPublicationPolicy,
        reason: RmapDeferredReason,
    ) {
        let mut state = RmapDiscoveryStatusState::searching(policy);
        state.stamp_phase = RmapStampPhase::Faulted;
        state.deferred_reason = Some(reason);
        self.publish(state);
    }

    /// Snapshot a secret-free API projection at the caller's monotonic time.
    pub fn snapshot(&self, now_seconds: u64, config_applied: bool) -> RmapRuntimeStatus {
        self.state
            .lock(|cell| cell.borrow().projection(now_seconds, config_applied))
    }
}

impl Default for RmapDiscoveryStatusCell {
    fn default() -> Self {
        Self::new()
    }
}

/// Compact proof search plus one small resident scheduling projection.
///
/// The search retains a pre-seeded SHA-256 state instead of the expanded
/// 5 KiB workblock. Since this runtime lives inside the appliance's external
/// application state, its remaining bounded state is PSRAM-resident.
pub struct RmapDiscoveryRuntime {
    packed: Option<PackedDiscoveryInfo>,
    search: Option<DiscoveryStampSearch>,
    app_data: Option<DiscoveryAppData>,
    next_announce_seconds: u64,
    published_count: u32,
    status: Option<&'static RmapDiscoveryStatusCell>,
    publication_policy: RmapPublicationPolicy,
}

impl RmapDiscoveryRuntime {
    /// Construct a disabled runtime.
    pub const fn disabled() -> Self {
        Self {
            packed: None,
            search: None,
            app_data: None,
            next_announce_seconds: 0,
            published_count: 0,
            status: None,
            publication_policy: RmapPublicationPolicy::Immediate,
        }
    }

    /// Install one immutable packed map and compact incremental stamp search.
    pub fn configure(
        &mut self,
        packed: PackedDiscoveryInfo,
        search: DiscoveryStampSearch,
        status: &'static RmapDiscoveryStatusCell,
        publication_policy: RmapPublicationPolicy,
    ) {
        self.packed = Some(packed);
        self.search = Some(search);
        self.app_data = None;
        self.next_announce_seconds = 0;
        self.published_count = 0;
        self.status = Some(status);
        self.publication_policy = publication_policy;
        status.publish(RmapDiscoveryStatusState::searching(publication_policy));
    }

    /// Advance a bounded number of proof-of-work candidates.
    ///
    /// The first completed stamp makes publication immediately due. A search
    /// counter exhaustion disables this boot's RMAP publication without
    /// affecting ordinary Reticulum traffic.
    pub fn step_stamp_search(&mut self, now_seconds: u64) -> RmapStampStep {
        let Some(search) = self.search.as_mut() else {
            return if self.app_data.is_some() {
                RmapStampStep::Ready
            } else {
                RmapStampStep::Disabled
            };
        };
        match search.step(RMAP_STAMP_ATTEMPTS_PER_TURN) {
            DiscoveryStampProgress::Pending => {
                let attempts = search.attempts();
                if let Some(status) = self.status {
                    status.update(|state| {
                        state.stamp_phase = RmapStampPhase::Searching;
                        state.stamp_attempts = attempts;
                    });
                }
                RmapStampStep::Progressed { attempts }
            }
            DiscoveryStampProgress::Found(stamp) => {
                let packed = self
                    .packed
                    .expect("an installed stamp search always retains its packed map");
                let attempts = search.attempts();
                self.app_data = Some(packed.with_stamp(stamp));
                self.search = None;
                self.next_announce_seconds = now_seconds;
                if let Some(status) = self.status {
                    status.update(|state| {
                        state.stamp_phase = RmapStampPhase::Ready;
                        state.stamp_attempts = attempts;
                        state.next_due_at_uptime_seconds = Some(now_seconds);
                    });
                }
                RmapStampStep::Completed { attempts }
            }
            DiscoveryStampProgress::Exhausted => {
                let attempts = search.attempts();
                self.search = None;
                if let Some(status) = self.status {
                    status.update(|state| {
                        state.stamp_phase = RmapStampPhase::Exhausted;
                        state.stamp_attempts = attempts;
                        state.next_due_at_uptime_seconds = None;
                        state.deferred_reason = Some(RmapDeferredReason::StampSearchExhausted);
                    });
                }
                RmapStampStep::Exhausted
            }
        }
    }

    /// Complete stamped application data once its schedule is due.
    pub fn due_app_data(&self, now_seconds: u64) -> Option<&DiscoveryAppData> {
        self.app_data
            .as_ref()
            .filter(|_| now_seconds >= self.next_announce_seconds)
    }

    /// Complete due application data once the initial publication gate opens.
    ///
    /// A closed gate does not alter the due timestamp or consume the cadence.
    /// The same stamped payload therefore remains immediately due when the
    /// awaited interface later reaches Ready.
    pub fn due_app_data_for_policy(
        &self,
        now_seconds: u64,
        policy: RmapPublicationPolicy,
        initial_interface_ready: bool,
    ) -> Option<&DiscoveryAppData> {
        policy
            .permits_publication(initial_interface_ready)
            .then(|| self.due_app_data(now_seconds))
            .flatten()
    }

    /// Record current readiness of the exact publication target.
    pub fn observe_publication_gate(&mut self, interface_ready: bool) {
        let gate = self.publication_policy.gate_state(interface_ready);
        if let Some(status) = self.status {
            status.update(|state| {
                state.initial_tcp_gate = gate;
                if gate == RmapInitialTcpGateState::Waiting {
                    state.deferred_reason = Some(RmapDeferredReason::InitialTcpNotReady);
                } else if state.deferred_reason == Some(RmapDeferredReason::InitialTcpNotReady) {
                    state.deferred_reason = None;
                }
            });
        }
    }

    /// Schedule the next six-hour publication after complete coordinator acceptance.
    pub fn mark_queue_accepted(&mut self, now_seconds: u64) {
        self.next_announce_seconds = now_seconds.saturating_add(RMAP_DISCOVERY_INTERVAL_SECONDS);
        self.published_count = self.published_count.saturating_add(1);
        if let Some(status) = self.status {
            status.update(|state| {
                state.queued_count = self.published_count;
                state.last_queue_outcome = RmapQueueOutcome::Accepted;
                state.last_queue_attempt_at_uptime_seconds = Some(now_seconds);
                state.egress_confirmation = RmapEgressConfirmation::NotObserved;
                state.next_due_at_uptime_seconds = Some(self.next_announce_seconds);
                state.deferred_reason = None;
            });
        }
    }

    /// Retain the stamped payload and retry after bounded queue pressure.
    pub fn defer_announce(&mut self, now_seconds: u64, reason: RmapDeferredReason) {
        self.next_announce_seconds = now_seconds.saturating_add(RMAP_DISCOVERY_RETRY_SECONDS);
        if let Some(status) = self.status {
            status.update(|state| {
                state.last_queue_outcome = RmapQueueOutcome::AnnounceAdmissionDeferred;
                state.last_queue_attempt_at_uptime_seconds = Some(now_seconds);
                state.next_due_at_uptime_seconds = Some(self.next_announce_seconds);
                state.deferred_reason = Some(reason);
            });
        }
    }

    /// Retain a due publication while its complete ordinary action owner retries admission.
    pub fn mark_ordinary_admission_deferred(&mut self, now_seconds: u64) {
        if let Some(status) = self.status {
            status.update(|state| {
                state.last_queue_outcome = RmapQueueOutcome::OrdinaryAdmissionDeferred;
                state.last_queue_attempt_at_uptime_seconds = Some(now_seconds);
                state.next_due_at_uptime_seconds = Some(self.next_announce_seconds);
                state.deferred_reason = Some(RmapDeferredReason::OrdinaryQueueRejected);
            });
        }
    }

    /// Record physical completion when an interface-specific correlation is available.
    pub fn mark_physical_egress_confirmed(&mut self) {
        if let Some(status) = self.status {
            status.update(|state| {
                state.egress_confirmation = RmapEgressConfirmation::Confirmed;
            });
        }
    }

    /// Make the cached stamped payload immediately due, if available.
    pub fn request_manual_publication(&mut self, now_seconds: u64) -> bool {
        if self.app_data.is_none() {
            return false;
        }
        self.next_announce_seconds = now_seconds;
        if let Some(status) = self.status {
            status.update(|state| {
                state.next_due_at_uptime_seconds = Some(now_seconds);
            });
        }
        true
    }

    /// Number of successfully queued RMAP discovery announces this boot.
    pub const fn published_count(&self) -> u32 {
        self.published_count
    }

    /// Whether a stamp is ready for publication.
    pub const fn is_ready(&self) -> bool {
        self.app_data.is_some()
    }
}

impl Default for RmapDiscoveryRuntime {
    fn default() -> Self {
        Self::disabled()
    }
}

/// Observable result of one cooperative stamp-search turn.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RmapStampStep {
    /// RMAP publication is disabled or its search exhausted.
    Disabled,
    /// A cached stamp is already ready.
    Ready,
    /// The bounded turn tested candidates but has not found a stamp.
    Progressed {
        /// Total candidates tested by this search.
        attempts: u64,
    },
    /// This turn found and cached a compatible stamp.
    Completed {
        /// Total candidates tested by this search.
        attempts: u64,
    },
    /// The deterministic candidate counter exhausted.
    Exhausted,
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::boxed::Box;

    use reticulum_rns_interface_discovery::{
        DiscoveryStampSearch, RnodeDiscoveryInfo, encode_rnode_info,
    };

    use super::*;

    fn packed() -> PackedDiscoveryInfo {
        encode_rnode_info(
            RnodeDiscoveryInfo::new(
                [0x42; 16],
                "Metalbeard E290 3f88",
                true,
                None,
                915_000_000,
                125_000,
                7,
                5,
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn configured_runtime(policy: RmapPublicationPolicy) -> RmapDiscoveryRuntime {
        let packed = packed();
        let search = DiscoveryStampSearch::new(&packed, 0).unwrap();
        let status = Box::leak(Box::new(RmapDiscoveryStatusCell::new()));
        let mut runtime = RmapDiscoveryRuntime::disabled();
        runtime.configure(packed, search, status, policy);
        runtime
    }

    #[test]
    fn completed_stamp_is_immediately_due_then_uses_six_hour_cadence() {
        let mut runtime = configured_runtime(RmapPublicationPolicy::immediate());

        assert!(matches!(
            runtime.step_stamp_search(10),
            RmapStampStep::Completed { .. }
        ));
        assert!(runtime.due_app_data(10).is_some());
        runtime.mark_queue_accepted(10);
        assert!(runtime.due_app_data(10).is_none());
        assert!(
            runtime
                .due_app_data(10 + RMAP_DISCOVERY_INTERVAL_SECONDS)
                .is_some()
        );
        assert_eq!(runtime.published_count(), 1);
    }

    #[test]
    fn public_uplink_gate_retains_initial_due_event_without_consuming_cadence() {
        let policy = RmapPublicationPolicy::require_interface(PacketInterfaceId::new(2));
        let mut runtime = configured_runtime(policy);
        assert!(matches!(
            runtime.step_stamp_search(10),
            RmapStampStep::Completed { .. }
        ));
        assert!(runtime.due_app_data_for_policy(10, policy, false).is_none());
        assert!(runtime.due_app_data(10).is_some());
        assert_eq!(runtime.published_count(), 0);
        assert!(
            runtime
                .due_app_data_for_policy(10 + RMAP_DISCOVERY_INTERVAL_SECONDS, policy, false)
                .is_none()
        );
        assert!(
            runtime
                .due_app_data_for_policy(10 + RMAP_DISCOVERY_INTERVAL_SECONDS, policy, true)
                .is_some()
        );

        runtime.mark_queue_accepted(10 + RMAP_DISCOVERY_INTERVAL_SECONDS);
        assert_eq!(runtime.published_count(), 1);
        assert!(
            runtime
                .due_app_data_for_policy(10 + (2 * RMAP_DISCOVERY_INTERVAL_SECONDS), policy, true,)
                .is_some()
        );
        assert!(
            runtime
                .due_app_data_for_policy(10 + (2 * RMAP_DISCOVERY_INTERVAL_SECONDS), policy, false,)
                .is_none()
        );
    }

    #[test]
    fn immediate_policy_preserves_lora_only_initial_publication() {
        let mut runtime = configured_runtime(RmapPublicationPolicy::immediate());
        runtime.step_stamp_search(3);

        assert!(
            runtime
                .due_app_data_for_policy(3, RmapPublicationPolicy::immediate(), false)
                .is_some()
        );
    }

    #[test]
    fn manual_service_announce_reuses_cached_stamp_and_makes_rmap_due() {
        let mut runtime = configured_runtime(RmapPublicationPolicy::immediate());
        runtime.step_stamp_search(1);
        runtime.mark_queue_accepted(1);

        assert!(runtime.request_manual_publication(2));
        assert!(runtime.due_app_data(2).is_some());
    }

    #[test]
    fn status_distinguishes_tcp_gate_deferral_and_accepted_cadence() {
        let packed = packed();
        let search = DiscoveryStampSearch::new(&packed, 0).unwrap();
        let status = Box::leak(Box::new(RmapDiscoveryStatusCell::new()));
        let policy = RmapPublicationPolicy::require_interface(PacketInterfaceId::new(2));
        let mut runtime = RmapDiscoveryRuntime::disabled();
        runtime.configure(packed, search, status, policy);
        runtime.step_stamp_search(10);
        runtime.observe_publication_gate(false);

        let waiting = status.snapshot(10, true);
        assert_eq!(waiting.stamp_phase, RmapStampPhase::Ready);
        assert_eq!(waiting.initial_tcp_gate, RmapInitialTcpGateState::Waiting);
        assert_eq!(waiting.next_due_in_seconds, Some(0));
        assert_eq!(
            waiting.deferred_reason,
            Some(RmapDeferredReason::InitialTcpNotReady)
        );

        runtime.observe_publication_gate(true);
        runtime.mark_ordinary_admission_deferred(11);
        let deferred = status.snapshot(11, true);
        assert_eq!(
            deferred.last_queue_outcome,
            RmapQueueOutcome::OrdinaryAdmissionDeferred
        );
        assert_eq!(deferred.queued_count, 0);
        assert_eq!(deferred.next_due_in_seconds, Some(0));

        runtime.mark_queue_accepted(12);
        let accepted = status.snapshot(12, false);
        assert!(!accepted.config_applied);
        assert_eq!(accepted.last_queue_outcome, RmapQueueOutcome::Accepted);
        assert_eq!(accepted.queued_count, 1);
        assert_eq!(
            accepted.next_due_in_seconds,
            Some(RMAP_DISCOVERY_INTERVAL_SECONDS)
        );
        assert_eq!(
            accepted.egress_confirmation,
            RmapEgressConfirmation::NotObserved
        );
        assert_eq!(accepted.deferred_reason, None);
    }
}
