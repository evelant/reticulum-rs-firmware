//! App-facing network configuration requests and secret-free projections.

use std::fmt;
use std::net::Ipv4Addr;

use reticulum_device_api as device_api;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use zeroize::Zeroize;

use crate::{
    BytesEncoding, BytesView, JsonSafeInteger, deserialize_json_safe_u64, serialize_json_safe_u64,
    serialize_optional_json_safe_u64,
};

/// One saved WPA2-Personal network without its credential bytes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct WifiNetworkProfileView {
    profile_id: String,
    enabled: bool,
    priority: u8,
    ssid: BytesView,
    credential_configured: bool,
}

impl WifiNetworkProfileView {
    /// Stable opaque profile identity encoded as lowercase hexadecimal.
    pub fn profile_id(&self) -> &str {
        &self.profile_id
    }

    /// Whether the station selector may use this profile.
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// Selection priority; larger values are preferred.
    pub const fn priority(&self) -> u8 {
        self.priority
    }

    /// Exact SSID bytes represented as UTF-8 when valid and hexadecimal otherwise.
    pub const fn ssid(&self) -> &BytesView {
        &self.ssid
    }

    /// Whether the board has a stored passphrase for this profile.
    pub const fn credential_configured(&self) -> bool {
        self.credential_configured
    }
}

impl From<device_api::WifiNetworkConfigSummary> for WifiNetworkProfileView {
    fn from(profile: device_api::WifiNetworkConfigSummary) -> Self {
        Self {
            profile_id: hex::encode(profile.profile_id().as_bytes()),
            enabled: profile.enabled(),
            priority: profile.priority(),
            ssid: BytesView::new(profile.ssid().as_bytes()),
            credential_configured: profile.credential_configured(),
        }
    }
}

/// One configured outbound Reticulum TCP peer.
///
/// A peer may use a literal IPv4 address or a hostname that is resolved again
/// on every reconnect.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(untagged)]
pub enum ReticulumTcpPeerView {
    /// Exact literal IPv4 peer.
    Ipv4 {
        /// Whether the board should connect to this peer.
        enabled: bool,
        /// Exact dotted-decimal IPv4 address.
        ipv4_address: String,
        /// Configured TCP port.
        port: u16,
    },
    /// DNS hostname peer resolved by the board on every reconnect.
    Hostname {
        /// Whether the board should connect to this peer.
        enabled: bool,
        /// Validated ASCII DNS hostname.
        hostname: String,
        /// Configured TCP port.
        port: u16,
    },
}

impl ReticulumTcpPeerView {
    /// Whether the board should connect to this peer.
    pub const fn enabled(&self) -> bool {
        match self {
            Self::Ipv4 { enabled, .. } | Self::Hostname { enabled, .. } => *enabled,
        }
    }

    /// Exact dotted-decimal IPv4 address, when this peer uses an IPv4 literal.
    pub fn ipv4_address(&self) -> Option<&str> {
        match self {
            Self::Ipv4 { ipv4_address, .. } => Some(ipv4_address),
            Self::Hostname { .. } => None,
        }
    }

    /// DNS hostname, when this peer is hostname-based.
    pub fn hostname(&self) -> Option<&str> {
        match self {
            Self::Ipv4 { .. } => None,
            Self::Hostname { hostname, .. } => Some(hostname),
        }
    }

    /// Configured TCP port.
    pub const fn port(&self) -> u16 {
        match self {
            Self::Ipv4 { port, .. } | Self::Hostname { port, .. } => *port,
        }
    }
}

impl From<device_api::ReticulumTcpPeerConfigSummary> for ReticulumTcpPeerView {
    fn from(peer: device_api::ReticulumTcpPeerConfigSummary) -> Self {
        Self::Ipv4 {
            enabled: peer.enabled(),
            ipv4_address: Ipv4Addr::from(peer.ipv4_address().octets()).to_string(),
            port: peer.port(),
        }
    }
}

impl From<device_api::ReticulumTcpPeerHostConfigSummary> for ReticulumTcpPeerView {
    fn from(peer: device_api::ReticulumTcpPeerHostConfigSummary) -> Self {
        Self::Hostname {
            enabled: peer.enabled(),
            hostname: peer.hostname().as_str().to_owned(),
            port: peer.port(),
        }
    }
}

/// Phone-sourced RMAP position in signed integer microdegrees.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct RmapPhoneLocation {
    latitude_e6: i32,
    longitude_e6: i32,
}

impl RmapPhoneLocation {
    /// Signed latitude in decimal degrees multiplied by one million.
    pub const fn latitude_e6(self) -> i32 {
        self.latitude_e6
    }

    /// Signed longitude in decimal degrees multiplied by one million.
    pub const fn longitude_e6(self) -> i32 {
        self.longitude_e6
    }

    fn into_device(self) -> Result<device_api::RmapLocation, NetworkRequestError> {
        device_api::RmapLocation::new(self.latitude_e6, self.longitude_e6)
            .map_err(|_| NetworkRequestError::InvalidRmapLocation)
    }
}

impl From<device_api::RmapLocation> for RmapPhoneLocation {
    fn from(location: device_api::RmapLocation) -> Self {
        Self {
            latitude_e6: location.latitude_e6(),
            longitude_e6: location.longitude_e6(),
        }
    }
}

/// Complete desired LoRa compatibility profile.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(deny_unknown_fields)]
pub struct LoraRadioProfileView {
    frequency_hz: u32,
    bandwidth_hz: u32,
    spreading_factor: u8,
    coding_rate_denominator: u8,
    #[ts(type = "14 | 17 | 20 | 22")]
    tx_power_dbm: u8,
}

impl LoraRadioProfileView {
    fn into_device(self) -> Result<device_api::LoraRadioProfile, NetworkRequestError> {
        let power = device_api::LoraTransmitPowerDbm::new(self.tx_power_dbm)
            .map_err(|_| NetworkRequestError::InvalidLoraTransmitPower)?;
        device_api::LoraRadioProfile::new(
            self.frequency_hz,
            self.bandwidth_hz,
            self.spreading_factor,
            self.coding_rate_denominator,
            power,
        )
        .map_err(|_| NetworkRequestError::InvalidLoraRadioProfile)
    }
}

