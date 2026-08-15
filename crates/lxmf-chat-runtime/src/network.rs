//! App-facing network configuration requests and secret-free projections.

use std::fmt;
use std::net::Ipv4Addr;

use reticulum_device_api as device_api;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use zeroize::Zeroize;

use crate::{
    BytesEncoding, BytesView, JsonSafeInteger, deserialize_json_safe_u64, serialize_json_safe_u64,
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
/// The IPv4 shape is retained for API-1.7 app compatibility. API 1.8 adds the
/// hostname shape so public endpoint presets survive address rotation and are
/// resolved again on every reconnect.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(untagged)]
pub enum ReticulumTcpPeerView {
    /// Exact IPv4 peer retained for local and legacy configurations.
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

    /// Exact dotted-decimal IPv4 address, when this is a legacy IPv4 peer.
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
mod tests {
    use super::*;

    fn profile_id() -> device_api::WifiNetworkProfileId {
        device_api::WifiNetworkProfileId::new([0x44; 16]).unwrap()
    }

    #[test]
    fn projections_preserve_non_utf8_ssids_and_exact_network_state() {
        let wifi =
            device_api::WifiNetworkConfigSummary::new(profile_id(), true, 200, b"field\xff", true)
                .unwrap();
        let peer = device_api::ReticulumTcpPeerConfigSummary::new(
            true,
            device_api::ReticulumTcpPeerIpv4Address::new([192, 0, 2, 9]).unwrap(),
            4242,
        )
        .unwrap();
        let config =
            device_api::NetworkConfigSnapshot::new(9, [Some(wifi), None, None, None], Some(peer))
                .unwrap();
        assert_eq!(
            serde_json::to_value(NetworkConfigView::from(config)).unwrap(),
            serde_json::json!({
                "revision": 9,
                "wifi_profiles": [{
                    "profile_id": "44".repeat(16),
                    "enabled": true,
                    "priority": 200,
                    "ssid": {"encoding": "hex", "value": "6669656c64ff"},
                    "credential_configured": true
                }],
                "tcp_peer": {
                    "enabled": true,
                    "ipv4_address": "192.0.2.9",
                    "port": 4242
                },
                "wifi_transport_enabled": true,
                "automatic_announces_enabled": true,
                "rmap_discovery_enabled": false,
                "rmap_share_location": false,
                "rmap_phone_location": null,
                "lora_tx_power_dbm": 14,
                "lora_profile": {
                    "frequency_hz": 915_000_000,
                    "bandwidth_hz": 125_000,
                    "spreading_factor": 7,
                    "coding_rate_denominator": 5,
                    "tx_power_dbm": 14
                }
            })
        );

        let status = device_api::NetworkRuntimeStatus::new(
            9,
            8,
            device_api::WifiStationState::Connecting,
            Some(profile_id()),
            Some(b"field\xff"),
            Some([198, 51, 100, 7]),
            Some(-81),
            device_api::ReticulumTcpPeerState::WaitingForNetwork,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(NetworkRuntimeStatusView::from(status)).unwrap(),
            serde_json::json!({
                "configured_revision": 9,
                "applied_revision": 8,
                "wifi_state": "connecting",
                "active_wifi_profile": "44".repeat(16),
                "connected_ssid": {"encoding": "hex", "value": "6669656c64ff"},
                "ipv4_address": "198.51.100.7",
                "rssi_dbm": -81,
                "tcp_peer_state": "waiting_for_network",
                "last_tcp_failure": null,
                "dns_diagnostics": null
            })
        );
    }

    #[test]
    fn tcp_backoff_and_last_failure_remain_typed_at_the_json_boundary() {
        let status = device_api::NetworkRuntimeStatus::new_with_tcp_failure(
            11,
            11,
            device_api::WifiStationState::Connected,
            None,
            Some(b"field"),
            Some([192, 0, 2, 7]),
            Some(-75),
            device_api::ReticulumTcpPeerState::Backoff,
            Some(device_api::ReticulumTcpFailure::DnsNoIpv4Result),
        )
        .unwrap();

        assert_eq!(
            serde_json::to_value(NetworkRuntimeStatusView::from(status)).unwrap(),
            serde_json::json!({
                "configured_revision": 11,
                "applied_revision": 11,
                "wifi_state": "connected",
                "active_wifi_profile": null,
                "connected_ssid": {"encoding": "utf8", "value": "field"},
                "ipv4_address": "192.0.2.7",
                "rssi_dbm": -75,
                "tcp_peer_state": "backoff",
                "last_tcp_failure": "dns_no_ipv4_result",
                "dns_diagnostics": null
            })
        );
    }

    #[test]
    fn dns_diagnostics_preserve_sparse_slots_sources_and_response_codes() {
        let diagnostics = device_api::ReticulumDnsDiagnostics::new(
            Some([192, 168, 50, 1]),
            [Some([192, 168, 50, 1]), None, Some([192, 0, 2, 53])],
            device_api::ReticulumDnsPrimaryOutcome::LookupFailed,
            device_api::ReticulumDnsRawSetupState::Ready,
            [
                Some(device_api::ReticulumDnsRawAttempt::new(
                    device_api::ReticulumDnsRawSource::Dhcp,
                    [192, 168, 50, 1],
                    device_api::ReticulumDnsRawOutcome::Timeout,
                )),
                None,
                Some(device_api::ReticulumDnsRawAttempt::new(
                    device_api::ReticulumDnsRawSource::Public,
                    [1, 1, 1, 1],
                    device_api::ReticulumDnsRawOutcome::response_code_outcome(3).unwrap(),
                )),
                Some(device_api::ReticulumDnsRawAttempt::new(
                    device_api::ReticulumDnsRawSource::Public,
                    [9, 9, 9, 9],
                    device_api::ReticulumDnsRawOutcome::Resolved,
                )),
                None,
            ],
            Some(device_api::ReticulumDnsResolution::new(
                [217, 154, 9, 220],
                device_api::ReticulumDnsResolutionSource::RawPublic,
                Some([9, 9, 9, 9]),
            )),
        );
        let status = device_api::NetworkRuntimeStatus::new_with_tcp_diagnostics(
            12,
            12,
            device_api::WifiStationState::Connected,
            None,
            Some(b"field"),
            Some([192, 168, 50, 42]),
            Some(-64),
            device_api::ReticulumTcpPeerState::Connecting,
            Some(device_api::ReticulumTcpFailure::DnsLookupFailed),
            Some(diagnostics),
        )
        .unwrap();

        assert_eq!(
            serde_json::to_value(NetworkRuntimeStatusView::from(status)).unwrap(),
            serde_json::json!({
                "configured_revision": 12,
                "applied_revision": 12,
                "wifi_state": "connected",
                "active_wifi_profile": null,
                "connected_ssid": {"encoding": "utf8", "value": "field"},
                "ipv4_address": "192.168.50.42",
                "rssi_dbm": -64,
                "tcp_peer_state": "connecting",
                "last_tcp_failure": "dns_lookup_failed",
                "dns_diagnostics": {
                    "gateway_ipv4": "192.168.50.1",
                    "dhcp_servers": ["192.168.50.1", null, "192.0.2.53"],
                    "primary_outcome": "lookup_failed",
                    "raw_setup_state": "ready",
                    "raw_attempts": [
                        {
                            "source": "dhcp",
                            "server": "192.168.50.1",
                            "outcome": {"kind": "timeout"}
                        },
                        null,
                        {
                            "source": "public",
                            "server": "1.1.1.1",
                            "outcome": {"kind": "response_code", "code": 3}
                        },
                        {
                            "source": "public",
                            "server": "9.9.9.9",
                            "outcome": {"kind": "resolved"}
                        },
                        null
                    ],
                    "resolution": {
                        "address": "217.154.9.220",
                        "source": "raw_public",
                        "resolver": "9.9.9.9"
                    }
                }
            })
        );
    }

    #[test]
    fn secret_bearing_upsert_maps_to_borrowed_device_request_without_formatting_secret() {
        let request: NetworkConfigMutationRequest = serde_json::from_value(serde_json::json!({
            "mutation": {
                "kind": "upsert_wifi",
                "profile_id": "55".repeat(16),
                "enabled": true,
                "priority": 240,
                "ssid": {"encoding": "hex", "value": "6d657368ff"},
                "credential": {
                    "kind": "replace",
                    "passphrase": "correct horse battery staple"
                }
            },
            "expected_revision": 7,
            "idempotency_key": "66".repeat(16)
        }))
        .unwrap();
        let observed = request
            .with_device_request(|request| match request.mutation() {
                device_api::NetworkConfigMutation::UpsertWifi {
                    profile_id,
                    network,
                } => (
                    *profile_id.as_bytes(),
                    network.enabled(),
                    network.priority(),
                    network.ssid().as_bytes().to_vec(),
                    network.credential().replacement().unwrap().to_vec(),
                    request.expected_revision(),
                    request.idempotency_key().0,
                ),
                _ => panic!("expected Wi-Fi upsert"),
            })
            .unwrap();
        assert_eq!(observed.0, [0x55; 16]);
        assert!(observed.1);
        assert_eq!(observed.2, 240);
        assert_eq!(observed.3, b"mesh\xff");
        assert_eq!(observed.4, b"correct horse battery staple");
        assert_eq!(observed.5, 7);
        assert_eq!(observed.6, [0x66; 16]);
    }

    #[test]
    fn mutation_outcomes_are_typed_and_validation_errors_are_secret_free() {
        assert_eq!(
            serde_json::to_value(NetworkConfigMutationOutcome::Applied {
                revision: 10,
                reboot_required: true,
            })
            .unwrap(),
            serde_json::json!({
                "outcome": "applied",
                "revision": 10,
                "reboot_required": true
            })
        );
        assert_eq!(
            serde_json::to_value(NetworkConfigMutationOutcome::RevisionConflict {
                current_revision: 11,
            })
            .unwrap(),
            serde_json::json!({
                "outcome": "revision_conflict",
                "current_revision": 11
            })
        );

        let request: NetworkConfigMutationRequest = serde_json::from_value(serde_json::json!({
            "mutation": {
                "kind": "upsert_wifi",
                "profile_id": "55".repeat(16),
                "enabled": true,
                "priority": 1,
                "ssid": {"encoding": "utf8", "value": "mesh"},
                "credential": {
                    "kind": "replace",
                    "passphrase": "TOP-SECRET"
                }
            },
            "expected_revision": 1,
            "idempotency_key": "invalid"
        }))
        .unwrap();
        let error = request.with_device_request(|_| ()).unwrap_err().to_string();
        assert!(!error.contains("TOP-SECRET"));
        assert_eq!(
            error,
            "idempotency key must contain exactly 32 hexadecimal characters"
        );
    }

    #[test]
    fn tcp_peer_mutation_preserves_exact_ipv4_and_rejects_non_peer_addresses() {
        let request: NetworkConfigMutationRequest = serde_json::from_value(serde_json::json!({
            "mutation": {
                "kind": "replace_tcp_peer",
                "peer": {
                    "enabled": true,
                    "ipv4_address": "198.51.100.42",
                    "port": 4242
                }
            },
            "expected_revision": 12,
            "idempotency_key": "77".repeat(16)
        }))
        .unwrap();
        let observed = request
            .with_device_request(|request| match request.mutation() {
                device_api::NetworkConfigMutation::ReplaceTcpPeer(Some(peer)) => (
                    peer.enabled(),
                    peer.ipv4_address().octets(),
                    peer.port(),
                    request.expected_revision(),
                ),
                _ => panic!("expected TCP peer replacement"),
            })
            .unwrap();
        assert_eq!(observed, (true, [198, 51, 100, 42], 4242, 12));

        let multicast: NetworkConfigMutationRequest = serde_json::from_value(serde_json::json!({
            "mutation": {
                "kind": "replace_tcp_peer",
                "peer": {
                    "enabled": true,
                    "ipv4_address": "239.1.2.3",
                    "port": 4242
                }
            },
            "expected_revision": 12,
            "idempotency_key": "77".repeat(16)
        }))
        .unwrap();
        assert_eq!(
            multicast.with_device_request(|_| ()),
            Err(NetworkRequestError::InvalidIpv4Address)
        );
    }

    #[test]
    fn hostname_gateway_and_rmap_state_project_without_losing_fixed_point_coordinates() {
        let wifi =
            device_api::WifiNetworkConfigSummary::new(profile_id(), true, 200, b"field", true)
                .unwrap();
        let peer =
            device_api::ReticulumTcpPeerHostConfigSummary::new(true, "rmap.world", 4242).unwrap();
        let location = device_api::RmapLocation::new(42_360_100, -71_058_900).unwrap();
        let config = device_api::NetworkConfigSnapshot::new_complete(
            13,
            [Some(wifi), None, None, None],
            None,
            Some(peer),
            device_api::GatewayPolicy::new(false, false),
            device_api::RmapConfig::new(true, true, Some(location)),
            device_api::LoraTransmitPowerDbm::DBM_22,
        )
        .unwrap();

        assert_eq!(
            serde_json::to_value(NetworkConfigView::from(config)).unwrap(),
            serde_json::json!({
                "revision": 13,
                "wifi_profiles": [{
                    "profile_id": "44".repeat(16),
                    "enabled": true,
                    "priority": 200,
                    "ssid": {"encoding": "utf8", "value": "field"},
                    "credential_configured": true
                }],
                "tcp_peer": {
                    "enabled": true,
                    "hostname": "rmap.world",
                    "port": 4242
                },
                "wifi_transport_enabled": false,
                "automatic_announces_enabled": false,
                "rmap_discovery_enabled": true,
                "rmap_share_location": true,
                "rmap_phone_location": {
                    "latitude_e6": 42_360_100,
                    "longitude_e6": -71_058_900
                },
                "lora_tx_power_dbm": 22,
                "lora_profile": {
                    "frequency_hz": 915_000_000,
                    "bandwidth_hz": 125_000,
                    "spreading_factor": 7,
                    "coding_rate_denominator": 5,
                    "tx_power_dbm": 22
                }
            })
        );
    }

    #[test]
    fn hostname_gateway_and_rmap_mutations_map_to_api_1_8_requests() {
        let hostname: NetworkConfigMutationRequest = serde_json::from_value(serde_json::json!({
            "mutation": {
                "kind": "replace_tcp_host_peer",
                "peer": {
                    "enabled": true,
                    "hostname": "node.reticulumnet.nl",
                    "port": 4242
                }
            },
            "expected_revision": 12,
            "idempotency_key": "88".repeat(16)
        }))
        .unwrap();
        let observed = hostname
            .with_device_request(|request| match request.mutation() {
                device_api::NetworkConfigMutation::ReplaceTcpHostPeer(Some(peer)) => (
                    peer.enabled(),
                    peer.hostname().as_str().to_owned(),
                    peer.port(),
                    request.expected_revision(),
                ),
                _ => panic!("expected hostname TCP peer replacement"),
            })
            .unwrap();
        assert_eq!(
            observed,
            (true, "node.reticulumnet.nl".to_owned(), 4242, 12)
        );

        let gateway: NetworkConfigMutationRequest = serde_json::from_value(serde_json::json!({
            "mutation": {
                "kind": "set_gateway_policy",
                "wifi_transport_enabled": false,
                "automatic_announces_enabled": true
            },
            "expected_revision": 13,
            "idempotency_key": "99".repeat(16)
        }))
        .unwrap();
        let observed = gateway
            .with_device_request(|request| match request.mutation() {
                device_api::NetworkConfigMutation::SetGatewayPolicy(policy) => (
                    policy.wifi_transport_enabled(),
                    policy.automatic_announces_enabled(),
                ),
                _ => panic!("expected gateway policy"),
            })
            .unwrap();
        assert_eq!(observed, (false, true));

        let rmap: NetworkConfigMutationRequest = serde_json::from_value(serde_json::json!({
            "mutation": {
                "kind": "set_rmap_config",
                "discovery_enabled": true,
                "share_location": true,
                "phone_location": {
                    "latitude_e6": 42_360_100,
                    "longitude_e6": -71_058_900
                }
            },
            "expected_revision": 14,
            "idempotency_key": "aa".repeat(16)
        }))
        .unwrap();
        let observed = rmap
            .with_device_request(|request| match request.mutation() {
                device_api::NetworkConfigMutation::SetRmapConfig(config) => {
                    let location = config.phone_location().unwrap();
                    (
                        config.discovery_enabled(),
                        config.share_location(),
                        location.latitude_e6(),
                        location.longitude_e6(),
                    )
                }
                _ => panic!("expected RMAP policy"),
            })
            .unwrap();
        assert_eq!(observed, (true, true, 42_360_100, -71_058_900));
    }

    #[test]
    fn rmap_mutation_rejects_out_of_world_fixed_point_coordinates() {
        let request: NetworkConfigMutationRequest = serde_json::from_value(serde_json::json!({
            "mutation": {
                "kind": "set_rmap_config",
                "discovery_enabled": true,
                "share_location": true,
                "phone_location": {
                    "latitude_e6": 90_000_001,
                    "longitude_e6": 0
                }
            },
            "expected_revision": 14,
            "idempotency_key": "aa".repeat(16)
        }))
        .unwrap();
        assert_eq!(
            request.with_device_request(|_| ()),
            Err(NetworkRequestError::InvalidRmapLocation)
        );
    }

    #[test]
    fn lora_transmit_power_mutation_accepts_only_qualified_radio_outputs() {
        let ts_config = ts_rs::Config::default();
        assert!(
            NetworkConfigView::decl(&ts_config).contains("lora_tx_power_dbm: 14 | 17 | 20 | 22")
        );
        assert!(
            NetworkConfigMutation::decl(&ts_config)
                .contains("lora_tx_power_dbm: 14 | 17 | 20 | 22")
        );

        let request: NetworkConfigMutationRequest = serde_json::from_value(serde_json::json!({
            "mutation": {
                "kind": "set_lora_tx_power",
                "lora_tx_power_dbm": 22
            },
            "expected_revision": 15,
            "idempotency_key": "bb".repeat(16)
        }))
        .unwrap();
        let observed = request
            .with_device_request(|request| match request.mutation() {
                device_api::NetworkConfigMutation::SetLoraTxPower(power) => (
                    power.get(),
                    request.expected_revision(),
                    request.idempotency_key().0,
                ),
                _ => panic!("expected LoRa transmit-power mutation"),
            })
            .unwrap();
        assert_eq!(observed, (22, 15, [0xbb; 16]));

        let invalid: NetworkConfigMutationRequest = serde_json::from_value(serde_json::json!({
            "mutation": {
                "kind": "set_lora_tx_power",
                "lora_tx_power_dbm": 21
            },
            "expected_revision": 15,
            "idempotency_key": "bb".repeat(16)
        }))
        .unwrap();
        assert_eq!(
            invalid.with_device_request(|_| ()),
            Err(NetworkRequestError::InvalidLoraTransmitPower)
        );
        assert_eq!(
            NetworkRequestError::InvalidLoraTransmitPower.to_string(),
            "LoRa transmit power must be one of 14, 17, 20, or 22 dBm"
        );
    }

    #[test]
    fn lora_profile_mutation_preserves_one_atomic_tuple() {
        let ts_config = ts_rs::Config::default();
        assert!(LoraRadioProfileView::decl(&ts_config).contains("frequency_hz: number"));
        assert!(NetworkConfigMutation::decl(&ts_config).contains("set_lora_profile"));

        let request: NetworkConfigMutationRequest = serde_json::from_value(serde_json::json!({
            "mutation": {
                "kind": "set_lora_profile",
                "profile": {
                    "frequency_hz": 914_875_000,
                    "bandwidth_hz": 250_000,
                    "spreading_factor": 9,
                    "coding_rate_denominator": 7,
                    "tx_power_dbm": 22
                }
            },
            "expected_revision": 16,
            "idempotency_key": "bc".repeat(16)
        }))
        .unwrap();
        let observed = request
            .with_device_request(|request| match request.mutation() {
                device_api::NetworkConfigMutation::SetLoraProfile(profile) => (
                    profile.frequency_hz(),
                    profile.bandwidth_hz(),
                    profile.spreading_factor(),
                    profile.coding_rate_denominator(),
                    profile.tx_power_dbm().get(),
                ),
                _ => panic!("expected LoRa profile mutation"),
            })
            .unwrap();
        assert_eq!(observed, (914_875_000, 250_000, 9, 7, 22));
    }

    #[test]
    fn tcp_peer_input_rejects_ambiguous_address_shapes() {
        assert!(
            serde_json::from_value::<ReticulumTcpPeerIpv4Input>(serde_json::json!({
                "enabled": true,
                "ipv4_address": "192.0.2.1",
                "hostname": "rmap.world",
                "port": 4242
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ReticulumTcpPeerHostnameInput>(serde_json::json!({
                "enabled": true,
                "ipv4_address": "192.0.2.1",
                "hostname": "rmap.world",
                "port": 4242
            }))
            .is_err()
        );
    }
}
