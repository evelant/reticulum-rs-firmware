//! Rete-independent logical API model and authorization vocabulary.

use core::{convert::Infallible, marker::PhantomData, num::NonZeroU16, ops::BitOr};

#[cfg(feature = "experimental-network-config")]
use core::num::NonZeroU8;
#[cfg(any(feature = "experimental-rns-inbox", feature = "experimental-lxmf"))]
use core::num::NonZeroU64;

/// Device API v1 major version.
pub const API_VERSION_MAJOR: u16 = 1;
/// Device API v1 revision adding an explicit transient retry response.
pub const API_VERSION_MINOR: u16 = 18;

/// Maximum size of one decoded or encoded logical CBOR message.
pub const MAX_MESSAGE_BYTES: usize = 512;
/// Maximum encoded size of the operation-specific body within a message.
pub const MAX_BODY_BYTES: usize = 448;
/// Maximum payload accepted by the experimental RNS DATA submission request.
pub const MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES: usize = 383;
/// Maximum payload returned by the experimental inbound RNS DATA mailbox.
#[cfg(feature = "experimental-rns-inbox")]
pub const MAX_RNS_INBOX_PAYLOAD_BYTES: usize = 383;
/// Maximum exact normalized LXMF wire bytes returned by one read response.
#[cfg(feature = "experimental-lxmf")]
pub const MAX_LXMF_READ_CHUNK_BYTES: usize = 416;
/// Structural per-field title limit accepted by the basic-LXMF codec.
///
/// The encoded body and product composer can impose a lower limit on a
/// particular title/content combination.
pub const MAX_LXMF_BASIC_TITLE_BYTES: usize = 295;
/// Structural per-field content limit accepted by the basic-LXMF codec.
///
/// The encoded body and product composer can impose a lower limit on a
/// particular title/content combination.
pub const MAX_LXMF_BASIC_CONTENT_BYTES: usize = 295;
/// Maximum authenticated announce application data returned for one nearby LXMF peer.
pub const MAX_LXMF_PEER_APP_DATA_BYTES: usize = 256;
/// Largest UTF-8 NomadNet request path accepted by the experimental fetch API.
pub const MAX_NOMAD_PAGE_PATH_BYTES: usize = 128;
/// Largest valid UTF-8 Micron page returned by the experimental fetch API.
pub const MAX_NOMAD_PAGE_BYTES: usize = 400;
/// Maximum Wi-Fi SSID length in bytes.
pub const MAX_WIFI_SSID_BYTES: usize = 32;
/// Maximum saved Wi-Fi station profiles exposed by this experimental API.
pub const MAX_WIFI_NETWORK_PROFILES: usize = 4;
/// Maximum interface records returned by one node-diagnostics snapshot.
pub const MAX_DIAGNOSTIC_INTERFACES: usize = 4;
/// Maximum route records returned by one diagnostics page.
pub const MAX_ROUTE_DIAGNOSTIC_PAGE_ENTRIES: usize = 4;
/// Maximum radio-trace events returned by one diagnostics page.
pub const MAX_RADIO_TRACE_PAGE_ENTRIES: usize = 3;
/// Maximum ASCII DNS hostname length for one outbound Reticulum TCP peer.
pub const MAX_RETICULUM_TCP_PEER_HOSTNAME_BYTES: usize = 96;
/// Minimum WPA2-Personal passphrase length in bytes.
pub const MIN_WIFI_PASSPHRASE_BYTES: usize = 8;
/// Maximum WPA2-Personal passphrase length in bytes.
pub const MAX_WIFI_PASSPHRASE_BYTES: usize = 63;
/// Conventional Reticulum TCP interface port.
pub const DEFAULT_RETICULUM_TCP_PORT: u16 = 4242;
/// Largest JavaScript-safe whole-millisecond request timestamp.
///
/// Converting extreme accepted values to binary64 seconds can lose
/// millisecond precision. Contemporary Unix dates retain millisecond
/// precision; the wire bound promises integer interchange, not exact
/// binary64 spacing across the complete range.
pub const MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS: u64 = (1_u64 << 53) - 1;

/// `system.capabilities` operation number.
pub const OP_SYSTEM_CAPABILITIES: u16 = 0x0001;
/// `submission.status` operation number.
pub const OP_SUBMISSION_STATUS: u16 = 0x0002;
/// `identity.summary` operation number.
pub const OP_IDENTITY_SUMMARY: u16 = 0x0003;
/// Error response kind used instead of a successful operation number.
pub const RESPONSE_ERROR: u16 = 0x0000;
/// Target-safe outbound RNS DATA submission operation in the experimental range.
#[cfg(feature = "experimental-rns-data")]
pub const OP_EXPERIMENTAL_SUBMIT_RNS_DATA: u16 = 0xf001;
/// Read bounded runtime state for the experimental inbound RNS DATA mailbox.
#[cfg(feature = "experimental-rns-inbox")]
pub const OP_EXPERIMENTAL_RNS_INBOX_STATUS: u16 = 0xf002;
/// Read the oldest item in the experimental inbound RNS DATA mailbox.
#[cfg(feature = "experimental-rns-inbox")]
pub const OP_EXPERIMENTAL_RNS_INBOX_PEEK: u16 = 0xf003;
/// Read the next committed LXMF message summary after an optional stable handle.
#[cfg(feature = "experimental-lxmf")]
pub const OP_EXPERIMENTAL_LXMF_NEXT: u16 = 0xf004;
/// Read one bounded chunk of a committed normalized LXMF wire message.
#[cfg(feature = "experimental-lxmf")]
pub const OP_EXPERIMENTAL_LXMF_READ: u16 = 0xf005;
/// Compose and durably submit one basic LXMF message through the local identity.
#[cfg(feature = "experimental-lxmf")]
pub const OP_EXPERIMENTAL_LXMF_BASIC_SEND: u16 = 0xf006;
/// Read one bounded nearby `lxmf.delivery` peer after an optional boot-scoped cursor.
#[cfg(feature = "experimental-lxmf")]
pub const OP_EXPERIMENTAL_LXMF_PEER_NEXT: u16 = 0xf007;
/// Begin one bounded authenticated NomadNet page fetch.
#[cfg(feature = "experimental-nomad")]
pub const OP_EXPERIMENTAL_NOMAD_FETCH_START: u16 = 0xf008;
/// Poll one principal-owned bounded NomadNet page fetch.
#[cfg(feature = "experimental-nomad")]
pub const OP_EXPERIMENTAL_NOMAD_FETCH_POLL: u16 = 0xf009;
/// Read the redacted desired Wi-Fi and Reticulum TCP configuration.
#[cfg(feature = "experimental-network-config")]
pub const OP_EXPERIMENTAL_NETWORK_CONFIG_GET: u16 = 0xf00a;
/// Mutate one saved Wi-Fi profile or the single Reticulum TCP peer.
#[cfg(feature = "experimental-network-config")]
pub const OP_EXPERIMENTAL_NETWORK_CONFIG_MUTATE: u16 = 0xf00b;
/// Read live Wi-Fi and Reticulum TCP interface state.
#[cfg(feature = "experimental-network-config")]
pub const OP_EXPERIMENTAL_NETWORK_STATUS: u16 = 0xf00c;
/// Queue the node's ordinary Reticulum service announces immediately.
pub const OP_EXPERIMENTAL_MANUAL_SERVICE_ANNOUNCE: u16 = 0xf00d;
/// Read one bounded cross-interface node diagnostics snapshot.
pub const OP_EXPERIMENTAL_NODE_DIAGNOSTICS: u16 = 0xf00e;
/// Read one bounded lexicographically ordered Reticulum route page.
pub const OP_EXPERIMENTAL_ROUTE_DIAGNOSTICS_PAGE: u16 = 0xf00f;
/// Read durable LXMF mailbox collection state.
#[cfg(feature = "experimental-lxmf")]
pub const OP_EXPERIMENTAL_LXMF_MAILBOX_STATUS: u16 = 0xf010;
/// Monotonically acknowledge locally collected LXMF messages.
#[cfg(feature = "experimental-lxmf")]
pub const OP_EXPERIMENTAL_LXMF_MAILBOX_ACKNOWLEDGE: u16 = 0xf011;
/// Begin one boot-scoped Reticulum path-and-proof probe.
pub const OP_EXPERIMENTAL_RETICULUM_PROBE_START: u16 = 0xf012;
/// Poll one principal-owned Reticulum path-and-proof probe.
pub const OP_EXPERIMENTAL_RETICULUM_PROBE_POLL: u16 = 0xf013;
/// Read one bounded boot-scoped packet-correlated radio trace page.
pub const OP_EXPERIMENTAL_RADIO_TRACE_PAGE: u16 = 0xf014;

/// Major/minor logical protocol version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApiVersion {
    /// Incompatible protocol generation.
    pub major: u16,
    /// Backward-compatible feature revision within a major generation.
    pub minor: u16,
}

impl ApiVersion {
    /// Version implemented by this crate.
    pub const CURRENT: Self = Self {
        major: API_VERSION_MAJOR,
        minor: API_VERSION_MINOR,
    };
}

/// Client-chosen identifier echoed in the corresponding response.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RequestId(pub u64);

/// Authenticated local-client principal derived from device-owned authority.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PrincipalId(pub [u8; 16]);

/// Client-chosen key used to deduplicate a state-changing operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct IdempotencyKey(pub [u8; 16]);

/// One phone-sourced location snapshot attached to an LXMF message.
///
/// The fixed-point representation is intentionally identical to the semantic
/// values carried by Sideband's LXMF telemetry location sensor. The device
/// owns the MessagePack encoding and never accepts caller-supplied fields.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LxmfMessageLocation {
    latitude_e6: i32,
    longitude_e6: i32,
    altitude_cm: i32,
    speed_cm_per_second: u32,
    bearing_centidegrees: i32,
    accuracy_cm: u16,
    updated_at_unix_seconds: u32,
}

#[cfg(feature = "experimental-lxmf")]
impl LxmfMessageLocation {
    /// Validate one complete Sideband-compatible location sample.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        latitude_e6: i32,
        longitude_e6: i32,
        altitude_cm: i32,
        speed_cm_per_second: u32,
        bearing_centidegrees: i32,
        accuracy_cm: u16,
        updated_at_unix_seconds: u32,
    ) -> Result<Self, InvalidLxmfMessageLocation> {
        if latitude_e6 < -90_000_000 || latitude_e6 > 90_000_000 {
            Err(InvalidLxmfMessageLocation::LatitudeOutOfRange)
        } else if longitude_e6 < -180_000_000 || longitude_e6 > 180_000_000 {
            Err(InvalidLxmfMessageLocation::LongitudeOutOfRange)
        } else {
            Ok(Self {
                latitude_e6,
                longitude_e6,
                altitude_cm,
                speed_cm_per_second,
                bearing_centidegrees,
                accuracy_cm,
                updated_at_unix_seconds,
            })
        }
    }

    /// Latitude in signed decimal microdegrees.
    pub const fn latitude_e6(self) -> i32 {
        self.latitude_e6
    }

    /// Longitude in signed decimal microdegrees.
    pub const fn longitude_e6(self) -> i32 {
        self.longitude_e6
    }

    /// Altitude above mean sea level in centimetres, or zero when unavailable.
    pub const fn altitude_cm(self) -> i32 {
        self.altitude_cm
    }

    /// Ground speed in centimetres per second, or zero when unavailable.
    pub const fn speed_cm_per_second(self) -> u32 {
        self.speed_cm_per_second
    }

    /// Bearing in hundredths of a degree, or zero when unavailable.
    pub const fn bearing_centidegrees(self) -> i32 {
        self.bearing_centidegrees
    }

    /// Horizontal accuracy in centimetres, or zero when unavailable.
    pub const fn accuracy_cm(self) -> u16 {
        self.accuracy_cm
    }

    /// Time of the location fix in whole Unix seconds.
    pub const fn updated_at_unix_seconds(self) -> u32 {
        self.updated_at_unix_seconds
    }
}

/// Why an LXMF message location was rejected.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidLxmfMessageLocation {
    /// Latitude was outside the world bounds.
    LatitudeOutOfRange,
    /// Longitude was outside the world bounds.
    LongitudeOutOfRange,
}

/// Validated borrowed Wi-Fi service-set identifier.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct WifiSsid<'a>(&'a [u8]);

#[cfg(feature = "experimental-network-config")]
impl<'a> WifiSsid<'a> {
    /// Validate a non-empty SSID within the IEEE 802.11 byte limit.
    pub const fn new(bytes: &'a [u8]) -> Result<Self, InvalidWifiSsid> {
        if bytes.is_empty() {
            Err(InvalidWifiSsid::Empty)
        } else if bytes.len() > MAX_WIFI_SSID_BYTES {
            Err(InvalidWifiSsid::TooLong {
                actual: bytes.len(),
            })
        } else {
            Ok(Self(bytes))
        }
    }

    /// Borrow the complete SSID bytes.
    pub const fn as_bytes(self) -> &'a [u8] {
        self.0
    }
}

#[cfg(feature = "experimental-network-config")]
impl core::fmt::Debug for WifiSsid<'_> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("WifiSsid")
            .field("bytes", &self.0)
            .finish()
    }
}

/// Why a Wi-Fi SSID was rejected.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidWifiSsid {
    /// An empty SSID cannot identify a saved network.
    Empty,
    /// The SSID exceeded [`MAX_WIFI_SSID_BYTES`].
    TooLong {
        /// Rejected byte count.
        actual: usize,
    },
}

/// How a WPA2-Personal profile mutation changes the stored credential.
///
/// Debug formatting never exposes replacement bytes.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum WifiCredentialUpdate<'a> {
    /// Retain the existing credential.
    Keep,
    /// Replace the credential with a validated WPA2-Personal passphrase.
    Replace(&'a [u8]),
}

#[cfg(feature = "experimental-network-config")]
impl<'a> WifiCredentialUpdate<'a> {
    /// Construct a redacted replacement update from a WPA2-Personal passphrase.
    pub const fn replace(passphrase: &'a [u8]) -> Result<Self, InvalidWifiPassphrase> {
        if passphrase.len() < MIN_WIFI_PASSPHRASE_BYTES {
            return Err(InvalidWifiPassphrase::TooShort {
                actual: passphrase.len(),
            });
        } else if passphrase.len() > MAX_WIFI_PASSPHRASE_BYTES {
            return Err(InvalidWifiPassphrase::TooLong {
                actual: passphrase.len(),
            });
        }
        let mut index = 0;
        while index < passphrase.len() {
            if passphrase[index] < 0x20 || passphrase[index] > 0x7e {
                return Err(InvalidWifiPassphrase::NonPrintableAscii);
            }
            index += 1;
        }
        Ok(Self::Replace(passphrase))
    }

    /// Borrow replacement bytes only when this update replaces the secret.
    pub const fn replacement(self) -> Option<&'a [u8]> {
        match self {
            Self::Replace(bytes) => Some(bytes),
            Self::Keep => None,
        }
    }

    /// Frozen experimental discriminator.
    pub const fn wire_code(self) -> u8 {
        match self {
            Self::Keep => 0,
            Self::Replace(_) => 1,
        }
    }
}

#[cfg(feature = "experimental-network-config")]
impl core::fmt::Debug for WifiCredentialUpdate<'_> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Keep => formatter.write_str("Keep"),
            Self::Replace(_) => formatter.write_str("Replace(<redacted>)"),
        }
    }
}

/// Why a WPA2-Personal passphrase was rejected.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidWifiPassphrase {
    /// The passphrase was shorter than [`MIN_WIFI_PASSPHRASE_BYTES`].
    TooShort {
        /// Rejected byte count.
        actual: usize,
    },
    /// The passphrase exceeded [`MAX_WIFI_PASSPHRASE_BYTES`].
    TooLong {
        /// Rejected byte count.
        actual: usize,
    },
    /// The passphrase contained a byte outside printable ASCII.
    NonPrintableAscii,
}

/// Desired WPA2-Personal Wi-Fi station profile in one mutation.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WifiNetworkUpdate<'a> {
    enabled: bool,
    priority: u8,
    ssid: WifiSsid<'a>,
    credential: WifiCredentialUpdate<'a>,
}

#[cfg(feature = "experimental-network-config")]
impl<'a> WifiNetworkUpdate<'a> {
    /// Construct a complete WPA2-Personal profile update.
    ///
    /// A higher layer rejects `Keep` when the named profile has no stored
    /// credential.
    pub const fn new(
        enabled: bool,
        priority: u8,
        ssid: WifiSsid<'a>,
        credential: WifiCredentialUpdate<'a>,
    ) -> Self {
        Self {
            enabled,
            priority,
            ssid,
            credential,
        }
    }

    /// Whether the saved network should be used.
    pub const fn enabled(self) -> bool {
        self.enabled
    }

    /// Station-selection priority; larger values are preferred.
    pub const fn priority(self) -> u8 {
        self.priority
    }

    /// Saved Wi-Fi SSID.
    pub const fn ssid(self) -> WifiSsid<'a> {
        self.ssid
    }

    /// Requested secret update.
    pub const fn credential(self) -> WifiCredentialUpdate<'a> {
        self.credential
    }
}

/// Validated IPv4 address for the single outbound Reticulum TCP peer.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ReticulumTcpPeerIpv4Address([u8; 4]);

#[cfg(feature = "experimental-network-config")]
impl ReticulumTcpPeerIpv4Address {
    /// Reject IPv4 ranges that cannot name a routable unicast peer.
    pub const fn new(octets: [u8; 4]) -> Result<Self, InvalidReticulumTcpPeerIpv4Address> {
        if octets[0] == 0 {
            Err(InvalidReticulumTcpPeerIpv4Address::CurrentNetwork)
        } else if octets[0] == 127 {
            Err(InvalidReticulumTcpPeerIpv4Address::Loopback)
        } else if octets[0] >= 224 && octets[0] <= 239 {
            Err(InvalidReticulumTcpPeerIpv4Address::Multicast)
        } else if octets[0] == 255 && octets[1] == 255 && octets[2] == 255 && octets[3] == 255 {
            Err(InvalidReticulumTcpPeerIpv4Address::LimitedBroadcast)
        } else if octets[0] >= 240 {
            Err(InvalidReticulumTcpPeerIpv4Address::Reserved)
        } else {
            Ok(Self(octets))
        }
    }

    /// Exact network-order IPv4 octets.
    pub const fn octets(self) -> [u8; 4] {
        self.0
    }
}

/// Why a Reticulum TCP peer IPv4 address was rejected.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidReticulumTcpPeerIpv4Address {
    /// The `0.0.0.0/8` current-network range cannot name a remote peer.
    CurrentNetwork,
    /// The `127.0.0.0/8` loopback range cannot name a remote appliance peer.
    Loopback,
    /// IPv4 multicast cannot name a TCP peer.
    Multicast,
    /// `255.255.255.255` cannot name a TCP peer.
    LimitedBroadcast,
    /// The `240.0.0.0/4` reserved range cannot name a routable peer.
    Reserved,
}

/// Validated borrowed ASCII DNS hostname for an outbound Reticulum TCP peer.
///
/// Hostnames use one or more dot-separated labels. Labels contain only ASCII
/// letters, digits, or interior hyphens and are at most 63 bytes each.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ReticulumTcpPeerHostname<'a>(&'a str);

#[cfg(feature = "experimental-network-config")]
impl<'a> ReticulumTcpPeerHostname<'a> {
    /// Validate a bounded DNS hostname without allocating or normalizing it.
    pub const fn new(hostname: &'a str) -> Result<Self, InvalidReticulumTcpPeerHostname> {
        let bytes = hostname.as_bytes();
        if bytes.is_empty() {
            return Err(InvalidReticulumTcpPeerHostname::Empty);
        }
        if bytes.len() > MAX_RETICULUM_TCP_PEER_HOSTNAME_BYTES {
            return Err(InvalidReticulumTcpPeerHostname::TooLong {
                actual: bytes.len(),
            });
        }

        let mut index = 0;
        let mut label_len = 0;
        let mut label_starts_with_hyphen = false;
        while index < bytes.len() {
            let byte = bytes[index];
            if byte == b'.' {
                if label_len == 0 || label_starts_with_hyphen || bytes[index - 1] == b'-' {
                    return Err(InvalidReticulumTcpPeerHostname::InvalidLabel);
                }
                label_len = 0;
                label_starts_with_hyphen = false;
            } else {
                let valid = (byte >= b'a' && byte <= b'z')
                    || (byte >= b'A' && byte <= b'Z')
                    || (byte >= b'0' && byte <= b'9')
                    || byte == b'-';
                if !valid {
                    return Err(InvalidReticulumTcpPeerHostname::InvalidCharacter);
                }
                if label_len == 0 {
                    label_starts_with_hyphen = byte == b'-';
                }
                label_len += 1;
                if label_len > 63 {
                    return Err(InvalidReticulumTcpPeerHostname::LabelTooLong);
                }
            }
            index += 1;
        }
        if label_len == 0 || label_starts_with_hyphen || bytes[bytes.len() - 1] == b'-' {
            return Err(InvalidReticulumTcpPeerHostname::InvalidLabel);
        }
        Ok(Self(hostname))
    }

    /// Borrow the exact validated hostname.
    pub const fn as_str(self) -> &'a str {
        self.0
    }
}

/// Why an outbound Reticulum TCP peer hostname was rejected.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidReticulumTcpPeerHostname {
    /// A hostname must contain at least one label.
    Empty,
    /// The complete hostname exceeded the fixed API limit.
    TooLong {
        /// Rejected UTF-8 byte count.
        actual: usize,
    },
    /// A byte was not an ASCII letter, digit, dot, or hyphen.
    InvalidCharacter,
    /// A label was empty or began or ended with a hyphen.
    InvalidLabel,
    /// A single DNS label exceeded 63 bytes.
    LabelTooLong,
}

/// Desired single outbound Reticulum TCP peer in a configuration mutation.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReticulumTcpPeerUpdate {
    enabled: bool,
    ipv4_address: ReticulumTcpPeerIpv4Address,
    port: u16,
}

#[cfg(feature = "experimental-network-config")]
impl ReticulumTcpPeerUpdate {
    /// Construct a peer, rejecting the reserved port zero.
    pub const fn new(
        enabled: bool,
        ipv4_address: ReticulumTcpPeerIpv4Address,
        port: u16,
    ) -> Result<Self, InvalidReticulumTcpPeerPort> {
        if port == 0 {
            Err(InvalidReticulumTcpPeerPort)
        } else {
            Ok(Self {
                enabled,
                ipv4_address,
                port,
            })
        }
    }

    /// Whether this TCP peer should be connected.
    pub const fn enabled(self) -> bool {
        self.enabled
    }

    /// Configured exact IPv4 address.
    pub const fn ipv4_address(self) -> ReticulumTcpPeerIpv4Address {
        self.ipv4_address
    }

    /// Configured TCP port.
    pub const fn port(self) -> u16 {
        self.port
    }
}

/// Desired hostname-based outbound Reticulum TCP peer in a configuration mutation.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReticulumTcpPeerHostUpdate<'a> {
    enabled: bool,
    hostname: ReticulumTcpPeerHostname<'a>,
    port: u16,
}

#[cfg(feature = "experimental-network-config")]
impl<'a> ReticulumTcpPeerHostUpdate<'a> {
    /// Construct a hostname peer, rejecting the reserved port zero.
    pub const fn new(
        enabled: bool,
        hostname: ReticulumTcpPeerHostname<'a>,
        port: u16,
    ) -> Result<Self, InvalidReticulumTcpPeerPort> {
        if port == 0 {
            Err(InvalidReticulumTcpPeerPort)
        } else {
            Ok(Self {
                enabled,
                hostname,
                port,
            })
        }
    }

    /// Whether this TCP peer should be connected.
    pub const fn enabled(self) -> bool {
        self.enabled
    }

    /// Configured DNS hostname.
    pub const fn hostname(self) -> ReticulumTcpPeerHostname<'a> {
        self.hostname
    }

    /// Configured TCP port.
    pub const fn port(self) -> u16 {
        self.port
    }
}

/// TCP port zero is not a valid peer endpoint.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidReticulumTcpPeerPort;

/// Opaque nonzero Wi-Fi profile identity.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WifiNetworkProfileId([u8; 16]);

#[cfg(feature = "experimental-network-config")]
impl WifiNetworkProfileId {
    /// Reject the all-zero erased identity.
    pub const fn new(bytes: [u8; 16]) -> Result<Self, InvalidWifiNetworkProfileId> {
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] != 0 {
                return Ok(Self(bytes));
            }
            index += 1;
        }
        Err(InvalidWifiNetworkProfileId)
    }

    /// Borrow all opaque identity bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// The all-zero Wi-Fi profile identity is reserved for erased state.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidWifiNetworkProfileId;

/// Desired gateway-wide policy independent of individual saved records.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GatewayPolicy {
    wifi_transport_enabled: bool,
    automatic_announces_enabled: bool,
}

#[cfg(feature = "experimental-network-config")]
impl GatewayPolicy {
    /// Construct the complete gateway policy.
    pub const fn new(wifi_transport_enabled: bool, automatic_announces_enabled: bool) -> Self {
        Self {
            wifi_transport_enabled,
            automatic_announces_enabled,
        }
    }

    /// Whether the Wi-Fi station and Reticulum TCP transport may run.
    pub const fn wifi_transport_enabled(self) -> bool {
        self.wifi_transport_enabled
    }

    /// Whether the firmware may emit scheduled ordinary service announces.
    pub const fn automatic_announces_enabled(self) -> bool {
        self.automatic_announces_enabled
    }
}

/// Phone-sourced position represented as decimal degrees multiplied by one million.
///
/// Integer microdegrees keep the bearer-neutral wire deterministic while
/// preserving substantially more precision than a phone location intended for
/// a public network map requires.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RmapLocation {
    latitude_e6: i32,
    longitude_e6: i32,
}