impl From<device_api::LoraRadioProfile> for LoraRadioProfileView {
    fn from(profile: device_api::LoraRadioProfile) -> Self {
        Self {
            frequency_hz: profile.frequency_hz(),
            bandwidth_hz: profile.bandwidth_hz(),
            spreading_factor: profile.spreading_factor(),
            coding_rate_denominator: profile.coding_rate_denominator(),
            tx_power_dbm: profile.tx_power_dbm().get(),
        }
    }
}

/// Complete board-owned desired network configuration with all secrets redacted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct NetworkConfigView {
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    revision: u64,
    wifi_profiles: Vec<WifiNetworkProfileView>,
    tcp_peer: Option<ReticulumTcpPeerView>,
    wifi_transport_enabled: bool,
    automatic_announces_enabled: bool,
    rmap_discovery_enabled: bool,
    rmap_share_location: bool,
    rmap_phone_location: Option<RmapPhoneLocation>,
    #[ts(type = "14 | 17 | 20 | 22")]
    lora_tx_power_dbm: u8,
    lora_profile: LoraRadioProfileView,
}

impl NetworkConfigView {
    /// Monotonic committed configuration revision.
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Saved Wi-Fi profiles in board-defined stable order.
    pub fn wifi_profiles(&self) -> &[WifiNetworkProfileView] {
        &self.wifi_profiles
    }

    /// Desired outbound TCP peer, when configured.
    pub const fn tcp_peer(&self) -> Option<&ReticulumTcpPeerView> {
        self.tcp_peer.as_ref()
    }

    /// Whether the board may run its Wi-Fi station and outbound TCP transport.
    pub const fn wifi_transport_enabled(&self) -> bool {
        self.wifi_transport_enabled
    }

    /// Whether scheduled ordinary service announces are enabled.
    pub const fn automatic_announces_enabled(&self) -> bool {
        self.automatic_announces_enabled
    }

    /// Whether signed RMAP interface discovery publication is enabled.
    pub const fn rmap_discovery_enabled(&self) -> bool {
        self.rmap_discovery_enabled
    }

    /// Whether a saved phone position may be included in RMAP publication.
    pub const fn rmap_share_location(&self) -> bool {
        self.rmap_share_location
    }

    /// Latest optional phone-sourced RMAP position.
    pub const fn rmap_phone_location(&self) -> Option<RmapPhoneLocation> {
        self.rmap_phone_location
    }

    /// Requested LoRa radio output in whole dBm.
    ///
    /// This is a requested radio output, not measured conducted power or EIRP.
    pub const fn lora_tx_power_dbm(&self) -> u8 {
        self.lora_tx_power_dbm
    }

    /// Complete profile saved for the next radio start.
    pub const fn lora_profile(&self) -> LoraRadioProfileView {
        self.lora_profile
    }
}

impl From<device_api::NetworkConfigSnapshot> for NetworkConfigView {
    fn from(config: device_api::NetworkConfigSnapshot) -> Self {
        let tcp_peer = config
            .tcp_peer()
            .map(ReticulumTcpPeerView::from)
            .or_else(|| config.tcp_host_peer().map(ReticulumTcpPeerView::from));
        Self {
            revision: config.revision,
            wifi_profiles: config
                .wifi_profiles()
                .iter()
                .flatten()
                .copied()
                .map(WifiNetworkProfileView::from)
                .collect(),
            tcp_peer,
            wifi_transport_enabled: config.wifi_transport_enabled(),
            automatic_announces_enabled: config.automatic_announces_enabled(),
            rmap_discovery_enabled: config.rmap_discovery_enabled(),
            rmap_share_location: config.rmap_share_location(),
            rmap_phone_location: config.rmap_phone_location().map(RmapPhoneLocation::from),
            lora_tx_power_dbm: config.lora_tx_power_dbm().get(),
            lora_profile: LoraRadioProfileView::from(config.lora_profile()),
        }
    }
}

/// Live Wi-Fi station state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum WifiStationStateView {
    /// No enabled Wi-Fi profile exists.
    Disabled,
    /// The station is enabled but not associated.
    Disconnected,
    /// Association or DHCP is in progress.
    Connecting,
    /// Association and DHCP completed.
    Connected,
}

impl From<device_api::WifiStationState> for WifiStationStateView {
    fn from(state: device_api::WifiStationState) -> Self {
        match state {
            device_api::WifiStationState::Disabled => Self::Disabled,
            device_api::WifiStationState::Disconnected => Self::Disconnected,
            device_api::WifiStationState::Connecting => Self::Connecting,
            device_api::WifiStationState::Connected => Self::Connected,
        }
    }
}

/// Live outbound Reticulum TCP interface state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReticulumTcpPeerStateView {
    /// No enabled peer exists.
    Disabled,
    /// A peer exists but the Wi-Fi network is not ready.
    WaitingForNetwork,
    /// A bounded TCP connection attempt is active.
    Connecting,
    /// A recoverable failure occurred and a bounded retry delay is active.
    Backoff,
    /// The Reticulum TCP interface is connected and ready.
    Connected,
    /// The configured actor failed a local ownership or fabric invariant.
    Faulted,
}

impl From<device_api::ReticulumTcpPeerState> for ReticulumTcpPeerStateView {
    fn from(state: device_api::ReticulumTcpPeerState) -> Self {
        match state {
            device_api::ReticulumTcpPeerState::Disabled => Self::Disabled,
            device_api::ReticulumTcpPeerState::WaitingForNetwork => Self::WaitingForNetwork,
            device_api::ReticulumTcpPeerState::Connecting => Self::Connecting,
            device_api::ReticulumTcpPeerState::Backoff => Self::Backoff,
            device_api::ReticulumTcpPeerState::Connected => Self::Connected,
            device_api::ReticulumTcpPeerState::Faulted => Self::Faulted,
        }
    }
}

