//! Wi-Fi, Reticulum TCP, RMAP, and LoRa network configuration model.

use core::num::NonZeroU8;

use super::*;

/// Read the redacted desired Wi-Fi and Reticulum TCP configuration.
#[cfg(feature = "network-config")]
pub const OP_NETWORK_CONFIG_GET: u16 = 0xf00a;
/// Mutate one saved Wi-Fi profile or the single Reticulum TCP peer.
#[cfg(feature = "network-config")]
pub const OP_NETWORK_CONFIG_MUTATE: u16 = 0xf00b;
/// Read live Wi-Fi and Reticulum TCP interface state.
#[cfg(feature = "network-config")]
pub const OP_NETWORK_STATUS: u16 = 0xf00c;
/// Validated borrowed Wi-Fi service-set identifier.
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct WifiSsid<'a>(&'a [u8]);

#[cfg(feature = "network-config")]
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

#[cfg(feature = "network-config")]
impl core::fmt::Debug for WifiSsid<'_> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("WifiSsid")
            .field("bytes", &self.0)
            .finish()
    }
}

/// Why a Wi-Fi SSID was rejected.
#[cfg(feature = "network-config")]
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
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum WifiCredentialUpdate<'a> {
    /// Retain the existing credential.
    Keep,
    /// Replace the credential with a validated WPA2-Personal passphrase.
    Replace(&'a [u8]),
}

#[cfg(feature = "network-config")]
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

    /// Stable wire discriminator.
    pub const fn wire_code(self) -> u8 {
        match self {
            Self::Keep => 0,
            Self::Replace(_) => 1,
        }
    }
}

#[cfg(feature = "network-config")]
impl core::fmt::Debug for WifiCredentialUpdate<'_> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Keep => formatter.write_str("Keep"),
            Self::Replace(_) => formatter.write_str("Replace(<redacted>)"),
        }
    }
}

/// Why a WPA2-Personal passphrase was rejected.
#[cfg(feature = "network-config")]
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
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WifiNetworkUpdate<'a> {
    enabled: bool,
    priority: u8,
    ssid: WifiSsid<'a>,
    credential: WifiCredentialUpdate<'a>,
}

#[cfg(feature = "network-config")]
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
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ReticulumTcpPeerIpv4Address([u8; 4]);

#[cfg(feature = "network-config")]
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
#[cfg(feature = "network-config")]
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
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ReticulumTcpPeerHostname<'a>(&'a str);

#[cfg(feature = "network-config")]
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
#[cfg(feature = "network-config")]
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

/// Validated borrowed UTF-8 board display name.
///
/// A display name is a single line of text without control or separator
/// characters. It is shown on the appliance e-paper panel and published as the
/// display-name field of the LXMF delivery announce.
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DeviceName<'a>(&'a str);

#[cfg(feature = "network-config")]
impl<'a> DeviceName<'a> {
    /// Validate a bounded single-line UTF-8 display name without normalizing it.
    pub fn new(name: &'a str) -> Result<Self, InvalidDeviceName> {
        let bytes = name.as_bytes();
        if bytes.is_empty() {
            return Err(InvalidDeviceName::Empty);
        }
        if bytes.len() > MAX_DEVICE_NAME_BYTES {
            return Err(InvalidDeviceName::TooLong {
                actual: bytes.len(),
            });
        }
        if name
            .chars()
            .any(|character| character.is_control() || matches!(character, '\u{2028}' | '\u{2029}'))
        {
            return Err(InvalidDeviceName::UnsupportedCharacter);
        }
        Ok(Self(name))
    }

    /// Borrow the exact validated display name.
    pub const fn as_str(self) -> &'a str {
        self.0
    }
}

/// Why a board display name was rejected.
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidDeviceName {
    /// A display name must contain at least one byte.
    Empty,
    /// The complete display name exceeded the fixed API limit.
    TooLong {
        /// Rejected UTF-8 byte count.
        actual: usize,
    },
    /// The display name contains a control or separator character unsuitable
    /// for one display line or an announce app-data field.
    UnsupportedCharacter,
}

/// Owned bounded UTF-8 board display name used by redacted configuration
/// responses.
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct DeviceNameSummary {
    bytes: [u8; MAX_DEVICE_NAME_BYTES],
    len: u8,
}