#[cfg(feature = "experimental-network-config")]
impl RmapLocation {
    /// Validate one world-bounded latitude/longitude pair.
    pub const fn new(latitude_e6: i32, longitude_e6: i32) -> Result<Self, InvalidRmapLocation> {
        if latitude_e6 < -90_000_000 || latitude_e6 > 90_000_000 {
            Err(InvalidRmapLocation::LatitudeOutOfRange {
                actual: latitude_e6,
            })
        } else if longitude_e6 < -180_000_000 || longitude_e6 > 180_000_000 {
            Err(InvalidRmapLocation::LongitudeOutOfRange {
                actual: longitude_e6,
            })
        } else {
            Ok(Self {
                latitude_e6,
                longitude_e6,
            })
        }
    }

    /// Signed latitude in decimal degrees multiplied by one million.
    pub const fn latitude_e6(self) -> i32 {
        self.latitude_e6
    }

    /// Signed longitude in decimal degrees multiplied by one million.
    pub const fn longitude_e6(self) -> i32 {
        self.longitude_e6
    }
}

/// Why a phone-sourced RMAP position was rejected.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidRmapLocation {
    /// Latitude was outside inclusive `-90_000_000..=90_000_000`.
    LatitudeOutOfRange {
        /// Rejected fixed-point latitude.
        actual: i32,
    },
    /// Longitude was outside inclusive `-180_000_000..=180_000_000`.
    LongitudeOutOfRange {
        /// Rejected fixed-point longitude.
        actual: i32,
    },
}

/// Complete opt-in configuration for RMAP discovery and location publication.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RmapConfig {
    discovery_enabled: bool,
    share_location: bool,
    phone_location: Option<RmapLocation>,
}

#[cfg(feature = "experimental-network-config")]
impl RmapConfig {
    /// Construct the complete RMAP policy and optional phone-sourced position.
    pub const fn new(
        discovery_enabled: bool,
        share_location: bool,
        phone_location: Option<RmapLocation>,
    ) -> Self {
        Self {
            discovery_enabled,
            share_location,
            phone_location,
        }
    }

    /// Whether the node may publish signed RMAP discovery announces.
    pub const fn discovery_enabled(self) -> bool {
        self.discovery_enabled
    }

    /// Whether a present phone position may be included in RMAP publication.
    pub const fn share_location(self) -> bool {
        self.share_location
    }

    /// Optional latest phone-sourced position.
    pub const fn phone_location(self) -> Option<RmapLocation> {
        self.phone_location
    }
}

/// Validated requested LoRa transmit power in whole dBm.
///
/// The experimental appliance profile deliberately exposes only the four
/// board-qualified power rows used by the E290 radio owner. This is a
/// requested radio output, not a calibrated conducted-power or EIRP claim.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LoraTransmitPowerDbm(u8);

#[cfg(feature = "experimental-network-config")]
impl LoraTransmitPowerDbm {
    /// Lowest supported requested output.
    pub const DBM_14: Self = Self(14);
    /// Second supported requested output.
    pub const DBM_17: Self = Self(17);
    /// Third supported requested output.
    pub const DBM_20: Self = Self(20);
    /// Highest supported requested output.
    pub const DBM_22: Self = Self(22);
    /// Backward-compatible output used when older configuration omits power.
    pub const DEFAULT: Self = Self::DBM_14;

    /// Validate one requested whole-dBm output.
    pub const fn new(value: u8) -> Result<Self, InvalidLoraTransmitPowerDbm> {
        match value {
            14 | 17 | 20 | 22 => Ok(Self(value)),
            _ => Err(InvalidLoraTransmitPowerDbm { actual: value }),
        }
    }

    /// Requested whole-dBm output.
    pub const fn get(self) -> u8 {
        self.0
    }
}

#[cfg(feature = "experimental-network-config")]
impl Default for LoraTransmitPowerDbm {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A requested LoRa transmit power was not one of the qualified values.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidLoraTransmitPowerDbm {
    actual: u8,
}

#[cfg(feature = "experimental-network-config")]
impl InvalidLoraTransmitPowerDbm {
    /// Rejected whole-dBm value.
    pub const fn actual(self) -> u8 {
        self.actual
    }
}

/// Complete LoRa compatibility profile saved for the next radio start.
///
/// This API-level value validates transport-neutral numeric shape. A product
/// still owns fitted RF-range, regional transmission, and driver-interoperability
/// validation before accepting a mutation.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LoraRadioProfile {
    frequency_hz: u32,
    bandwidth_hz: u32,
    spreading_factor: u8,
    coding_rate_denominator: u8,
    tx_power_dbm: LoraTransmitPowerDbm,
}

#[cfg(feature = "experimental-network-config")]
impl LoraRadioProfile {
    /// Backward-compatible profile used when an older peer omits modulation.
    pub const DEFAULT: Self = Self {
        frequency_hz: 915_000_000,
        bandwidth_hz: 125_000,
        spreading_factor: 7,
        coding_rate_denominator: 5,
        tx_power_dbm: LoraTransmitPowerDbm::DEFAULT,
    };

    /// Validate and construct one complete profile.
    pub const fn new(
        frequency_hz: u32,
        bandwidth_hz: u32,
        spreading_factor: u8,
        coding_rate_denominator: u8,
        tx_power_dbm: LoraTransmitPowerDbm,
    ) -> Result<Self, InvalidLoraRadioProfile> {
        if frequency_hz == 0 {
            return Err(InvalidLoraRadioProfile::Frequency);
        }
        if !matches!(
            bandwidth_hz,
            7_810
                | 10_420
                | 15_630
                | 20_830
                | 31_250
                | 41_670
                | 62_500
                | 125_000
                | 250_000
                | 500_000
        ) {
            return Err(InvalidLoraRadioProfile::Bandwidth);
        }
        if spreading_factor < 7 || spreading_factor > 12 {
            return Err(InvalidLoraRadioProfile::SpreadingFactor);
        }
        if coding_rate_denominator < 5 || coding_rate_denominator > 8 {
            return Err(InvalidLoraRadioProfile::CodingRate);
        }
        Ok(Self {
            frequency_hz,
            bandwidth_hz,
            spreading_factor,
            coding_rate_denominator,
            tx_power_dbm,
        })
    }

    /// Center frequency in whole hertz.
    pub const fn frequency_hz(self) -> u32 {
        self.frequency_hz
    }

    /// Canonical LoRa bandwidth in whole hertz.
    pub const fn bandwidth_hz(self) -> u32 {
        self.bandwidth_hz
    }

    /// LoRa spreading factor.
    pub const fn spreading_factor(self) -> u8 {
        self.spreading_factor
    }

    /// Denominator of coding rate `4/n`.
    pub const fn coding_rate_denominator(self) -> u8 {
        self.coding_rate_denominator
    }

    /// Requested whole-dBm radio output.
    pub const fn tx_power_dbm(self) -> LoraTransmitPowerDbm {
        self.tx_power_dbm
    }

    /// Preserve modulation while replacing only the power field.
    pub const fn with_tx_power(self, tx_power_dbm: LoraTransmitPowerDbm) -> Self {
        Self {
            tx_power_dbm,
            ..self
        }
    }
}

#[cfg(feature = "experimental-network-config")]
impl Default for LoraRadioProfile {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Invalid numeric field in a requested LoRa profile.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidLoraRadioProfile {
    /// Center frequency was zero.
    Frequency,
    /// Bandwidth was not one of the canonical SX1262 widths.
    Bandwidth,
    /// Spreading factor was outside 7 through 12.
    SpreadingFactor,
    /// Coding-rate denominator was outside 5 through 8.
    CodingRate,
}

/// One bounded desired-network mutation.
///
/// At most one Wi-Fi profile changes per request, keeping secret-bearing
/// messages comfortably inside the fixed logical envelope.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkConfigMutation<'a> {
    /// Create or replace one Wi-Fi profile record.
    UpsertWifi {
        /// Stable opaque profile identity.
        profile_id: WifiNetworkProfileId,
        /// Complete desired profile.
        network: WifiNetworkUpdate<'a>,
    },
    /// Remove one Wi-Fi profile record.
    RemoveWifi {
        /// Stable opaque profile identity.
        profile_id: WifiNetworkProfileId,
    },
    /// Replace or clear the IPv4 outbound Reticulum TCP peer.
    ///
    /// Applying this mutation also clears any hostname peer so the desired
    /// endpoint remains unambiguous.
    ReplaceTcpPeer(Option<ReticulumTcpPeerUpdate>),
    /// Replace or clear the hostname-based outbound Reticulum TCP peer.
    ///
    /// Applying this mutation also clears any IPv4 peer so the desired
    /// endpoint remains unambiguous.
    ReplaceTcpHostPeer(Option<ReticulumTcpPeerHostUpdate<'a>>),
    /// Replace gateway-wide Wi-Fi and automatic-announce policy.
    SetGatewayPolicy(GatewayPolicy),
    /// Replace opt-in RMAP discovery and location-sharing configuration.
    SetRmapConfig(RmapConfig),
    /// Replace the requested LoRa transmit power.
    SetLoraTxPower(LoraTransmitPowerDbm),
    /// Atomically replace frequency, bandwidth, spreading factor, coding rate,
    /// and requested transmit power.
    SetLoraProfile(LoraRadioProfile),
}

/// Correlated compare-and-swap request for one desired-network mutation.
///
/// Debug output remains safe because [`WifiCredentialUpdate`] redacts
/// replacement bytes.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NetworkConfigMutationRequest<'a> {
    mutation: NetworkConfigMutation<'a>,
    expected_revision: u64,
    idempotency_key: IdempotencyKey,
}

#[cfg(feature = "experimental-network-config")]
impl<'a> NetworkConfigMutationRequest<'a> {
    /// Construct one compare-and-swap desired-network mutation.
    pub const fn new(
        mutation: NetworkConfigMutation<'a>,
        expected_revision: u64,
        idempotency_key: IdempotencyKey,
    ) -> Self {
        Self {
            mutation,
            expected_revision,
            idempotency_key,
        }
    }

    /// Requested bounded mutation.
    pub const fn mutation(self) -> NetworkConfigMutation<'a> {
        self.mutation
    }

    /// Complete configuration revision required for this mutation.
    ///
    /// Revision zero names the exactly-erased empty configuration.
    pub const fn expected_revision(self) -> u64 {
        self.expected_revision
    }

    /// Client correlation key for safely resolving an ambiguous response.
    ///
    /// The firmware may recognize an exact already-applied mutation, but this
    /// key is not durable replay authority by itself.
    pub const fn idempotency_key(self) -> IdempotencyKey {
        self.idempotency_key
    }
}

/// Complete 128-bit Reticulum destination hash.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DestinationHash(pub [u8; 16]);

/// Public 128-bit hash of a Reticulum identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct IdentityHash([u8; 16]);

impl IdentityHash {
    /// Construct a public identity hash from all wire bytes.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Borrow all public hash bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Physical or logical transport family represented by diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticInterfaceKind {
    /// A LoRa packet-radio interface.
    LoRa,
    /// A Reticulum TCP client or server interface.
    Tcp,
    /// Another transport family not yet represented by a stable category.
    Other,
}

impl DiagnosticInterfaceKind {
    /// Frozen API-v1 numeric representation.
    pub const fn wire_code(self) -> u8 {
        match self {
            Self::LoRa => 0,
            Self::Tcp => 1,
            Self::Other => 2,
        }
    }
}

/// Current usable state of one configured interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticInterfaceState {
    /// The interface is configured but not currently usable.
    Offline,
    /// The interface is online and eligible for Reticulum traffic.
    Online,
    /// The interface owner has latched a fault.
    Faulted,
}

impl DiagnosticInterfaceState {
    /// Frozen API-v1 numeric representation.
    pub const fn wire_code(self) -> u8 {
        match self {
            Self::Offline => 0,
            Self::Online => 1,
            Self::Faulted => 2,
        }
    }
}

/// One fixed-capacity interface record in a node diagnostics snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticInterfaceRecord {
    id: u8,
    kind: DiagnosticInterfaceKind,
    state: DiagnosticInterfaceState,
    generation: u64,
    logical_mtu: u16,
    bitrate: Option<u32>,
}

impl DiagnosticInterfaceRecord {
    /// Construct one complete interface record.
    pub const fn new(
        id: u8,
        kind: DiagnosticInterfaceKind,
        state: DiagnosticInterfaceState,
        generation: u64,
        logical_mtu: u16,
        bitrate: Option<u32>,
    ) -> Self {
        Self {
            id,
            kind,
            state,
            generation,
            logical_mtu,
            bitrate,
        }
    }

    /// Product-owned interface identifier.
    pub const fn id(self) -> u8 {
        self.id
    }

    /// Stable transport family.
    pub const fn kind(self) -> DiagnosticInterfaceKind {
        self.kind
    }

    /// Current usable state.
    pub const fn state(self) -> DiagnosticInterfaceState {
        self.state
    }

    /// Product-owned incarnation or reconfiguration generation.
    pub const fn generation(self) -> u64 {
        self.generation
    }

    /// Maximum logical Reticulum packet bytes accepted by this interface.
    pub const fn logical_mtu(self) -> u16 {
        self.logical_mtu
    }

    /// Approximate raw interface bitrate, when meaningful and known.
    pub const fn bitrate(self) -> Option<u32> {
        self.bitrate
    }
}

/// Stable terminal category for the most recent LoRa transmission job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticLoraTxOutcome {
    /// Every physical frame in the job completed successfully.
    Completed,
    /// Channel-access policy rejected the job before successful completion.
    AccessRejected,
    /// Radio setup, transmission, or completion failed.
    Failed,
}

impl DiagnosticLoraTxOutcome {
    /// Frozen API-v1 numeric representation.
    pub const fn wire_code(self) -> u8 {
        match self {
            Self::Completed => 0,
            Self::AccessRejected => 1,
            Self::Failed => 2,
        }
    }
}

/// Packet-owner family of one terminal LoRa dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticLoraTxFamily {
    /// Destination DATA associated with a durable application attempt.
    Data,
    /// Ordinary Reticulum control or application packet.
    Ordinary,
}

impl DiagnosticLoraTxFamily {
    /// Frozen API-v1 numeric representation.
    pub const fn wire_code(self) -> u8 {
        match self {
            Self::Data => 0,
            Self::Ordinary => 1,
        }
    }
}

/// Prepared-packet identity for one terminal LoRa DATA dispatch.
///
/// This evidence is available for pre-authorization failures and therefore
/// does not assert RF exposure. Length and digest intentionally match the
/// message timeline's encoded-packet evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticLoraDataTxEvidence {
    interface_id: u8,
    encoded_packet_len: NonZeroU16,
    encoded_packet_sha256: EncodedPacketSha256,
}

impl DiagnosticLoraDataTxEvidence {
    /// Construct exact prepared DATA packet evidence.
    ///
    /// A complete encoded Reticulum packet cannot be empty.
    pub const fn try_new(
        interface_id: u8,
        encoded_packet_len: u16,
        encoded_packet_sha256: EncodedPacketSha256,
    ) -> Option<Self> {
        let Some(encoded_packet_len) = NonZeroU16::new(encoded_packet_len) else {
            return None;
        };
        Some(Self {
            interface_id,
            encoded_packet_len,
            encoded_packet_sha256,
        })
    }

    /// Exact Reticulum interface selected for this dispatch attempt.
    pub const fn interface_id(self) -> u8 {
        self.interface_id
    }

    /// Complete encoded interface-packet length.
    pub const fn encoded_packet_len(self) -> u16 {
        self.encoded_packet_len.get()
    }

    /// SHA-256 over every byte in the complete encoded interface packet.
    pub const fn encoded_packet_sha256(self) -> EncodedPacketSha256 {
        self.encoded_packet_sha256
    }
}

/// Conservative signal metadata for the most recently accepted logical LoRa
/// packet.
///
/// A single-frame packet reports that frame. A split packet reports the
/// field-wise weaker RSSI and SNR across both frames. A later invalid or
/// over-MTU physical frame does not replace this record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticLoraLastRx {
    age_ms: u64,
    rssi_dbm: i16,
    snr_db: i16,
}

impl DiagnosticLoraLastRx {
    /// Construct one conservative signal observation for an accepted packet.
    pub const fn new(age_ms: u64, rssi_dbm: i16, snr_db: i16) -> Self {
        Self {
            age_ms,
            rssi_dbm,
            snr_db,
        }
    }

    /// Saturating observation age at snapshot time.
    pub const fn age_ms(self) -> u64 {
        self.age_ms
    }

    /// Whole-dBm received signal strength.
    pub const fn rssi_dbm(self) -> i16 {
        self.rssi_dbm
    }

    /// Whole-dB signal-to-noise ratio.
    pub const fn snr_db(self) -> i16 {
        self.snr_db
    }
}

/// Terminal metadata for the most recent LoRa transmission job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticLoraLastTx {
    age_ms: u64,
    outcome: DiagnosticLoraTxOutcome,
    family: Option<DiagnosticLoraTxFamily>,
    data: Option<DiagnosticLoraDataTxEvidence>,
}

/// Most recent terminal DATA dispatch retained across later ordinary packets.
///
/// This dedicated type makes the LoRa key-18 slot incapable of containing a
/// legacy or ordinary last-TX record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticLoraLastDataTx {
    age_ms: u64,
    outcome: DiagnosticLoraTxOutcome,
    data: DiagnosticLoraDataTxEvidence,
}

impl DiagnosticLoraLastDataTx {
    /// Construct one retained DATA terminal observation.
    pub const fn new(
        age_ms: u64,
        outcome: DiagnosticLoraTxOutcome,
        data: DiagnosticLoraDataTxEvidence,
    ) -> Self {
        Self {
            age_ms,
            outcome,
            data,
        }
    }

    /// Saturating terminal-event age at snapshot time.
    pub const fn age_ms(self) -> u64 {
        self.age_ms
    }

    /// Stable terminal result category.
    pub const fn outcome(self) -> DiagnosticLoraTxOutcome {
        self.outcome
    }

    /// Exact prepared DATA packet evidence.
    pub const fn data_evidence(self) -> DiagnosticLoraDataTxEvidence {
        self.data
    }
}

impl DiagnosticLoraLastTx {
    /// Construct one legacy observation without packet-family evidence.
    ///
    /// New producers should use [`Self::ordinary`] or [`Self::data`]. This
    /// constructor remains for same-major decoding and older callers.
    pub const fn new(age_ms: u64, outcome: DiagnosticLoraTxOutcome) -> Self {
        Self {
            age_ms,
            outcome,
            family: None,
            data: None,
        }
    }

    /// Construct one ordinary-packet terminal observation.
    pub const fn ordinary(age_ms: u64, outcome: DiagnosticLoraTxOutcome) -> Self {
        Self {
            age_ms,
            outcome,
            family: Some(DiagnosticLoraTxFamily::Ordinary),
            data: None,
        }
    }

    /// Construct one DATA terminal observation with prepared-packet evidence.
    pub const fn data(
        age_ms: u64,
        outcome: DiagnosticLoraTxOutcome,
        data: DiagnosticLoraDataTxEvidence,
    ) -> Self {
        Self {
            age_ms,
            outcome,
            family: Some(DiagnosticLoraTxFamily::Data),
            data: Some(data),
        }
    }

    pub(crate) const fn from_wire(
        age_ms: u64,
        outcome: DiagnosticLoraTxOutcome,
        family: Option<DiagnosticLoraTxFamily>,
        data: Option<DiagnosticLoraDataTxEvidence>,
    ) -> Self {
        Self {
            age_ms,
            outcome,
            family,
            data,
        }
    }

    /// Saturating terminal-event age at snapshot time.
    pub const fn age_ms(self) -> u64 {
        self.age_ms
    }

    /// Stable terminal result category.
    pub const fn outcome(self) -> DiagnosticLoraTxOutcome {
        self.outcome
    }

    /// Packet-owner family, absent for an API 1.14-compatible record.
    pub const fn family(self) -> Option<DiagnosticLoraTxFamily> {
        self.family
    }

    /// Prepared DATA packet evidence, present only for a DATA record.
    pub const fn data_evidence(self) -> Option<DiagnosticLoraDataTxEvidence> {
        self.data
    }
}

/// Bounded LoRa radio and scheduler diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoraDiagnostics {
    applied_tx_power_dbm: i16,
    frequency_hz: u32,
    bandwidth_hz: u32,
    spreading_factor: u8,
    coding_rate_denominator: u8,
    rx_physical_frames: u64,
    rx_packets: u64,
    rx_errors: u64,
    rx_drops: u64,
    tx_terminal_jobs: u64,
    tx_successes: u64,
    tx_completed_frames: u64,
    tx_access_rejects: u64,
    tx_failures: u64,
    cad_busy: u64,
    cad_clear: u64,
    last_rx: Option<DiagnosticLoraLastRx>,
    last_tx: Option<DiagnosticLoraLastTx>,
    last_data_tx: Option<DiagnosticLoraLastDataTx>,
}

impl LoraDiagnostics {
    /// Construct one complete LoRa diagnostics record.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        applied_tx_power_dbm: i16,
        frequency_hz: u32,
        bandwidth_hz: u32,
        spreading_factor: u8,
        coding_rate_denominator: u8,
        rx_physical_frames: u64,
        rx_packets: u64,
        rx_errors: u64,
        rx_drops: u64,
        tx_terminal_jobs: u64,
        tx_successes: u64,
        tx_completed_frames: u64,
        tx_access_rejects: u64,
        tx_failures: u64,
        cad_busy: u64,
        cad_clear: u64,
        last_rx: Option<DiagnosticLoraLastRx>,
        last_tx: Option<DiagnosticLoraLastTx>,
        last_data_tx: Option<DiagnosticLoraLastDataTx>,
    ) -> Self {
        Self {
            applied_tx_power_dbm,
            frequency_hz,
            bandwidth_hz,
            spreading_factor,
            coding_rate_denominator,
            rx_physical_frames,
            rx_packets,
            rx_errors,
            rx_drops,
            tx_terminal_jobs,
            tx_successes,
            tx_completed_frames,
            tx_access_rejects,
            tx_failures,
            cad_busy,
            cad_clear,
            last_rx,
            last_tx,
            last_data_tx,
        }
    }

    /// Applied whole-dBm radio output setting.
    pub const fn applied_tx_power_dbm(self) -> i16 {
        self.applied_tx_power_dbm
    }

    /// Applied carrier center frequency.
    pub const fn frequency_hz(self) -> u32 {
        self.frequency_hz
    }

    /// Applied LoRa bandwidth.
    pub const fn bandwidth_hz(self) -> u32 {
        self.bandwidth_hz
    }

    /// Applied LoRa spreading factor.
    pub const fn spreading_factor(self) -> u8 {
        self.spreading_factor
    }

    /// Denominator of the applied LoRa coding rate.
    pub const fn coding_rate_denominator(self) -> u8 {
        self.coding_rate_denominator
    }

    /// Physical receive frames presented by the radio.
    pub const fn rx_physical_frames(self) -> u64 {
        self.rx_physical_frames
    }

    /// Reticulum packets reconstructed from received physical frames.
    pub const fn rx_packets(self) -> u64 {
        self.rx_packets
    }

    /// Receive operations ending in radio or decode error.
    pub const fn rx_errors(self) -> u64 {
        self.rx_errors
    }

    /// Received frames or packets dropped after radio delivery.
    pub const fn rx_drops(self) -> u64 {
        self.rx_drops
    }

    /// Transmission jobs reaching a terminal result.
    pub const fn tx_terminal_jobs(self) -> u64 {
        self.tx_terminal_jobs
    }

    /// Terminal jobs that completed successfully.
    pub const fn tx_successes(self) -> u64 {
        self.tx_successes
    }

    /// Physical frames completed across successful or partially completed jobs.
    pub const fn tx_completed_frames(self) -> u64 {
        self.tx_completed_frames
    }

    /// Jobs rejected by channel-access policy.
    pub const fn tx_access_rejects(self) -> u64 {
        self.tx_access_rejects
    }

    /// Jobs ending in another radio or scheduler failure.
    pub const fn tx_failures(self) -> u64 {
        self.tx_failures
    }

    /// Channel-activity detections reporting a busy channel.
    pub const fn cad_busy(self) -> u64 {
        self.cad_busy
    }

    /// Channel-activity detections reporting a clear channel.
    pub const fn cad_clear(self) -> u64 {
        self.cad_clear
    }

    /// Conservative signal observation for the most recently accepted packet.
    pub const fn last_rx(self) -> Option<DiagnosticLoraLastRx> {
        self.last_rx
    }

    /// Most recent terminal transmission observation.
    pub const fn last_tx(self) -> Option<DiagnosticLoraLastTx> {
        self.last_tx
    }

    /// Most recent DATA terminal observation retained across ordinary TX.
    ///
    /// Producers may omit this duplicate when [`Self::last_tx`] is itself a
    /// DATA record; host projections recover that equivalent view.
    pub const fn last_data_tx(self) -> Option<DiagnosticLoraLastDataTx> {
        self.last_data_tx
    }
}

/// Reticulum transport and path-table counters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RnsDiagnostics {
    received: u64,
    forwarded: u64,
    dedup_drops: u64,
    invalid_drops: u64,
    announces_received: u64,
    paths_learned: u64,
    paths_expired: u64,
    links_established: u64,
    links_closed: u64,
    links_failed: u64,
}

impl RnsDiagnostics {
    /// Construct one complete Reticulum diagnostics record.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        received: u64,
        forwarded: u64,
        dedup_drops: u64,
        invalid_drops: u64,
        announces_received: u64,
        paths_learned: u64,
        paths_expired: u64,
        links_established: u64,
        links_closed: u64,
        links_failed: u64,
    ) -> Self {
        Self {
            received,
            forwarded,
            dedup_drops,
            invalid_drops,
            announces_received,
            paths_learned,
            paths_expired,
            links_established,
            links_closed,
            links_failed,
        }
    }

    /// Packets admitted by the Reticulum owner.
    pub const fn received(self) -> u64 {
        self.received
    }

    /// Packets forwarded by the Reticulum owner.
    pub const fn forwarded(self) -> u64 {
        self.forwarded
    }

    /// Duplicate packets dropped before processing.
    pub const fn dedup_drops(self) -> u64 {
        self.dedup_drops
    }

    /// Structurally or cryptographically invalid packets dropped.
    pub const fn invalid_drops(self) -> u64 {
        self.invalid_drops
    }

    /// Valid announces admitted by the Reticulum owner.
    pub const fn announces_received(self) -> u64 {
        self.announces_received
    }

    /// Route records learned or replaced.
    pub const fn paths_learned(self) -> u64 {
        self.paths_learned
    }

    /// Route records expired or removed.
    pub const fn paths_expired(self) -> u64 {
        self.paths_expired
    }

    /// Saturating route-table revision used by diagnostics pagination.
    pub const fn route_revision(self) -> u64 {
        self.paths_learned.saturating_add(self.paths_expired)
    }

    /// Links reaching the established state.
    pub const fn links_established(self) -> u64 {
        self.links_established
    }

    /// Established links closed normally.
    pub const fn links_closed(self) -> u64 {
        self.links_closed
    }

    /// Link establishment attempts ending in failure.
    pub const fn links_failed(self) -> u64 {
        self.links_failed
    }
}