/// Most recent recoverable outbound Reticulum TCP failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReticulumTcpFailureView {
    /// The DNS query exceeded its bounded deadline.
    DnsTimeout,
    /// The configured resolver rejected or could not complete the query.
    DnsLookupFailed,
    /// DNS completed without a usable IPv4 address.
    DnsNoIpv4Result,
    /// The socket stack rejected connect in its current state.
    ConnectInvalidState,
    /// The remote peer reset the connection attempt.
    ConnectReset,
    /// The TCP stack or outer connection deadline expired.
    ConnectTimeout,
    /// The stack had no route to the resolved peer.
    ConnectNoRoute,
    /// An established socket closed or its receive path failed.
    SocketClosed,
    /// An established socket could not transmit a complete frame.
    TransmitFailed,
}

impl From<device_api::ReticulumTcpFailure> for ReticulumTcpFailureView {
    fn from(failure: device_api::ReticulumTcpFailure) -> Self {
        match failure {
            device_api::ReticulumTcpFailure::DnsTimeout => Self::DnsTimeout,
            device_api::ReticulumTcpFailure::DnsLookupFailed => Self::DnsLookupFailed,
            device_api::ReticulumTcpFailure::DnsNoIpv4Result => Self::DnsNoIpv4Result,
            device_api::ReticulumTcpFailure::ConnectInvalidState => Self::ConnectInvalidState,
            device_api::ReticulumTcpFailure::ConnectReset => Self::ConnectReset,
            device_api::ReticulumTcpFailure::ConnectTimeout => Self::ConnectTimeout,
            device_api::ReticulumTcpFailure::ConnectNoRoute => Self::ConnectNoRoute,
            device_api::ReticulumTcpFailure::SocketClosed => Self::SocketClosed,
            device_api::ReticulumTcpFailure::TransmitFailed => Self::TransmitFailed,
        }
    }
}

/// Outcome of the network stack's built-in DNS path.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReticulumDnsPrimaryOutcomeView {
    /// No system DNS query has started.
    NotStarted,
    /// The system DNS query is awaiting a terminal result.
    Resolving,
    /// System DNS returned a usable IPv4 address.
    Resolved,
    /// DHCP supplied no DNS resolver addresses.
    NoServers,
    /// The bounded system DNS deadline expired.
    Timeout,
    /// System DNS returned a resolver or protocol failure.
    LookupFailed,
    /// System DNS returned no usable IPv4 address.
    NoIpv4Result,
}

impl From<device_api::ReticulumDnsPrimaryOutcome> for ReticulumDnsPrimaryOutcomeView {
    fn from(outcome: device_api::ReticulumDnsPrimaryOutcome) -> Self {
        match outcome {
            device_api::ReticulumDnsPrimaryOutcome::NotStarted => Self::NotStarted,
            device_api::ReticulumDnsPrimaryOutcome::Resolving => Self::Resolving,
            device_api::ReticulumDnsPrimaryOutcome::Resolved => Self::Resolved,
            device_api::ReticulumDnsPrimaryOutcome::NoServers => Self::NoServers,
            device_api::ReticulumDnsPrimaryOutcome::Timeout => Self::Timeout,
            device_api::ReticulumDnsPrimaryOutcome::LookupFailed => Self::LookupFailed,
            device_api::ReticulumDnsPrimaryOutcome::NoIpv4Result => Self::NoIpv4Result,
        }
    }
}

/// Lifecycle of the raw UDP DNS fallback socket.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReticulumDnsRawSetupStateView {
    /// No raw DNS path has started.
    NotStarted,
    /// The actor is binding its bounded UDP socket.
    Binding,
    /// The raw UDP socket is ready.
    Ready,
    /// The raw UDP socket could not bind.
    BindFailed,
    /// The hostname could not be encoded as an A query.
    EncodeFailed,
}

impl From<device_api::ReticulumDnsRawSetupState> for ReticulumDnsRawSetupStateView {
    fn from(state: device_api::ReticulumDnsRawSetupState) -> Self {
        match state {
            device_api::ReticulumDnsRawSetupState::NotStarted => Self::NotStarted,
            device_api::ReticulumDnsRawSetupState::Binding => Self::Binding,
            device_api::ReticulumDnsRawSetupState::Ready => Self::Ready,
            device_api::ReticulumDnsRawSetupState::BindFailed => Self::BindFailed,
            device_api::ReticulumDnsRawSetupState::EncodeFailed => Self::EncodeFailed,
        }
    }
}

/// Policy source of one raw UDP DNS resolver.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReticulumDnsRawSourceView {
    /// The active DHCP lease supplied the resolver.
    Dhcp,
    /// Product policy supplied the public resolver.
    Public,
}

impl From<device_api::ReticulumDnsRawSource> for ReticulumDnsRawSourceView {
    fn from(source: device_api::ReticulumDnsRawSource) -> Self {
        match source {
            device_api::ReticulumDnsRawSource::Dhcp => Self::Dhcp,
            device_api::ReticulumDnsRawSource::Public => Self::Public,
        }
    }
}

/// Latest outcome of one raw UDP DNS attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReticulumDnsRawOutcomeView {
    /// This configured resolver has not started.
    NotStarted,
    /// An identical resolver was already attempted.
    SkippedDuplicate,
    /// Public DNS was suppressed for a local or private hostname.
    SkippedLocalName,
    /// The query is being queued to the UDP socket.
    Sending,
    /// The query was sent and is awaiting a response.
    AwaitingResponse,
    /// The resolver returned a usable IPv4 address.
    Resolved,
    /// The UDP socket could not send the query.
    SendFailed,
    /// The bounded response deadline expired.
    Timeout,
    /// The packet was not a standard DNS response.
    NotAResponse,
    /// The UDP response was marked truncated.
    Truncated,
    /// The resolver returned a nonzero DNS response code.
    ResponseCode {
        /// Exact nonzero DNS response code.
        code: u8,
    },
    /// The response echoed a different DNS question.
    QuestionMismatch,
    /// The response was structurally malformed or incomplete.
    Malformed,
    /// The response contained no usable IPv4 answer.
    NoIpv4Result,
}

