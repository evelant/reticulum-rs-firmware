//! Bounded product model for the outbound Reticulum TCP packet interface.
//!
//! This module is portable and owns no socket, network stack, flash, or
//! interface-fabric capability. It defines the immutable boot endpoint, the
//! passphrase-free runtime projection, and the exact stream-credit policy used
//! by the concrete TCP actor before native packet bytes are exposed.

use core::cell::RefCell;

use embassy_sync::blocking_mutex::{Mutex, raw::CriticalSectionRawMutex};
use reticulum_device_api::ReticulumDnsDiagnostics;
use reticulum_node_core::{
    PacketInterfaceId, TxAuthorizationCandidate, TxAuthorizationPolicy, TxPermitRequirements,
    TxPermitReservation, TxPermitResourceId, TxPolicyDecision, TxPolicyDenial,
};
use reticulum_radio_tx_dispatch::ExactLoRaAirtimePolicy;

/// Maximum durable DNS hostname bytes accepted by the network store.
pub const MAX_TCP_PEER_HOSTNAME_BYTES: usize =
    reticulum_network_config_store::MAX_DNS_HOSTNAME_LENGTH;
/// Stable packet-interface identity for the first outbound TCP peer.
pub const TCP_INTERFACE: PacketInterfaceId = PacketInterfaceId::new(2);
/// Opaque authorization resource for bounded HDLC stream writes.
pub const TCP_STREAM_RESOURCE: TxPermitResourceId = TxPermitResourceId::new(*b"tcp-hdlc-stream!");

/// Why a durable peer cannot become the immutable boot endpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WifiTcpBootstrapError {
    /// TCP port zero is not a usable remote endpoint.
    ZeroPort,
    /// The address is not a usable unicast station-network peer.
    NonUnicastIpv4,
    /// The hostname was empty, too long, or not valid UTF-8.
    InvalidDnsHostname,
}

/// Immutable address of the selected outbound TCP peer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WifiTcpPeerAddress {
    /// Exact IPv4 endpoint.
    Ipv4([u8; 4]),
    /// DNS hostname resolved afresh for each connection attempt.
    Dns {
        /// Fixed-capacity UTF-8 hostname storage.
        bytes: [u8; MAX_TCP_PEER_HOSTNAME_BYTES],
        /// Number of initialized hostname bytes.
        length: u8,
    },
}

impl WifiTcpPeerAddress {
    /// Construct a validated exact IPv4 endpoint.
    pub const fn ipv4(value: [u8; 4]) -> Result<Self, WifiTcpBootstrapError> {
        if !is_unicast_ipv4(value) {
            Err(WifiTcpBootstrapError::NonUnicastIpv4)
        } else {
            Ok(Self::Ipv4(value))
        }
    }

    /// Copy one validated durable DNS hostname into the boot plan.
    pub fn dns(value: &[u8]) -> Result<Self, WifiTcpBootstrapError> {
        if value.is_empty()
            || value.len() > MAX_TCP_PEER_HOSTNAME_BYTES
            || core::str::from_utf8(value).is_err()
        {
            return Err(WifiTcpBootstrapError::InvalidDnsHostname);
        }
        let mut bytes = [0_u8; MAX_TCP_PEER_HOSTNAME_BYTES];
        bytes[..value.len()].copy_from_slice(value);
        Ok(Self::Dns {
            bytes,
            length: value.len() as u8,
        })
    }

    /// Exact DNS hostname for a DNS endpoint.
    pub fn dns_hostname(&self) -> Option<&str> {
        let Self::Dns { bytes, length } = self else {
            return None;
        };
        Some(
            core::str::from_utf8(&bytes[..usize::from(*length)])
                .expect("the boot constructor accepted only UTF-8 DNS bytes"),
        )
    }
}

/// One enabled immutable outbound peer copied from durable boot state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WifiTcpBootstrap {
    applied_revision: u64,
    address: WifiTcpPeerAddress,
    port: u16,
}