/// Authenticated, bounded cross-interface node diagnostics snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeDiagnosticsSnapshot {
    uptime_ms: u64,
    interfaces: [Option<DiagnosticInterfaceRecord>; MAX_DIAGNOSTIC_INTERFACES],
    lora: Option<LoraDiagnostics>,
    rns: RnsDiagnostics,
    observed_peer_count: u32,
    retained_route_count: u32,
    usable_route_count: u32,
}

impl NodeDiagnosticsSnapshot {
    /// Construct one complete node diagnostics snapshot.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        uptime_ms: u64,
        interfaces: [Option<DiagnosticInterfaceRecord>; MAX_DIAGNOSTIC_INTERFACES],
        lora: Option<LoraDiagnostics>,
        rns: RnsDiagnostics,
        observed_peer_count: u32,
        retained_route_count: u32,
        usable_route_count: u32,
    ) -> Self {
        Self {
            uptime_ms,
            interfaces,
            lora,
            rns,
            observed_peer_count,
            retained_route_count,
            usable_route_count,
        }
    }

    /// Milliseconds since this node incarnation started.
    pub const fn uptime_ms(self) -> u64 {
        self.uptime_ms
    }

    /// Fixed optional interface slots.
    pub const fn interfaces(
        &self,
    ) -> &[Option<DiagnosticInterfaceRecord>; MAX_DIAGNOSTIC_INTERFACES] {
        &self.interfaces
    }

    /// LoRa-specific diagnostics when a LoRa owner is present.
    pub const fn lora(self) -> Option<LoraDiagnostics> {
        self.lora
    }

    /// Reticulum transport and path-table counters.
    pub const fn rns(self) -> RnsDiagnostics {
        self.rns
    }

    /// Volatile authenticated or otherwise observed peer records.
    pub const fn observed_peer_count(self) -> u32 {
        self.observed_peer_count
    }

    /// Route records retained regardless of current interface usability.
    pub const fn retained_route_count(self) -> u32 {
        self.retained_route_count
    }

    /// Retained routes whose selected interface is currently usable.
    pub const fn usable_route_count(self) -> u32 {
        self.usable_route_count
    }
}

/// Exclusive boot-scoped cursor for radio trace pagination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTraceCursor {
    boot_id: u64,
    after_sequence: u64,
}

impl RadioTraceCursor {
    /// Bind an exclusive event sequence to the boot that allocated it.
    pub const fn new(boot_id: u64, after_sequence: u64) -> Self {
        Self {
            boot_id,
            after_sequence,
        }
    }

    /// Opaque node-incarnation identifier scoping the sequence.
    pub const fn boot_id(self) -> u64 {
        self.boot_id
    }

    /// Exclusive event sequence within this boot.
    pub const fn after_sequence(self) -> u64 {
        self.after_sequence
    }
}

/// Optional exclusive boot-and-sequence cursor for a radio trace page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTracePageRequest {
    after: Option<RadioTraceCursor>,
}

impl RadioTracePageRequest {
    /// Construct a request beginning after `after`, or at the oldest retained
    /// event when no boot-scoped cursor is supplied.
    pub const fn new(after: Option<RadioTraceCursor>) -> Self {
        Self { after }
    }

    /// Exclusive boot-and-event-sequence cursor.
    pub const fn after(self) -> Option<RadioTraceCursor> {
        self.after
    }
}

/// Immutable LoRa configuration applied for one radio-trace boot.
///
/// The complete board-owned fingerprint is retained alongside human-readable
/// modulation fields so exported traces can detect any configuration mismatch
/// without reverse-engineering the fingerprint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTraceAppliedLoraProfile {
    configuration_fingerprint: [u8; 16],
    frequency_hz: u32,
    bandwidth_hz: u32,
    preamble_symbols: u16,
    requested_power_dbm: i16,
    spreading_factor: u8,
    coding_rate_denominator: u8,
    explicit_header: bool,
    crc: bool,
    iq_inverted: bool,
}

impl RadioTraceAppliedLoraProfile {
    /// Construct the exact immutable profile owned by the running radio actor.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        configuration_fingerprint: [u8; 16],
        frequency_hz: u32,
        bandwidth_hz: u32,
        preamble_symbols: u16,
        requested_power_dbm: i16,
        spreading_factor: u8,
        coding_rate_denominator: u8,
        explicit_header: bool,
        crc: bool,
        iq_inverted: bool,
    ) -> Self {
        Self {
            configuration_fingerprint,
            frequency_hz,
            bandwidth_hz,
            preamble_symbols,
            requested_power_dbm,
            spreading_factor,
            coding_rate_denominator,
            explicit_header,
            crc,
            iq_inverted,
        }
    }

    /// Complete board-owned immutable configuration fingerprint.
    pub const fn configuration_fingerprint(self) -> [u8; 16] {
        self.configuration_fingerprint
    }

    /// Applied carrier center frequency in whole hertz.
    pub const fn frequency_hz(self) -> u32 {
        self.frequency_hz
    }

    /// Applied LoRa bandwidth in whole hertz.
    pub const fn bandwidth_hz(self) -> u32 {
        self.bandwidth_hz
    }

    /// Applied preamble length in symbols.
    pub const fn preamble_symbols(self) -> u16 {
        self.preamble_symbols
    }

    /// Requested radio output in whole dBm, without an antenna-path claim.
    pub const fn requested_power_dbm(self) -> i16 {
        self.requested_power_dbm
    }

    /// Applied LoRa spreading-factor number.
    pub const fn spreading_factor(self) -> u8 {
        self.spreading_factor
    }

    /// Denominator of the applied `4/x` LoRa coding rate.
    pub const fn coding_rate_denominator(self) -> u8 {
        self.coding_rate_denominator
    }

    /// Whether the explicit packet header is enabled.
    pub const fn explicit_header(self) -> bool {
        self.explicit_header
    }

    /// Whether the packet CRC is enabled.
    pub const fn crc(self) -> bool {
        self.crc
    }

    /// Whether LoRa IQ polarity is inverted.
    pub const fn iq_inverted(self) -> bool {
        self.iq_inverted
    }
}

/// Hop-invariant Reticulum proof-correlation hash for one traced packet.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RadioTraceAttemptToken([u8; 32]);

impl RadioTraceAttemptToken {
    /// Construct a token from all proof-correlation hash bytes.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow all proof-correlation hash bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Packet identity common to transmit and receive trace events.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTracePacketEvidence {
    interface_id: u8,
    packet_len: NonZeroU16,
    encoded_packet_sha256: EncodedPacketSha256,
    attempt_token: Option<RadioTraceAttemptToken>,
}

impl RadioTracePacketEvidence {
    /// Construct complete packet evidence, rejecting an impossible empty
    /// encoded Reticulum packet.
    pub const fn try_new(
        interface_id: u8,
        packet_len: u16,
        encoded_packet_sha256: EncodedPacketSha256,
        attempt_token: Option<RadioTraceAttemptToken>,
    ) -> Option<Self> {
        let Some(packet_len) = NonZeroU16::new(packet_len) else {
            return None;
        };
        Some(Self {
            interface_id,
            packet_len,
            encoded_packet_sha256,
            attempt_token,
        })
    }

    /// Product-owned Reticulum interface identifier.
    pub const fn interface_id(self) -> u8 {
        self.interface_id
    }

    /// Complete encoded interface-packet length.
    pub const fn packet_len(self) -> u16 {
        self.packet_len.get()
    }

    /// SHA-256 over every complete encoded interface-packet byte.
    pub const fn encoded_packet_sha256(self) -> EncodedPacketSha256 {
        self.encoded_packet_sha256
    }

    /// Hop-invariant Reticulum proof-correlation hash when derivable.
    pub const fn attempt_token(self) -> Option<RadioTraceAttemptToken> {
        self.attempt_token
    }
}

/// Detailed terminal result of one traced LoRa DATA dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadioTraceTxOutcome {
    /// Every planned physical frame completed successfully.
    Transmitted,
    /// Initial channel access rejected the logical packet.
    AccessRejected,
    /// The node owner denied the exact permit request.
    PermitDenied,
    /// A matching authorization arrived after its deadline.
    AuthorizationExpired,
    /// Fresh post-grant channel access rejected the logical packet.
    PostGrantAccessRejected,
    /// Airtime could not be calculated or admitted.
    AirtimeRejected,
    /// A dispatch deadline could not be represented.
    DeadlineConversionOverflow,
    /// The sole radio was already inactive.
    RadioInactive,
    /// Router and dispatcher configuration identities differed.
    InterfaceConfigurationMismatch,
    /// Immutable radio configuration changed before permit negotiation.
    RadioConfigurationChangedBeforePermit,
    /// Immutable radio configuration changed after permit negotiation.
    RadioConfigurationChangedAfterPermit,
    /// Channel-activity detection failed.
    CadFault,
    /// Physical transmission failed.
    TxFault,
    /// A permit exchange could not be reconciled.
    ControlPlaneRecovery,
    /// Authorized framing or byte exposure violated an invariant.
    FrameInvariantRecovery,
    /// A dropped CAD or transmit future was explicitly reconciled.
    CancelledRadioOperation,
}

impl RadioTraceTxOutcome {
    /// Frozen numeric representation within this experimental operation.
    pub const fn wire_code(self) -> u8 {
        match self {
            Self::Transmitted => 0,
            Self::AccessRejected => 1,
            Self::PermitDenied => 2,
            Self::AuthorizationExpired => 3,
            Self::PostGrantAccessRejected => 4,
            Self::AirtimeRejected => 5,
            Self::DeadlineConversionOverflow => 6,
            Self::RadioInactive => 7,
            Self::InterfaceConfigurationMismatch => 8,
            Self::RadioConfigurationChangedBeforePermit => 9,
            Self::RadioConfigurationChangedAfterPermit => 10,
            Self::CadFault => 11,
            Self::TxFault => 12,
            Self::ControlPlaneRecovery => 13,
            Self::FrameInvariantRecovery => 14,
            Self::CancelledRadioOperation => 15,
        }
    }
}

/// One terminal LoRa DATA dispatch trace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTraceDataTx {
    packet: RadioTracePacketEvidence,
    outcome: RadioTraceTxOutcome,
    planned_frames: u8,
    completed_frames: u8,
    authorization_observed: bool,
    frame_completed_at_us: [Option<u64>; 2],
}

impl RadioTraceDataTx {
    /// Construct a consistent terminal DATA dispatch trace.
    pub const fn try_new(
        packet: RadioTracePacketEvidence,
        outcome: RadioTraceTxOutcome,
        planned_frames: u8,
        completed_frames: u8,
        authorization_observed: bool,
        frame_completed_at_us: [Option<u64>; 2],
    ) -> Result<Self, InvalidRadioTraceDataTx> {
        if planned_frames == 0 || planned_frames > 2 {
            return Err(InvalidRadioTraceDataTx::InvalidPlannedFrames);
        }
        if completed_frames > planned_frames {
            return Err(InvalidRadioTraceDataTx::CompletedFramesExceedPlanned);
        }
        let timestamp_count = match frame_completed_at_us {
            [None, Some(_)] => {
                return Err(InvalidRadioTraceDataTx::SparseCompletionTimestamps);
            }
            [Some(_), Some(_)] => 2,
            [Some(_), None] => 1,
            [None, None] => 0,
        };
        if timestamp_count != completed_frames {
            return Err(InvalidRadioTraceDataTx::CompletionTimestampCountMismatch);
        }
        Ok(Self {
            packet,
            outcome,
            planned_frames,
            completed_frames,
            authorization_observed,
            frame_completed_at_us,
        })
    }

    /// Complete prepared packet identity.
    pub const fn packet(self) -> RadioTracePacketEvidence {
        self.packet
    }

    /// Detailed terminal dispatch category.
    pub const fn outcome(self) -> RadioTraceTxOutcome {
        self.outcome
    }

    /// Physical frames planned for the logical packet.
    pub const fn planned_frames(self) -> u8 {
        self.planned_frames
    }

    /// Physical frames whose radio completion was observed.
    pub const fn completed_frames(self) -> u8 {
        self.completed_frames
    }

    /// Whether the exact byte-exposure authorization was observed.
    pub const fn authorization_observed(self) -> bool {
        self.authorization_observed
    }

    /// Per-frame radio-completion monotonic timestamps in physical order.
    pub const fn frame_completed_at_us(self) -> [Option<u64>; 2] {
        self.frame_completed_at_us
    }
}

/// A DATA dispatch trace violated a physical-frame invariant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidRadioTraceDataTx {
    /// An RNode logical packet must plan one or two physical frames.
    InvalidPlannedFrames,
    /// Reported completed frames exceeded the planned frame count.
    CompletedFramesExceedPlanned,
    /// A populated completion timestamp followed an empty slot.
    SparseCompletionTimestamps,
    /// Completion timestamp count differed from the completed frame count.
    CompletionTimestampCountMismatch,
}

/// One complete logical LoRa packet accepted by the receive pipeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTraceLogicalRx {
    packet: RadioTracePacketEvidence,
    rssi_dbm: i16,
    snr_db: i16,
}

impl RadioTraceLogicalRx {
    /// Construct receiver-local evidence for one accepted logical packet.
    pub const fn new(packet: RadioTracePacketEvidence, rssi_dbm: i16, snr_db: i16) -> Self {
        Self {
            packet,
            rssi_dbm,
            snr_db,
        }
    }

    /// Complete received packet identity.
    pub const fn packet(self) -> RadioTracePacketEvidence {
        self.packet
    }

    /// Conservative whole-packet received signal strength in dBm.
    pub const fn rssi_dbm(self) -> i16 {
        self.rssi_dbm
    }

    /// Conservative whole-packet signal-to-noise ratio in dB.
    pub const fn snr_db(self) -> i16 {
        self.snr_db
    }
}

/// One exact DATA route selected before radio dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTraceRouteSelected {
    submission_id: SubmissionId,
    destination: DestinationHash,
    next_hop_identity: Option<IdentityHash>,
    hops: u8,
    resolution: RouteDiagnosticResolution,
    packet: RadioTracePacketEvidence,
}

impl RadioTraceRouteSelected {
    /// Construct an exact route decision and prepared-packet identity.
    pub const fn try_new(
        submission_id: SubmissionId,
        destination: DestinationHash,
        next_hop_identity: Option<IdentityHash>,
        hops: u8,
        resolution: RouteDiagnosticResolution,
        packet: RadioTracePacketEvidence,
    ) -> Result<Self, InvalidRadioTraceRouteSelected> {
        if submission_id.0 == 0 {
            return Err(InvalidRadioTraceRouteSelected::ZeroSubmissionId);
        }
        if packet.attempt_token().is_none() {
            return Err(InvalidRadioTraceRouteSelected::MissingAttemptToken);
        }
        Ok(Self {
            submission_id,
            destination,
            next_hop_identity,
            hops,
            resolution,
            packet,
        })
    }

    /// Durable device submission correlated with this exact prepared packet.
    pub const fn submission_id(self) -> SubmissionId {
        self.submission_id
    }

    /// Complete routed destination hash.
    pub const fn destination(self) -> DestinationHash {
        self.destination
    }

    /// Public identity hash selected as next hop, when known.
    pub const fn next_hop_identity(self) -> Option<IdentityHash> {
        self.next_hop_identity
    }

    /// Selected Reticulum hop count.
    pub const fn hops(self) -> u8 {
        self.hops
    }

    /// Exact or broadcast route-resolution result at selection time.
    pub const fn resolution(self) -> RouteDiagnosticResolution {
        self.resolution
    }

    /// Complete routed prepared-packet identity, including its attempt token.
    pub const fn packet(self) -> RadioTracePacketEvidence {
        self.packet
    }
}

/// A route-selection trace omitted required correlation evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidRadioTraceRouteSelected {
    /// Device submission identifiers reserve zero.
    ZeroSubmissionId,
    /// A destination-DATA route must retain its proof-correlation token.
    MissingAttemptToken,
}

/// Terminal application-visible state of one proof-correlated DATA attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadioTraceAttemptOutcome {
    /// A valid Reticulum delivery proof was accepted.
    Delivered,
    /// The receipt expired without a proof.
    DeliveryTimeout,
    /// The complete serialized route ended definitely unsent.
    Unsent,
}

impl RadioTraceAttemptOutcome {
    /// Frozen numeric representation within this experimental operation.
    pub const fn wire_code(self) -> u8 {
        match self {
            Self::Delivered => 0,
            Self::DeliveryTimeout => 1,
            Self::Unsent => 2,
        }
    }
}

/// One proof-correlated attempt reaching an application-visible terminal state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTraceAttemptTerminal {
    attempt_token: RadioTraceAttemptToken,
    outcome: RadioTraceAttemptOutcome,
    proof_ingress: Option<IngressObservation>,
}

impl RadioTraceAttemptTerminal {
    /// Construct one immutable attempt terminal trace.
    pub const fn new(
        attempt_token: RadioTraceAttemptToken,
        outcome: RadioTraceAttemptOutcome,
        proof_ingress: Option<IngressObservation>,
    ) -> Self {
        Self {
            attempt_token,
            outcome,
            proof_ingress,
        }
    }

    /// Complete hop-invariant Reticulum proof-correlation hash.
    pub const fn attempt_token(self) -> RadioTraceAttemptToken {
        self.attempt_token
    }

    /// Application-visible terminal result.
    pub const fn outcome(self) -> RadioTraceAttemptOutcome {
        self.outcome
    }

    /// First-arrival interface and optional signal for an accepted proof.
    pub const fn proof_ingress(self) -> Option<IngressObservation> {
        self.proof_ingress
    }
}

/// Event-specific payload for one radio trace record.
///
/// Additional bounded event families can be introduced by later experimental
/// API revisions without changing event identity or page pagination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RadioTraceEventKind {
    /// One terminal destination-DATA dispatch.
    DataTx(RadioTraceDataTx),
    /// One complete logical packet accepted by LoRa receive.
    LogicalRx(RadioTraceLogicalRx),
    /// One exact route selected for a destination-DATA attempt.
    RouteSelected(RadioTraceRouteSelected),
    /// One proof-correlated DATA attempt reaching terminal state.
    AttemptTerminal(RadioTraceAttemptTerminal),
}

impl RadioTraceEventKind {
    /// Numeric event discriminator within this experimental operation.
    pub const fn wire_code(self) -> u8 {
        match self {
            Self::DataTx(_) => 0,
            Self::LogicalRx(_) => 1,
            Self::RouteSelected(_) => 2,
            Self::AttemptTerminal(_) => 3,
        }
    }
}

/// One immutable boot-scoped radio trace record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTraceEvent {
    sequence: u64,
    observed_at_us: u64,
    kind: RadioTraceEventKind,
}

impl RadioTraceEvent {
    /// Construct one event with its boot-scoped identity and monotonic time.
    pub const fn new(sequence: u64, observed_at_us: u64, kind: RadioTraceEventKind) -> Self {
        Self {
            sequence,
            observed_at_us,
            kind,
        }
    }

    /// Monotonic event identity within the page's boot identifier.
    pub const fn sequence(self) -> u64 {
        self.sequence
    }

    /// Microseconds since this node incarnation started.
    pub const fn observed_at_us(self) -> u64 {
        self.observed_at_us
    }

    /// Event-specific trace evidence.
    pub const fn kind(self) -> RadioTraceEventKind {
        self.kind
    }
}

/// One bounded ascending page from the boot-scoped radio trace ring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTracePage {
    boot_id: u64,
    applied_lora_profile: RadioTraceAppliedLoraProfile,
    oldest_sequence: u64,
    next_sequence: u64,
    history_lost: bool,
    entries: [Option<RadioTraceEvent>; MAX_RADIO_TRACE_PAGE_ENTRIES],
    next_cursor: Option<RadioTraceCursor>,
}

impl RadioTracePage {
    /// Construct one dense, strictly ascending page.
    ///
    /// `oldest_sequence == next_sequence` represents an empty ring. A present
    /// continuation cursor must equal the last returned sequence and is used
    /// as the following request's exclusive cursor.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        boot_id: u64,
        applied_lora_profile: RadioTraceAppliedLoraProfile,
        oldest_sequence: u64,
        next_sequence: u64,
        history_lost: bool,
        entries: [Option<RadioTraceEvent>; MAX_RADIO_TRACE_PAGE_ENTRIES],
        next_cursor: Option<RadioTraceCursor>,
    ) -> Result<Self, InvalidRadioTracePage> {
        if oldest_sequence > next_sequence {
            return Err(InvalidRadioTracePage::InvalidSequenceWindow);
        }
        let mut previous = None;
        let mut saw_empty = false;
        let mut maximum_event_bytes = 0_u16;
        for entry in entries {
            match entry {
                Some(entry) => {
                    if saw_empty {
                        return Err(InvalidRadioTracePage::SparseEntries);
                    }
                    if entry.sequence < oldest_sequence || entry.sequence >= next_sequence {
                        return Err(InvalidRadioTracePage::EventOutsideWindow);
                    }
                    if let Some(previous) = previous
                        && entry.sequence <= previous
                    {
                        return Err(InvalidRadioTracePage::NotStrictlyOrdered);
                    }
                    maximum_event_bytes += match entry.kind {
                        RadioTraceEventKind::DataTx(_) => 117,
                        RadioTraceEventKind::LogicalRx(_) => 100,
                        RadioTraceEventKind::RouteSelected(_) => 140,
                        RadioTraceEventKind::AttemptTerminal(_) => 68,
                    };
                    previous = Some(entry.sequence);
                }
                None => saw_empty = true,
            }
        }
        // These exact per-kind maxima include the event envelope and
        // worst-width scalar encodings. The remaining page/profile fields use
        // at most 72 bytes, plus 18 when a continuation cursor replaces null.
        let event_budget = if next_cursor.is_some() { 358 } else { 376 };
        if maximum_event_bytes > event_budget {
            return Err(InvalidRadioTracePage::EventCombinationExceedsWireBudget);
        }
        if let Some(next_cursor) = next_cursor
            && (next_cursor.boot_id != boot_id || previous != Some(next_cursor.after_sequence))
        {
            return Err(InvalidRadioTracePage::InvalidNextCursor);
        }
        Ok(Self {
            boot_id,
            applied_lora_profile,
            oldest_sequence,
            next_sequence,
            history_lost,
            entries,
            next_cursor,
        })
    }

    /// Opaque node-incarnation identifier scoping all event sequences.
    pub const fn boot_id(self) -> u64 {
        self.boot_id
    }

    /// Immutable LoRa configuration applied for this boot.
    pub const fn applied_lora_profile(self) -> RadioTraceAppliedLoraProfile {
        self.applied_lora_profile
    }

    /// Oldest event sequence still retained, or `next_sequence` when empty.
    pub const fn oldest_sequence(self) -> u64 {
        self.oldest_sequence
    }

    /// Sequence that will be allocated to the next event.
    pub const fn next_sequence(self) -> u64 {
        self.next_sequence
    }

    /// Whether events preceding this page's starting position were overwritten.
    pub const fn history_lost(self) -> bool {
        self.history_lost
    }

    /// Dense ascending fixed-capacity event slots.
    pub const fn entries(&self) -> &[Option<RadioTraceEvent>; MAX_RADIO_TRACE_PAGE_ENTRIES] {
        &self.entries
    }

    /// Exclusive sequence cursor for the following page, when more remain.
    pub const fn next_cursor(self) -> Option<RadioTraceCursor> {
        self.next_cursor
    }
}

/// A radio trace page violated its pagination invariants.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidRadioTracePage {
    /// The retained sequence window ran backwards.
    InvalidSequenceWindow,
    /// A populated event followed an empty fixed-capacity slot.
    SparseEntries,
    /// Event sequences were not strictly ascending.
    NotStrictlyOrdered,
    /// An event did not belong to the advertised retained window.
    EventOutsideWindow,
    /// Continuation cursor did not equal the last returned event sequence.
    InvalidNextCursor,
    /// The selected event combination exceeds the frozen response-body limit.
    EventCombinationExceedsWireBudget,
}

/// Optional exclusive destination cursor for a route diagnostics page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RouteDiagnosticsRequest {
    after: Option<DestinationHash>,
}

impl RouteDiagnosticsRequest {
    /// Construct a request beginning after `after`, or at the first route.
    pub const fn new(after: Option<DestinationHash>) -> Self {
        Self { after }
    }

    /// Exclusive lexicographic destination cursor.
    pub const fn after(self) -> Option<DestinationHash> {
        self.after
    }
}

/// How one retained destination currently resolves.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteDiagnosticResolution {
    /// An exact retained route is usable now.
    ExactReady,
    /// An exact retained route exists but its interface is offline or faulted.
    ExactOffline,
    /// An exact retained route has incomplete next-hop or interface state.
    ExactMissing,
    /// No exact route exists, but at least one broadcast interface is usable.
    BroadcastReady,
    /// Neither an exact route nor a usable broadcast interface exists.
    BroadcastUnavailable,
}

impl RouteDiagnosticResolution {
    /// Frozen API-v1 numeric representation.
    pub const fn wire_code(self) -> u8 {
        match self {
            Self::ExactReady => 0,
            Self::ExactOffline => 1,
            Self::ExactMissing => 2,
            Self::BroadcastReady => 3,
            Self::BroadcastUnavailable => 4,
        }
    }
}