impl From<device_api::ReticulumDnsRawOutcome> for ReticulumDnsRawOutcomeView {
    fn from(outcome: device_api::ReticulumDnsRawOutcome) -> Self {
        match outcome {
            device_api::ReticulumDnsRawOutcome::NotStarted => Self::NotStarted,
            device_api::ReticulumDnsRawOutcome::SkippedDuplicate => Self::SkippedDuplicate,
            device_api::ReticulumDnsRawOutcome::SkippedLocalName => Self::SkippedLocalName,
            device_api::ReticulumDnsRawOutcome::Sending => Self::Sending,
            device_api::ReticulumDnsRawOutcome::AwaitingResponse => Self::AwaitingResponse,
            device_api::ReticulumDnsRawOutcome::Resolved => Self::Resolved,
            device_api::ReticulumDnsRawOutcome::SendFailed => Self::SendFailed,
            device_api::ReticulumDnsRawOutcome::Timeout => Self::Timeout,
            device_api::ReticulumDnsRawOutcome::NotAResponse => Self::NotAResponse,
            device_api::ReticulumDnsRawOutcome::Truncated => Self::Truncated,
            device_api::ReticulumDnsRawOutcome::ResponseCode(code) => {
                Self::ResponseCode { code: code.get() }
            }
            device_api::ReticulumDnsRawOutcome::QuestionMismatch => Self::QuestionMismatch,
            device_api::ReticulumDnsRawOutcome::Malformed => Self::Malformed,
            device_api::ReticulumDnsRawOutcome::NoIpv4Result => Self::NoIpv4Result,
        }
    }
}

/// Resolver-specific raw UDP DNS attempt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct ReticulumDnsRawAttemptView {
    source: ReticulumDnsRawSourceView,
    server: String,
    outcome: ReticulumDnsRawOutcomeView,
}

impl From<device_api::ReticulumDnsRawAttempt> for ReticulumDnsRawAttemptView {
    fn from(attempt: device_api::ReticulumDnsRawAttempt) -> Self {
        Self {
            source: attempt.source.into(),
            server: Ipv4Addr::from(attempt.server).to_string(),
            outcome: attempt.outcome.into(),
        }
    }
}

/// DNS path that produced the selected TCP peer address.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReticulumDnsResolutionSourceView {
    /// The network stack's built-in resolver produced the address.
    SystemDns,
    /// Raw DNS through a DHCP-provided resolver produced the address.
    RawDhcp,
    /// Raw DNS through a product-selected public resolver produced the address.
    RawPublic,
}

impl From<device_api::ReticulumDnsResolutionSource> for ReticulumDnsResolutionSourceView {
    fn from(source: device_api::ReticulumDnsResolutionSource) -> Self {
        match source {
            device_api::ReticulumDnsResolutionSource::SystemDns => Self::SystemDns,
            device_api::ReticulumDnsResolutionSource::RawDhcp => Self::RawDhcp,
            device_api::ReticulumDnsResolutionSource::RawPublic => Self::RawPublic,
        }
    }
}

/// Successful DNS resolution retained for diagnosis.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct ReticulumDnsResolutionView {
    address: String,
    source: ReticulumDnsResolutionSourceView,
    resolver: Option<String>,
}

impl From<device_api::ReticulumDnsResolution> for ReticulumDnsResolutionView {
    fn from(resolution: device_api::ReticulumDnsResolution) -> Self {
        Self {
            address: Ipv4Addr::from(resolution.address).to_string(),
            source: resolution.source.into(),
            resolver: resolution
                .resolver
                .map(|resolver| Ipv4Addr::from(resolver).to_string()),
        }
    }
}

/// Bounded, secret-free diagnostics for one hostname-resolution attempt.
///
/// Null array entries preserve the board's fixed incremental slots. App
/// surfaces may filter those entries while retaining every populated slot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct ReticulumDnsDiagnosticsView {
    gateway_ipv4: Option<String>,
    dhcp_servers: Vec<Option<String>>,
    primary_outcome: ReticulumDnsPrimaryOutcomeView,
    raw_setup_state: ReticulumDnsRawSetupStateView,
    raw_attempts: Vec<Option<ReticulumDnsRawAttemptView>>,
    resolution: Option<ReticulumDnsResolutionView>,
}

impl From<device_api::ReticulumDnsDiagnostics> for ReticulumDnsDiagnosticsView {
    fn from(diagnostics: device_api::ReticulumDnsDiagnostics) -> Self {
        Self {
            gateway_ipv4: diagnostics
                .gateway_ipv4
                .map(|address| Ipv4Addr::from(address).to_string()),
            dhcp_servers: diagnostics
                .dhcp_servers
                .into_iter()
                .map(|server| server.map(|address| Ipv4Addr::from(address).to_string()))
                .collect(),
            primary_outcome: diagnostics.primary_outcome.into(),
            raw_setup_state: diagnostics.raw_setup_state.into(),
            raw_attempts: diagnostics
                .raw_attempts
                .into_iter()
                .map(|attempt| attempt.map(ReticulumDnsRawAttemptView::from))
                .collect(),
            resolution: diagnostics.resolution.map(ReticulumDnsResolutionView::from),
        }
    }
}

/// Cooperative RMAP discovery-stamp lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RmapStampPhaseView {
    /// Disabled by applied configuration.
    Disabled,
    /// Incremental proof-of-work is running.
    Searching,
    /// A stamped payload is resident.
    Ready,
    /// The candidate space was exhausted.
    Exhausted,
    /// Activation failed before search.
    Faulted,
}

impl From<device_api::RmapStampPhase> for RmapStampPhaseView {
    fn from(phase: device_api::RmapStampPhase) -> Self {
        match phase {
            device_api::RmapStampPhase::Disabled => Self::Disabled,
            device_api::RmapStampPhase::Searching => Self::Searching,
            device_api::RmapStampPhase::Ready => Self::Ready,
            device_api::RmapStampPhase::Exhausted => Self::Exhausted,
            device_api::RmapStampPhase::Faulted => Self::Faulted,
        }
    }
}

/// Readiness of the applied public TCP target for RMAP publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RmapInitialTcpGateStateView {
    /// No exact public TCP target is applied.
    NotRequired,
    /// The applied TCP target is offline.
    Waiting,
    /// The applied TCP target is ready.
    Open,
}

impl From<device_api::RmapInitialTcpGateState> for RmapInitialTcpGateStateView {
    fn from(state: device_api::RmapInitialTcpGateState) -> Self {
        match state {
            device_api::RmapInitialTcpGateState::NotRequired => Self::NotRequired,
            device_api::RmapInitialTcpGateState::Waiting => Self::Waiting,
            device_api::RmapInitialTcpGateState::Open => Self::Open,
        }
    }
}