#[cfg(feature = "network-config")]
impl DeviceNameSummary {
    /// Validate and copy one display name into fixed response storage.
    pub fn new(name: &str) -> Result<Self, InvalidDeviceName> {
        let validated = DeviceName::new(name)?;
        let bytes = validated.as_str().as_bytes();
        let mut owned = [0_u8; MAX_DEVICE_NAME_BYTES];
        owned[..bytes.len()].copy_from_slice(bytes);
        Ok(Self {
            bytes: owned,
            len: bytes.len() as u8,
        })
    }

    /// Borrow the exact validated display name.
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len as usize])
            .expect("display-name validation accepts UTF-8 only")
    }
}

#[cfg(feature = "network-config")]
impl core::fmt::Debug for DeviceNameSummary {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_tuple("DeviceNameSummary")
            .field(&self.as_str())
            .finish()
    }
}

/// Desired single outbound Reticulum TCP peer in a configuration mutation.
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReticulumTcpPeerUpdate {
    enabled: bool,
    ipv4_address: ReticulumTcpPeerIpv4Address,
    port: u16,
}

#[cfg(feature = "network-config")]
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
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReticulumTcpPeerHostUpdate<'a> {
    enabled: bool,
    hostname: ReticulumTcpPeerHostname<'a>,
    port: u16,
}

#[cfg(feature = "network-config")]
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
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidReticulumTcpPeerPort;

/// Opaque nonzero Wi-Fi profile identity.
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WifiNetworkProfileId([u8; 16]);

#[cfg(feature = "network-config")]
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
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidWifiNetworkProfileId;

/// Desired gateway-wide policy independent of individual saved records.
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GatewayPolicy {
    wifi_transport_enabled: bool,
    automatic_announces_enabled: bool,
}

#[cfg(feature = "network-config")]
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
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RmapLocation {
    latitude_e6: i32,
    longitude_e6: i32,
}

#[cfg(feature = "network-config")]
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
#[cfg(feature = "network-config")]
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
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RmapConfig {
    discovery_enabled: bool,
    share_location: bool,
    phone_location: Option<RmapLocation>,
}

#[cfg(feature = "network-config")]
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
/// The appliance profile deliberately exposes only the four
/// board-qualified power rows used by the E290 radio owner. This is a
/// requested radio output, not a calibrated conducted-power or EIRP claim.
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LoraTransmitPowerDbm(u8);

#[cfg(feature = "network-config")]
impl LoraTransmitPowerDbm {
    /// Lowest supported requested output.
    pub const DBM_14: Self = Self(14);
    /// Second supported requested output.
    pub const DBM_17: Self = Self(17);
    /// Third supported requested output.
    pub const DBM_20: Self = Self(20);
    /// Highest supported requested output.
    pub const DBM_22: Self = Self(22);
    /// Default requested output for a fresh configuration.
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

#[cfg(feature = "network-config")]
impl Default for LoraTransmitPowerDbm {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A requested LoRa transmit power was not one of the qualified values.
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidLoraTransmitPowerDbm {
    actual: u8,
}

#[cfg(feature = "network-config")]
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
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LoraRadioProfile {
    frequency_hz: u32,
    bandwidth_hz: u32,
    spreading_factor: u8,
    coding_rate_denominator: u8,
    tx_power_dbm: LoraTransmitPowerDbm,
}

#[cfg(feature = "network-config")]
impl LoraRadioProfile {
    /// Default complete profile for a fresh configuration.
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

#[cfg(feature = "network-config")]
impl Default for LoraRadioProfile {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Invalid numeric field in a requested LoRa profile.
#[cfg(feature = "network-config")]
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
#[cfg(feature = "network-config")]
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
    /// Replace or clear the board display name shown on the appliance and in
    /// LXMF delivery announces.
    SetDeviceName(Option<DeviceName<'a>>),
}

/// Correlated compare-and-swap request for one desired-network mutation.
///
/// Debug output remains safe because [`WifiCredentialUpdate`] redacts
/// replacement bytes.
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NetworkConfigMutationRequest<'a> {
    mutation: NetworkConfigMutation<'a>,
    expected_revision: u64,
    idempotency_key: IdempotencyKey,
}

#[cfg(feature = "network-config")]
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

/// Owned bounded Wi-Fi SSID used by redacted responses.
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct WifiSsidSummary {
    bytes: [u8; MAX_WIFI_SSID_BYTES],
    len: u8,
}

#[cfg(feature = "network-config")]
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

#[cfg(feature = "network-config")]
impl core::fmt::Debug for WifiSsidSummary {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("WifiSsidSummary")
            .field("bytes", &self.as_bytes())
            .finish()
    }
}

/// Redacted desired Wi-Fi configuration.
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WifiNetworkConfigSummary {
    profile_id: WifiNetworkProfileId,
    enabled: bool,
    priority: u8,
    ssid: WifiSsidSummary,
    credential_configured: bool,
}

#[cfg(feature = "network-config")]
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
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReticulumTcpPeerConfigSummary {
    enabled: bool,
    ipv4_address: ReticulumTcpPeerIpv4Address,
    port: u16,
}

#[cfg(feature = "network-config")]
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
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct ReticulumTcpPeerHostnameSummary {
    bytes: [u8; MAX_RETICULUM_TCP_PEER_HOSTNAME_BYTES],
    len: u8,
}

#[cfg(feature = "network-config")]
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

#[cfg(feature = "network-config")]
impl core::fmt::Debug for ReticulumTcpPeerHostnameSummary {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_tuple("ReticulumTcpPeerHostnameSummary")
            .field(&self.as_str())
            .finish()
    }
}

/// Redacted desired hostname-based outbound Reticulum TCP peer configuration.
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReticulumTcpPeerHostConfigSummary {
    enabled: bool,
    hostname: ReticulumTcpPeerHostnameSummary,
    port: u16,
}

#[cfg(feature = "network-config")]
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
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidReticulumTcpPeerHostConfig {
    /// The DNS hostname was malformed or exceeded its fixed bound.
    InvalidHostname(InvalidReticulumTcpPeerHostname),
    /// TCP port zero is reserved.
    InvalidPort,
}

/// Complete redacted desired network configuration.
#[cfg(feature = "network-config")]
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
    device_name: Option<DeviceNameSummary>,
}