/// One retained route or route-resolution diagnostics record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RouteDiagnosticEntry {
    destination: DestinationHash,
    next_hop_identity: Option<IdentityHash>,
    hops: u8,
    retained_interface: Option<u8>,
    resolution: RouteDiagnosticResolution,
    learned_age_ms: Option<u64>,
    last_used_age_ms: Option<u64>,
    expires_in_ms: Option<u64>,
}

impl RouteDiagnosticEntry {
    /// Construct one complete route diagnostics record.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        destination: DestinationHash,
        next_hop_identity: Option<IdentityHash>,
        hops: u8,
        retained_interface: Option<u8>,
        resolution: RouteDiagnosticResolution,
        learned_age_ms: Option<u64>,
        last_used_age_ms: Option<u64>,
        expires_in_ms: Option<u64>,
    ) -> Self {
        Self {
            destination,
            next_hop_identity,
            hops,
            retained_interface,
            resolution,
            learned_age_ms,
            last_used_age_ms,
            expires_in_ms,
        }
    }

    /// Complete route destination hash.
    pub const fn destination(self) -> DestinationHash {
        self.destination
    }

    /// Public identity hash selected as next hop, when known.
    pub const fn next_hop_identity(self) -> Option<IdentityHash> {
        self.next_hop_identity
    }

    /// Reticulum hop count.
    pub const fn hops(self) -> u8 {
        self.hops
    }

    /// Product-owned retained interface identifier, when known.
    pub const fn retained_interface(self) -> Option<u8> {
        self.retained_interface
    }

    /// Current exact or broadcast resolution result.
    pub const fn resolution(self) -> RouteDiagnosticResolution {
        self.resolution
    }

    /// Saturating age since the route was learned, when tracked.
    pub const fn learned_age_ms(self) -> Option<u64> {
        self.learned_age_ms
    }

    /// Saturating age since the route was used, when tracked.
    pub const fn last_used_age_ms(self) -> Option<u64> {
        self.last_used_age_ms
    }

    /// Remaining lifetime before expiry, when tracked.
    pub const fn expires_in_ms(self) -> Option<u64> {
        self.expires_in_ms
    }
}

/// One bounded lexicographically ordered page of route diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RouteDiagnosticsPage {
    revision: u64,
    total_count: u32,
    entries: [Option<RouteDiagnosticEntry>; MAX_ROUTE_DIAGNOSTIC_PAGE_ENTRIES],
    next_cursor: Option<DestinationHash>,
}

impl RouteDiagnosticsPage {
    /// Construct one dense, strictly ordered route page.
    ///
    /// A present next cursor must equal the last returned destination so the
    /// following request remains an unambiguous exclusive continuation.
    pub fn new(
        revision: u64,
        total_count: u32,
        entries: [Option<RouteDiagnosticEntry>; MAX_ROUTE_DIAGNOSTIC_PAGE_ENTRIES],
        next_cursor: Option<DestinationHash>,
    ) -> Result<Self, InvalidRouteDiagnosticsPage> {
        let mut previous: Option<DestinationHash> = None;
        let mut saw_empty = false;
        for entry in entries {
            match entry {
                Some(entry) => {
                    if saw_empty {
                        return Err(InvalidRouteDiagnosticsPage::SparseEntries);
                    }
                    if let Some(previous) = previous
                        && entry.destination.0 <= previous.0
                    {
                        return Err(InvalidRouteDiagnosticsPage::NotStrictlyOrdered);
                    }
                    previous = Some(entry.destination);
                }
                None => saw_empty = true,
            }
        }
        if let Some(next_cursor) = next_cursor
            && previous != Some(next_cursor)
        {
            return Err(InvalidRouteDiagnosticsPage::InvalidNextCursor);
        }
        Ok(Self {
            revision,
            total_count,
            entries,
            next_cursor,
        })
    }

    /// Path-table revision computed as learned plus expired path counters.
    pub const fn revision(self) -> u64 {
        self.revision
    }

    /// Complete retained route count at snapshot time.
    pub const fn total_count(self) -> u32 {
        self.total_count
    }

    /// Dense ordered fixed-capacity route slots.
    pub const fn entries(
        &self,
    ) -> &[Option<RouteDiagnosticEntry>; MAX_ROUTE_DIAGNOSTIC_PAGE_ENTRIES] {
        &self.entries
    }

    /// Exclusive destination cursor for a following page, when more remain.
    pub const fn next_cursor(self) -> Option<DestinationHash> {
        self.next_cursor
    }
}

/// A route diagnostics page violated a pagination invariant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidRouteDiagnosticsPage {
    /// A populated entry followed an empty fixed-capacity slot.
    SparseEntries,
    /// Destinations were not strictly increasing in lexicographic byte order.
    NotStrictlyOrdered,
    /// The continuation cursor did not equal the last returned destination.
    InvalidNextCursor,
}

/// Validated borrowed UTF-8 NomadNet request path.
///
/// The path is absolute, contains no NUL byte, and remains borrowed directly
/// from the decoded request message.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct NomadPagePath<'a>(&'a str);

#[cfg(feature = "experimental-nomad")]
impl<'a> NomadPagePath<'a> {
    /// Validate one bounded absolute NomadNet path.
    pub fn new(path: &'a str) -> Result<Self, InvalidNomadPagePath> {
        let bytes = path.as_bytes();
        if bytes.is_empty() || bytes[0] != b'/' || bytes.contains(&0) {
            return Err(InvalidNomadPagePath::Invalid);
        }
        if bytes.len() > MAX_NOMAD_PAGE_PATH_BYTES {
            return Err(InvalidNomadPagePath::TooLong {
                actual: bytes.len(),
            });
        }
        Ok(Self(path))
    }

    /// Borrow the complete validated path.
    pub const fn as_str(self) -> &'a str {
        self.0
    }

    /// Path length in UTF-8 bytes.
    pub const fn len(self) -> usize {
        self.0.len()
    }

    /// Whether the path is empty.
    ///
    /// A constructed path is never empty; this method supports conventional
    /// collection-style inspection.
    pub const fn is_empty(self) -> bool {
        self.0.is_empty()
    }
}

/// Why a NomadNet request path was rejected.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidNomadPagePath {
    /// The path was empty, relative, or contained a NUL byte.
    Invalid,
    /// The path exceeded the fixed UTF-8 byte limit.
    TooLong {
        /// Rejected path length in bytes.
        actual: usize,
    },
}

#[cfg(feature = "experimental-nomad")]
impl InvalidNomadPagePath {
    /// Largest accepted UTF-8 path length.
    pub const fn maximum(self) -> usize {
        MAX_NOMAD_PAGE_PATH_BYTES
    }
}

/// Caller-selected Unix timestamp for one anonymous NomadNet request.
///
/// The inclusive range is lossless in JSON and JavaScript integer
/// interchange. Conversion to the Reticulum binary64-seconds wire timestamp
/// can lose millisecond precision at extreme dates.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NomadRequestTimestampUnixMs(u64);

#[cfg(feature = "experimental-nomad")]
impl NomadRequestTimestampUnixMs {
    /// Validate a nonzero whole-millisecond Unix timestamp.
    pub const fn new(value: u64) -> Result<Self, InvalidNomadRequestTimestamp> {
        if value == 0 || value > MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS {
            Err(InvalidNomadRequestTimestamp { actual: value })
        } else {
            Ok(Self(value))
        }
    }

    /// Complete validated millisecond value.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A NomadNet request timestamp was zero or outside the exact millisecond range.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidNomadRequestTimestamp {
    actual: u64,
}

#[cfg(feature = "experimental-nomad")]
impl InvalidNomadRequestTimestamp {
    /// Rejected millisecond value.
    pub const fn actual(self) -> u64 {
        self.actual
    }

    /// Largest accepted millisecond value.
    pub const fn maximum(self) -> u64 {
        MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS
    }
}

/// Opaque boot-scoped identifier for one principal-owned NomadNet fetch.
///
/// The first eight bytes identify the boot incarnation. The final eight bytes
/// contain a nonzero big-endian sequence. Clients compare and return all 16
/// bytes without deriving authority from either component.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NomadFetchId([u8; 16]);

#[cfg(feature = "experimental-nomad")]
impl NomadFetchId {
    /// Construct a boot-scoped identifier from its two exact components.
    pub const fn new(incarnation: [u8; 8], sequence: u64) -> Result<Self, InvalidNomadFetchId> {
        if sequence == 0 {
            return Err(InvalidNomadFetchId);
        }
        let sequence = sequence.to_be_bytes();
        Ok(Self([
            incarnation[0],
            incarnation[1],
            incarnation[2],
            incarnation[3],
            incarnation[4],
            incarnation[5],
            incarnation[6],
            incarnation[7],
            sequence[0],
            sequence[1],
            sequence[2],
            sequence[3],
            sequence[4],
            sequence[5],
            sequence[6],
            sequence[7],
        ]))
    }

    /// Validate all opaque bytes received from the wire.
    pub const fn from_bytes(bytes: [u8; 16]) -> Result<Self, InvalidNomadFetchId> {
        let sequence = u64::from_be_bytes([
            bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
        ]);
        if sequence == 0 {
            Err(InvalidNomadFetchId)
        } else {
            Ok(Self(bytes))
        }
    }

    /// Borrow all opaque identifier bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Boot-incarnation component.
    pub const fn incarnation(self) -> [u8; 8] {
        [
            self.0[0], self.0[1], self.0[2], self.0[3], self.0[4], self.0[5], self.0[6], self.0[7],
        ]
    }

    /// Nonzero sequence component.
    pub const fn sequence(self) -> u64 {
        u64::from_be_bytes([
            self.0[8], self.0[9], self.0[10], self.0[11], self.0[12], self.0[13], self.0[14],
            self.0[15],
        ])
    }
}

/// A fetch identifier's sequence component was zero.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidNomadFetchId;

/// Complete borrowed request to begin one bounded NomadNet page fetch.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NomadFetchStartRequest<'a> {
    destination: DestinationHash,
    path: NomadPagePath<'a>,
    timestamp_unix_ms: NomadRequestTimestampUnixMs,
    idempotency_key: IdempotencyKey,
}

#[cfg(feature = "experimental-nomad")]
impl<'a> NomadFetchStartRequest<'a> {
    /// Construct one invariant-preserving start request.
    pub const fn new(
        destination: DestinationHash,
        path: NomadPagePath<'a>,
        timestamp_unix_ms: NomadRequestTimestampUnixMs,
        idempotency_key: IdempotencyKey,
    ) -> Self {
        Self {
            destination,
            path,
            timestamp_unix_ms,
            idempotency_key,
        }
    }

    /// Complete remote `nomadnetwork.node` destination hash.
    pub const fn destination(self) -> DestinationHash {
        self.destination
    }

    /// Exact validated remote page path.
    pub const fn path(self) -> NomadPagePath<'a> {
        self.path
    }

    /// Caller-selected request timestamp.
    pub const fn timestamp_unix_ms(self) -> NomadRequestTimestampUnixMs {
        self.timestamp_unix_ms
    }

    /// Principal-scoped idempotency key.
    pub const fn idempotency_key(self) -> IdempotencyKey {
        self.idempotency_key
    }
}

/// Request to poll one principal-owned NomadNet fetch.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NomadFetchPollRequest {
    /// Device-assigned fetch identifier.
    pub id: NomadFetchId,
}

/// Public, copy-only summary of the node's Reticulum destinations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IdentitySummary {
    /// Primary destination served by the node.
    primary_destination: DestinationHash,
    /// Optional local `lxmf.delivery` destination served by the node.
    lxmf_delivery_destination: Option<DestinationHash>,
}

impl IdentitySummary {
    /// Construct the public summary for a node's primary destination.
    pub const fn new(primary_destination: DestinationHash) -> Self {
        Self {
            primary_destination,
            lxmf_delivery_destination: None,
        }
    }

    /// Construct a summary that also advertises the local `lxmf.delivery` destination.
    pub const fn with_lxmf_delivery_destination(
        primary_destination: DestinationHash,
        lxmf_delivery_destination: DestinationHash,
    ) -> Self {
        Self {
            primary_destination,
            lxmf_delivery_destination: Some(lxmf_delivery_destination),
        }
    }

    /// Primary destination served by the node.
    pub const fn primary_destination(self) -> DestinationHash {
        self.primary_destination
    }

    /// Local `lxmf.delivery` destination when that service is active.
    pub const fn lxmf_delivery_destination(self) -> Option<DestinationHash> {
        self.lxmf_delivery_destination
    }
}

/// Device-assigned identifier for a submitted operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SubmissionId(pub u64);

/// Permissions derived from device-owned authority, never from CBOR input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Permissions(u32);

impl Permissions {
    /// No authenticated operation permissions.
    pub const NONE: Self = Self(0);
    /// Read submission state belonging to the authenticated principal.
    pub const READ_SUBMISSION_STATUS: Self = Self(1 << 0);
    /// Submit outbound RNS DATA through the node's transport-neutral router.
    ///
    /// The bit remains part of the stable persisted permission vocabulary even
    /// when this build omits the experimental operation itself.
    pub const EXPERIMENTAL_SUBMIT_RNS_DATA: Self = Self(1 << 1);
    /// Mutate saved Wi-Fi and Reticulum TCP network configuration.
    ///
    /// This bit remains part of the stable persisted permission vocabulary
    /// even when a build omits the experimental network operations.
    pub const MANAGE_NETWORK_CONFIG: Self = Self(1 << 2);

    const KNOWN_BITS: u32 = Self::READ_SUBMISSION_STATUS.0
        | Self::EXPERIMENTAL_SUBMIT_RNS_DATA.0
        | Self::MANAGE_NETWORK_CONFIG.0;

    /// Whether all bits in `required` are present.
    pub const fn contains(self, required: Self) -> bool {
        self.0 & required.0 == required.0
    }

    /// Raw representation for session-policy adapters and diagnostics.
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Decode the stable persisted permission vocabulary without feature drift.
    pub const fn from_bits(bits: u32) -> Result<Self, UnknownPermissionBits> {
        let unknown = bits & !Self::KNOWN_BITS;
        if unknown == 0 {
            Ok(Self(bits))
        } else {
            Err(UnknownPermissionBits { unknown })
        }
    }
}

/// Persisted permission bits unknown to this device-API schema.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnknownPermissionBits {
    unknown: u32,
}

impl UnknownPermissionBits {
    /// Bits outside the stable schema-v1 permission vocabulary.
    pub const fn unknown(self) -> u32 {
        self.unknown
    }
}

impl BitOr for Permissions {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

/// Device-owned credential facts that authorized one dispatch attempt.
///
/// This value is supplied out of band with the trusted dispatch context and is
/// never decoded from the device-API wire message. Its public constructor lets
/// trusted integration code move these scalar facts between portable crates;
/// it is not an unforgeable authorization capability against linked Rust code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DispatchProvenance {
    credential_id: [u8; 16],
    credential_generation: u64,
    authority_revision: u64,
    policy_version: u32,
}

/// Invalid device-owned facts supplied for dispatch provenance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchProvenanceError {
    /// The all-zero credential identifier is reserved for erased state.
    ZeroCredentialId,
    /// Credential generation zero is reserved for erased state.
    ZeroCredentialGeneration,
    /// Authority revision zero is reserved for erased state.
    ZeroAuthorityRevision,
    /// Authorization-policy version zero is reserved for erased state.
    ZeroPolicyVersion,
    /// A credential generation cannot originate after the observed authority.
    GenerationExceedsAuthorityRevision {
        /// Candidate credential generation.
        credential_generation: u64,
        /// Candidate complete-authority revision.
        authority_revision: u64,
    },
}

impl DispatchProvenance {
    /// Validate and construct provenance from credential-authority state.
    pub const fn new(
        credential_id: [u8; 16],
        credential_generation: u64,
        authority_revision: u64,
        policy_version: u32,
    ) -> Result<Self, DispatchProvenanceError> {
        let mut byte = 0;
        let mut has_nonzero_id_byte = false;
        while byte < credential_id.len() {
            if credential_id[byte] != 0 {
                has_nonzero_id_byte = true;
                break;
            }
            byte += 1;
        }
        if !has_nonzero_id_byte {
            return Err(DispatchProvenanceError::ZeroCredentialId);
        }
        if credential_generation == 0 {
            return Err(DispatchProvenanceError::ZeroCredentialGeneration);
        }
        if authority_revision == 0 {
            return Err(DispatchProvenanceError::ZeroAuthorityRevision);
        }
        if policy_version == 0 {
            return Err(DispatchProvenanceError::ZeroPolicyVersion);
        }
        if credential_generation > authority_revision {
            return Err(
                DispatchProvenanceError::GenerationExceedsAuthorityRevision {
                    credential_generation,
                    authority_revision,
                },
            );
        }
        Ok(Self {
            credential_id,
            credential_generation,
            authority_revision,
            policy_version,
        })
    }

    /// Opaque identifier of the credential revalidated for this attempt.
    pub const fn credential_id(self) -> [u8; 16] {
        self.credential_id
    }

    /// Exact credential generation revalidated for this attempt.
    pub const fn credential_generation(self) -> u64 {
        self.credential_generation
    }

    /// Complete credential-authority revision observed at revalidation.
    pub const fn authority_revision(self) -> u64 {
        self.authority_revision
    }

    /// Authorization-policy version applied by the credential record.
    pub const fn policy_version(self) -> u32 {
        self.policy_version
    }
}

/// Trusted authentication and authorization facts supplied out of band.
#[derive(Debug, Eq, PartialEq)]
pub struct DispatchContext {
    /// Principal derived from device-owned authenticated credential state, if any.
    principal: Option<PrincipalId>,
    /// Permissions granted to that authenticated principal.
    permissions: Permissions,
    /// Credential-authority facts captured by exact pre-dispatch revalidation.
    provenance: Option<DispatchProvenance>,
}

impl DispatchContext {
    /// Context for a connection without an authenticated application session.
    pub const UNAUTHENTICATED: Self = Self {
        principal: None,
        permissions: Permissions::NONE,
        provenance: None,
    };

    /// Construct a trusted context for an authenticated principal.
    pub const fn authenticated(
        principal: PrincipalId,
        permissions: Permissions,
        provenance: DispatchProvenance,
    ) -> Self {
        Self {
            principal: Some(principal),
            permissions,
            provenance: Some(provenance),
        }
    }

    /// Device-owned authenticated principal, if this context has one.
    pub const fn principal(&self) -> Option<PrincipalId> {
        self.principal
    }

    /// Device-owned permissions bound to this dispatch attempt.
    pub const fn permissions(&self) -> Permissions {
        self.permissions
    }

    /// Credential-authority facts for this authenticated attempt, if any.
    pub const fn provenance(&self) -> Option<DispatchProvenance> {
        self.provenance
    }
}

/// Permission category required by an operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequiredPermission {
    /// Read submission status.
    ReadSubmissionStatus,
    /// Submit outbound RNS DATA through the unstable transport-neutral path.
    ExperimentalSubmitRnsData,
    /// Mutate saved Wi-Fi and Reticulum TCP network configuration.
    ManageNetworkConfig,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuthorizationRequirement {
    Public,
    Authenticated,
    Permission(RequiredPermission),
}

impl RequiredPermission {
    const fn bits(self) -> Permissions {
        match self {
            Self::ReadSubmissionStatus => Permissions::READ_SUBMISSION_STATUS,
            Self::ExperimentalSubmitRnsData => Permissions::EXPERIMENTAL_SUBMIT_RNS_DATA,
            Self::ManageNetworkConfig => Permissions::MANAGE_NETWORK_CONFIG,
        }
    }
}

/// Authorization failure established without consulting untrusted message data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorizationError {
    /// The operation requires an authenticated principal.
    AuthenticationRequired,
    /// The authenticated principal lacks the operation permission.
    PermissionDenied(RequiredPermission),
}

/// Logical request body. It contains no transport, session, or Rete owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DeviceRequest<'a> {
    /// Read API version, safety capabilities, and hard codec limits.
    SystemCapabilities,
    /// Read the node's public primary Reticulum destination.
    IdentitySummary,
    /// Read status for a previously accepted submission.
    SubmissionStatus {
        /// Device-assigned submission identifier.
        id: SubmissionId,
    },
    /// Read bounded runtime state for the inbound RNS DATA mailbox.
    #[cfg(feature = "experimental-rns-inbox")]
    RnsInboxStatus,
    /// Read the oldest inbound RNS DATA item without consuming it.
    #[cfg(feature = "experimental-rns-inbox")]
    RnsInboxPeek,
    /// Read the next committed LXMF summary after an optional stable handle.
    #[cfg(feature = "experimental-lxmf")]
    LxmfNext {
        /// Exclusive physical-commit-order cursor; `None` selects the first message.
        after: Option<LxmfMessageHandle>,
    },
    /// Read one bounded chunk of an exact committed normalized LXMF wire message.
    #[cfg(feature = "experimental-lxmf")]
    LxmfRead {
        /// Stable committed-message handle.
        handle: LxmfMessageHandle,
        /// Zero-based byte offset in the normalized wire representation.
        offset: u32,
        /// Maximum bytes requested in this response.
        max_bytes: LxmfReadLength,
    },
    /// Read the durable collection watermark for the LXMF mailbox.
    #[cfg(feature = "experimental-lxmf")]
    LxmfMailboxStatus,
    /// Advance the durable collection watermark through one committed message.
    ///
    /// Repeating an already-applied watermark is an idempotent success.
    #[cfg(feature = "experimental-lxmf")]
    LxmfMailboxAcknowledge {
        /// Highest committed message durably imported by the authenticated client.
        through: LxmfMessageHandle,
    },
    /// Compose and durably submit a basic LXMF message using the device-owned source.
    ///
    /// Empty title and content values are valid. The codec bounds fields and
    /// message size; product composition applies its additional carrier rules.
    #[cfg(feature = "experimental-lxmf")]
    LxmfBasicSend {
        /// Complete remote `lxmf.delivery` destination hash.
        destination: DestinationHash,
        /// Caller-selected Unix timestamp in milliseconds.
        ///
        /// The bearer-neutral codec accepts `u64`; the current product composer
        /// accepts exactly `1..=8_796_093_022_207_999`.
        timestamp_unix_ms: u64,
        /// Borrowed binary title; interpretation belongs to the LXMF application.
        ///
        /// The codec's 295-byte field bound is not a guarantee that every
        /// title/content combination fits the encoded body or product carrier.
        title: &'a [u8],
        /// Borrowed binary content; interpretation belongs to the LXMF application.
        ///
        /// The codec's 295-byte field bound is not a guarantee that every
        /// title/content combination fits the encoded body or product carrier.
        content: &'a [u8],
        /// Optional phone location frozen into the signed LXMF payload.
        location: Option<LxmfMessageLocation>,
        /// Deduplication key scoped by the authenticated principal and composed message.
        idempotency_key: IdempotencyKey,
    },
    /// Read one nearby `lxmf.delivery` peer from the volatile bounded projection.
    #[cfg(feature = "experimental-lxmf")]
    LxmfPeerNext {
        /// Optional exclusive cursor scoped to one device boot/incarnation.
        ///
        /// `None` starts from the oldest retained record. The wire requires
        /// both cursor fields together, preventing ambiguous partial cursors.
        after: Option<LxmfPeerDiscoveryCursor>,
    },
    /// Begin one authenticated bounded NomadNet page fetch.
    #[cfg(feature = "experimental-nomad")]
    NomadFetchStart(NomadFetchStartRequest<'a>),
    /// Poll one authenticated principal-owned NomadNet page fetch.
    #[cfg(feature = "experimental-nomad")]
    NomadFetchPoll(NomadFetchPollRequest),
    /// Begin one authenticated boot-scoped Reticulum path-and-proof probe.
    ReticulumProbeStart(ProbeStartRequest),
    /// Poll one authenticated principal-owned Reticulum path-and-proof probe.
    ReticulumProbePoll(ProbePollRequest),
    /// Read the complete desired configuration with Wi-Fi secrets redacted.
    #[cfg(feature = "experimental-network-config")]
    NetworkConfigGet,
    /// Mutate one saved Wi-Fi profile or the single Reticulum TCP peer.
    #[cfg(feature = "experimental-network-config")]
    NetworkConfigMutate(NetworkConfigMutationRequest<'a>),
    /// Read live Wi-Fi station and Reticulum TCP peer state.
    #[cfg(feature = "experimental-network-config")]
    NetworkStatus,
    /// Read one bounded cross-interface node diagnostics snapshot.
    NodeDiagnostics,
    /// Read one bounded lexicographically ordered route diagnostics page.
    RouteDiagnosticsPage(RouteDiagnosticsRequest),
    /// Read one bounded boot-scoped packet-correlated radio trace page.
    RadioTracePage(RadioTracePageRequest),
    /// Queue the node's ordinary primary, LXMF, and NomadNet service announces.
    ManualServiceAnnounce,
    /// Durably submit outbound RNS DATA without selecting a physical transport.
    #[cfg(feature = "experimental-rns-data")]
    SubmitRnsData {
        /// Complete Reticulum destination hash.
        destination: DestinationHash,
        /// Borrowed application data; never allocated or copied by decoding.
        payload: &'a [u8],
        /// Deduplication key scoped by the authenticated principal and content.
        idempotency_key: IdempotencyKey,
    },
    /// Uninhabited marker keeping the decode lifetime stable without the
    /// experimental RNS DATA operation.
    #[doc(hidden)]
    __Borrowed(Infallible, PhantomData<&'a [u8]>),
}

impl DeviceRequest<'_> {
    /// Stable or experimental operation number encoded on the wire.
    pub const fn operation(&self) -> u16 {
        match self {
            Self::SystemCapabilities => OP_SYSTEM_CAPABILITIES,
            Self::IdentitySummary => OP_IDENTITY_SUMMARY,
            Self::SubmissionStatus { .. } => OP_SUBMISSION_STATUS,
            #[cfg(feature = "experimental-rns-inbox")]
            Self::RnsInboxStatus => OP_EXPERIMENTAL_RNS_INBOX_STATUS,
            #[cfg(feature = "experimental-rns-inbox")]
            Self::RnsInboxPeek => OP_EXPERIMENTAL_RNS_INBOX_PEEK,
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfNext { .. } => OP_EXPERIMENTAL_LXMF_NEXT,
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfRead { .. } => OP_EXPERIMENTAL_LXMF_READ,
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfMailboxStatus => OP_EXPERIMENTAL_LXMF_MAILBOX_STATUS,
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfMailboxAcknowledge { .. } => OP_EXPERIMENTAL_LXMF_MAILBOX_ACKNOWLEDGE,
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfBasicSend { .. } => OP_EXPERIMENTAL_LXMF_BASIC_SEND,
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfPeerNext { .. } => OP_EXPERIMENTAL_LXMF_PEER_NEXT,
            #[cfg(feature = "experimental-nomad")]
            Self::NomadFetchStart(_) => OP_EXPERIMENTAL_NOMAD_FETCH_START,
            #[cfg(feature = "experimental-nomad")]
            Self::NomadFetchPoll(_) => OP_EXPERIMENTAL_NOMAD_FETCH_POLL,
            Self::ReticulumProbeStart(_) => OP_EXPERIMENTAL_RETICULUM_PROBE_START,
            Self::ReticulumProbePoll(_) => OP_EXPERIMENTAL_RETICULUM_PROBE_POLL,
            #[cfg(feature = "experimental-network-config")]
            Self::NetworkConfigGet => OP_EXPERIMENTAL_NETWORK_CONFIG_GET,
            #[cfg(feature = "experimental-network-config")]
            Self::NetworkConfigMutate(_) => OP_EXPERIMENTAL_NETWORK_CONFIG_MUTATE,
            #[cfg(feature = "experimental-network-config")]
            Self::NetworkStatus => OP_EXPERIMENTAL_NETWORK_STATUS,
            Self::NodeDiagnostics => OP_EXPERIMENTAL_NODE_DIAGNOSTICS,
            Self::RouteDiagnosticsPage(_) => OP_EXPERIMENTAL_ROUTE_DIAGNOSTICS_PAGE,
            Self::RadioTracePage(_) => OP_EXPERIMENTAL_RADIO_TRACE_PAGE,
            Self::ManualServiceAnnounce => OP_EXPERIMENTAL_MANUAL_SERVICE_ANNOUNCE,
            #[cfg(feature = "experimental-rns-data")]
            Self::SubmitRnsData { .. } => OP_EXPERIMENTAL_SUBMIT_RNS_DATA,
            Self::__Borrowed(never, _) => match *never {},
        }
    }

    /// Whether this operation can change node state.
    pub const fn is_mutating(&self) -> bool {
        match self {
            Self::SystemCapabilities | Self::IdentitySummary | Self::SubmissionStatus { .. } => {
                false
            }
            #[cfg(feature = "experimental-rns-inbox")]
            Self::RnsInboxStatus | Self::RnsInboxPeek => false,
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfNext { .. }
            | Self::LxmfRead { .. }
            | Self::LxmfMailboxStatus
            | Self::LxmfPeerNext { .. } => false,
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfMailboxAcknowledge { .. } => true,
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfBasicSend { .. } => true,
            #[cfg(feature = "experimental-nomad")]
            Self::NomadFetchStart(_) => true,
            #[cfg(feature = "experimental-nomad")]
            Self::NomadFetchPoll(_) => false,
            Self::ReticulumProbeStart(_) => true,
            Self::ReticulumProbePoll(_) => false,
            #[cfg(feature = "experimental-network-config")]
            Self::NetworkConfigGet | Self::NetworkStatus => false,
            #[cfg(feature = "experimental-network-config")]
            Self::NetworkConfigMutate(_) => true,
            Self::NodeDiagnostics | Self::RouteDiagnosticsPage(_) | Self::RadioTracePage(_) => {
                false
            }
            Self::ManualServiceAnnounce => true,
            #[cfg(feature = "experimental-rns-data")]
            Self::SubmitRnsData { .. } => true,
            Self::__Borrowed(never, _) => match *never {},
        }
    }

    const fn authorization_requirement(&self) -> AuthorizationRequirement {
        match self {
            Self::SystemCapabilities | Self::IdentitySummary => AuthorizationRequirement::Public,
            Self::SubmissionStatus { .. } => {
                AuthorizationRequirement::Permission(RequiredPermission::ReadSubmissionStatus)
            }
            #[cfg(feature = "experimental-rns-inbox")]
            Self::RnsInboxStatus | Self::RnsInboxPeek => AuthorizationRequirement::Authenticated,
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfNext { .. }
            | Self::LxmfRead { .. }
            | Self::LxmfMailboxStatus
            | Self::LxmfMailboxAcknowledge { .. }
            | Self::LxmfPeerNext { .. } => AuthorizationRequirement::Authenticated,
            #[cfg(feature = "experimental-nomad")]
            Self::NomadFetchStart(_) | Self::NomadFetchPoll(_) => {
                AuthorizationRequirement::Authenticated
            }
            Self::ReticulumProbeStart(_) => {
                AuthorizationRequirement::Permission(RequiredPermission::ExperimentalSubmitRnsData)
            }
            Self::ReticulumProbePoll(_) => AuthorizationRequirement::Authenticated,
            #[cfg(feature = "experimental-network-config")]
            Self::NetworkConfigGet | Self::NetworkStatus => AuthorizationRequirement::Authenticated,
            #[cfg(feature = "experimental-network-config")]
            Self::NetworkConfigMutate(_) => {
                AuthorizationRequirement::Permission(RequiredPermission::ManageNetworkConfig)
            }
            Self::NodeDiagnostics | Self::RouteDiagnosticsPage(_) | Self::RadioTracePage(_) => {
                AuthorizationRequirement::Authenticated
            }
            Self::ManualServiceAnnounce => AuthorizationRequirement::Authenticated,
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfBasicSend { .. } => {
                AuthorizationRequirement::Permission(RequiredPermission::ExperimentalSubmitRnsData)
            }
            #[cfg(feature = "experimental-rns-data")]
            Self::SubmitRnsData { .. } => {
                AuthorizationRequirement::Permission(RequiredPermission::ExperimentalSubmitRnsData)
            }
            Self::__Borrowed(never, _) => match *never {},
        }
    }
}

/// Logical request envelope decoded from exactly one CBOR item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestEnvelope<'a> {
    /// Protocol version selected by the client.
    pub version: ApiVersion,
    /// Client request identifier.
    pub request_id: RequestId,
    /// Operation-specific request.
    pub request: DeviceRequest<'a>,
}