/// Most recent RMAP announce admission outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RmapQueueOutcomeView {
    /// No attempt reached admission.
    NotAttempted,
    /// The ordinary coordinator accepted the action.
    Accepted,
    /// Native announce admission deferred the attempt.
    AnnounceAdmissionDeferred,
    /// Ordinary coordinator admission deferred the action.
    OrdinaryAdmissionDeferred,
}

impl From<device_api::RmapQueueOutcome> for RmapQueueOutcomeView {
    fn from(outcome: device_api::RmapQueueOutcome) -> Self {
        match outcome {
            device_api::RmapQueueOutcome::NotAttempted => Self::NotAttempted,
            device_api::RmapQueueOutcome::Accepted => Self::Accepted,
            device_api::RmapQueueOutcome::AnnounceAdmissionDeferred => {
                Self::AnnounceAdmissionDeferred
            }
            device_api::RmapQueueOutcome::OrdinaryAdmissionDeferred => {
                Self::OrdinaryAdmissionDeferred
            }
        }
    }
}

/// Physical-egress evidence for the latest accepted RMAP publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RmapEgressConfirmationView {
    /// No publication has been accepted.
    NotApplicable,
    /// This build has no correlated physical completion.
    NotObserved,
    /// The selected interface reported physical completion.
    Confirmed,
}

impl From<device_api::RmapEgressConfirmation> for RmapEgressConfirmationView {
    fn from(confirmation: device_api::RmapEgressConfirmation) -> Self {
        match confirmation {
            device_api::RmapEgressConfirmation::NotApplicable => Self::NotApplicable,
            device_api::RmapEgressConfirmation::NotObserved => Self::NotObserved,
            device_api::RmapEgressConfirmation::Confirmed => Self::Confirmed,
        }
    }
}

/// Stable reason why RMAP activation or publication is deferred.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RmapDeferredReasonView {
    /// Discovery model validation failed.
    DiscoveryModelInvalid,
    /// Discovery payload encoding failed.
    PayloadEncodingFailed,
    /// Stamp-search construction failed.
    StampInitializationFailed,
    /// Local destination activation failed.
    DestinationActivationFailed,
    /// The stamp candidate space was exhausted.
    StampSearchExhausted,
    /// The exact public TCP target is offline.
    InitialTcpNotReady,
    /// Announce application data was too large.
    AnnouncePayloadTooLarge,
    /// The native announce queue was full.
    AnnounceQueueFull,
    /// Native announce construction or queueing rejected the request.
    AnnounceConstructionRejected,
    /// The ordinary coordinator rejected the action owner.
    OrdinaryQueueRejected,
}

impl From<device_api::RmapDeferredReason> for RmapDeferredReasonView {
    fn from(reason: device_api::RmapDeferredReason) -> Self {
        match reason {
            device_api::RmapDeferredReason::DiscoveryModelInvalid => Self::DiscoveryModelInvalid,
            device_api::RmapDeferredReason::PayloadEncodingFailed => Self::PayloadEncodingFailed,
            device_api::RmapDeferredReason::StampInitializationFailed => {
                Self::StampInitializationFailed
            }
            device_api::RmapDeferredReason::DestinationActivationFailed => {
                Self::DestinationActivationFailed
            }
            device_api::RmapDeferredReason::StampSearchExhausted => Self::StampSearchExhausted,
            device_api::RmapDeferredReason::InitialTcpNotReady => Self::InitialTcpNotReady,
            device_api::RmapDeferredReason::AnnouncePayloadTooLarge => {
                Self::AnnouncePayloadTooLarge
            }
            device_api::RmapDeferredReason::AnnounceQueueFull => Self::AnnounceQueueFull,
            device_api::RmapDeferredReason::AnnounceConstructionRejected => {
                Self::AnnounceConstructionRejected
            }
            device_api::RmapDeferredReason::OrdinaryQueueRejected => Self::OrdinaryQueueRejected,
        }
    }
}

/// Current compact opt-in RMAP publication status.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct RmapRuntimeStatusView {
    config_applied: bool,
    stamp_phase: RmapStampPhaseView,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    stamp_attempts: u64,
    initial_tcp_gate: RmapInitialTcpGateStateView,
    queued_count: u32,
    last_queue_outcome: RmapQueueOutcomeView,
    #[serde(serialize_with = "serialize_optional_json_safe_u64")]
    #[ts(as = "Option<JsonSafeInteger>")]
    last_queue_attempt_at_uptime_seconds: Option<u64>,
    egress_confirmation: RmapEgressConfirmationView,
    #[serde(serialize_with = "serialize_optional_json_safe_u64")]
    #[ts(as = "Option<JsonSafeInteger>")]
    next_due_in_seconds: Option<u64>,
    deferred_reason: Option<RmapDeferredReasonView>,
}

impl From<device_api::RmapRuntimeStatus> for RmapRuntimeStatusView {
    fn from(status: device_api::RmapRuntimeStatus) -> Self {
        Self {
            config_applied: status.config_applied,
            stamp_phase: status.stamp_phase.into(),
            stamp_attempts: status.stamp_attempts,
            initial_tcp_gate: status.initial_tcp_gate.into(),
            queued_count: status.queued_count,
            last_queue_outcome: status.last_queue_outcome.into(),
            last_queue_attempt_at_uptime_seconds: status.last_queue_attempt_at_uptime_seconds,
            egress_confirmation: status.egress_confirmation.into(),
            next_due_in_seconds: status.next_due_in_seconds,
            deferred_reason: status.deferred_reason.map(RmapDeferredReasonView::from),
        }
    }
}

/// Current secret-free Wi-Fi and Reticulum TCP state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct NetworkRuntimeStatusView {
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    configured_revision: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    applied_revision: u64,
    wifi_state: WifiStationStateView,
    active_wifi_profile: Option<String>,
    connected_ssid: Option<BytesView>,
    ipv4_address: Option<String>,
    rssi_dbm: Option<i16>,
    tcp_peer_state: ReticulumTcpPeerStateView,
    last_tcp_failure: Option<ReticulumTcpFailureView>,
    dns_diagnostics: Option<ReticulumDnsDiagnosticsView>,
    rmap_status: Option<RmapRuntimeStatusView>,
}