impl WifiTcpBootstrap {
    /// Validate one configured endpoint for this boot.
    pub const fn new(
        applied_revision: u64,
        ipv4: [u8; 4],
        port: u16,
    ) -> Result<Self, WifiTcpBootstrapError> {
        Self::with_address(
            applied_revision,
            match WifiTcpPeerAddress::ipv4(ipv4) {
                Ok(address) => address,
                Err(error) => return Err(error),
            },
            port,
        )
    }

    /// Validate one configured DNS endpoint for this boot.
    pub fn with_dns(
        applied_revision: u64,
        hostname: &[u8],
        port: u16,
    ) -> Result<Self, WifiTcpBootstrapError> {
        Self::with_address(applied_revision, WifiTcpPeerAddress::dns(hostname)?, port)
    }

    /// Validate one address-kind-neutral endpoint for this boot.
    pub const fn with_address(
        applied_revision: u64,
        address: WifiTcpPeerAddress,
        port: u16,
    ) -> Result<Self, WifiTcpBootstrapError> {
        if port == 0 {
            return Err(WifiTcpBootstrapError::ZeroPort);
        }
        Ok(Self {
            applied_revision,
            address,
            port,
        })
    }

    /// Durable configuration generation applied by the running actor.
    pub const fn applied_revision(self) -> u64 {
        self.applied_revision
    }

    /// Configured remote address.
    pub const fn address(self) -> WifiTcpPeerAddress {
        self.address
    }

    /// Configured nonzero remote TCP port.
    pub const fn port(self) -> u16 {
        self.port
    }
}

const fn is_unicast_ipv4(ipv4: [u8; 4]) -> bool {
    ipv4[0] != 0
        && ipv4[0] != 127
        && ipv4[0] < 224
        && !(ipv4[0] == 255 && ipv4[1] == 255 && ipv4[2] == 255 && ipv4[3] == 255)
}

/// Whether the station can keep its TCP packet interface router-eligible.
///
/// A retained DHCP configuration is insufficient after physical link loss.
/// Requiring both signals keeps one unavailable IP bearer from capturing work
/// that another currently usable interface can carry.
pub const fn tcp_network_ready(link_up: bool, config_up: bool) -> bool {
    link_up && config_up
}

/// Volatile lifecycle of the configured outbound TCP packet interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WifiTcpPhase {
    /// No enabled, valid durable peer was selected.
    Disabled,
    /// The peer exists, but the station has no usable IPv4 configuration.
    WaitingForNetwork,
    /// A bounded outbound TCP connection attempt is active.
    Connecting,
    /// The socket is established and interface 2 is Ready in the fabric.
    Connected,
    /// A bounded delay precedes the next connection attempt.
    Backoff,
    /// An actor/fabric invariant failed closed for this boot.
    Faulted,
}

/// Most recent recoverable failure observed by the outbound TCP actor.
///
/// This intentionally excludes secrets and remote packet data. It is retained
/// across a reconnect attempt so management clients can distinguish an active
/// attempt from the failure that caused the preceding backoff.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WifiTcpFailure {
    /// The DNS query exceeded its bounded deadline.
    DnsTimeout,
    /// The configured resolver rejected or could not complete the query.
    DnsLookupFailed,
    /// DNS completed without a usable IPv4 address.
    DnsNoIpv4Result,
    /// Embassy rejected the socket connect operation in its current state.
    ConnectInvalidState,
    /// The remote side reset the connection attempt.
    ConnectReset,
    /// The TCP stack or the outer operation deadline expired.
    ConnectTimeout,
    /// The stack had no route to the resolved peer.
    ConnectNoRoute,
    /// An established socket was closed or its receive path failed.
    SocketClosed,
    /// An established socket could not transmit a complete Reticulum frame.
    TransmitFailed,
}

/// Secret-free latest-value TCP runtime projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WifiTcpStatus {
    /// Durable configuration generation applied by the running actor.
    pub applied_revision: u64,
    /// Current actor lifecycle.
    pub phase: WifiTcpPhase,
    /// Most recent recoverable DNS, connect, or stream failure.
    pub last_failure: Option<WifiTcpFailure>,
    /// Bounded diagnostics for the latest hostname-resolution attempt.
    pub dns_diagnostics: Option<ReticulumDnsDiagnostics>,
}