/// Apply common authentication and permission policy to a decoded request.
///
/// The principal is intentionally absent from [`RequestEnvelope`]. Callers
/// must obtain `context` from their separately authenticated session.
pub const fn authorize_request(
    context: &DispatchContext,
    request: &DeviceRequest<'_>,
) -> Result<(), AuthorizationError> {
    match request.authorization_requirement() {
        AuthorizationRequirement::Public => Ok(()),
        AuthorizationRequirement::Authenticated => {
            if context.principal.is_some() {
                Ok(())
            } else {
                Err(AuthorizationError::AuthenticationRequired)
            }
        }
        AuthorizationRequirement::Permission(required) => {
            if context.principal.is_none() {
                return Err(AuthorizationError::AuthenticationRequired);
            }
            if !context.permissions.contains(required.bits()) {
                return Err(AuthorizationError::PermissionDenied(required));
            }
            Ok(())
        }
    }
}

/// Runtime availability of a logical capability.
///
/// This is a closed API-v1 wire vocabulary. Adding a numeric value requires a
/// new API major version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum CapabilityAvailability {
    /// This build cannot perform the capability.
    Unavailable = 0,
    /// Code exists, but profile or runtime policy has disabled it.
    Disabled = 1,
    /// The capability is present and enabled.
    Available = 2,
}

impl CapabilityAvailability {
    /// Frozen API-v1 numeric representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Device-owned capability and codec-limit handshake.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapabilitySnapshot {
    /// Highest API version spoken by this device.
    pub(crate) api_version: ApiVersion,
    /// Whether any public operation can return raw prepared packet bytes.
    pub(crate) packet_output: bool,
    /// Availability of raw/direct physical-radio transmission to local clients.
    pub(crate) direct_radio_tx: CapabilityAvailability,
    /// Whether this snapshot advertises transport-neutral outbound RNS DATA submission.
    pub(crate) experimental_submit_rns_data: bool,
    /// Hard maximum logical CBOR message size.
    pub(crate) max_message_bytes: u16,
    /// Hard maximum encoded operation-body size.
    pub(crate) max_body_bytes: u16,
    /// Maximum experimental submission payload.
    pub(crate) max_submit_rns_data_payload_bytes: u16,
    /// Runtime availability of the experimental inbound RNS DATA mailbox.
    pub(crate) experimental_rns_inbox: CapabilityAvailability,
    /// Maximum payload returned by one experimental inbox item.
    pub(crate) max_rns_inbox_payload_bytes: u16,
    /// Runtime availability of committed LXMF discovery and bounded reads.
    pub(crate) experimental_lxmf: CapabilityAvailability,
    /// Maximum exact normalized wire bytes returned by one LXMF read.
    pub(crate) max_lxmf_read_chunk_bytes: u16,
    /// Runtime availability of source-free basic LXMF composition and submission.
    pub(crate) experimental_lxmf_basic_send: CapabilityAvailability,
    /// Structural per-field title limit advertised by the logical codec.
    ///
    /// Product composition and carrier limits can reduce the accepted
    /// title/content combination.
    pub(crate) max_lxmf_basic_title_bytes: u16,
    /// Structural per-field content limit advertised by the logical codec.
    ///
    /// Product composition and carrier limits can reduce the accepted
    /// title/content combination.
    pub(crate) max_lxmf_basic_content_bytes: u16,
    /// Runtime availability of bounded nearby `lxmf.delivery` peer discovery.
    pub(crate) experimental_lxmf_peer_discovery: CapabilityAvailability,
    /// Maximum authenticated announce application data returned with one peer.
    pub(crate) max_lxmf_peer_app_data_bytes: u16,
    /// Runtime availability of bounded authenticated NomadNet page fetch.
    pub(crate) experimental_nomad: CapabilityAvailability,
    /// Maximum UTF-8 request-path bytes accepted by NomadNet fetch.
    pub(crate) max_nomad_page_path_bytes: u16,
    /// Maximum valid UTF-8 Micron page bytes returned by NomadNet fetch.
    pub(crate) max_nomad_page_bytes: u16,
    /// Runtime availability of redacted network configuration and status.
    pub(crate) experimental_network_config: CapabilityAvailability,
    /// Runtime availability of authenticated ordinary service announces.
    pub(crate) manual_service_announce: CapabilityAvailability,
    /// Runtime availability of authenticated Reticulum path-and-proof probes.
    pub(crate) experimental_reticulum_probe: CapabilityAvailability,
}

impl CapabilitySnapshot {
    /// Snapshot for this crate's current build.
    ///
    /// Packet output and direct-radio TX remain deliberately unavailable in
    /// every feature composition. Outbound RNS submission is a separate,
    /// transport-neutral capability.
    pub const fn current() -> Self {
        Self {
            api_version: ApiVersion::CURRENT,
            packet_output: false,
            direct_radio_tx: CapabilityAvailability::Unavailable,
            experimental_submit_rns_data: cfg!(feature = "experimental-rns-data"),
            max_message_bytes: MAX_MESSAGE_BYTES as u16,
            max_body_bytes: MAX_BODY_BYTES as u16,
            max_submit_rns_data_payload_bytes: MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES as u16,
            experimental_rns_inbox: if cfg!(feature = "experimental-rns-inbox") {
                CapabilityAvailability::Available
            } else {
                CapabilityAvailability::Unavailable
            },
            max_rns_inbox_payload_bytes: if cfg!(feature = "experimental-rns-inbox") {
                383
            } else {
                0
            },
            experimental_lxmf: if cfg!(feature = "experimental-lxmf") {
                CapabilityAvailability::Available
            } else {
                CapabilityAvailability::Unavailable
            },
            max_lxmf_read_chunk_bytes: if cfg!(feature = "experimental-lxmf") {
                416
            } else {
                0
            },
            experimental_lxmf_basic_send: if cfg!(feature = "experimental-lxmf") {
                CapabilityAvailability::Available
            } else {
                CapabilityAvailability::Unavailable
            },
            max_lxmf_basic_title_bytes: if cfg!(feature = "experimental-lxmf") {
                MAX_LXMF_BASIC_TITLE_BYTES as u16
            } else {
                0
            },
            max_lxmf_basic_content_bytes: if cfg!(feature = "experimental-lxmf") {
                MAX_LXMF_BASIC_CONTENT_BYTES as u16
            } else {
                0
            },
            experimental_lxmf_peer_discovery: if cfg!(feature = "experimental-lxmf") {
                CapabilityAvailability::Available
            } else {
                CapabilityAvailability::Unavailable
            },
            max_lxmf_peer_app_data_bytes: if cfg!(feature = "experimental-lxmf") {
                MAX_LXMF_PEER_APP_DATA_BYTES as u16
            } else {
                0
            },
            experimental_nomad: if cfg!(feature = "experimental-nomad") {
                CapabilityAvailability::Available
            } else {
                CapabilityAvailability::Unavailable
            },
            max_nomad_page_path_bytes: if cfg!(feature = "experimental-nomad") {
                MAX_NOMAD_PAGE_PATH_BYTES as u16
            } else {
                0
            },
            max_nomad_page_bytes: if cfg!(feature = "experimental-nomad") {
                MAX_NOMAD_PAGE_BYTES as u16
            } else {
                0
            },
            experimental_network_config: if cfg!(feature = "experimental-network-config") {
                CapabilityAvailability::Available
            } else {
                CapabilityAvailability::Unavailable
            },
            manual_service_announce: CapabilityAvailability::Available,
            experimental_reticulum_probe: CapabilityAvailability::Available,
        }
    }

    /// Snapshot restricted to operations implemented by a higher dispatch layer.
    ///
    /// `experimental_submit_rns_data` can disable the codec-build capability,
    /// but cannot enable an operation omitted from this crate's build. This
    /// keeps Cargo feature unification in another dependency edge from making a
    /// dispatcher advertise an operation that it did not compile locally.
    pub const fn for_dispatch(experimental_submit_rns_data: bool) -> Self {
        let mut snapshot = Self::current();
        snapshot.experimental_submit_rns_data &= experimental_submit_rns_data;
        snapshot.experimental_rns_inbox = CapabilityAvailability::Unavailable;
        snapshot.max_rns_inbox_payload_bytes = 0;
        snapshot.experimental_lxmf = CapabilityAvailability::Unavailable;
        snapshot.max_lxmf_read_chunk_bytes = 0;
        snapshot.experimental_lxmf_basic_send = CapabilityAvailability::Unavailable;
        snapshot.max_lxmf_basic_title_bytes = 0;
        snapshot.max_lxmf_basic_content_bytes = 0;
        snapshot.experimental_lxmf_peer_discovery = CapabilityAvailability::Unavailable;
        snapshot.max_lxmf_peer_app_data_bytes = 0;
        snapshot.experimental_nomad = CapabilityAvailability::Unavailable;
        snapshot.max_nomad_page_path_bytes = 0;
        snapshot.max_nomad_page_bytes = 0;
        snapshot.experimental_network_config = CapabilityAvailability::Unavailable;
        snapshot.manual_service_announce = CapabilityAvailability::Unavailable;
        snapshot.experimental_reticulum_probe = CapabilityAvailability::Unavailable;
        snapshot
    }

    /// Snapshot restricted to submission and inbox operations implemented by a dispatcher.
    pub const fn for_dispatch_with_inbox(
        experimental_submit_rns_data: bool,
        experimental_rns_inbox: CapabilityAvailability,
    ) -> Self {
        let mut snapshot = Self::current();
        snapshot.experimental_submit_rns_data &= experimental_submit_rns_data;
        if cfg!(feature = "experimental-rns-inbox") {
            snapshot.experimental_rns_inbox = experimental_rns_inbox;
            snapshot.max_rns_inbox_payload_bytes =
                if matches!(experimental_rns_inbox, CapabilityAvailability::Unavailable) {
                    0
                } else {
                    383
                };
        } else {
            snapshot.experimental_rns_inbox = CapabilityAvailability::Unavailable;
            snapshot.max_rns_inbox_payload_bytes = 0;
        }
        snapshot.experimental_lxmf = CapabilityAvailability::Unavailable;
        snapshot.max_lxmf_read_chunk_bytes = 0;
        snapshot.experimental_lxmf_basic_send = CapabilityAvailability::Unavailable;
        snapshot.max_lxmf_basic_title_bytes = 0;
        snapshot.max_lxmf_basic_content_bytes = 0;
        snapshot.experimental_lxmf_peer_discovery = CapabilityAvailability::Unavailable;
        snapshot.max_lxmf_peer_app_data_bytes = 0;
        snapshot.experimental_nomad = CapabilityAvailability::Unavailable;
        snapshot.max_nomad_page_path_bytes = 0;
        snapshot.max_nomad_page_bytes = 0;
        snapshot.experimental_network_config = CapabilityAvailability::Unavailable;
        snapshot.manual_service_announce = CapabilityAvailability::Unavailable;
        snapshot.experimental_reticulum_probe = CapabilityAvailability::Unavailable;
        snapshot
    }

    /// Restrict submission and NomadNet fetch to two independent dispatcher ports.
    pub const fn for_dispatch_with_nomad(
        experimental_submit_rns_data: bool,
        experimental_nomad: CapabilityAvailability,
    ) -> Self {
        Self::for_dispatch(experimental_submit_rns_data).with_dispatch_nomad(experimental_nomad)
    }

    /// Add the independently owned NomadNet port to an existing dispatcher snapshot.
    ///
    /// This preserves every capability already selected by the higher
    /// dispatcher. It cannot enable NomadNet fetch when that codec feature was
    /// omitted from this crate's build.
    pub const fn with_dispatch_nomad(mut self, experimental_nomad: CapabilityAvailability) -> Self {
        if cfg!(feature = "experimental-nomad") {
            self.experimental_nomad = experimental_nomad;
            let available = !matches!(experimental_nomad, CapabilityAvailability::Unavailable);
            self.max_nomad_page_path_bytes = if available {
                MAX_NOMAD_PAGE_PATH_BYTES as u16
            } else {
                0
            };
            self.max_nomad_page_bytes = if available {
                MAX_NOMAD_PAGE_BYTES as u16
            } else {
                0
            };
        }
        self
    }

    /// Add the independently owned network-configuration port to a dispatcher snapshot.
    ///
    /// This cannot enable the port when the codec feature was omitted from
    /// this crate's build.
    pub const fn with_dispatch_network_config(
        mut self,
        experimental_network_config: CapabilityAvailability,
    ) -> Self {
        if cfg!(feature = "experimental-network-config") {
            self.experimental_network_config = experimental_network_config;
        }
        self
    }

    /// Add authenticated ordinary service announces to a dispatcher snapshot.
    pub const fn with_dispatch_manual_service_announce(
        mut self,
        manual_service_announce: CapabilityAvailability,
    ) -> Self {
        self.manual_service_announce = manual_service_announce;
        self
    }

    /// Add the independently owned Reticulum probe port to a dispatcher snapshot.
    pub const fn with_dispatch_reticulum_probe(
        mut self,
        experimental_reticulum_probe: CapabilityAvailability,
    ) -> Self {
        self.experimental_reticulum_probe = experimental_reticulum_probe;
        self
    }

    /// Restrict submission, raw-inbox, and LXMF capabilities to a higher dispatcher.
    pub const fn for_dispatch_with_inbox_and_lxmf(
        experimental_submit_rns_data: bool,
        experimental_rns_inbox: CapabilityAvailability,
        experimental_lxmf: CapabilityAvailability,
    ) -> Self {
        let mut snapshot =
            Self::for_dispatch_with_inbox(experimental_submit_rns_data, experimental_rns_inbox);
        if cfg!(feature = "experimental-lxmf") {
            snapshot.experimental_lxmf = experimental_lxmf;
            snapshot.max_lxmf_read_chunk_bytes =
                if matches!(experimental_lxmf, CapabilityAvailability::Unavailable) {
                    0
                } else {
                    416
                };
        }
        snapshot
    }

    /// Restrict submission, inbox, LXMF reads, and basic LXMF send to one dispatcher.
    pub const fn for_dispatch_with_inbox_lxmf_and_basic_send(
        experimental_submit_rns_data: bool,
        experimental_rns_inbox: CapabilityAvailability,
        experimental_lxmf: CapabilityAvailability,
        experimental_lxmf_basic_send: CapabilityAvailability,
    ) -> Self {
        let mut snapshot = Self::for_dispatch_with_inbox_and_lxmf(
            experimental_submit_rns_data,
            experimental_rns_inbox,
            experimental_lxmf,
        );
        if cfg!(feature = "experimental-lxmf") {
            snapshot.experimental_lxmf_basic_send = experimental_lxmf_basic_send;
            let available = !matches!(
                experimental_lxmf_basic_send,
                CapabilityAvailability::Unavailable
            );
            snapshot.max_lxmf_basic_title_bytes = if available {
                MAX_LXMF_BASIC_TITLE_BYTES as u16
            } else {
                0
            };
            snapshot.max_lxmf_basic_content_bytes = if available {
                MAX_LXMF_BASIC_CONTENT_BYTES as u16
            } else {
                0
            };
        }
        snapshot
    }

    /// Restrict submission, inbox, LXMF, send, and peer discovery to one dispatcher.
    #[allow(clippy::too_many_arguments)]
    pub const fn for_dispatch_with_inbox_lxmf_basic_send_and_peer_discovery(
        experimental_submit_rns_data: bool,
        experimental_rns_inbox: CapabilityAvailability,
        experimental_lxmf: CapabilityAvailability,
        experimental_lxmf_basic_send: CapabilityAvailability,
        experimental_lxmf_peer_discovery: CapabilityAvailability,
        max_lxmf_peer_app_data_bytes: u16,
    ) -> Self {
        let mut snapshot = Self::for_dispatch_with_inbox_lxmf_and_basic_send(
            experimental_submit_rns_data,
            experimental_rns_inbox,
            experimental_lxmf,
            experimental_lxmf_basic_send,
        );
        if cfg!(feature = "experimental-lxmf") {
            snapshot.experimental_lxmf_peer_discovery = experimental_lxmf_peer_discovery;
            snapshot.max_lxmf_peer_app_data_bytes = if matches!(
                experimental_lxmf_peer_discovery,
                CapabilityAvailability::Unavailable
            ) {
                0
            } else if max_lxmf_peer_app_data_bytes > MAX_LXMF_PEER_APP_DATA_BYTES as u16 {
                MAX_LXMF_PEER_APP_DATA_BYTES as u16
            } else {
                max_lxmf_peer_app_data_bytes
            };
        }
        snapshot
    }

    /// Highest API version spoken by this device.
    pub const fn api_version(self) -> ApiVersion {
        self.api_version
    }

    /// Whether any public operation can return raw prepared packet bytes.
    pub const fn packet_output(self) -> bool {
        self.packet_output
    }

    /// Availability of raw/direct physical-radio transmission to local clients.
    ///
    /// This does not describe transport-neutral RNS submission, which may
    /// route over LoRa or another enabled Reticulum interface.
    pub const fn direct_radio_tx(self) -> CapabilityAvailability {
        self.direct_radio_tx
    }

    /// Whether this snapshot advertises transport-neutral RNS DATA submission.
    pub const fn experimental_submit_rns_data(self) -> bool {
        self.experimental_submit_rns_data
    }

    /// Hard maximum logical CBOR message size.
    pub const fn max_message_bytes(self) -> u16 {
        self.max_message_bytes
    }

    /// Hard maximum encoded operation-body size.
    pub const fn max_body_bytes(self) -> u16 {
        self.max_body_bytes
    }

    /// Maximum experimental submission payload.
    pub const fn max_submit_rns_data_payload_bytes(self) -> u16 {
        self.max_submit_rns_data_payload_bytes
    }

    /// Runtime availability of the experimental inbound RNS DATA mailbox.
    pub const fn experimental_rns_inbox(self) -> CapabilityAvailability {
        self.experimental_rns_inbox
    }

    /// Maximum payload returned by one experimental inbox item.
    pub const fn max_rns_inbox_payload_bytes(self) -> u16 {
        self.max_rns_inbox_payload_bytes
    }

    /// Runtime availability of committed LXMF discovery and bounded reads.
    pub const fn experimental_lxmf(self) -> CapabilityAvailability {
        self.experimental_lxmf
    }

    /// Maximum exact normalized wire bytes returned by one LXMF read.
    pub const fn max_lxmf_read_chunk_bytes(self) -> u16 {
        self.max_lxmf_read_chunk_bytes
    }

    /// Runtime availability of source-free basic LXMF composition and submission.
    pub const fn experimental_lxmf_basic_send(self) -> CapabilityAvailability {
        self.experimental_lxmf_basic_send
    }

    /// Structural codec limit for one source-free basic-LXMF title.
    ///
    /// The encoded-body and product-carrier limits can reject a smaller
    /// title/content combination.
    pub const fn max_lxmf_basic_title_bytes(self) -> u16 {
        self.max_lxmf_basic_title_bytes
    }

    /// Structural codec limit for one source-free basic-LXMF content value.
    ///
    /// The encoded-body and product-carrier limits can reject a smaller
    /// title/content combination.
    pub const fn max_lxmf_basic_content_bytes(self) -> u16 {
        self.max_lxmf_basic_content_bytes
    }

    /// Runtime availability of bounded nearby `lxmf.delivery` peer discovery.
    pub const fn experimental_lxmf_peer_discovery(self) -> CapabilityAvailability {
        self.experimental_lxmf_peer_discovery
    }

    /// Maximum authenticated announce application data returned with one peer.
    pub const fn max_lxmf_peer_app_data_bytes(self) -> u16 {
        self.max_lxmf_peer_app_data_bytes
    }

    /// Runtime availability of bounded authenticated NomadNet page fetch.
    pub const fn experimental_nomad(self) -> CapabilityAvailability {
        self.experimental_nomad
    }

    /// Maximum UTF-8 request-path bytes accepted by NomadNet fetch.
    pub const fn max_nomad_page_path_bytes(self) -> u16 {
        self.max_nomad_page_path_bytes
    }