impl NetworkRuntimeStatusView {
    /// Latest committed desired configuration revision.
    pub const fn configured_revision(&self) -> u64 {
        self.configured_revision
    }

    /// Configuration revision currently applied by the network actors.
    pub const fn applied_revision(&self) -> u64 {
        self.applied_revision
    }

    /// Current Wi-Fi station state.
    pub const fn wifi_state(&self) -> WifiStationStateView {
        self.wifi_state
    }

    /// Active saved profile identity, encoded as lowercase hexadecimal.
    pub fn active_wifi_profile(&self) -> Option<&str> {
        self.active_wifi_profile.as_deref()
    }

    /// Exact associated SSID bytes, when connected.
    pub const fn connected_ssid(&self) -> Option<&BytesView> {
        self.connected_ssid.as_ref()
    }

    /// DHCP-assigned dotted-decimal IPv4 address.
    pub fn ipv4_address(&self) -> Option<&str> {
        self.ipv4_address.as_deref()
    }

    /// Current whole-dBm station RSSI.
    pub const fn rssi_dbm(&self) -> Option<i16> {
        self.rssi_dbm
    }

    /// Current outbound Reticulum TCP interface state.
    pub const fn tcp_peer_state(&self) -> ReticulumTcpPeerStateView {
        self.tcp_peer_state
    }

    /// Most recent recoverable TCP failure retained by the running actor.
    pub const fn last_tcp_failure(&self) -> Option<ReticulumTcpFailureView> {
        self.last_tcp_failure
    }

    /// Latest bounded DNS diagnostics for a hostname peer.
    pub const fn dns_diagnostics(&self) -> Option<&ReticulumDnsDiagnosticsView> {
        self.dns_diagnostics.as_ref()
    }

    /// Current opt-in RMAP publication state, when exposed by the firmware.
    pub const fn rmap_status(&self) -> Option<&RmapRuntimeStatusView> {
        self.rmap_status.as_ref()
    }
}

impl From<device_api::NetworkRuntimeStatus> for NetworkRuntimeStatusView {
    fn from(status: device_api::NetworkRuntimeStatus) -> Self {
        Self {
            configured_revision: status.configured_revision,
            applied_revision: status.applied_revision,
            wifi_state: status.wifi_state.into(),
            active_wifi_profile: status
                .active_wifi_profile
                .map(|profile| hex::encode(profile.as_bytes())),
            connected_ssid: status
                .connected_ssid()
                .map(|ssid| BytesView::new(ssid.as_bytes())),
            ipv4_address: status
                .ipv4_address
                .map(|address| Ipv4Addr::from(address).to_string()),
            rssi_dbm: status.rssi_dbm,
            tcp_peer_state: status.tcp_peer_state.into(),
            last_tcp_failure: status.last_tcp_failure.map(ReticulumTcpFailureView::from),
            dns_diagnostics: status
                .dns_diagnostics
                .map(ReticulumDnsDiagnosticsView::from),
            rmap_status: status.rmap_status.map(RmapRuntimeStatusView::from),
        }
    }
}

/// Secret update for one saved WPA2-Personal profile.
///
/// This value deliberately has no `Debug`, `Clone`, or serialization
/// implementation. Replacement text is zeroized when the actor-owned request
/// is dropped.
#[derive(Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WifiCredentialUpdate {
    /// Retain the credential already stored for this profile.
    Keep,
    /// Replace the stored credential with this WPA2-Personal passphrase.
    Replace {
        /// Printable ASCII passphrase accepted only for the duration of one request.
        passphrase: String,
    },
}

impl Drop for WifiCredentialUpdate {
    fn drop(&mut self) {
        if let Self::Replace { passphrase } = self {
            passphrase.zeroize();
        }
    }
}

/// Desired IPv4 outbound Reticulum TCP peer supplied by an app.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct ReticulumTcpPeerIpv4Input {
    /// Whether the board should connect to this peer.
    enabled: bool,
    /// Exact dotted-decimal IPv4 address.
    ipv4_address: String,
    /// Configured TCP port.
    port: u16,
}

/// Desired hostname-based outbound Reticulum TCP peer supplied by an app.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, TS)]
#[serde(deny_unknown_fields)]
pub struct ReticulumTcpPeerHostnameInput {
    /// Whether the board should connect to this peer.
    enabled: bool,
    /// ASCII DNS hostname resolved by the board on every reconnect.
    hostname: String,
    /// Configured TCP port.
    port: u16,
}

/// One app-requested desired-network mutation.
///
/// The enum intentionally omits `Debug` because an upsert can own a
/// secret-bearing [`WifiCredentialUpdate`].
#[derive(Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NetworkConfigMutation {
    /// Create or replace one saved WPA2-Personal network.
    UpsertWifi {
        /// Stable nonzero 16-byte profile identity as hexadecimal.
        profile_id: String,
        /// Whether the station selector may use this profile.
        enabled: bool,
        /// Selection priority; larger values are preferred.
        priority: u8,
        /// Exact SSID represented as UTF-8 or hexadecimal bytes.
        ssid: BytesView,
        /// Whether to retain or replace the stored passphrase.
        credential: WifiCredentialUpdate,
    },
    /// Remove one saved network.
    RemoveWifi {
        /// Stable nonzero 16-byte profile identity as hexadecimal.
        profile_id: String,
    },
    /// Replace or clear the single outbound Reticulum TCP peer.
    ReplaceTcpPeer {
        /// New peer, or `null` to clear the peer.
        peer: Option<ReticulumTcpPeerIpv4Input>,
    },
    /// Replace or clear the hostname-based outbound Reticulum TCP peer.
    ReplaceTcpHostPeer {
        /// New hostname peer, or `null` to clear the active peer.
        peer: Option<ReticulumTcpPeerHostnameInput>,
    },
    /// Replace gateway-wide transport and automatic-announce policy.
    SetGatewayPolicy {
        /// Whether the board may run its Wi-Fi station and TCP transport.
        wifi_transport_enabled: bool,
        /// Whether the board may emit scheduled ordinary service announces.
        automatic_announces_enabled: bool,
    },
    /// Replace opt-in RMAP discovery and phone-location publication policy.
    SetRmapConfig {
        /// Whether the board may publish signed RMAP discovery announces.
        discovery_enabled: bool,
        /// Whether a present phone position may be included in publication.
        share_location: bool,
        /// Latest optional phone-sourced position in integer microdegrees.
        phone_location: Option<RmapPhoneLocation>,
    },
    /// Replace the requested LoRa radio output.
    SetLoraTxPower {
        /// Qualified requested output in whole dBm.
        #[ts(type = "14 | 17 | 20 | 22")]
        lora_tx_power_dbm: u8,
    },
    /// Atomically replace all LoRa compatibility fields.
    SetLoraProfile {
        /// Complete profile saved for the next restart.
        profile: LoraRadioProfileView,
    },
}