impl WifiTcpStatus {
    /// Status when no enabled valid peer is applied.
    pub const DISABLED: Self = Self {
        applied_revision: 0,
        phase: WifiTcpPhase::Disabled,
        last_failure: None,
        dns_diagnostics: None,
    };

    /// Construct one boot-bound status.
    pub const fn for_bootstrap(bootstrap: WifiTcpBootstrap, phase: WifiTcpPhase) -> Self {
        Self {
            applied_revision: bootstrap.applied_revision,
            phase,
            last_failure: None,
            dns_diagnostics: None,
        }
    }

    /// Construct one boot-bound status retaining a recoverable failure.
    pub const fn for_bootstrap_failure(
        bootstrap: WifiTcpBootstrap,
        phase: WifiTcpPhase,
        last_failure: WifiTcpFailure,
    ) -> Self {
        Self {
            applied_revision: bootstrap.applied_revision,
            phase,
            last_failure: Some(last_failure),
            dns_diagnostics: None,
        }
    }

    /// Construct one boot-bound status while retaining live DNS diagnostics.
    pub const fn with_runtime_diagnostics(
        bootstrap: WifiTcpBootstrap,
        phase: WifiTcpPhase,
        last_failure: Option<WifiTcpFailure>,
        dns_diagnostics: Option<ReticulumDnsDiagnostics>,
    ) -> Self {
        Self {
            applied_revision: bootstrap.applied_revision,
            phase,
            last_failure,
            dns_diagnostics,
        }
    }
}

/// Blocking latest-value cell shared by the TCP actor and management owner.
pub struct WifiTcpStatusCell {
    state: Mutex<CriticalSectionRawMutex, RefCell<WifiTcpStatus>>,
}

impl WifiTcpStatusCell {
    /// Construct a disabled status cell.
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(RefCell::new(WifiTcpStatus::DISABLED)),
        }
    }

    /// Replace the complete secret-free projection.
    pub fn publish(&self, status: WifiTcpStatus) {
        self.state.lock(|state| *state.borrow_mut() = status);
    }

    /// Copy the latest complete projection.
    pub fn snapshot(&self) -> WifiTcpStatus {
        self.state.lock(|state| *state.borrow())
    }
}

impl Default for WifiTcpStatusCell {
    fn default() -> Self {
        Self::new()
    }
}

/// Worst-case HDLC stream bytes needed for one native packet.
pub const fn maximum_hdlc_encoded_len(packet_len: u16) -> u64 {
    (packet_len as u64).saturating_mul(2).saturating_add(2)
}

/// Exact authorization requirement generated by the TCP actor.
pub fn tcp_stream_requirements(packet_len: u16) -> TxPermitRequirements {
    TxPermitRequirements::try_new(TCP_STREAM_RESOURCE, maximum_hdlc_encoded_len(packet_len))
        .expect("a routed nonempty native packet always requires nonzero stream credit")
}

/// Product policy that retains exact LoRa authorization and adds interface 2.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoRaAndTcpAuthorizationPolicy {
    lora: ExactLoRaAirtimePolicy,
}

impl LoRaAndTcpAuthorizationPolicy {
    /// Extend the exact immutable LoRa policy with bounded TCP stream credit.
    pub const fn new(lora: ExactLoRaAirtimePolicy) -> Self {
        Self { lora }
    }
}

impl TxAuthorizationPolicy for LoRaAndTcpAuthorizationPolicy {
    fn authorize(&mut self, candidate: TxAuthorizationCandidate) -> TxPolicyDecision {
        if candidate.interface == self.lora.interface() {
            return self.lora.authorize(candidate);
        }
        if candidate.interface != TCP_INTERFACE {
            return TxPolicyDecision::Deny(TxPolicyDenial::PolicyDenied);
        }
        let required = maximum_hdlc_encoded_len(candidate.packet_len);
        if candidate.requirements.resource() != TCP_STREAM_RESOURCE
            || candidate.requirements.required_units() != required
        {
            return TxPolicyDecision::Deny(TxPolicyDenial::ResourceUnavailable);
        }
        match TxPermitReservation::try_new(TCP_STREAM_RESOURCE, required) {
            Ok(reservation) => TxPolicyDecision::Authorize(reservation),
            Err(_) => TxPolicyDecision::Deny(TxPolicyDenial::CapacityUnavailable),
        }
    }
}