    /// Maximum valid UTF-8 Micron page bytes returned by NomadNet fetch.
    pub const fn max_nomad_page_bytes(self) -> u16 {
        self.max_nomad_page_bytes
    }

    /// Runtime availability of redacted network configuration and status.
    pub const fn experimental_network_config(self) -> CapabilityAvailability {
        self.experimental_network_config
    }

    /// Runtime availability of authenticated ordinary service announces.
    pub const fn manual_service_announce(self) -> CapabilityAvailability {
        self.manual_service_announce
    }

    /// Runtime availability of authenticated Reticulum path-and-proof probes.
    pub const fn experimental_reticulum_probe(self) -> CapabilityAvailability {
        self.experimental_reticulum_probe
    }
}

/// Bounded runtime state of the experimental inbound RNS DATA mailbox.
#[cfg(feature = "experimental-rns-inbox")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RnsInboxStatus {
    /// Number of items currently retained.
    pub depth: u16,
    /// Maximum retained item count.
    pub capacity: u16,
    /// Number of inbound items dropped since this boot.
    pub dropped_since_boot: u64,
    /// Maximum payload length accepted by this mailbox instance.
    pub max_payload_bytes: u16,
    /// Whether retained items survive reboot.
    pub durable: bool,
}

/// An inbound RNS DATA payload exceeded the fixed logical API limit.
#[cfg(feature = "experimental-rns-inbox")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RnsInboxPayloadTooLarge {
    actual: usize,
}

#[cfg(feature = "experimental-rns-inbox")]
impl RnsInboxPayloadTooLarge {
    /// Rejected payload length.
    pub const fn actual(self) -> usize {
        self.actual
    }

    /// Maximum accepted payload length.
    pub const fn maximum(self) -> usize {
        MAX_RNS_INBOX_PAYLOAD_BYTES
    }
}

/// One complete owned item returned by the experimental inbound RNS DATA mailbox.
#[cfg(feature = "experimental-rns-inbox")]
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct RnsInboxItem {
    id: NonZeroU64,
    destination: DestinationHash,
    payload_len: u16,
    payload: [u8; MAX_RNS_INBOX_PAYLOAD_BYTES],
}

#[cfg(feature = "experimental-rns-inbox")]
impl RnsInboxItem {
    /// Copy one bounded payload into a fixed-capacity response owner.
    pub fn new(
        id: NonZeroU64,
        destination: DestinationHash,
        payload: &[u8],
    ) -> Result<Self, RnsInboxPayloadTooLarge> {
        if payload.len() > MAX_RNS_INBOX_PAYLOAD_BYTES {
            return Err(RnsInboxPayloadTooLarge {
                actual: payload.len(),
            });
        }
        let mut owned = [0_u8; MAX_RNS_INBOX_PAYLOAD_BYTES];
        owned[..payload.len()].copy_from_slice(payload);
        Ok(Self {
            id,
            destination,
            payload_len: payload.len() as u16,
            payload: owned,
        })
    }

    /// Device-assigned mailbox item identifier.
    pub const fn id(&self) -> u64 {
        self.id.get()
    }

    /// Local Reticulum destination that received this item.
    pub const fn destination(&self) -> DestinationHash {
        self.destination
    }

    /// Exact payload bytes, excluding unused fixed-buffer capacity.
    pub fn payload(&self) -> &[u8] {
        &self.payload[..self.payload_len as usize]
    }

    /// Valid payload length.
    pub const fn payload_len(&self) -> u16 {
        self.payload_len
    }
}

#[cfg(feature = "experimental-rns-inbox")]
impl core::fmt::Debug for RnsInboxItem {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("RnsInboxItem")
            .field("id", &self.id)
            .field("destination", &self.destination)
            .field("payload_len", &self.payload_len)
            .finish_non_exhaustive()
    }
}

/// Stable non-zero handle for one committed inbound LXMF message.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LxmfMessageHandle(NonZeroU64);

#[cfg(feature = "experimental-lxmf")]
impl LxmfMessageHandle {
    /// Construct a stable handle from its complete numeric representation.
    pub const fn new(value: u64) -> Result<Self, InvalidLxmfMessageHandle> {
        match NonZeroU64::new(value) {
            Some(value) => Ok(Self(value)),
            None => Err(InvalidLxmfMessageHandle),
        }
    }

    /// Complete numeric representation.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Zero cannot identify a committed LXMF message.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidLxmfMessageHandle;

/// Durable appliance-side collection state for the committed LXMF mailbox.
///
/// Message handles are allocated contiguously in commit order. Consequently
/// the difference between the latest and acknowledged handles is the exact
/// number of committed messages not yet collected by the client.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LxmfMailboxStatus {
    latest: Option<LxmfMessageHandle>,
    acknowledged_through: Option<LxmfMessageHandle>,
    uncollected_count: u32,
}

#[cfg(feature = "experimental-lxmf")]
impl LxmfMailboxStatus {
    /// Validate and construct one complete mailbox collection snapshot.
    pub const fn new(
        latest: Option<LxmfMessageHandle>,
        acknowledged_through: Option<LxmfMessageHandle>,
    ) -> Result<Self, InvalidLxmfMailboxStatus> {
        let latest_value = match latest {
            Some(handle) => handle.get(),
            None => 0,
        };
        let acknowledged_value = match acknowledged_through {
            Some(handle) => handle.get(),
            None => 0,
        };
        if acknowledged_value > latest_value {
            return Err(InvalidLxmfMailboxStatus::AcknowledgementAhead {
                latest: latest_value,
                acknowledged_through: acknowledged_value,
            });
        }
        let uncollected = latest_value - acknowledged_value;
        if uncollected > u32::MAX as u64 {
            return Err(InvalidLxmfMailboxStatus::CountOverflow { uncollected });
        }
        Ok(Self {
            latest,
            acknowledged_through,
            uncollected_count: uncollected as u32,
        })
    }

    /// Latest committed message, or `None` when the mailbox is empty.
    pub const fn latest(self) -> Option<LxmfMessageHandle> {
        self.latest
    }

    /// Highest committed message durably collected by the client.
    pub const fn acknowledged_through(self) -> Option<LxmfMessageHandle> {
        self.acknowledged_through
    }

    /// Exact number of committed messages after the collection watermark.
    pub const fn uncollected_count(self) -> u32 {
        self.uncollected_count
    }
}

/// Contradictory or unrepresentable LXMF mailbox collection state.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidLxmfMailboxStatus {
    /// The collection watermark names a message after the durable tail.
    AcknowledgementAhead {
        /// Latest committed handle, or zero for an empty mailbox.
        latest: u64,
        /// Invalid collection watermark.
        acknowledged_through: u64,
    },
    /// The uncollected difference exceeded the bounded wire representation.
    CountOverflow {
        /// Exact unrepresentable difference.
        uncollected: u64,
    },
}

/// Valid non-zero upper bound for one bounded LXMF read response.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LxmfReadLength(u16);

#[cfg(feature = "experimental-lxmf")]
impl LxmfReadLength {
    /// Validate a requested response length against the frozen logical limit.
    pub const fn new(value: u16) -> Result<Self, InvalidLxmfReadLength> {
        if value == 0 || value as usize > MAX_LXMF_READ_CHUNK_BYTES {
            Err(InvalidLxmfReadLength { actual: value })
        } else {
            Ok(Self(value))
        }
    }

    /// Maximum bytes requested from the named offset.
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// A requested LXMF chunk length was zero or exceeded the logical limit.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidLxmfReadLength {
    actual: u16,
}

#[cfg(feature = "experimental-lxmf")]
impl InvalidLxmfReadLength {
    /// Rejected requested length.
    pub const fn actual(self) -> u16 {
        self.actual
    }

    /// Maximum accepted requested length.
    pub const fn maximum(self) -> u16 {
        MAX_LXMF_READ_CHUNK_BYTES as u16
    }
}

/// Opaque public token identifying one volatile peer-discovery incarnation.
///
/// The device changes this value whenever its boot-scoped discovery table is
/// reconstructed. It is not a credential or secret.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LxmfPeerDiscoveryIncarnation([u8; 8]);

#[cfg(feature = "experimental-lxmf")]
impl LxmfPeerDiscoveryIncarnation {
    /// Construct a token from its complete public wire bytes.
    pub const fn new(bytes: [u8; 8]) -> Self {
        Self(bytes)
    }

    /// Borrow all public token bytes.
    pub const fn as_bytes(&self) -> &[u8; 8] {
        &self.0
    }
}

/// Exclusive peer-discovery cursor scoped to one device incarnation.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LxmfPeerDiscoveryCursor {
    incarnation: LxmfPeerDiscoveryIncarnation,
    after_generation: u64,
}

#[cfg(feature = "experimental-lxmf")]
impl LxmfPeerDiscoveryCursor {
    /// Construct a complete boot-scoped exclusive cursor.
    pub const fn new(incarnation: LxmfPeerDiscoveryIncarnation, after_generation: u64) -> Self {
        Self {
            incarnation,
            after_generation,
        }
    }

    /// Incarnation in which the exclusive generation was observed.
    pub const fn incarnation(self) -> LxmfPeerDiscoveryIncarnation {
        self.incarnation
    }

    /// Exclusive observation generation; zero starts before all observations.
    pub const fn after_generation(self) -> u64 {
        self.after_generation
    }
}

/// Nonzero generation assigned to one retained peer's latest observation.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LxmfPeerGeneration(NonZeroU64);

#[cfg(feature = "experimental-lxmf")]
impl LxmfPeerGeneration {
    /// Construct a generation, rejecting the reserved pre-history value zero.
    pub const fn new(value: u64) -> Result<Self, InvalidLxmfPeerGeneration> {
        match NonZeroU64::new(value) {
            Some(value) => Ok(Self(value)),
            None => Err(InvalidLxmfPeerGeneration),
        }
    }

    /// Complete nonzero generation value.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Zero is reserved for the cursor before any peer observation.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidLxmfPeerGeneration;

/// One retained, authenticated nearby `lxmf.delivery` announce observation.
///
/// This is display and contact-selection evidence, not routing authority.
/// The complete 64-byte Reticulum public key intentionally remains inside the
/// firmware's protocol owner and is not exposed by the local device API.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct LxmfDiscoveredPeer {
    destination: DestinationHash,
    identity_hash: IdentityHash,
    app_data_len: u16,
    app_data: [u8; MAX_LXMF_PEER_APP_DATA_BYTES],
    hops: u8,
    interface_id: u8,
    rssi_dbm: Option<i16>,
    snr_db: Option<i16>,
    observed_age_ms: u64,
    generation: LxmfPeerGeneration,
}

#[cfg(feature = "experimental-lxmf")]
impl LxmfDiscoveredPeer {
    /// Copy one bounded, already-authenticated discovery observation.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        destination: DestinationHash,
        identity_hash: IdentityHash,
        app_data: &[u8],
        hops: u8,
        interface_id: u8,
        rssi_dbm: Option<i16>,
        snr_db: Option<i16>,
        observed_age_ms: u64,
        generation: LxmfPeerGeneration,
    ) -> Result<Self, LxmfPeerAppDataTooLarge> {
        if app_data.len() > MAX_LXMF_PEER_APP_DATA_BYTES {
            return Err(LxmfPeerAppDataTooLarge {
                actual: app_data.len(),
            });
        }
        let mut owned = [0_u8; MAX_LXMF_PEER_APP_DATA_BYTES];
        owned[..app_data.len()].copy_from_slice(app_data);
        Ok(Self {
            destination,
            identity_hash,
            app_data_len: app_data.len() as u16,
            app_data: owned,
            hops,
            interface_id,
            rssi_dbm,
            snr_db,
            observed_age_ms,
            generation,
        })
    }

    /// Complete announced `lxmf.delivery` destination hash.
    pub const fn destination(&self) -> DestinationHash {
        self.destination
    }

    /// Public hash of the identity that authenticated the announce.
    pub const fn identity_hash(&self) -> IdentityHash {
        self.identity_hash
    }

    /// Exact authenticated announce application data.
    pub fn app_data(&self) -> &[u8] {
        &self.app_data[..self.app_data_len as usize]
    }

    /// Reticulum hop count reported for the latest observation.
    pub const fn hops(&self) -> u8 {
        self.hops
    }

    /// Product-owned scalar identifying the observing interface.
    pub const fn interface_id(&self) -> u8 {
        self.interface_id
    }

    /// Observed RSSI in whole dBm, when available.
    pub const fn rssi_dbm(&self) -> Option<i16> {
        self.rssi_dbm
    }

    /// Observed signal-to-noise ratio in whole dB, when available.
    pub const fn snr_db(&self) -> Option<i16> {
        self.snr_db
    }

    /// Saturating age in milliseconds at the response snapshot.
    pub const fn observed_age_ms(&self) -> u64 {
        self.observed_age_ms
    }

    /// Generation of the latest retained observation.
    pub const fn generation(&self) -> LxmfPeerGeneration {
        self.generation
    }
}

#[cfg(feature = "experimental-lxmf")]
impl core::fmt::Debug for LxmfDiscoveredPeer {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("LxmfDiscoveredPeer")
            .field("destination", &self.destination)
            .field("identity_hash", &self.identity_hash)
            .field("app_data_len", &self.app_data_len)
            .field("app_data", &"<redacted>")
            .field("hops", &self.hops)
            .field("interface_id", &self.interface_id)
            .field("rssi_dbm", &self.rssi_dbm)
            .field("snr_db", &self.snr_db)
            .field("observed_age_ms", &self.observed_age_ms)
            .field("generation", &self.generation)
            .finish()
    }
}

/// Authenticated announce application data exceeded the logical response bound.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LxmfPeerAppDataTooLarge {
    actual: usize,
}

#[cfg(feature = "experimental-lxmf")]
impl LxmfPeerAppDataTooLarge {
    /// Rejected application-data byte count.
    pub const fn actual(self) -> usize {
        self.actual
    }

    /// Maximum application-data byte count.
    pub const fn maximum(self) -> usize {
        MAX_LXMF_PEER_APP_DATA_BYTES
    }
}

/// One-record page from the volatile nearby-LXMF peer projection.
///
/// A changed incarnation or an ahead-of-history cursor resets the port to its
/// first retained record and sets `history_gap`. The returned next cursor is
/// always scoped to the current incarnation.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LxmfPeerDiscoveryPage {
    next_cursor: LxmfPeerDiscoveryCursor,
    latest_generation: Option<LxmfPeerGeneration>,
    oldest_retained_generation: Option<LxmfPeerGeneration>,
    history_gap: bool,
    peer: Option<LxmfDiscoveredPeer>,
}

#[cfg(feature = "experimental-lxmf")]
impl LxmfPeerDiscoveryPage {
    /// Construct one bounded projection page.
    pub const fn new(
        next_cursor: LxmfPeerDiscoveryCursor,
        latest_generation: Option<LxmfPeerGeneration>,
        oldest_retained_generation: Option<LxmfPeerGeneration>,
        history_gap: bool,
        peer: Option<LxmfDiscoveredPeer>,
    ) -> Self {
        Self {
            next_cursor,
            latest_generation,
            oldest_retained_generation,
            history_gap,
            peer,
        }
    }

    /// Exclusive cursor for the next read, scoped to the current incarnation.
    pub const fn next_cursor(&self) -> LxmfPeerDiscoveryCursor {
        self.next_cursor
    }

    /// Latest accepted observation generation when this page was produced.
    pub const fn latest_generation(&self) -> Option<LxmfPeerGeneration> {
        self.latest_generation
    }

    /// Oldest generation represented by a currently retained peer.
    pub const fn oldest_retained_generation(&self) -> Option<LxmfPeerGeneration> {
        self.oldest_retained_generation
    }

    /// Whether requested history was reset, updated away, or evicted.
    pub const fn history_gap(&self) -> bool {
        self.history_gap
    }

    /// One retained peer after the requested cursor, if present.
    pub const fn peer(&self) -> Option<&LxmfDiscoveredPeer> {
        self.peer.as_ref()
    }
}

/// Receiver-local physical signal values for one received Reticulum carrier.
///
/// RSSI and SNR are one indivisible observation: the wire carries both values
/// or neither value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IngressSignal {
    rssi_dbm: i16,
    snr_db: i16,
}

impl IngressSignal {
    /// Preserve receiver-reported RSSI and SNR.
    pub const fn new(rssi_dbm: i16, snr_db: i16) -> Self {
        Self { rssi_dbm, snr_db }
    }

    /// Receiver-reported RSSI in dBm.
    pub const fn rssi_dbm(self) -> i16 {
        self.rssi_dbm
    }

    /// Receiver-reported SNR in dB.
    pub const fn snr_db(self) -> i16 {
        self.snr_db
    }
}

/// First-arrival interface and optional final-hop signal evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IngressObservation {
    interface_id: u8,
    signal: Option<IngressSignal>,
}

impl IngressObservation {
    /// Construct one immutable first-arrival observation.
    pub const fn new(interface_id: u8, signal: Option<IngressSignal>) -> Self {
        Self {
            interface_id,
            signal,
        }
    }

    /// Device-local interface that received the carrier.
    pub const fn interface_id(self) -> u8 {
        self.interface_id
    }

    /// Optional receiver-local physical signal values.
    pub const fn signal(self) -> Option<IngressSignal> {
        self.signal
    }
}

/// Backward-compatible LXMF name for transport-neutral receiver signal values.
#[cfg(feature = "experimental-lxmf")]
pub type LxmfIngressSignal = IngressSignal;

/// Backward-compatible LXMF name for transport-neutral ingress evidence.
#[cfg(feature = "experimental-lxmf")]
pub type LxmfIngressObservation = IngressObservation;

/// Boot-scoped nonzero identifier for one Reticulum probe.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProbeId([u8; 16]);

impl ProbeId {
    /// Validate one complete opaque boot-scoped probe identifier.
    pub const fn new(bytes: [u8; 16]) -> Result<Self, InvalidProbeId> {
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] != 0 {
                return Ok(Self(bytes));
            }
            index += 1;
        }
        Err(InvalidProbeId)
    }

    /// Borrow the complete public identifier bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// The all-zero value cannot identify a Reticulum probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidProbeId;

/// Request to begin one path-and-proof probe to a known Reticulum destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProbeStartRequest {
    destination: DestinationHash,
    idempotency_key: IdempotencyKey,
}

impl ProbeStartRequest {
    /// Construct one principal-scoped idempotent probe request.
    pub const fn new(destination: DestinationHash, idempotency_key: IdempotencyKey) -> Self {
        Self {
            destination,
            idempotency_key,
        }
    }

    /// Known remote Reticulum destination being measured.
    pub const fn destination(self) -> DestinationHash {
        self.destination
    }

    /// Principal-scoped request deduplication key.
    pub const fn idempotency_key(self) -> IdempotencyKey {
        self.idempotency_key
    }
}

/// Request to read one principal-owned boot-scoped probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProbePollRequest {
    id: ProbeId,
}

impl ProbePollRequest {
    /// Construct a poll request for one accepted probe.
    pub const fn new(id: ProbeId) -> Self {
        Self { id }
    }

    /// Boot-scoped probe identifier.
    pub const fn id(self) -> ProbeId {
        self.id
    }
}

/// Whether probe start admitted fresh work or replayed an exact prior request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ProbeStartOutcome {
    /// Fresh probe work was accepted.
    Accepted = 0,
    /// An exact principal-scoped idempotent request was already accepted.
    Replayed = 1,
}

impl ProbeStartOutcome {
    /// Frozen experimental numeric representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Successful admission of one boot-scoped Reticulum probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProbeStartAccepted {
    id: ProbeId,
    outcome: ProbeStartOutcome,
}

impl ProbeStartAccepted {
    /// Construct one start response.
    pub const fn new(id: ProbeId, outcome: ProbeStartOutcome) -> Self {
        Self { id, outcome }
    }

    /// Boot-scoped probe identifier.
    pub const fn id(self) -> ProbeId {
        self.id
    }

    /// Fresh-versus-replayed admission result.
    pub const fn outcome(self) -> ProbeStartOutcome {
        self.outcome
    }
}

/// Non-terminal phase of one accepted Reticulum probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ProbePhase {
    /// The node is resolving a usable path to the known destination.
    PathLookup = 0,
    /// The probe is waiting for transport-neutral outbound dispatch.
    AwaitingDispatch = 1,
    /// The probe was dispatched and is waiting for its Reticulum proof.
    AwaitingProof = 2,
}

impl ProbePhase {
    /// Frozen experimental numeric representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Terminal failure of one accepted Reticulum probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ProbeFailure {
    /// The destination's public identity was not available for proof validation.
    IdentityUnavailable = 0,
    /// No usable Reticulum path was available.
    NoPath = 1,
    /// Transport-neutral packet dispatch failed.
    Dispatch = 2,
    /// A path, dispatch, or proof deadline expired.
    Timeout = 3,
    /// A local invariant failed without exposing implementation details.
    Internal = 4,
}

impl ProbeFailure {
    /// Frozen experimental numeric representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Successful end-to-end Reticulum probe measurement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProbeSuccess {
    round_trip_ms: u32,
    hops: u8,
    ingress: IngressObservation,
}

impl ProbeSuccess {
    /// Preserve the bounded round-trip, route, and proof-arrival evidence.
    pub const fn new(round_trip_ms: u32, hops: u8, ingress: IngressObservation) -> Self {
        Self {
            round_trip_ms,
            hops,
            ingress,
        }
    }

    /// Complete measured round-trip duration in milliseconds.
    pub const fn round_trip_ms(self) -> u32 {
        self.round_trip_ms
    }

    /// Reticulum hop count associated with the successful probe.
    pub const fn hops(self) -> u8 {
        self.hops
    }

    /// Receiver-local final-hop evidence for the returning proof.
    pub const fn ingress_observation(self) -> IngressObservation {
        self.ingress
    }
}

/// Current or terminal state of one boot-scoped Reticulum probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProbePollResponse {
    /// Work remains in progress at the named phase.
    Pending(ProbePhase),
    /// A valid Reticulum proof completed the probe.
    Succeeded(ProbeSuccess),
    /// The probe ended with a bounded public failure category.
    Failed(ProbeFailure),
}

/// Metadata-only physical-commit-order entry returned by `experimental.lxmf.next`.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LxmfMessageSummary {
    handle: LxmfMessageHandle,
    message_id: [u8; 32],
    destination: DestinationHash,
    source: DestinationHash,
    timestamp_bits: u64,
    normalized_wire_len: u32,
    title_len: u32,
    content_len: u32,
    fields_encoded_len: u32,
    exact_wire_sha256: [u8; 32],
    ingress: Option<LxmfIngressObservation>,
}

#[cfg(feature = "experimental-lxmf")]
const LXMF_NORMALIZED_WIRE_PREFIX_BYTES: u64 = 16 + 16 + 64;
#[cfg(feature = "experimental-lxmf")]
const LXMF_PAYLOAD_ARRAY_HEADER_BYTES: u64 = 1;
#[cfg(feature = "experimental-lxmf")]
const LXMF_TIMESTAMP_BYTES: u64 = 1 + 8;

#[cfg(feature = "experimental-lxmf")]
const fn minimum_msgpack_binary_bytes(decoded_len: u32) -> u64 {
    let header_len = if decoded_len <= u8::MAX as u32 {
        2
    } else if decoded_len <= u16::MAX as u32 {
        3
    } else {
        5
    };
    decoded_len as u64 + header_len
}

#[cfg(feature = "experimental-lxmf")]
impl LxmfMessageSummary {
    /// Construct one complete committed-message summary.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        handle: LxmfMessageHandle,
        message_id: [u8; 32],
        destination: DestinationHash,
        source: DestinationHash,
        timestamp_bits: u64,
        normalized_wire_len: u32,
        title_len: u32,
        content_len: u32,
        fields_encoded_len: u32,
        exact_wire_sha256: [u8; 32],
    ) -> Result<Self, InvalidLxmfMessageSummary> {
        // A normalized LXMF wire is the destination, source and signature
        // prefix followed by a four- or five-item MessagePack array. The
        // shortest valid payload has a one-byte array header, one float64
        // timestamp, two binary values with their shortest possible length
        // prefixes, and the exact encoded fields map. A fifth stamp can only
        // increase this lower bound. Perform the sum in u64 so adversarial u32
        // component lengths cannot wrap into an apparently small message.
        let minimum_wire_len = LXMF_NORMALIZED_WIRE_PREFIX_BYTES
            + LXMF_PAYLOAD_ARRAY_HEADER_BYTES
            + LXMF_TIMESTAMP_BYTES
            + minimum_msgpack_binary_bytes(title_len)
            + minimum_msgpack_binary_bytes(content_len)
            + fields_encoded_len as u64;
        if fields_encoded_len == 0 || (normalized_wire_len as u64) < minimum_wire_len {
            return Err(InvalidLxmfMessageSummary);
        }
        Ok(Self {
            handle,
            message_id,
            destination,
            source,
            timestamp_bits,
            normalized_wire_len,
            title_len,
            content_len,
            fields_encoded_len,
            exact_wire_sha256,
            ingress: None,
        })
    }

    /// Attach immutable first-arrival transport evidence.
    pub const fn with_ingress_observation(
        mut self,
        ingress: Option<LxmfIngressObservation>,
    ) -> Self {
        self.ingress = ingress;
        self
    }

    /// Stable committed-message handle.
    pub const fn handle(self) -> LxmfMessageHandle {
        self.handle
    }

    /// Python-compatible LXMF authenticated-message identifier.
    pub const fn message_id(&self) -> &[u8; 32] {
        &self.message_id
    }

    /// Local `lxmf.delivery` destination that received the message.
    pub const fn destination(self) -> DestinationHash {
        self.destination
    }

    /// Authenticated source `lxmf.delivery` destination.
    pub const fn source(self) -> DestinationHash {
        self.source
    }

    /// Exact IEEE-754 bits of the LXMF timestamp.
    pub const fn timestamp_bits(self) -> u64 {
        self.timestamp_bits
    }

    /// Complete normalized wire length.
    pub const fn normalized_wire_len(self) -> u32 {
        self.normalized_wire_len
    }

    /// Decoded title byte length.
    pub const fn title_len(self) -> u32 {
        self.title_len
    }

    /// Decoded content byte length.
    pub const fn content_len(self) -> u32 {
        self.content_len
    }

    /// Encoded MessagePack fields-map length.
    pub const fn fields_encoded_len(self) -> u32 {
        self.fields_encoded_len
    }

    /// SHA-256 of the complete normalized wire representation.
    pub const fn exact_wire_sha256(&self) -> &[u8; 32] {
        &self.exact_wire_sha256
    }

    /// First-arrival interface and optional final-hop signal evidence.
    pub const fn ingress_observation(self) -> Option<LxmfIngressObservation> {
        self.ingress
    }
}