/// Compare-and-swap request for one desired-network mutation.
///
/// This type intentionally omits `Debug` and serialization because it can own
/// a replacement passphrase. It is consumed by the appliance actor and then
/// dropped, zeroizing that passphrase.
#[derive(Deserialize, TS)]
pub struct NetworkConfigMutationRequest {
    mutation: NetworkConfigMutation,
    #[serde(deserialize_with = "deserialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    expected_revision: u64,
    idempotency_key: String,
}

impl NetworkConfigMutationRequest {
    pub(crate) fn with_device_request<R>(
        &self,
        invoke: impl FnOnce(device_api::NetworkConfigMutationRequest<'_>) -> R,
    ) -> Result<R, NetworkRequestError> {
        let idempotency_key = parse_hex16(
            &self.idempotency_key,
            NetworkRequestError::InvalidIdempotencyKey,
        )?;
        let mutation = &self.mutation;
        match mutation {
            NetworkConfigMutation::UpsertWifi {
                profile_id,
                enabled,
                priority,
                ssid,
                credential,
            } => {
                let profile_id = profile_id_from_text(profile_id)?;
                let ssid_bytes = decode_bytes_view(ssid)?;
                let ssid = device_api::WifiSsid::new(&ssid_bytes)
                    .map_err(|_| NetworkRequestError::InvalidSsid)?;
                let credential = match credential {
                    WifiCredentialUpdate::Keep => device_api::WifiCredentialUpdate::Keep,
                    WifiCredentialUpdate::Replace { passphrase } => {
                        device_api::WifiCredentialUpdate::replace(passphrase.as_bytes())
                            .map_err(|_| NetworkRequestError::InvalidPassphrase)?
                    }
                };
                Ok(invoke(device_api::NetworkConfigMutationRequest::new(
                    device_api::NetworkConfigMutation::UpsertWifi {
                        profile_id,
                        network: device_api::WifiNetworkUpdate::new(
                            *enabled, *priority, ssid, credential,
                        ),
                    },
                    self.expected_revision,
                    device_api::IdempotencyKey(idempotency_key),
                )))
            }
            NetworkConfigMutation::RemoveWifi { profile_id } => {
                let profile_id = profile_id_from_text(profile_id)?;
                Ok(invoke(device_api::NetworkConfigMutationRequest::new(
                    device_api::NetworkConfigMutation::RemoveWifi { profile_id },
                    self.expected_revision,
                    device_api::IdempotencyKey(idempotency_key),
                )))
            }
            NetworkConfigMutation::ReplaceTcpPeer { peer } => {
                let peer = peer.as_ref().map(parse_ipv4_tcp_peer).transpose()?;
                Ok(invoke(device_api::NetworkConfigMutationRequest::new(
                    device_api::NetworkConfigMutation::ReplaceTcpPeer(peer),
                    self.expected_revision,
                    device_api::IdempotencyKey(idempotency_key),
                )))
            }
            NetworkConfigMutation::ReplaceTcpHostPeer { peer } => {
                let peer = peer.as_ref().map(parse_hostname_tcp_peer).transpose()?;
                Ok(invoke(device_api::NetworkConfigMutationRequest::new(
                    device_api::NetworkConfigMutation::ReplaceTcpHostPeer(peer),
                    self.expected_revision,
                    device_api::IdempotencyKey(idempotency_key),
                )))
            }
            NetworkConfigMutation::SetGatewayPolicy {
                wifi_transport_enabled,
                automatic_announces_enabled,
            } => Ok(invoke(device_api::NetworkConfigMutationRequest::new(
                device_api::NetworkConfigMutation::SetGatewayPolicy(
                    device_api::GatewayPolicy::new(
                        *wifi_transport_enabled,
                        *automatic_announces_enabled,
                    ),
                ),
                self.expected_revision,
                device_api::IdempotencyKey(idempotency_key),
            ))),
            NetworkConfigMutation::SetRmapConfig {
                discovery_enabled,
                share_location,
                phone_location,
            } => {
                let phone_location = phone_location
                    .map(RmapPhoneLocation::into_device)
                    .transpose()?;
                Ok(invoke(device_api::NetworkConfigMutationRequest::new(
                    device_api::NetworkConfigMutation::SetRmapConfig(device_api::RmapConfig::new(
                        *discovery_enabled,
                        *share_location,
                        phone_location,
                    )),
                    self.expected_revision,
                    device_api::IdempotencyKey(idempotency_key),
                )))
            }
            NetworkConfigMutation::SetLoraTxPower { lora_tx_power_dbm } => {
                let power = device_api::LoraTransmitPowerDbm::new(*lora_tx_power_dbm)
                    .map_err(|_| NetworkRequestError::InvalidLoraTransmitPower)?;
                Ok(invoke(device_api::NetworkConfigMutationRequest::new(
                    device_api::NetworkConfigMutation::SetLoraTxPower(power),
                    self.expected_revision,
                    device_api::IdempotencyKey(idempotency_key),
                )))
            }
            NetworkConfigMutation::SetLoraProfile { profile } => {
                let profile = profile.into_device()?;
                Ok(invoke(device_api::NetworkConfigMutationRequest::new(
                    device_api::NetworkConfigMutation::SetLoraProfile(profile),
                    self.expected_revision,
                    device_api::IdempotencyKey(idempotency_key),
                )))
            }
        }
    }
}

fn profile_id_from_text(
    value: &str,
) -> Result<device_api::WifiNetworkProfileId, NetworkRequestError> {
    device_api::WifiNetworkProfileId::new(parse_hex16(
        value,
        NetworkRequestError::InvalidProfileId,
    )?)
    .map_err(|_| NetworkRequestError::InvalidProfileId)
}

fn parse_hex16(value: &str, error: NetworkRequestError) -> Result<[u8; 16], NetworkRequestError> {
    let mut bytes = [0_u8; 16];
    hex::decode_to_slice(value, &mut bytes).map_err(|_| error)?;
    Ok(bytes)
}

fn decode_bytes_view(value: &BytesView) -> Result<Vec<u8>, NetworkRequestError> {
    match value.encoding() {
        BytesEncoding::Utf8 => Ok(value.value().as_bytes().to_vec()),
        BytesEncoding::Hex => {
            hex::decode(value.value()).map_err(|_| NetworkRequestError::InvalidSsidEncoding)
        }
    }
}

fn parse_ipv4_tcp_peer(
    peer: &ReticulumTcpPeerIpv4Input,
) -> Result<device_api::ReticulumTcpPeerUpdate, NetworkRequestError> {
    let ReticulumTcpPeerIpv4Input {
        enabled,
        ipv4_address,
        port,
    } = peer;
    let address = ipv4_address
        .parse::<Ipv4Addr>()
        .map_err(|_| NetworkRequestError::InvalidIpv4Address)?;
    let address = device_api::ReticulumTcpPeerIpv4Address::new(address.octets())
        .map_err(|_| NetworkRequestError::InvalidIpv4Address)?;
    device_api::ReticulumTcpPeerUpdate::new(*enabled, address, *port)
        .map_err(|_| NetworkRequestError::InvalidTcpPort)
}

fn parse_hostname_tcp_peer(
    peer: &ReticulumTcpPeerHostnameInput,
) -> Result<device_api::ReticulumTcpPeerHostUpdate<'_>, NetworkRequestError> {
    let ReticulumTcpPeerHostnameInput {
        enabled,
        hostname,
        port,
    } = peer;
    let hostname = device_api::ReticulumTcpPeerHostname::new(hostname)
        .map_err(|_| NetworkRequestError::InvalidHostname)?;
    device_api::ReticulumTcpPeerHostUpdate::new(*enabled, hostname, *port)
        .map_err(|_| NetworkRequestError::InvalidTcpPort)
}