#[cfg(test)]
mod tests {
    use rete_core::{
        MTU,
        hdlc::{self, ESC, FLAG, HdlcDecoder, MAX_ENCODED},
    };
    use reticulum_device_api::{
        MAX_RETICULUM_DNS_DHCP_SERVERS, MAX_RETICULUM_DNS_RAW_ATTEMPTS, ReticulumDnsDiagnostics,
        ReticulumDnsPrimaryOutcome, ReticulumDnsRawAttempt, ReticulumDnsRawOutcome,
        ReticulumDnsRawSetupState, ReticulumDnsRawSource,
    };
    use reticulum_node_core::{
        MonotonicMillis, TxAuthorizationCandidate, TxAuthorizationPolicy, TxLeaseDeadline,
        TxPermitRequirements, TxPolicyDecision, TxPolicyDenial,
    };
    use reticulum_radio_tx_dispatch::ExactLoRaAirtimePolicy;

    use super::{
        LoRaAndTcpAuthorizationPolicy, TCP_INTERFACE, TCP_STREAM_RESOURCE, WifiTcpBootstrap,
        WifiTcpBootstrapError, WifiTcpFailure, WifiTcpPeerAddress, WifiTcpPhase, WifiTcpStatus,
        WifiTcpStatusCell, maximum_hdlc_encoded_len, tcp_network_ready, tcp_stream_requirements,
    };

    fn policy() -> LoRaAndTcpAuthorizationPolicy {
        LoRaAndTcpAuthorizationPolicy::new(ExactLoRaAirtimePolicy::new(
            crate::live_admission_test_support::LORA_INTERFACE,
            reticulum_board_heltec_vision_master_e290_radio::E290_NA915_DEV_PROFILE,
            reticulum_board_heltec_vision_master_e290_radio::E290_NA915_DEV_CONFIGURATION_FINGERPRINT,
        ))
    }

    fn candidate(requirements: TxPermitRequirements) -> TxAuthorizationCandidate {
        TxAuthorizationCandidate {
            interface: TCP_INTERFACE,
            packet_len: 500,
            requirements,
            now: MonotonicMillis::new(10),
            deadline: TxLeaseDeadline::new(MonotonicMillis::new(20)),
            may_have_transmitted: false,
        }
    }

    #[test]
    fn bootstrap_rejects_non_peer_endpoints() {
        assert_eq!(
            WifiTcpBootstrap::new(3, [192, 0, 2, 1], 0),
            Err(WifiTcpBootstrapError::ZeroPort)
        );
        assert_eq!(
            WifiTcpBootstrap::new(3, [224, 0, 0, 1], 4242),
            Err(WifiTcpBootstrapError::NonUnicastIpv4)
        );
        let peer = WifiTcpBootstrap::new(3, [192, 0, 2, 1], 4242).unwrap();
        assert_eq!(peer.applied_revision(), 3);
        assert_eq!(peer.address(), WifiTcpPeerAddress::Ipv4([192, 0, 2, 1]));
        assert_eq!(peer.port(), 4242);
    }

    #[test]
    fn router_eligibility_requires_both_physical_link_and_ip_configuration() {
        assert!(tcp_network_ready(true, true));
        assert!(!tcp_network_ready(false, true));
        assert!(!tcp_network_ready(true, false));
        assert!(!tcp_network_ready(false, false));
    }

    #[test]
    fn bootstrap_retains_dns_hostname_without_resolving_it_at_boot() {
        let peer = WifiTcpBootstrap::with_dns(4, b"rmap.world", 4242).unwrap();
        assert_eq!(peer.address().dns_hostname(), Some("rmap.world"));
        assert_eq!(peer.port(), 4242);
    }

    #[test]
    fn status_cell_tracks_applied_revision_and_lifecycle() {
        let cell = WifiTcpStatusCell::new();
        assert_eq!(cell.snapshot(), WifiTcpStatus::DISABLED);
        let bootstrap = WifiTcpBootstrap::new(9, [198, 51, 100, 7], 4242).unwrap();
        let status = WifiTcpStatus::for_bootstrap(bootstrap, WifiTcpPhase::Connected);
        cell.publish(status);
        assert_eq!(cell.snapshot(), status);
    }