/// A committed LXMF summary contained an impossible zero wire length.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidLxmfMessageSummary;

/// One owned exact normalized-wire chunk returned by `experimental.lxmf.read`.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct LxmfReadChunk {
    handle: LxmfMessageHandle,
    offset: u32,
    total_len: u32,
    bytes_len: u16,
    bytes: [u8; MAX_LXMF_READ_CHUNK_BYTES],
}

#[cfg(feature = "experimental-lxmf")]
impl LxmfReadChunk {
    /// Copy and validate one non-empty bounded chunk of a committed message.
    pub fn new(
        handle: LxmfMessageHandle,
        offset: u32,
        total_len: u32,
        bytes: &[u8],
    ) -> Result<Self, InvalidLxmfReadChunk> {
        if bytes.is_empty() {
            return Err(InvalidLxmfReadChunk::Empty);
        }
        if bytes.len() > MAX_LXMF_READ_CHUNK_BYTES {
            return Err(InvalidLxmfReadChunk::TooLarge {
                actual: bytes.len(),
            });
        }
        let end = u64::from(offset) + bytes.len() as u64;
        if total_len == 0 || offset >= total_len || end > u64::from(total_len) {
            return Err(InvalidLxmfReadChunk::OutsideMessage {
                offset,
                length: bytes.len(),
                total_len,
            });
        }
        let mut owned = [0_u8; MAX_LXMF_READ_CHUNK_BYTES];
        owned[..bytes.len()].copy_from_slice(bytes);
        Ok(Self {
            handle,
            offset,
            total_len,
            bytes_len: bytes.len() as u16,
            bytes: owned,
        })
    }

    /// Stable committed-message handle.
    pub const fn handle(&self) -> LxmfMessageHandle {
        self.handle
    }

    /// Zero-based offset of the first returned byte.
    pub const fn offset(&self) -> u32 {
        self.offset
    }

    /// Complete normalized wire length.
    pub const fn total_len(&self) -> u32 {
        self.total_len
    }

    /// Exact bytes in this response.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.bytes_len as usize]
    }

    /// Whether this chunk reaches the exact end of the committed message.
    pub const fn is_final(&self) -> bool {
        self.offset as u64 + self.bytes_len as u64 == self.total_len as u64
    }
}

#[cfg(feature = "experimental-lxmf")]
impl core::fmt::Debug for LxmfReadChunk {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("LxmfReadChunk")
            .field("handle", &self.handle)
            .field("offset", &self.offset)
            .field("total_len", &self.total_len)
            .field("bytes_len", &self.bytes_len)
            .finish_non_exhaustive()
    }
}

/// A returned LXMF chunk violated its fixed logical or message boundary.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidLxmfReadChunk {
    /// A successful read must make forward progress.
    Empty,
    /// Returned bytes exceeded the fixed response owner.
    TooLarge {
        /// Supplied byte count.
        actual: usize,
    },
    /// Offset and bytes did not fit inside the declared complete message.
    OutsideMessage {
        /// Supplied zero-based offset.
        offset: u32,
        /// Supplied chunk byte count.
        length: usize,
        /// Declared complete normalized wire length.
        total_len: u32,
    },
}

/// SHA-256 digest of every byte in one complete encoded Reticulum packet.
///
/// This is deliberately a distinct type from Reticulum's proof-correlation
/// hash, which covers only the protocol-defined hashable part of a packet.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EncodedPacketSha256([u8; 32]);

impl EncodedPacketSha256 {
    /// Construct an encoded-packet digest from its complete bytes.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow the complete digest bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Prepared-packet diagnostics that never expose the packet itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedPacketDetails {
    /// Encoded packet length.
    pub packet_len: u16,
    /// SHA-256 of every byte in the complete encoded packet.
    pub encoded_packet_sha256: EncodedPacketSha256,
}

/// Progress of an accepted submission, without prepared packet bytes.
///
/// State-specific data lives in the corresponding variant, so contradictory
/// combinations such as a queued submission with a packet hash or a failed
/// submission without a failure category cannot be represented. This is a
/// closed API-v1 wire vocabulary; adding a numeric state requires a new API
/// major version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmissionState {
    /// Accepted into a bounded intent queue.
    Queued,
    /// Currently being processed by the node owner.
    Preparing,
    /// Packet preparation completed and delivery is pending.
    ///
    /// The details are durable status metadata; this state does not imply that
    /// encoded packet bytes still occupy the node's private transmit outbox.
    AwaitingDelivery(PreparedPacketDetails),
    /// A later proof or application acknowledgement completed the submission.
    Delivered(PreparedPacketDetails),
    /// Submission terminated with a typed failure.
    Failed(SubmissionFailure),
    /// Submission was cancelled before it became irreversible.
    Cancelled,
}

impl SubmissionState {
    /// Frozen API-v1 numeric representation.
    pub const fn wire_code(self) -> u8 {
        match self {
            Self::Queued => 0,
            Self::Preparing => 1,
            Self::AwaitingDelivery(_) => 2,
            Self::Delivered(_) => 3,
            Self::Failed(_) => 4,
            Self::Cancelled => 5,
        }
    }
}

/// Stable failure category suitable for a submission status response.
///
/// This is a closed API-v1 wire vocabulary. Adding a numeric category requires
/// a new API major version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum SubmissionFailure {
    /// Destination is not currently reachable through a known path.
    NoPath = 0,
    /// An accepted submission received no required proof or acknowledgement
    /// before its delivery deadline.
    DeliveryTimeout = 1,
    /// Accepted work was later rejected by a downstream protocol or policy
    /// stage that could not decide at request admission.
    Rejected = 2,
    /// Processing failed for a non-client fault.
    Internal = 3,
}

impl SubmissionFailure {
    /// Frozen API-v1 numeric representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Owned bounded Wi-Fi SSID used by redacted responses.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct WifiSsidSummary {
    bytes: [u8; MAX_WIFI_SSID_BYTES],
    len: u8,
}

#[cfg(feature = "experimental-network-config")]
impl WifiSsidSummary {
    /// Copy one validated SSID into a fixed-capacity response owner.
    pub fn new(bytes: &[u8]) -> Result<Self, InvalidWifiSsid> {
        let validated = WifiSsid::new(bytes)?;
        let mut owned = [0_u8; MAX_WIFI_SSID_BYTES];
        owned[..bytes.len()].copy_from_slice(validated.as_bytes());
        Ok(Self {
            bytes: owned,
            len: bytes.len() as u8,
        })
    }

    /// Borrow the complete SSID bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

#[cfg(feature = "experimental-network-config")]
impl core::fmt::Debug for WifiSsidSummary {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("WifiSsidSummary")
            .field("bytes", &self.as_bytes())
            .finish()
    }
}

/// Redacted desired Wi-Fi configuration.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WifiNetworkConfigSummary {
    profile_id: WifiNetworkProfileId,
    enabled: bool,
    priority: u8,
    ssid: WifiSsidSummary,
    credential_configured: bool,
}

#[cfg(feature = "experimental-network-config")]
impl WifiNetworkConfigSummary {
    /// Construct one redacted WPA2-Personal profile.
    pub fn new(
        profile_id: WifiNetworkProfileId,
        enabled: bool,
        priority: u8,
        ssid: &[u8],
        credential_configured: bool,
    ) -> Result<Self, InvalidWifiSsid> {
        let ssid = WifiSsidSummary::new(ssid)?;
        Ok(Self {
            profile_id,
            enabled,
            priority,
            ssid,
            credential_configured,
        })
    }

    /// Stable opaque profile identity.
    pub const fn profile_id(self) -> WifiNetworkProfileId {
        self.profile_id
    }

    /// Whether this saved network should be used.
    pub const fn enabled(self) -> bool {
        self.enabled
    }

    /// Station-selection priority; larger values are preferred.
    pub const fn priority(self) -> u8 {
        self.priority
    }

    /// Saved SSID.
    pub const fn ssid(self) -> WifiSsidSummary {
        self.ssid
    }

    /// Whether a secret is stored, without exposing its bytes.
    pub const fn credential_configured(self) -> bool {
        self.credential_configured
    }
}

/// Redacted desired outbound Reticulum TCP peer configuration.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReticulumTcpPeerConfigSummary {
    enabled: bool,
    ipv4_address: ReticulumTcpPeerIpv4Address,
    port: u16,
}

#[cfg(feature = "experimental-network-config")]
impl ReticulumTcpPeerConfigSummary {
    /// Construct a validated redacted peer configuration.
    pub const fn new(
        enabled: bool,
        ipv4_address: ReticulumTcpPeerIpv4Address,
        port: u16,
    ) -> Result<Self, InvalidReticulumTcpPeerPort> {
        if port == 0 {
            return Err(InvalidReticulumTcpPeerPort);
        }
        Ok(Self {
            enabled,
            ipv4_address,
            port,
        })
    }

    /// Whether the peer should be connected.
    pub const fn enabled(self) -> bool {
        self.enabled
    }

    /// Configured exact IPv4 address.
    pub const fn ipv4_address(self) -> ReticulumTcpPeerIpv4Address {
        self.ipv4_address
    }

    /// Configured TCP port.
    pub const fn port(self) -> u16 {
        self.port
    }
}

/// Owned bounded DNS hostname used by redacted configuration responses.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct ReticulumTcpPeerHostnameSummary {
    bytes: [u8; MAX_RETICULUM_TCP_PEER_HOSTNAME_BYTES],
    len: u8,
}

#[cfg(feature = "experimental-network-config")]
impl ReticulumTcpPeerHostnameSummary {
    /// Validate and copy one DNS hostname into fixed response storage.
    pub fn new(hostname: &str) -> Result<Self, InvalidReticulumTcpPeerHostname> {
        let validated = ReticulumTcpPeerHostname::new(hostname)?;
        let bytes = validated.as_str().as_bytes();
        let mut owned = [0_u8; MAX_RETICULUM_TCP_PEER_HOSTNAME_BYTES];
        owned[..bytes.len()].copy_from_slice(bytes);
        Ok(Self {
            bytes: owned,
            len: bytes.len() as u8,
        })
    }

    /// Borrow the exact validated hostname.
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len as usize])
            .expect("hostname validation accepts ASCII only")
    }
}

#[cfg(feature = "experimental-network-config")]
impl core::fmt::Debug for ReticulumTcpPeerHostnameSummary {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_tuple("ReticulumTcpPeerHostnameSummary")
            .field(&self.as_str())
            .finish()
    }
}

/// Redacted desired hostname-based outbound Reticulum TCP peer configuration.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReticulumTcpPeerHostConfigSummary {
    enabled: bool,
    hostname: ReticulumTcpPeerHostnameSummary,
    port: u16,
}

#[cfg(feature = "experimental-network-config")]
impl ReticulumTcpPeerHostConfigSummary {
    /// Construct a validated hostname peer configuration.
    pub fn new(
        enabled: bool,
        hostname: &str,
        port: u16,
    ) -> Result<Self, InvalidReticulumTcpPeerHostConfig> {
        if port == 0 {
            return Err(InvalidReticulumTcpPeerHostConfig::InvalidPort);
        }
        Ok(Self {
            enabled,
            hostname: ReticulumTcpPeerHostnameSummary::new(hostname)
                .map_err(InvalidReticulumTcpPeerHostConfig::InvalidHostname)?,
            port,
        })
    }

    /// Whether the peer should be connected.
    pub const fn enabled(self) -> bool {
        self.enabled
    }

    /// Configured DNS hostname.
    pub const fn hostname(self) -> ReticulumTcpPeerHostnameSummary {
        self.hostname
    }

    /// Configured TCP port.
    pub const fn port(self) -> u16 {
        self.port
    }
}

/// Why a hostname-based peer summary was rejected.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidReticulumTcpPeerHostConfig {
    /// The DNS hostname was malformed or exceeded its fixed bound.
    InvalidHostname(InvalidReticulumTcpPeerHostname),
    /// TCP port zero is reserved.
    InvalidPort,
}

/// Complete redacted desired network configuration.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NetworkConfigSnapshot {
    /// Monotonic committed configuration revision.
    pub revision: u64,
    wifi_profiles: [Option<WifiNetworkConfigSummary>; MAX_WIFI_NETWORK_PROFILES],
    tcp_peer: Option<ReticulumTcpPeerConfigSummary>,
    tcp_host_peer: Option<ReticulumTcpPeerHostConfigSummary>,
    wifi_transport_enabled: bool,
    automatic_announces_enabled: bool,
    rmap_discovery_enabled: bool,
    rmap_share_location: bool,
    rmap_phone_location: Option<RmapLocation>,
    lora_profile: LoraRadioProfile,
}

#[cfg(feature = "experimental-network-config")]
impl NetworkConfigSnapshot {
    /// Validate and construct a complete redacted desired configuration.
    pub fn new(
        revision: u64,
        wifi_profiles: [Option<WifiNetworkConfigSummary>; MAX_WIFI_NETWORK_PROFILES],
        tcp_peer: Option<ReticulumTcpPeerConfigSummary>,
    ) -> Result<Self, InvalidNetworkConfigSnapshot> {
        Self::new_full(
            revision,
            wifi_profiles,
            tcp_peer,
            None,
            GatewayPolicy::new(true, true),
            RmapConfig::new(false, false, None),
        )
    }

    /// Validate and construct a complete API-1.8 desired configuration.
    ///
    /// The IPv4 and hostname peer slots are mutually exclusive.
    pub fn new_full(
        revision: u64,
        wifi_profiles: [Option<WifiNetworkConfigSummary>; MAX_WIFI_NETWORK_PROFILES],
        tcp_peer: Option<ReticulumTcpPeerConfigSummary>,
        tcp_host_peer: Option<ReticulumTcpPeerHostConfigSummary>,
        gateway_policy: GatewayPolicy,
        rmap_config: RmapConfig,
    ) -> Result<Self, InvalidNetworkConfigSnapshot> {
        Self::new_complete(
            revision,
            wifi_profiles,
            tcp_peer,
            tcp_host_peer,
            gateway_policy,
            rmap_config,
            LoraTransmitPowerDbm::DEFAULT,
        )
    }

    /// Validate and construct a complete current desired configuration.
    ///
    /// The IPv4 and hostname peer slots are mutually exclusive. Revision zero
    /// represents exactly erased media and therefore requires the
    /// backward-compatible default LoRa transmit power.
    pub fn new_complete(
        revision: u64,
        wifi_profiles: [Option<WifiNetworkConfigSummary>; MAX_WIFI_NETWORK_PROFILES],
        tcp_peer: Option<ReticulumTcpPeerConfigSummary>,
        tcp_host_peer: Option<ReticulumTcpPeerHostConfigSummary>,
        gateway_policy: GatewayPolicy,
        rmap_config: RmapConfig,
        lora_tx_power_dbm: LoraTransmitPowerDbm,
    ) -> Result<Self, InvalidNetworkConfigSnapshot> {
        Self::new_with_lora_profile(
            revision,
            wifi_profiles,
            tcp_peer,
            tcp_host_peer,
            gateway_policy,
            rmap_config,
            LoraRadioProfile::DEFAULT.with_tx_power(lora_tx_power_dbm),
        )
    }

    /// Validate and construct a complete current desired configuration with
    /// one atomic LoRa profile.
    pub fn new_with_lora_profile(
        revision: u64,
        wifi_profiles: [Option<WifiNetworkConfigSummary>; MAX_WIFI_NETWORK_PROFILES],
        tcp_peer: Option<ReticulumTcpPeerConfigSummary>,
        tcp_host_peer: Option<ReticulumTcpPeerHostConfigSummary>,
        gateway_policy: GatewayPolicy,
        rmap_config: RmapConfig,
        lora_profile: LoraRadioProfile,
    ) -> Result<Self, InvalidNetworkConfigSnapshot> {
        let mut saw_empty = false;
        let mut index = 0;
        while index < wifi_profiles.len() {
            match wifi_profiles[index] {
                Some(_) if saw_empty => {
                    return Err(InvalidNetworkConfigSnapshot::SparseWifiProfiles);
                }
                Some(profile) => {
                    let mut prior = 0;
                    while prior < index {
                        if wifi_profiles[prior]
                            .is_some_and(|candidate| candidate.profile_id == profile.profile_id)
                        {
                            return Err(InvalidNetworkConfigSnapshot::DuplicateWifiProfileId);
                        }
                        prior += 1;
                    }
                }
                None => saw_empty = true,
            }
            index += 1;
        }
        if tcp_peer.is_some() && tcp_host_peer.is_some() {
            return Err(InvalidNetworkConfigSnapshot::AmbiguousTcpPeer);
        }
        if revision == 0
            && (wifi_profiles.iter().any(Option::is_some)
                || tcp_peer.is_some()
                || tcp_host_peer.is_some()
                || !gateway_policy.wifi_transport_enabled()
                || !gateway_policy.automatic_announces_enabled()
                || rmap_config.discovery_enabled()
                || rmap_config.share_location()
                || rmap_config.phone_location().is_some()
                || lora_profile != LoraRadioProfile::DEFAULT)
        {
            return Err(InvalidNetworkConfigSnapshot::NonEmptyErasedRevision);
        }
        Ok(Self {
            revision,
            wifi_profiles,
            tcp_peer,
            tcp_host_peer,
            wifi_transport_enabled: gateway_policy.wifi_transport_enabled(),
            automatic_announces_enabled: gateway_policy.automatic_announces_enabled(),
            rmap_discovery_enabled: rmap_config.discovery_enabled(),
            rmap_share_location: rmap_config.share_location(),
            rmap_phone_location: rmap_config.phone_location(),
            lora_profile,
        })
    }

    /// Ordered Wi-Fi records followed by empty capacity.
    pub const fn wifi_profiles(
        &self,
    ) -> &[Option<WifiNetworkConfigSummary>; MAX_WIFI_NETWORK_PROFILES] {
        &self.wifi_profiles
    }

    /// Find one desired Wi-Fi profile by opaque identity.
    pub fn wifi_profile(
        &self,
        profile_id: WifiNetworkProfileId,
    ) -> Option<WifiNetworkConfigSummary> {
        self.wifi_profiles
            .iter()
            .flatten()
            .copied()
            .find(|profile| profile.profile_id == profile_id)
    }

    /// Desired Reticulum TCP peer, when one is saved.
    pub const fn tcp_peer(self) -> Option<ReticulumTcpPeerConfigSummary> {
        self.tcp_peer
    }

    /// Desired hostname-based Reticulum TCP peer, when one is saved.
    pub const fn tcp_host_peer(self) -> Option<ReticulumTcpPeerHostConfigSummary> {
        self.tcp_host_peer
    }

    /// Whether the Wi-Fi transport is globally enabled.
    pub const fn wifi_transport_enabled(self) -> bool {
        self.wifi_transport_enabled
    }

    /// Whether scheduled ordinary service announces are enabled.
    pub const fn automatic_announces_enabled(self) -> bool {
        self.automatic_announces_enabled
    }

    /// Whether signed RMAP interface discovery is enabled.
    pub const fn rmap_discovery_enabled(self) -> bool {
        self.rmap_discovery_enabled
    }

    /// Whether the optional phone position may be published to RMAP.
    pub const fn rmap_share_location(self) -> bool {
        self.rmap_share_location
    }

    /// Latest optional phone-sourced RMAP position.
    pub const fn rmap_phone_location(self) -> Option<RmapLocation> {
        self.rmap_phone_location
    }

    /// Requested LoRa transmit power.
    pub const fn lora_tx_power_dbm(self) -> LoraTransmitPowerDbm {
        self.lora_profile.tx_power_dbm()
    }

    /// Complete desired LoRa profile saved for the next boot.
    pub const fn lora_profile(self) -> LoraRadioProfile {
        self.lora_profile
    }

    /// Complete gateway-wide policy.
    pub const fn gateway_policy(self) -> GatewayPolicy {
        GatewayPolicy::new(
            self.wifi_transport_enabled,
            self.automatic_announces_enabled,
        )
    }

    /// Complete RMAP discovery and location-sharing configuration.
    pub const fn rmap_config(self) -> RmapConfig {
        RmapConfig::new(
            self.rmap_discovery_enabled,
            self.rmap_share_location,
            self.rmap_phone_location,
        )
    }
}

/// A desired-network snapshot violated its fixed ordered representation.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidNetworkConfigSnapshot {
    /// An occupied Wi-Fi record followed an empty capacity entry.
    SparseWifiProfiles,
    /// Two Wi-Fi records shared one opaque identity.
    DuplicateWifiProfileId,
    /// Both incompatible endpoint representations were populated.
    AmbiguousTcpPeer,
    /// Revision zero contained configuration instead of erased state.
    NonEmptyErasedRevision,
}

/// Normal result of one compare-and-swap network-configuration mutation.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkConfigMutationOutcome {
    /// The requested mutation was committed, or an exact ambiguous retry was
    /// recognized as already applied.
    Applied {
        /// Monotonic committed configuration revision.
        revision: u64,
        /// Whether the committed configuration needs a controlled reboot to apply.
        reboot_required: bool,
    },
    /// The expected revision did not match current committed state.
    RevisionConflict {
        /// Current committed revision the client should refresh from.
        current_revision: u64,
    },
}

/// Admission result for an authenticated manual ordinary service announce.
///
/// This is a closed API-v1 wire vocabulary. Both outcomes are successful:
/// duplicate requests coalesce instead of consuming additional queue capacity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ManualServiceAnnounceDisposition {
    /// A fresh set of ordinary service announces was queued.
    Queued = 0,
    /// An equivalent ordinary service announce was already pending.
    AlreadyPending = 1,
}

impl ManualServiceAnnounceDisposition {
    /// Frozen API-v1 numeric representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Live Wi-Fi station state.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum WifiStationState {
    /// No enabled Wi-Fi profile exists.
    Disabled = 0,
    /// The station is enabled but not associated.
    Disconnected = 1,
    /// Association or DHCP is in progress.
    Connecting = 2,
    /// Association and DHCP completed.
    Connected = 3,
}

#[cfg(feature = "experimental-network-config")]
impl WifiStationState {
    /// Frozen experimental numeric representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Live outbound Reticulum TCP peer state.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ReticulumTcpPeerState {
    /// No enabled peer exists.
    Disabled = 0,
    /// A peer exists but the Wi-Fi network is not ready.
    WaitingForNetwork = 1,
    /// A TCP connection is in progress.
    Connecting = 2,
    /// The Reticulum TCP interface is connected and ready.
    Connected = 3,
    /// The configured actor failed a local ownership or fabric invariant.
    Faulted = 4,
    /// A retryable DNS, connection, socket, or transmit failure is backing off.
    Backoff = 5,
}

#[cfg(feature = "experimental-network-config")]
impl ReticulumTcpPeerState {
    /// Frozen experimental numeric representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Most recent retryable outbound Reticulum TCP failure category.
///
/// This is a closed, secret-free API-v1 diagnostic vocabulary. It deliberately
/// excludes hostnames, addresses, credentials, and implementation-specific
/// error values.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ReticulumTcpFailure {
    /// DNS resolution did not complete before its bounded deadline.
    DnsTimeout = 0,
    /// DNS resolution completed with a resolver or protocol failure.
    DnsLookupFailed = 1,
    /// DNS resolution succeeded but returned no usable IPv4 address.
    DnsNoIpv4Result = 2,
    /// The TCP actor reached connect with an invalid local network state.
    ConnectInvalidState = 3,
    /// The remote peer reset or refused the connection.
    ConnectReset = 4,
    /// TCP connection establishment exceeded its bounded deadline.
    ConnectTimeout = 5,
    /// No usable route to the configured peer was available.
    ConnectNoRoute = 6,
    /// An established socket closed before the actor intentionally disconnected.
    SocketClosed = 7,
    /// Writing a Reticulum frame to the established socket failed.
    TransmitFailed = 8,
}

#[cfg(feature = "experimental-network-config")]
impl ReticulumTcpFailure {
    /// Frozen experimental numeric representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Maximum DHCP-provided IPv4 resolver addresses retained in one DNS snapshot.
#[cfg(feature = "experimental-network-config")]
pub const MAX_RETICULUM_DNS_DHCP_SERVERS: usize = 3;

/// Maximum raw UDP resolver attempts retained in one DNS snapshot.
///
/// This covers all three possible DHCP-provided resolvers followed by two
/// product-selected public resolvers.
#[cfg(feature = "experimental-network-config")]
pub const MAX_RETICULUM_DNS_RAW_ATTEMPTS: usize = 5;

/// Outcome of the network stack's built-in DNS resolver path.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ReticulumDnsPrimaryOutcome {
    /// No system-resolver query has started.
    NotStarted = 0,
    /// The system resolver is waiting for a terminal result.
    Resolving = 1,
    /// The system resolver returned a usable IPv4 address.
    Resolved = 2,
    /// DHCP supplied no DNS resolver addresses.
    NoServers = 3,
    /// The bounded system-resolver deadline expired.
    Timeout = 4,
    /// The system resolver reported a protocol or resolver failure.
    LookupFailed = 5,
    /// The system resolver completed without a usable IPv4 address.
    NoIpv4Result = 6,
}

#[cfg(feature = "experimental-network-config")]
impl ReticulumDnsPrimaryOutcome {
    /// Frozen API-v1.10 numeric representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Lifecycle of the common raw UDP DNS socket used after system DNS fails.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ReticulumDnsRawSetupState {
    /// No raw resolver path has started.
    NotStarted = 0,
    /// The actor is binding its bounded UDP socket.
    Binding = 1,
    /// The UDP socket is ready for bounded resolver attempts.
    Ready = 2,
    /// The UDP socket could not bind.
    BindFailed = 3,
    /// The configured hostname could not be encoded as an A query.
    EncodeFailed = 4,
}