/// Safe validation failures for app-supplied network configuration.
///
/// No variant stores or formats caller data, so a passphrase cannot enter an
/// error or log message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkRequestError {
    /// Profile identity was not an exact nonzero 16-byte hexadecimal value.
    InvalidProfileId,
    /// Idempotency key was not an exact 16-byte hexadecimal value.
    InvalidIdempotencyKey,
    /// SSID hexadecimal was malformed.
    InvalidSsidEncoding,
    /// SSID was empty or exceeded 32 bytes.
    InvalidSsid,
    /// Passphrase did not satisfy the WPA2-Personal byte policy.
    InvalidPassphrase,
    /// TCP peer address was malformed or cannot name a unicast peer.
    InvalidIpv4Address,
    /// TCP peer hostname was malformed or exceeded its fixed bound.
    InvalidHostname,
    /// TCP peer port was zero.
    InvalidTcpPort,
    /// Phone location was outside the world-bounded fixed-point range.
    InvalidRmapLocation,
    /// LoRa transmit power was not one of the qualified radio outputs.
    InvalidLoraTransmitPower,
    /// LoRa profile contained an unsupported numeric field.
    InvalidLoraRadioProfile,
}

impl fmt::Display for NetworkRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidProfileId => {
                "Wi-Fi profile ID must contain exactly 32 hexadecimal characters and not be all zero"
            }
            Self::InvalidIdempotencyKey => {
                "idempotency key must contain exactly 32 hexadecimal characters"
            }
            Self::InvalidSsidEncoding => "SSID hexadecimal is malformed",
            Self::InvalidSsid => "SSID must contain between 1 and 32 bytes",
            Self::InvalidPassphrase => {
                "WPA2-Personal passphrase must contain 8 to 63 printable ASCII bytes"
            }
            Self::InvalidIpv4Address => {
                "TCP peer must be a valid routable unicast IPv4 address"
            }
            Self::InvalidHostname => {
                "TCP peer hostname must contain valid bounded ASCII DNS labels"
            }
            Self::InvalidTcpPort => "TCP peer port must be nonzero",
            Self::InvalidRmapLocation => {
                "RMAP phone location must use world-bounded integer microdegrees"
            }
            Self::InvalidLoraTransmitPower => {
                "LoRa transmit power must be one of 14, 17, 20, or 22 dBm"
            }
            Self::InvalidLoraRadioProfile => {
                "LoRa profile must use a nonzero frequency, supported bandwidth, SF7-SF12, and coding rate 4/5-4/8"
            }
        })
    }
}

impl std::error::Error for NetworkRequestError {}

/// Normal compare-and-swap result from the board.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum NetworkConfigMutationOutcome {
    /// The mutation was committed or an exact retry was recognized.
    Applied {
        /// New committed revision.
        #[serde(serialize_with = "serialize_json_safe_u64")]
        #[ts(as = "JsonSafeInteger")]
        revision: u64,
        /// Whether the board must reboot before actors use the new revision.
        reboot_required: bool,
    },
    /// The caller's expected revision was stale.
    RevisionConflict {
        /// Current committed revision the caller should refresh from.
        #[serde(serialize_with = "serialize_json_safe_u64")]
        #[ts(as = "JsonSafeInteger")]
        current_revision: u64,
    },
}

impl From<device_api::NetworkConfigMutationOutcome> for NetworkConfigMutationOutcome {
    fn from(outcome: device_api::NetworkConfigMutationOutcome) -> Self {
        match outcome {
            device_api::NetworkConfigMutationOutcome::Applied {
                revision,
                reboot_required,
            } => Self::Applied {
                revision,
                reboot_required,
            },
            device_api::NetworkConfigMutationOutcome::RevisionConflict { current_revision } => {
                Self::RevisionConflict { current_revision }
            }
        }
    }
}

#[cfg(test)]
mod tests;