    #[test]
    fn status_can_retain_a_recoverable_failure_during_backoff() {
        let bootstrap = WifiTcpBootstrap::with_dns(11, b"rmap.world", 4242).unwrap();
        let status = WifiTcpStatus::for_bootstrap_failure(
            bootstrap,
            WifiTcpPhase::Backoff,
            WifiTcpFailure::DnsLookupFailed,
        );
        assert_eq!(status.applied_revision, 11);
        assert_eq!(status.phase, WifiTcpPhase::Backoff);
        assert_eq!(status.last_failure, Some(WifiTcpFailure::DnsLookupFailed));
        assert_eq!(status.dns_diagnostics, None);
    }

    #[test]
    fn status_cell_retains_bounded_dns_progress_without_host_or_secret_data() {
        let bootstrap = WifiTcpBootstrap::with_dns(12, b"rmap.world", 4242).unwrap();
        let mut dhcp_servers = [None; MAX_RETICULUM_DNS_DHCP_SERVERS];
        dhcp_servers[0] = Some([192, 168, 50, 1]);
        let mut raw_attempts = [None; MAX_RETICULUM_DNS_RAW_ATTEMPTS];
        raw_attempts[0] = Some(ReticulumDnsRawAttempt::new(
            ReticulumDnsRawSource::Dhcp,
            [192, 168, 50, 1],
            ReticulumDnsRawOutcome::AwaitingResponse,
        ));
        let diagnostics = ReticulumDnsDiagnostics::new(
            Some([192, 168, 50, 1]),
            dhcp_servers,
            ReticulumDnsPrimaryOutcome::LookupFailed,
            ReticulumDnsRawSetupState::Ready,
            raw_attempts,
            None,
        );
        let cell = WifiTcpStatusCell::new();
        cell.publish(WifiTcpStatus::with_runtime_diagnostics(
            bootstrap,
            WifiTcpPhase::Connecting,
            Some(WifiTcpFailure::DnsLookupFailed),
            Some(diagnostics),
        ));

        let snapshot = cell.snapshot();
        assert_eq!(snapshot.dns_diagnostics, Some(diagnostics));
        assert_eq!(snapshot.last_failure, Some(WifiTcpFailure::DnsLookupFailed));
    }

    #[test]
    fn policy_requires_exact_worst_case_stream_credit() {
        assert_eq!(maximum_hdlc_encoded_len(500), 1_002);
        let mut policy = policy();
        let decision = policy.authorize(candidate(tcp_stream_requirements(500)));
        let TxPolicyDecision::Authorize(reservation) = decision else {
            panic!("exact TCP stream requirement must authorize");
        };
        assert_eq!(reservation.resource(), TCP_STREAM_RESOURCE);
        assert_eq!(reservation.reserved_units(), 1_002);

        let wrong = TxPermitRequirements::try_new(TCP_STREAM_RESOURCE, 500).unwrap();
        assert_eq!(
            policy.authorize(candidate(wrong)),
            TxPolicyDecision::Deny(TxPolicyDenial::ResourceUnavailable)
        );
    }

    #[test]
    fn rete_hdlc_round_trips_fragmented_worst_case_native_packet() {
        let native = [FLAG; MTU];
        let mut encoded = [0_u8; MAX_ENCODED];
        let encoded_length = hdlc::encode(&native, &mut encoded).unwrap();
        assert_eq!(encoded_length, MAX_ENCODED);
        assert_eq!(encoded[0], FLAG);
        assert_eq!(encoded[1], ESC);

        let mut decoder = HdlcDecoder::<MTU>::new();
        let mut completed = 0;
        for chunk in encoded[..encoded_length].chunks(37) {
            for byte in chunk {
                if decoder.feed(*byte) {
                    completed += 1;
                    assert_eq!(decoder.frame(), Some(native.as_slice()));
                }
            }
        }
        assert_eq!(completed, 1);
    }
}