#[cfg(feature = "experimental-network-config")]
impl ReticulumDnsRawSetupState {
    /// Frozen API-v1.10 numeric representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Ownership of one raw UDP DNS resolver address.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ReticulumDnsRawSource {
    /// The resolver address came from the active DHCP lease.
    Dhcp = 0,
    /// The resolver address came from the product's public fallback set.
    Public = 1,
}

#[cfg(feature = "experimental-network-config")]
impl ReticulumDnsRawSource {
    /// Frozen API-v1.10 numeric representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Latest outcome of one bounded raw UDP DNS attempt.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReticulumDnsRawOutcome {
    /// This configured slot has not started.
    NotStarted,
    /// An identical resolver was already attempted earlier in the raw path.
    SkippedDuplicate,
    /// Public resolution was suppressed for a local or private hostname.
    SkippedLocalName,
    /// The DNS request is being queued to the UDP socket.
    Sending,
    /// The query was sent and is waiting for a response.
    AwaitingResponse,
    /// The resolver returned a usable IPv4 address.
    Resolved,
    /// The UDP socket could not queue or route the DNS request.
    SendFailed,
    /// The bounded response deadline expired.
    Timeout,
    /// The received packet was not a standard DNS response.
    NotAResponse,
    /// The resolver marked its UDP response as truncated.
    Truncated,
    /// The resolver returned one nonzero DNS response code.
    ResponseCode(NonZeroU8),
    /// The echoed DNS question did not match the requested A record.
    QuestionMismatch,
    /// The DNS response was structurally malformed or incomplete.
    Malformed,
    /// The response contained no usable IPv4 answer.
    NoIpv4Result,
}

#[cfg(feature = "experimental-network-config")]
impl ReticulumDnsRawOutcome {
    /// Frozen API-v1.10 numeric representation.
    pub const fn wire_code(self) -> u8 {
        match self {
            Self::NotStarted => 0,
            Self::SkippedDuplicate => 1,
            Self::SkippedLocalName => 2,
            Self::Sending => 3,
            Self::AwaitingResponse => 4,
            Self::Resolved => 5,
            Self::SendFailed => 6,
            Self::Timeout => 7,
            Self::NotAResponse => 8,
            Self::Truncated => 9,
            Self::ResponseCode(_) => 10,
            Self::QuestionMismatch => 11,
            Self::Malformed => 12,
            Self::NoIpv4Result => 13,
        }
    }

    /// Nonzero DNS response code carried only by [`Self::ResponseCode`].
    pub const fn response_code(self) -> Option<u8> {
        match self {
            Self::ResponseCode(code) => Some(code.get()),
            _ => None,
        }
    }

    /// Construct a typed nonzero resolver response-code outcome.
    pub const fn response_code_outcome(code: u8) -> Option<Self> {
        match NonZeroU8::new(code) {
            Some(code) => Some(Self::ResponseCode(code)),
            None => None,
        }
    }
}

/// One resolver-specific raw UDP DNS attempt.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReticulumDnsRawAttempt {
    /// Whether DHCP or product policy supplied this resolver.
    pub source: ReticulumDnsRawSource,
    /// Exact resolver IPv4 address.
    pub server: [u8; 4],
    /// Latest bounded attempt outcome.
    pub outcome: ReticulumDnsRawOutcome,
}

#[cfg(feature = "experimental-network-config")]
impl ReticulumDnsRawAttempt {
    /// Construct one resolver-specific raw attempt snapshot.
    pub const fn new(
        source: ReticulumDnsRawSource,
        server: [u8; 4],
        outcome: ReticulumDnsRawOutcome,
    ) -> Self {
        Self {
            source,
            server,
            outcome,
        }
    }
}

/// DNS path that produced the currently resolved TCP peer address.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ReticulumDnsResolutionSource {
    /// The network stack's built-in DNS resolver produced the address.
    SystemDns = 0,
    /// A raw UDP query to a DHCP-provided resolver produced the address.
    RawDhcp = 1,
    /// A raw UDP query to a product-selected public resolver produced the address.
    RawPublic = 2,
}

#[cfg(feature = "experimental-network-config")]
impl ReticulumDnsResolutionSource {
    /// Frozen API-v1.10 numeric representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Successful DNS resolution retained for TCP connection diagnosis.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReticulumDnsResolution {
    /// IPv4 address selected for the TCP connection.
    pub address: [u8; 4],
    /// DNS path that produced the address.
    pub source: ReticulumDnsResolutionSource,
    /// Exact resolver address when the successful path identifies it.
    pub resolver: Option<[u8; 4]>,
}

#[cfg(feature = "experimental-network-config")]
impl ReticulumDnsResolution {
    /// Construct one successful DNS resolution snapshot.
    pub const fn new(
        address: [u8; 4],
        source: ReticulumDnsResolutionSource,
        resolver: Option<[u8; 4]>,
    ) -> Self {
        Self {
            address,
            source,
            resolver,
        }
    }
}

/// Bounded, secret-free diagnostics for one hostname resolution attempt.
///
/// Fixed optional slots preserve allocation-free incremental updates. DHCP
/// resolver slots and raw-attempt slots need not be densely populated.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReticulumDnsDiagnostics {
    /// DHCP default gateway used while resolving and connecting.
    pub gateway_ipv4: Option<[u8; 4]>,
    /// Up to three DNS resolver addresses supplied by the active DHCP lease.
    pub dhcp_servers: [Option<[u8; 4]>; MAX_RETICULUM_DNS_DHCP_SERVERS],
    /// Current or terminal outcome of the network stack's built-in resolver.
    pub primary_outcome: ReticulumDnsPrimaryOutcome,
    /// Lifecycle of the common raw UDP fallback socket.
    pub raw_setup_state: ReticulumDnsRawSetupState,
    /// Up to five DHCP-then-public raw UDP resolver attempt snapshots.
    pub raw_attempts: [Option<ReticulumDnsRawAttempt>; MAX_RETICULUM_DNS_RAW_ATTEMPTS],
    /// Successful resolved address and its source, when available.
    pub resolution: Option<ReticulumDnsResolution>,
}

#[cfg(feature = "experimental-network-config")]
impl ReticulumDnsDiagnostics {
    /// Construct one complete bounded DNS diagnostic snapshot.
    pub const fn new(
        gateway_ipv4: Option<[u8; 4]>,
        dhcp_servers: [Option<[u8; 4]>; MAX_RETICULUM_DNS_DHCP_SERVERS],
        primary_outcome: ReticulumDnsPrimaryOutcome,
        raw_setup_state: ReticulumDnsRawSetupState,
        raw_attempts: [Option<ReticulumDnsRawAttempt>; MAX_RETICULUM_DNS_RAW_ATTEMPTS],
        resolution: Option<ReticulumDnsResolution>,
    ) -> Self {
        Self {
            gateway_ipv4,
            dhcp_servers,
            primary_outcome,
            raw_setup_state,
            raw_attempts,
            resolution,
        }
    }
}

/// Live, secret-free Wi-Fi and Reticulum TCP state.
#[cfg(feature = "experimental-network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NetworkRuntimeStatus {
    /// Latest committed desired configuration revision.
    pub configured_revision: u64,
    /// Configuration revision currently applied by network actors.
    pub applied_revision: u64,
    /// Current Wi-Fi state.
    pub wifi_state: WifiStationState,
    /// Saved profile currently being connected or used, when known.
    pub active_wifi_profile: Option<WifiNetworkProfileId>,
    connected_ssid: Option<WifiSsidSummary>,
    /// DHCP-assigned IPv4 address, when available.
    pub ipv4_address: Option<[u8; 4]>,
    /// Current whole-dBm station RSSI, when available.
    pub rssi_dbm: Option<i16>,
    /// Current outbound Reticulum TCP peer state.
    pub tcp_peer_state: ReticulumTcpPeerState,
    /// Most recent retryable outbound TCP failure, when one has occurred.
    pub last_tcp_failure: Option<ReticulumTcpFailure>,
    /// Bounded hostname-resolution diagnostics, when a DNS peer is active.
    pub dns_diagnostics: Option<ReticulumDnsDiagnostics>,
}

#[cfg(feature = "experimental-network-config")]
impl NetworkRuntimeStatus {
    /// Construct one bounded runtime-status snapshot.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        configured_revision: u64,
        applied_revision: u64,
        wifi_state: WifiStationState,
        active_wifi_profile: Option<WifiNetworkProfileId>,
        connected_ssid: Option<&[u8]>,
        ipv4_address: Option<[u8; 4]>,
        rssi_dbm: Option<i16>,
        tcp_peer_state: ReticulumTcpPeerState,
    ) -> Result<Self, InvalidWifiSsid> {
        Self::new_with_tcp_failure(
            configured_revision,
            applied_revision,
            wifi_state,
            active_wifi_profile,
            connected_ssid,
            ipv4_address,
            rssi_dbm,
            tcp_peer_state,
            None,
        )
    }

    /// Construct one bounded runtime-status snapshot with a typed TCP failure.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_tcp_failure(
        configured_revision: u64,
        applied_revision: u64,
        wifi_state: WifiStationState,
        active_wifi_profile: Option<WifiNetworkProfileId>,
        connected_ssid: Option<&[u8]>,
        ipv4_address: Option<[u8; 4]>,
        rssi_dbm: Option<i16>,
        tcp_peer_state: ReticulumTcpPeerState,
        last_tcp_failure: Option<ReticulumTcpFailure>,
    ) -> Result<Self, InvalidWifiSsid> {
        Self::new_with_tcp_diagnostics(
            configured_revision,
            applied_revision,
            wifi_state,
            active_wifi_profile,
            connected_ssid,
            ipv4_address,
            rssi_dbm,
            tcp_peer_state,
            last_tcp_failure,
            None,
        )
    }

    /// Construct one bounded runtime-status snapshot with full TCP and DNS diagnostics.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_tcp_diagnostics(
        configured_revision: u64,
        applied_revision: u64,
        wifi_state: WifiStationState,
        active_wifi_profile: Option<WifiNetworkProfileId>,
        connected_ssid: Option<&[u8]>,
        ipv4_address: Option<[u8; 4]>,
        rssi_dbm: Option<i16>,
        tcp_peer_state: ReticulumTcpPeerState,
        last_tcp_failure: Option<ReticulumTcpFailure>,
        dns_diagnostics: Option<ReticulumDnsDiagnostics>,
    ) -> Result<Self, InvalidWifiSsid> {
        Ok(Self {
            configured_revision,
            applied_revision,
            wifi_state,
            active_wifi_profile,
            connected_ssid: match connected_ssid {
                Some(ssid) => Some(WifiSsidSummary::new(ssid)?),
                None => None,
            },
            ipv4_address,
            rssi_dbm,
            tcp_peer_state,
            last_tcp_failure,
            dns_diagnostics,
        })
    }

    /// Associated SSID, when available.
    pub const fn connected_ssid(self) -> Option<WifiSsidSummary> {
        self.connected_ssid
    }
}

/// Scalar-only status for an accepted submission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubmissionStatus {
    /// Submission being described.
    pub id: SubmissionId,
    /// Current state.
    pub state: SubmissionState,
}

/// Acceptance result for the experimental outbound RNS DATA submission.
///
/// The response contains only the device-assigned identifier used with
/// `submission.status`; it never contains prepared packet bytes.
#[cfg(feature = "experimental-rns-data")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubmissionAccepted {
    /// Device-assigned submission identifier.
    ///
    /// Acceptance means the device reserved the bounded capacity needed to own
    /// the submission. It does not guarantee delivery; a later status may
    /// report [`SubmissionFailure::DeliveryTimeout`].
    pub id: SubmissionId,
}

/// Acceptance result for source-free basic LXMF submission.
///
/// The device selects the authenticated local LXMF source. The returned
/// message identifier names the exact composed LXMF message, while the
/// submission identifier is used with [`DeviceRequest::SubmissionStatus`]. A
/// successful response means durable acceptance, not peer delivery.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LxmfBasicSendAccepted {
    /// Device-assigned durable submission identifier.
    pub id: SubmissionId,
    message_id: [u8; 32],
}

#[cfg(feature = "experimental-lxmf")]
impl LxmfBasicSendAccepted {
    /// Construct a successful basic-LXMF acceptance result.
    pub const fn new(id: SubmissionId, message_id: [u8; 32]) -> Self {
        Self { id, message_id }
    }

    /// Python-compatible LXMF authenticated-message identifier.
    pub const fn message_id(&self) -> &[u8; 32] {
        &self.message_id
    }
}

/// Acceptance result for one authenticated NomadNet page fetch.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NomadFetchStartAccepted {
    /// Device-assigned principal-owned fetch identifier.
    pub id: NomadFetchId,
    /// Whether this request created a fresh fetch or replayed an identical one.
    pub outcome: NomadFetchStartOutcome,
}

/// Principal-scoped idempotency outcome for a successful fetch start.
///
/// This is a closed API-v1 wire vocabulary.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum NomadFetchStartOutcome {
    /// A fresh fetch was accepted.
    Accepted = 0,
    /// An identical request for this principal and idempotency key was replayed.
    Replayed = 1,
}

#[cfg(feature = "experimental-nomad")]
impl NomadFetchStartOutcome {
    /// Frozen API-v1 numeric representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Non-terminal phase returned by a NomadNet fetch poll.
///
/// This is a closed API-v1 wire vocabulary.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum NomadFetchPhase {
    /// Path discovery is in progress.
    PathLookup = 0,
    /// Link establishment is in progress.
    LinkEstablishment = 1,
    /// The anonymous request is being prepared.
    RequestPreparation = 2,
    /// A prepared request awaits first-dispatch confirmation.
    AwaitingDispatchConfirmation = 3,
    /// A confirmed request awaits its exactly correlated response.
    AwaitingResponse = 4,
}

#[cfg(feature = "experimental-nomad")]
impl NomadFetchPhase {
    /// Frozen API-v1 numeric representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Stable terminal failure returned by a NomadNet fetch poll.
///
/// Link identifiers, request identifiers, and adapter-local diagnostic codes
/// remain inside the product owner. This is a closed API-v1 wire vocabulary.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum NomadFetchFailure {
    /// Path discovery completed without a usable path.
    NoPath = 0,
    /// Link preparation, dispatch, establishment, or retention failed.
    Link = 1,
    /// Request preparation, dispatch, or remote processing failed.
    Request = 2,
    /// A confirmed request exceeded its bounded response window.
    Timeout = 3,
    /// The decoded page exceeded the fixed direct-response limit.
    PageTooLarge = 4,
    /// The decoded page was not valid UTF-8 Micron text.
    InvalidUtf8 = 5,
    /// The product owner detected an internal invariant or backend failure.
    Internal = 6,
}

#[cfg(feature = "experimental-nomad")]
impl NomadFetchFailure {
    /// Frozen API-v1 numeric representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// One owned bounded valid UTF-8 Micron page.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct NomadPage {
    bytes: [u8; MAX_NOMAD_PAGE_BYTES],
    len: u16,
}

#[cfg(feature = "experimental-nomad")]
impl NomadPage {
    /// Validate and copy one complete page body.
    pub fn new(bytes: &[u8]) -> Result<Self, InvalidNomadPage> {
        if bytes.len() > MAX_NOMAD_PAGE_BYTES {
            return Err(InvalidNomadPage::TooLarge {
                actual: bytes.len(),
            });
        }
        if core::str::from_utf8(bytes).is_err() {
            return Err(InvalidNomadPage::InvalidUtf8);
        }
        let mut owned = [0_u8; MAX_NOMAD_PAGE_BYTES];
        owned[..bytes.len()].copy_from_slice(bytes);
        Ok(Self {
            bytes: owned,
            len: bytes.len() as u16,
        })
    }

    /// Borrow the complete page bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }

    /// Borrow the complete page as valid UTF-8.
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(self.as_bytes()).expect("NomadPage validates UTF-8 at construction")
    }

    /// Complete page length in bytes.
    pub const fn len(&self) -> usize {
        self.len as usize
    }

    /// Whether the page is empty.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[cfg(feature = "experimental-nomad")]
impl core::fmt::Debug for NomadPage {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("NomadPage")
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

/// A candidate NomadNet page violated its fixed logical boundary.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidNomadPage {
    /// The page exceeded fixed response storage.
    TooLarge {
        /// Rejected byte count.
        actual: usize,
    },
    /// The page was not valid UTF-8.
    InvalidUtf8,
}

/// Result returned by polling one principal-owned NomadNet fetch.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
// The ready page remains inline so this no-alloc boundary owns one complete
// response without indirection or a product-lifetime borrow.
#[allow(clippy::large_enum_variant)]
pub enum NomadFetchPollResponse {
    /// The fetch remains in progress.
    Pending(NomadFetchPhase),
    /// One complete bounded Micron page is ready.
    Ready(NomadPage),
    /// The fetch ended with a stable terminal failure.
    Failed(NomadFetchFailure),
}

#[cfg(feature = "experimental-nomad")]
impl NomadFetchPollResponse {
    /// Frozen state discriminator encoded at response body key zero.
    pub const fn wire_code(&self) -> u8 {
        match self {
            Self::Pending(_) => 0,
            Self::Ready(_) => 1,
            Self::Failed(_) => 2,
        }
    }
}

/// Typed API error returned in a logical response.
///
/// This is a closed API-v1 wire vocabulary. The unreleased alpha advances its
/// lockstep minor revision when adding a numeric error category; a released
/// protocol would require a new API major version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum ApiErrorCode {
    /// Client selected an unknown operation number.
    UnsupportedOperation = 1,
    /// Client selected an incompatible API major version.
    UnsupportedVersion = 2,
    /// Operation requires an authenticated application session.
    AuthenticationRequired = 3,
    /// Authenticated principal lacks the required permission.
    PermissionDenied = 4,
    /// Requested object does not exist for this principal.
    NotFound = 5,
    /// Request is semantically invalid after decoding.
    InvalidRequest = 6,
    /// Build or runtime profile cannot perform the operation.
    CapabilityUnavailable = 7,
    /// Device failed without a client-actionable category.
    Internal = 8,
    /// Operation was not accepted because a bounded queue or table is full.
    ///
    /// No submission identifier is allocated. Retrying later may succeed.
    CapacityExhausted = 9,
    /// This principal already used the supplied idempotency key for different
    /// request content.
    ///
    /// The conflicting request is not accepted. Repeating the original
    /// request content remains safe.
    IdempotencyConflict = 10,
    /// A transient device-owned resource is busy with another retained
    /// operation.
    ///
    /// The request was not rejected on semantic or capacity grounds. The
    /// authenticated session remains valid and retrying the exact operation
    /// after a short bounded delay is safe.
    RetryLater = 11,
}

impl ApiErrorCode {
    /// Frozen API-v1 numeric representation.
    pub const fn wire_code(self) -> u16 {
        self as u16
    }
}

/// Error response body with optional numeric operation context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApiErrorResponse {
    /// Stable machine-readable category.
    pub code: ApiErrorCode,
    /// Request operation related to the error, when known.
    pub operation: Option<u16>,
}

/// Successful or failed logical response body.
// The inbox variant deliberately owns its fixed-capacity payload so response
// dispatch remains allocation-free and cannot retain a mailbox borrow.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DeviceResponse {
    /// Result of `system.capabilities`.
    SystemCapabilities(CapabilitySnapshot),
    /// Result of `identity.summary`.
    IdentitySummary(IdentitySummary),
    /// Result of `submission.status`.
    SubmissionStatus(SubmissionStatus),
    /// Result of `experimental.rns_inbox.status`.
    #[cfg(feature = "experimental-rns-inbox")]
    RnsInboxStatus(RnsInboxStatus),
    /// Occupied result of `experimental.rns_inbox.peek`.
    #[cfg(feature = "experimental-rns-inbox")]
    RnsInboxPeek(RnsInboxItem),
    /// Next committed LXMF metadata entry in physical commit order.
    #[cfg(feature = "experimental-lxmf")]
    LxmfNext(LxmfMessageSummary),
    /// Bounded exact normalized-wire bytes for one committed LXMF message.
    #[cfg(feature = "experimental-lxmf")]
    LxmfRead(LxmfReadChunk),
    /// Durable LXMF collection state.
    #[cfg(feature = "experimental-lxmf")]
    LxmfMailboxStatus(LxmfMailboxStatus),
    /// Collection state after an idempotent monotonic acknowledgement.
    #[cfg(feature = "experimental-lxmf")]
    LxmfMailboxAcknowledged(LxmfMailboxStatus),
    /// Accepted source-free basic LXMF submission.
    #[cfg(feature = "experimental-lxmf")]
    LxmfBasicSendAccepted(LxmfBasicSendAccepted),
    /// One bounded page from nearby `lxmf.delivery` peer discovery.
    #[cfg(feature = "experimental-lxmf")]
    LxmfPeerNext(LxmfPeerDiscoveryPage),
    /// Accepted bounded NomadNet page fetch.
    #[cfg(feature = "experimental-nomad")]
    NomadFetchStartAccepted(NomadFetchStartAccepted),
    /// Current or terminal state of one bounded NomadNet page fetch.
    #[cfg(feature = "experimental-nomad")]
    NomadFetchPoll(NomadFetchPollResponse),
    /// Redacted desired Wi-Fi and Reticulum TCP configuration.
    #[cfg(feature = "experimental-network-config")]
    NetworkConfig(NetworkConfigSnapshot),
    /// Normal compare-and-swap desired-network mutation result.
    #[cfg(feature = "experimental-network-config")]
    NetworkConfigMutation(NetworkConfigMutationOutcome),
    /// Live Wi-Fi and Reticulum TCP state.
    #[cfg(feature = "experimental-network-config")]
    NetworkStatus(NetworkRuntimeStatus),
    /// Bounded cross-interface node diagnostics.
    NodeDiagnostics(NodeDiagnosticsSnapshot),
    /// Bounded lexicographically ordered route diagnostics.
    RouteDiagnosticsPage(RouteDiagnosticsPage),
    /// Bounded boot-scoped packet-correlated radio trace.
    RadioTracePage(RadioTracePage),
    /// Admission result for a manual ordinary service announce.
    ManualServiceAnnounce(ManualServiceAnnounceDisposition),
    /// Accepted boot-scoped Reticulum path-and-proof probe.
    ReticulumProbeStartAccepted(ProbeStartAccepted),
    /// Current or terminal state of one Reticulum path-and-proof probe.
    ReticulumProbePoll(ProbePollResponse),
    /// Accepted experimental outbound RNS DATA submission.
    #[cfg(feature = "experimental-rns-data")]
    SubmitRnsDataAccepted(SubmissionAccepted),
    /// Typed request failure.
    Error(ApiErrorResponse),
}

impl DeviceResponse {
    /// Operation or response-kind number encoded on the wire.
    pub const fn kind(&self) -> u16 {
        match self {
            Self::SystemCapabilities(_) => OP_SYSTEM_CAPABILITIES,
            Self::IdentitySummary(_) => OP_IDENTITY_SUMMARY,
            Self::SubmissionStatus(_) => OP_SUBMISSION_STATUS,
            #[cfg(feature = "experimental-rns-inbox")]
            Self::RnsInboxStatus(_) => OP_EXPERIMENTAL_RNS_INBOX_STATUS,
            #[cfg(feature = "experimental-rns-inbox")]
            Self::RnsInboxPeek(_) => OP_EXPERIMENTAL_RNS_INBOX_PEEK,
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfNext(_) => OP_EXPERIMENTAL_LXMF_NEXT,
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfRead(_) => OP_EXPERIMENTAL_LXMF_READ,
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfMailboxStatus(_) => OP_EXPERIMENTAL_LXMF_MAILBOX_STATUS,
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfMailboxAcknowledged(_) => OP_EXPERIMENTAL_LXMF_MAILBOX_ACKNOWLEDGE,
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfBasicSendAccepted(_) => OP_EXPERIMENTAL_LXMF_BASIC_SEND,
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfPeerNext(_) => OP_EXPERIMENTAL_LXMF_PEER_NEXT,
            #[cfg(feature = "experimental-nomad")]
            Self::NomadFetchStartAccepted(_) => OP_EXPERIMENTAL_NOMAD_FETCH_START,
            #[cfg(feature = "experimental-nomad")]
            Self::NomadFetchPoll(_) => OP_EXPERIMENTAL_NOMAD_FETCH_POLL,
            #[cfg(feature = "experimental-network-config")]
            Self::NetworkConfig(_) => OP_EXPERIMENTAL_NETWORK_CONFIG_GET,
            #[cfg(feature = "experimental-network-config")]
            Self::NetworkConfigMutation(_) => OP_EXPERIMENTAL_NETWORK_CONFIG_MUTATE,
            #[cfg(feature = "experimental-network-config")]
            Self::NetworkStatus(_) => OP_EXPERIMENTAL_NETWORK_STATUS,
            Self::NodeDiagnostics(_) => OP_EXPERIMENTAL_NODE_DIAGNOSTICS,
            Self::RouteDiagnosticsPage(_) => OP_EXPERIMENTAL_ROUTE_DIAGNOSTICS_PAGE,
            Self::RadioTracePage(_) => OP_EXPERIMENTAL_RADIO_TRACE_PAGE,
            Self::ManualServiceAnnounce(_) => OP_EXPERIMENTAL_MANUAL_SERVICE_ANNOUNCE,
            Self::ReticulumProbeStartAccepted(_) => OP_EXPERIMENTAL_RETICULUM_PROBE_START,
            Self::ReticulumProbePoll(_) => OP_EXPERIMENTAL_RETICULUM_PROBE_POLL,
            #[cfg(feature = "experimental-rns-data")]
            Self::SubmitRnsDataAccepted(_) => OP_EXPERIMENTAL_SUBMIT_RNS_DATA,
            Self::Error(_) => RESPONSE_ERROR,
        }
    }
}

/// Logical response envelope encoded as exactly one CBOR item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResponseEnvelope {
    /// Protocol version selected by the device.
    pub version: ApiVersion,
    /// Request identifier copied from the request.
    pub request_id: RequestId,
    /// Operation-specific response.
    pub response: DeviceResponse,
}
