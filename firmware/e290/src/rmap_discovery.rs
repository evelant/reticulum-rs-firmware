//! Cooperative runtime policy for opt-in RMAP interface discovery.

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

/// Transport-neutral policy for the first RMAP discovery publication.
///
/// A node with only local transports can publish as soon as its stamp is
/// ready. A node configured with a public uplink can instead retain that first
/// due publication until the selected interface becomes authoritative and
/// online. Once one publication has been queued, the ordinary six-hour cadence
/// is no longer gated by this boot-time policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RmapPublicationPolicy {
    /// Publish the first stamped payload on any currently eligible transport.
    Immediate,
    /// Retain the first due payload until this interface is online.
    AwaitInitialInterface(PacketInterfaceId),
}

impl RmapPublicationPolicy {
    /// Construct the LoRa-only/default policy with no initial interface gate.
    pub const fn immediate() -> Self {
        Self::Immediate
    }

    /// Gate only the first publication on one transport-neutral interface ID.
    pub const fn await_initial_interface(interface: PacketInterfaceId) -> Self {
        Self::AwaitInitialInterface(interface)
    }

    /// Interface whose Ready lifecycle state gates the first publication.
    pub const fn initial_interface(self) -> Option<PacketInterfaceId> {
        match self {
            Self::Immediate => None,
            Self::AwaitInitialInterface(interface) => Some(interface),
        }
    }

    const fn permits_publication(
        self,
        published_count: u32,
        initial_interface_ready: bool,
    ) -> bool {
        published_count != 0 || matches!(self, Self::Immediate) || initial_interface_ready
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
        }
    }

    /// Install one immutable packed map and compact incremental stamp search.
    pub fn configure(&mut self, packed: PackedDiscoveryInfo, search: DiscoveryStampSearch) {
        self.packed = Some(packed);
        self.search = Some(search);
        self.app_data = None;
        self.next_announce_seconds = 0;
        self.published_count = 0;
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
            DiscoveryStampProgress::Pending => RmapStampStep::Progressed {
                attempts: search.attempts(),
            },
            DiscoveryStampProgress::Found(stamp) => {
                let packed = self
                    .packed
                    .expect("an installed stamp search always retains its packed map");
                let attempts = search.attempts();
                self.app_data = Some(packed.with_stamp(stamp));
                self.search = None;
                self.next_announce_seconds = now_seconds;
                RmapStampStep::Completed { attempts }
            }
            DiscoveryStampProgress::Exhausted => {
                self.search = None;
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
            .permits_publication(self.published_count, initial_interface_ready)
            .then(|| self.due_app_data(now_seconds))
            .flatten()
    }

    /// Schedule the next six-hour publication after one announce was queued.
    pub fn mark_announced(&mut self, now_seconds: u64) {
        self.next_announce_seconds = now_seconds.saturating_add(RMAP_DISCOVERY_INTERVAL_SECONDS);
        self.published_count = self.published_count.saturating_add(1);
    }

    /// Retain the stamped payload and retry after bounded queue pressure.
    pub fn defer_announce(&mut self, now_seconds: u64) {
        self.next_announce_seconds = now_seconds.saturating_add(RMAP_DISCOVERY_RETRY_SECONDS);
    }

    /// Make the cached stamped payload immediately due, if available.
    pub fn request_manual_publication(&mut self, now_seconds: u64) -> bool {
        if self.app_data.is_none() {
            return false;
        }
        self.next_announce_seconds = now_seconds;
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

    #[test]
    fn completed_stamp_is_immediately_due_then_uses_six_hour_cadence() {
        let packed = packed();
        let search = DiscoveryStampSearch::new(&packed, 0).unwrap();
        let mut runtime = RmapDiscoveryRuntime::disabled();
        runtime.configure(packed, search);

        assert!(matches!(
            runtime.step_stamp_search(10),
            RmapStampStep::Completed { .. }
        ));
        assert!(runtime.due_app_data(10).is_some());
        runtime.mark_announced(10);
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
        let packed = packed();
        let search = DiscoveryStampSearch::new(&packed, 0).unwrap();
        let mut runtime = RmapDiscoveryRuntime::disabled();
        runtime.configure(packed, search);
        assert!(matches!(
            runtime.step_stamp_search(10),
            RmapStampStep::Completed { .. }
        ));
        let policy = RmapPublicationPolicy::await_initial_interface(PacketInterfaceId::new(2));

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

        runtime.mark_announced(10 + RMAP_DISCOVERY_INTERVAL_SECONDS);
        assert_eq!(runtime.published_count(), 1);
        assert!(
            runtime
                .due_app_data_for_policy(10 + (2 * RMAP_DISCOVERY_INTERVAL_SECONDS), policy, false,)
                .is_some()
        );
    }

    #[test]
    fn immediate_policy_preserves_lora_only_initial_publication() {
        let packed = packed();
        let search = DiscoveryStampSearch::new(&packed, 0).unwrap();
        let mut runtime = RmapDiscoveryRuntime::disabled();
        runtime.configure(packed, search);
        runtime.step_stamp_search(3);

        assert!(
            runtime
                .due_app_data_for_policy(3, RmapPublicationPolicy::immediate(), false)
                .is_some()
        );
    }

    #[test]
    fn manual_service_announce_reuses_cached_stamp_and_makes_rmap_due() {
        let packed = packed();
        let search = DiscoveryStampSearch::new(&packed, 0).unwrap();
        let mut runtime = RmapDiscoveryRuntime::disabled();
        runtime.configure(packed, search);
        runtime.step_stamp_search(1);
        runtime.mark_announced(1);

        assert!(runtime.request_manual_publication(2));
        assert!(runtime.due_app_data(2).is_some());
    }
}