#[cfg(feature = "network-config")]
impl NetworkConfigSnapshot {
    /// Construct a snapshot with the default gateway, RMAP, and LoRa policies.
    pub fn with_defaults(
        revision: u64,
        wifi_profiles: [Option<WifiNetworkConfigSummary>; MAX_WIFI_NETWORK_PROFILES],
        tcp_peer: Option<ReticulumTcpPeerConfigSummary>,
    ) -> Result<Self, InvalidNetworkConfigSnapshot> {
        Self::new(
            revision,
            wifi_profiles,
            tcp_peer,
            None,
            GatewayPolicy::new(true, true),
            RmapConfig::new(false, false, None),
            LoraRadioProfile::DEFAULT,
            None,
        )
    }

    /// Validate and construct a complete desired configuration.
    ///
    /// The IPv4 and hostname peer slots are mutually exclusive. Revision zero
    /// represents erased media and therefore requires all default values.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        revision: u64,
        wifi_profiles: [Option<WifiNetworkConfigSummary>; MAX_WIFI_NETWORK_PROFILES],
        tcp_peer: Option<ReticulumTcpPeerConfigSummary>,
        tcp_host_peer: Option<ReticulumTcpPeerHostConfigSummary>,
        gateway_policy: GatewayPolicy,
        rmap_config: RmapConfig,
        lora_profile: LoraRadioProfile,
        device_name: Option<DeviceNameSummary>,
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
                || lora_profile != LoraRadioProfile::DEFAULT
                || device_name.is_some())
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
            device_name,
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

    /// Configured board display name, if any.
    pub const fn device_name(self) -> Option<DeviceNameSummary> {
        self.device_name
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
#[cfg(feature = "network-config")]
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
#[cfg(feature = "network-config")]
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

/// Live Wi-Fi station state.
#[cfg(feature = "network-config")]
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

#[cfg(feature = "network-config")]
impl WifiStationState {
    /// Stable numeric representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Live outbound Reticulum TCP peer state.
#[cfg(feature = "network-config")]
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

#[cfg(feature = "network-config")]
impl ReticulumTcpPeerState {
    /// Stable numeric representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Most recent retryable outbound Reticulum TCP failure category.
///
/// This is a closed, secret-free diagnostic vocabulary. It deliberately
/// excludes hostnames, addresses, credentials, and implementation-specific
/// error values.
#[cfg(feature = "network-config")]
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

#[cfg(feature = "network-config")]
impl ReticulumTcpFailure {
    /// Stable numeric representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Maximum DHCP-provided IPv4 resolver addresses retained in one DNS snapshot.
#[cfg(feature = "network-config")]
pub const MAX_RETICULUM_DNS_DHCP_SERVERS: usize = 3;

/// Maximum raw UDP resolver attempts retained in one DNS snapshot.
///
/// This covers all three possible DHCP-provided resolvers followed by two
/// product-selected public resolvers.
#[cfg(feature = "network-config")]
pub const MAX_RETICULUM_DNS_RAW_ATTEMPTS: usize = 5;

/// Outcome of the network stack's built-in DNS resolver path.
#[cfg(feature = "network-config")]
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

#[cfg(feature = "network-config")]
impl ReticulumDnsPrimaryOutcome {
    /// Stable numeric wire representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Lifecycle of the common raw UDP DNS socket used after system DNS fails.
#[cfg(feature = "network-config")]
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

#[cfg(feature = "network-config")]
impl ReticulumDnsRawSetupState {
    /// Stable numeric wire representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Ownership of one raw UDP DNS resolver address.
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ReticulumDnsRawSource {
    /// The resolver address came from the active DHCP lease.
    Dhcp = 0,
    /// The resolver address came from the product's public fallback set.
    Public = 1,
}

#[cfg(feature = "network-config")]
impl ReticulumDnsRawSource {
    /// Stable numeric wire representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Latest outcome of one bounded raw UDP DNS attempt.
#[cfg(feature = "network-config")]
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

#[cfg(feature = "network-config")]
impl ReticulumDnsRawOutcome {
    /// Stable numeric wire representation.
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
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReticulumDnsRawAttempt {
    /// Whether DHCP or product policy supplied this resolver.
    pub source: ReticulumDnsRawSource,
    /// Exact resolver IPv4 address.
    pub server: [u8; 4],
    /// Latest bounded attempt outcome.
    pub outcome: ReticulumDnsRawOutcome,
}

#[cfg(feature = "network-config")]
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
#[cfg(feature = "network-config")]
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

#[cfg(feature = "network-config")]
impl ReticulumDnsResolutionSource {
    /// Stable numeric wire representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Successful DNS resolution retained for TCP connection diagnosis.
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReticulumDnsResolution {
    /// IPv4 address selected for the TCP connection.
    pub address: [u8; 4],
    /// DNS path that produced the address.
    pub source: ReticulumDnsResolutionSource,
    /// Exact resolver address when the successful path identifies it.
    pub resolver: Option<[u8; 4]>,
}

#[cfg(feature = "network-config")]
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
#[cfg(feature = "network-config")]
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

#[cfg(feature = "network-config")]
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

/// Cooperative proof-of-work state for the opt-in RMAP discovery payload.
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum RmapStampPhase {
    /// RMAP publication is disabled by the applied configuration.
    Disabled = 0,
    /// The board is incrementally searching for the required discovery stamp.
    Searching = 1,
    /// A complete stamped discovery payload is resident and reusable.
    Ready = 2,
    /// The deterministic stamp candidate space was exhausted.
    Exhausted = 3,
    /// RMAP activation failed before a stamp search could run.
    Faulted = 4,
}

#[cfg(feature = "network-config")]
impl RmapStampPhase {
    /// Stable numeric wire representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// State of the TCP readiness gate for an RMAP publication target.
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum RmapInitialTcpGateState {
    /// No public TCP peer was applied, so publication uses ordinary broadcast policy.
    NotRequired = 0,
    /// A public TCP peer was applied but its packet interface is not ready.
    Waiting = 1,
    /// The applied public TCP interface is ready for an exact-interface publication.
    Open = 2,
}

#[cfg(feature = "network-config")]
impl RmapInitialTcpGateState {
    /// Stable numeric wire representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Most recent RMAP announce admission outcome.
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum RmapQueueOutcome {
    /// No publication attempt has reached announce admission this boot.
    NotAttempted = 0,
    /// The complete announce action entered the ordinary transmission coordinator.
    Accepted = 1,
    /// Native announce construction or its bounded queue deferred the attempt.
    AnnounceAdmissionDeferred = 2,
    /// The ordinary transmission coordinator deferred the complete action owner.
    OrdinaryAdmissionDeferred = 3,
}

#[cfg(feature = "network-config")]
impl RmapQueueOutcome {
    /// Stable numeric wire representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Physical-egress evidence retained for the latest accepted RMAP publication.
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum RmapEgressConfirmation {
    /// No RMAP publication has been accepted this boot.
    NotApplicable = 0,
    /// Queue admission is authoritative, but this build cannot correlate its physical completion.
    NotObserved = 1,
    /// The selected interface reported physical completion for the publication.
    Confirmed = 2,
}

#[cfg(feature = "network-config")]
impl RmapEgressConfirmation {
    /// Stable numeric wire representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Stable, secret-free reason why RMAP activation or publication is deferred.
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum RmapDeferredReason {
    /// Discovery payload validation rejected the applied configuration.
    DiscoveryModelInvalid = 0,
    /// The discovery payload could not be encoded.
    PayloadEncodingFailed = 1,
    /// The compact stamp search could not be initialized.
    StampInitializationFailed = 2,
    /// The local discovery destination could not be activated.
    DestinationActivationFailed = 3,
    /// The deterministic stamp candidate space was exhausted.
    StampSearchExhausted = 4,
    /// The exact public TCP publication target is not ready.
    InitialTcpNotReady = 5,
    /// Discovery application data exceeded announce admission limits.
    AnnouncePayloadTooLarge = 6,
    /// The bounded native announce queue was full.
    AnnounceQueueFull = 7,
    /// Native announce construction or queueing rejected the request.
    AnnounceConstructionRejected = 8,
    /// The ordinary transmission coordinator rejected the complete action owner.
    OrdinaryQueueRejected = 9,
}

#[cfg(feature = "network-config")]
impl RmapDeferredReason {
    /// Stable numeric wire representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Compact current state of opt-in RMAP discovery publication.
///
/// Uptime values are board-local monotonic seconds and intentionally cannot be
/// compared across reboots. `next_due_in_seconds` is relative to the status
/// snapshot so the app never has to infer the board's current uptime.
#[cfg(feature = "network-config")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RmapRuntimeStatus {
    /// Whether the desired configuration revision is the revision running this boot.
    pub config_applied: bool,
    /// Current discovery-stamp lifecycle.
    pub stamp_phase: RmapStampPhase,
    /// Total deterministic stamp candidates tested this boot.
    pub stamp_attempts: u64,
    /// Readiness of the applied public TCP publication gate.
    pub initial_tcp_gate: RmapInitialTcpGateState,
    /// Publications accepted by the ordinary transmission coordinator this boot.
    pub queued_count: u32,
    /// Outcome of the most recent queue attempt.
    pub last_queue_outcome: RmapQueueOutcome,
    /// Board uptime when the most recent queue attempt ran.
    pub last_queue_attempt_at_uptime_seconds: Option<u64>,
    /// Physical completion evidence for the latest accepted publication.
    pub egress_confirmation: RmapEgressConfirmation,
    /// Relative delay until the next eligible publication attempt.
    pub next_due_in_seconds: Option<u64>,
    /// Current activation failure or publication deferral, when present.
    pub deferred_reason: Option<RmapDeferredReason>,
}

#[cfg(feature = "network-config")]
impl RmapRuntimeStatus {
    /// Construct one complete current RMAP projection.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        config_applied: bool,
        stamp_phase: RmapStampPhase,
        stamp_attempts: u64,
        initial_tcp_gate: RmapInitialTcpGateState,
        queued_count: u32,
        last_queue_outcome: RmapQueueOutcome,
        last_queue_attempt_at_uptime_seconds: Option<u64>,
        egress_confirmation: RmapEgressConfirmation,
        next_due_in_seconds: Option<u64>,
        deferred_reason: Option<RmapDeferredReason>,
    ) -> Self {
        Self {
            config_applied,
            stamp_phase,
            stamp_attempts,
            initial_tcp_gate,
            queued_count,
            last_queue_outcome,
            last_queue_attempt_at_uptime_seconds,
            egress_confirmation,
            next_due_in_seconds,
            deferred_reason,
        }
    }
}

/// Live, secret-free Wi-Fi and Reticulum TCP state.
#[cfg(feature = "network-config")]
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
    /// Current opt-in RMAP publication diagnostics, when this firmware exposes them.
    pub rmap_status: Option<RmapRuntimeStatus>,
}

#[cfg(feature = "network-config")]
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
            rmap_status: None,
        })
    }

    /// Attach the current RMAP publication projection.
    pub const fn with_rmap_status(mut self, status: RmapRuntimeStatus) -> Self {
        self.rmap_status = Some(status);
        self
    }

    /// Associated SSID, when available.
    pub const fn connected_ssid(self) -> Option<WifiSsidSummary> {
        self.connected_ssid
    }
}
