//! Power-loss-safe bounded network configuration storage.
//!
//! The physical format owns exactly two 4 KiB erase sectors. Every generation
//! is a complete, device-bound snapshot written to the inactive sector. Its
//! protected prefix and SHA-256 digest are programmed and read back before an
//! irregular commit marker is programmed last. A mount is always read-only:
//! exactly erased media requires explicit [`provision_erased`], while
//! programmed media without a valid committed snapshot fails closed.
//!
//! The semantic model owns four WPA2-Personal access points, one optional
//! outbound IPv4-or-DNS Reticulum TCP peer, explicit transport and announcement
//! policy, an optional phone-supplied fixed-point location, and one atomic LoRa
//! radio profile. Version-1 through version-3 snapshots remain readable with
//! explicit compatibility defaults; every new snapshot uses semantic version
//! 4, whose extension bytes are decoded only after the committed semantic
//! version selects that layout.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![allow(
    clippy::large_enum_variant,
    reason = "allocation-free mount selection temporarily owns complete secret-bearing snapshots"
)]

use core::num::NonZeroU64;

use embedded_storage::nor_flash::{ErrorType, MultiwriteNorFlash, NorFlash, ReadNorFlash};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;

/// Exact raw-NOR partition length required by physical format version 1.
pub const PARTITION_SIZE: usize = 8_192;
/// Size of either alternating snapshot sector.
pub const SECTOR_SIZE: usize = 4_096;
/// Current on-flash physical format version.
pub const PHYSICAL_FORMAT_VERSION: u16 = 1;
/// Current semantic configuration version written by every mutation.
pub const SEMANTIC_FORMAT_VERSION: u16 = 4;
/// Oldest semantic configuration version accepted by the reader.
pub const MIN_SUPPORTED_SEMANTIC_FORMAT_VERSION: u16 = 1;
/// Maximum number of saved WPA2 access-point profiles.
pub const WIFI_PROFILE_CAPACITY: usize = 4;
/// Maximum IEEE 802.11 SSID length in bytes.
pub const MAX_SSID_LENGTH: usize = 32;
/// Minimum WPA2-Personal passphrase length in bytes.
pub const MIN_WPA2_PASSWORD_LENGTH: usize = 8;
/// Maximum WPA2-Personal passphrase length in bytes.
pub const MAX_WPA2_PASSWORD_LENGTH: usize = 63;
/// Maximum retained ASCII DNS hostname length in bytes.
pub const MAX_DNS_HOSTNAME_LENGTH: usize = 96;
/// Default Reticulum TCP interface port.
pub const DEFAULT_RETICULUM_TCP_PORT: u16 = 4_242;
/// Smallest accepted latitude in signed millionths of a degree.
pub const MIN_LATITUDE_E6: i32 = -90_000_000;
/// Largest accepted latitude in signed millionths of a degree.
pub const MAX_LATITUDE_E6: i32 = 90_000_000;
/// Smallest accepted longitude in signed millionths of a degree.
pub const MIN_LONGITUDE_E6: i32 = -180_000_000;
/// Largest accepted longitude in signed millionths of a degree.
pub const MAX_LONGITUDE_E6: i32 = 180_000_000;

const HEADER_SIZE: usize = 128;
const WIFI_SLOT_SIZE: usize = 128;
const TCP_PEER_SLOT_SIZE: usize = 64;
const WIFI_SLOTS_OFFSET: usize = HEADER_SIZE;
const TCP_PEER_OFFSET: usize = WIFI_SLOTS_OFFSET + WIFI_PROFILE_CAPACITY * WIFI_SLOT_SIZE;
const PAYLOAD_RESERVED_OFFSET: usize = TCP_PEER_OFFSET + TCP_PEER_SLOT_SIZE;
const V2_EXTENSION_OFFSET: usize = PAYLOAD_RESERVED_OFFSET;
const V2_POLICY_FLAGS_OFFSET: usize = V2_EXTENSION_OFFSET;
const V2_LOCATION_PRESENT_OFFSET: usize = V2_EXTENSION_OFFSET + 1;
const V2_HOSTNAME_LENGTH_OFFSET: usize = V2_EXTENSION_OFFSET + 2;
const V2_EXTENSION_RESERVED_OFFSET: usize = V2_EXTENSION_OFFSET + 3;
const V3_LORA_TX_POWER_DBM_OFFSET: usize = V2_EXTENSION_RESERVED_OFFSET;
const V2_LATITUDE_OFFSET: usize = V2_EXTENSION_OFFSET + 4;
const V2_LONGITUDE_OFFSET: usize = V2_EXTENSION_OFFSET + 8;
const V2_HOSTNAME_OFFSET: usize = V2_EXTENSION_OFFSET + 12;
const V2_HOSTNAME_END: usize = V2_HOSTNAME_OFFSET + MAX_DNS_HOSTNAME_LENGTH;
const V4_LORA_FREQUENCY_HZ_OFFSET: usize = V2_HOSTNAME_END;
const V4_LORA_BANDWIDTH_HZ_OFFSET: usize = V4_LORA_FREQUENCY_HZ_OFFSET + 4;
const V4_LORA_SPREADING_FACTOR_OFFSET: usize = V4_LORA_BANDWIDTH_HZ_OFFSET + 4;
const V4_LORA_CODING_RATE_DENOMINATOR_OFFSET: usize = V4_LORA_SPREADING_FACTOR_OFFSET + 1;
const V4_LORA_PROFILE_END: usize = V4_LORA_CODING_RATE_DENOMINATOR_OFFSET + 1;
const PROTECTED_SIZE: usize = 1_024;
const DIGEST_OFFSET: usize = PROTECTED_SIZE;
const DIGEST_SIZE: usize = 32;
const COMMIT_OFFSET: usize = DIGEST_OFFSET + DIGEST_SIZE;
const COMMIT_SIZE: usize = 32;
const RECORD_SIZE: usize = COMMIT_OFFSET + COMMIT_SIZE;
const INSPECTION_CHUNK_SIZE: usize = 256;

const MAGIC: &[u8; 8] = b"RTNETC01";
const HEADER_FLAG_TCP_PEER_PRESENT: u8 = 1 << 0;
const TCP_ADDRESS_FAMILY_IPV4: u8 = 4;
const TCP_ADDRESS_FAMILY_DNS: u8 = 16;
const V2_POLICY_WIFI_TRANSPORT_ENABLED: u8 = 1 << 0;
const V2_POLICY_AUTOMATIC_ANNOUNCES_ENABLED: u8 = 1 << 1;
const V2_POLICY_RMAP_DISCOVERY_ENABLED: u8 = 1 << 2;
const V2_POLICY_RMAP_SHARE_LOCATION: u8 = 1 << 3;
const V2_POLICY_VALID_MASK: u8 = V2_POLICY_WIFI_TRANSPORT_ENABLED
    | V2_POLICY_AUTOMATIC_ANNOUNCES_ENABLED
    | V2_POLICY_RMAP_DISCOVERY_ENABLED
    | V2_POLICY_RMAP_SHARE_LOCATION;
const ZERO_DIGEST: [u8; DIGEST_SIZE] = [0; DIGEST_SIZE];

// Keep this domain exactly one SHA-256 block. A final public block overwrites
// sha2's internal buffer after the last secret-bearing protected block.
const DIGEST_DOMAIN: [u8; 64] =
    *b"reticulum-rs-firmware/network-config-store/snapshot/v1\0_________";
const DIGEST_FLUSH_TRAILER: [u8; 64] = [0xa5; 64];
const COMMIT_MARKER: [u8; COMMIT_SIZE] = [
    0x8f, 0x2c, 0x61, 0xd7, 0x35, 0xa9, 0x04, 0xeb, 0x72, 0x18, 0xc6, 0x4d, 0xb3, 0x59, 0x0e, 0xf1,
    0x47, 0x9a, 0x25, 0xdc, 0x63, 0x10, 0xbe, 0x84, 0x3a, 0xf5, 0x6b, 0x01, 0xd2, 0x78, 0x4c, 0x96,
];

const _: () = assert!(PARTITION_SIZE == 2 * SECTOR_SIZE);
const _: () = assert!(TCP_PEER_OFFSET == 640);
const _: () = assert!(PAYLOAD_RESERVED_OFFSET == 704);
const _: () = assert!(V2_EXTENSION_RESERVED_OFFSET == 707);
const _: () = assert!(V3_LORA_TX_POWER_DBM_OFFSET == 707);
const _: () = assert!(V2_HOSTNAME_OFFSET == 716);
const _: () = assert!(V2_HOSTNAME_END == 812);
const _: () = assert!(V4_LORA_PROFILE_END == 822);
const _: () = assert!(PAYLOAD_RESERVED_OFFSET < PROTECTED_SIZE);
const _: () = assert!(V2_HOSTNAME_END < PROTECTED_SIZE);
const _: () = assert!(COMMIT_OFFSET + COMMIT_SIZE == RECORD_SIZE);
const _: () =
    assert!((DIGEST_DOMAIN.len() + PROTECTED_SIZE + DIGEST_FLUSH_TRAILER.len()).is_multiple_of(64));

/// Stable opaque identifier for one saved access-point profile.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct WifiProfileId([u8; 16]);

impl WifiProfileId {
    /// Construct an identifier, rejecting the reserved all-zero value.
    pub fn new(bytes: [u8; 16]) -> Result<Self, NetworkConfigModelError> {
        if bytes == [0; 16] {
            return Err(NetworkConfigModelError::ZeroWifiProfileId);
        }
        Ok(Self(bytes))
    }

    /// Borrow the exact opaque identifier bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Validation failure while constructing or modifying semantic configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkConfigModelError {
    /// The all-zero access-point identifier is reserved.
    ZeroWifiProfileId,
    /// SSID length must be between one and 32 bytes.
    InvalidSsidLength,
    /// WPA2-Personal passphrase length must be between 8 and 63 bytes.
    InvalidWpa2PasswordLength,
    /// WPA2-Personal passphrases must contain only printable ASCII bytes.
    InvalidWpa2PasswordCharacter,
    /// A profile with the same opaque identifier already exists.
    DuplicateWifiProfileId,
    /// All four access-point slots are occupied.
    WifiProfileCapacityExceeded,
    /// TCP port zero is not a valid configured peer port.
    InvalidTcpPort,
    /// The `0.0.0.0/8` current-network range cannot name a remote TCP peer.
    CurrentNetworkTcpPeerAddress,
    /// The `127.0.0.0/8` loopback range cannot name a remote appliance peer.
    LoopbackTcpPeerAddress,
    /// IPv4 multicast cannot name a TCP peer.
    MulticastTcpPeerAddress,
    /// `255.255.255.255` cannot name a remote TCP peer.
    LimitedBroadcastTcpPeerAddress,
    /// The `240.0.0.0/4` reserved range cannot name a routable TCP peer.
    ReservedTcpPeerAddress,
    /// A DNS hostname must contain between one and 96 ASCII bytes.
    InvalidDnsHostnameLength,
    /// A DNS hostname contains a byte outside letters, digits, hyphen, and dot.
    InvalidDnsHostnameCharacter,
    /// A DNS label is empty, longer than 63 bytes, or begins or ends with a hyphen.
    InvalidDnsHostnameLabel,
    /// Latitude must be between -90 and 90 degrees, in signed millionths.
    InvalidLatitude,
    /// Longitude must be between -180 and 180 degrees, in signed millionths.
    InvalidLongitude,
    /// LoRa transmit power must be one of 14, 17, 20, or 22 dBm.
    InvalidLoraTxPower,
    /// A LoRa center frequency must be nonzero.
    InvalidLoraFrequency,
    /// LoRa bandwidth must be one of the SX1262/RNode canonical widths.
    InvalidLoraBandwidth,
    /// LoRa spreading factor must be between 7 and 12.
    InvalidLoraSpreadingFactor,
    /// LoRa coding-rate denominator must be between 5 and 8.
    InvalidLoraCodingRate,
}

/// Bounded LoRa transmit-power selection persisted as whole dBm.
///
/// These are the four optimal SX1262 high-power PA operating points. Arbitrary
/// intermediate values are rejected instead of being rounded or clamped.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum LoraTxPower {
    /// Requested +14 dBm output and the compatibility default.
    #[default]
    Dbm14,
    /// Requested +17 dBm output.
    Dbm17,
    /// Requested +20 dBm output.
    Dbm20,
    /// Requested +22 dBm output.
    Dbm22,
}

impl LoraTxPower {
    /// Validate a numeric transmit-power selection.
    pub const fn try_from_dbm(requested_dbm: i32) -> Result<Self, NetworkConfigModelError> {
        match requested_dbm {
            14 => Ok(Self::Dbm14),
            17 => Ok(Self::Dbm17),
            20 => Ok(Self::Dbm20),
            22 => Ok(Self::Dbm22),
            _ => Err(NetworkConfigModelError::InvalidLoraTxPower),
        }
    }

    /// Requested LoRa output power in whole dBm.
    pub const fn requested_dbm(self) -> i32 {
        match self {
            Self::Dbm14 => 14,
            Self::Dbm17 => 17,
            Self::Dbm20 => 20,
            Self::Dbm22 => 22,
        }
    }

    const fn encoded_dbm(self) -> u8 {
        match self {
            Self::Dbm14 => 14,
            Self::Dbm17 => 17,
            Self::Dbm20 => 20,
            Self::Dbm22 => 22,
        }
    }
}

impl TryFrom<i32> for LoraTxPower {
    type Error = NetworkConfigModelError;

    fn try_from(requested_dbm: i32) -> Result<Self, Self::Error> {
        Self::try_from_dbm(requested_dbm)
    }
}

/// Atomic, transport-independent LoRa compatibility profile.
///
/// This persistent model validates numeric shape only. A product board must
/// additionally validate the occupied channel, fitted RF path, and driver
/// interoperability before committing a mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoraRadioProfile {
    frequency_hz: u32,
    bandwidth_hz: u32,
    spreading_factor: u8,
    coding_rate_denominator: u8,
    tx_power: LoraTxPower,
}

impl LoraRadioProfile {
    /// Current backward-compatible NA915 profile used for legacy snapshots.
    pub const DEFAULT: Self = Self {
        frequency_hz: 915_000_000,
        bandwidth_hz: 125_000,
        spreading_factor: 7,
        coding_rate_denominator: 5,
        tx_power: LoraTxPower::Dbm14,
    };

    /// Validate and construct one complete persisted profile.
    pub const fn new(
        frequency_hz: u32,
        bandwidth_hz: u32,
        spreading_factor: u8,
        coding_rate_denominator: u8,
        tx_power: LoraTxPower,
    ) -> Result<Self, NetworkConfigModelError> {
        if frequency_hz == 0 {
            return Err(NetworkConfigModelError::InvalidLoraFrequency);
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
            return Err(NetworkConfigModelError::InvalidLoraBandwidth);
        }
        if spreading_factor < 7 || spreading_factor > 12 {
            return Err(NetworkConfigModelError::InvalidLoraSpreadingFactor);
        }
        if coding_rate_denominator < 5 || coding_rate_denominator > 8 {
            return Err(NetworkConfigModelError::InvalidLoraCodingRate);
        }
        Ok(Self {
            frequency_hz,
            bandwidth_hz,
            spreading_factor,
            coding_rate_denominator,
            tx_power,
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

    /// Denominator of the LoRa coding rate `4/n`.
    pub const fn coding_rate_denominator(self) -> u8 {
        self.coding_rate_denominator
    }

    /// Requested radio output power.
    pub const fn tx_power(self) -> LoraTxPower {
        self.tx_power
    }

    const fn with_tx_power(mut self, tx_power: LoraTxPower) -> Self {
        self.tx_power = tx_power;
        self
    }
}

impl Default for LoraRadioProfile {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Secret-bearing WPA2-Personal station profile.
///
/// The type intentionally implements neither `Clone`, `Copy`, nor `Debug`.
/// Dropping it zeroizes the retained passphrase.
pub struct WifiProfile {
    id: WifiProfileId,
    ssid: [u8; MAX_SSID_LENGTH],
    ssid_length: u8,
    password: [u8; MAX_WPA2_PASSWORD_LENGTH],
    password_length: u8,
    enabled: bool,
    priority: u8,
}

impl WifiProfile {
    /// Construct one bounded WPA2-Personal station profile.
    pub fn new(
        id: WifiProfileId,
        ssid: &[u8],
        password: &[u8],
        enabled: bool,
        priority: u8,
    ) -> Result<Self, NetworkConfigModelError> {
        if ssid.is_empty() || ssid.len() > MAX_SSID_LENGTH {
            return Err(NetworkConfigModelError::InvalidSsidLength);
        }
        if !(MIN_WPA2_PASSWORD_LENGTH..=MAX_WPA2_PASSWORD_LENGTH).contains(&password.len()) {
            return Err(NetworkConfigModelError::InvalidWpa2PasswordLength);
        }
        if !password.iter().all(|byte| (0x20..=0x7e).contains(byte)) {
            return Err(NetworkConfigModelError::InvalidWpa2PasswordCharacter);
        }
        let mut stored_ssid = [0_u8; MAX_SSID_LENGTH];
        stored_ssid[..ssid.len()].copy_from_slice(ssid);
        let mut stored_password = [0_u8; MAX_WPA2_PASSWORD_LENGTH];
        stored_password[..password.len()].copy_from_slice(password);
        Ok(Self {
            id,
            ssid: stored_ssid,
            ssid_length: ssid.len() as u8,
            password: stored_password,
            password_length: password.len() as u8,
            enabled,
            priority,
        })
    }

    /// Opaque stable identifier used for updates and deletion.
    pub const fn id(&self) -> WifiProfileId {
        self.id
    }

    /// Exact SSID bytes.
    pub fn ssid(&self) -> &[u8] {
        &self.ssid[..usize::from(self.ssid_length)]
    }

    /// Secret passphrase bytes for the Wi-Fi station integration.
    pub fn password(&self) -> &[u8] {
        &self.password[..usize::from(self.password_length)]
    }

    /// Whether connection attempts are enabled for this profile.
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// Caller-defined selection priority. Larger values have higher priority.
    pub const fn priority(&self) -> u8 {
        self.priority
    }
}

impl Zeroize for WifiProfile {
    fn zeroize(&mut self) {
        self.password.zeroize();
        self.password_length.zeroize();
    }
}

impl ZeroizeOnDrop for WifiProfile {}

impl Drop for WifiProfile {
    fn drop(&mut self) {
        self.zeroize();
    }
}

/// Bounded canonical ASCII DNS hostname.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DnsHostname {
    bytes: [u8; MAX_DNS_HOSTNAME_LENGTH],
    length: u8,
}

impl DnsHostname {
    /// Validate and retain one DNS hostname without allocating.
    pub fn new(hostname: &[u8]) -> Result<Self, NetworkConfigModelError> {
        if hostname.is_empty() || hostname.len() > MAX_DNS_HOSTNAME_LENGTH {
            return Err(NetworkConfigModelError::InvalidDnsHostnameLength);
        }
        if !hostname
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
        {
            return Err(NetworkConfigModelError::InvalidDnsHostnameCharacter);
        }
        for label in hostname.split(|byte| *byte == b'.') {
            if label.is_empty()
                || label.len() > 63
                || label.first() == Some(&b'-')
                || label.last() == Some(&b'-')
            {
                return Err(NetworkConfigModelError::InvalidDnsHostnameLabel);
            }
        }
        let mut bytes = [0_u8; MAX_DNS_HOSTNAME_LENGTH];
        bytes[..hostname.len()].copy_from_slice(hostname);
        Ok(Self {
            bytes,
            length: hostname.len() as u8,
        })
    }

    /// Exact configured ASCII hostname bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.length)]
    }
}

/// Address of one outbound Reticulum TCP peer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutboundTcpPeerAddress {
    /// IPv4 address in network byte order.
    Ipv4([u8; 4]),
    /// Bounded ASCII DNS hostname resolved by the transport integration.
    Dns(DnsHostname),
}

/// One optional outbound Reticulum TCP peer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutboundTcpPeer {
    address: OutboundTcpPeerAddress,
    port: u16,
    enabled: bool,
}

impl OutboundTcpPeer {
    /// Construct a configured IPv4 peer.
    pub fn new(ipv4: [u8; 4], port: u16, enabled: bool) -> Result<Self, NetworkConfigModelError> {
        if port == 0 {
            return Err(NetworkConfigModelError::InvalidTcpPort);
        }
        validate_ipv4_peer(ipv4)?;
        Self::from_address(OutboundTcpPeerAddress::Ipv4(ipv4), port, enabled)
    }

    /// Construct a configured DNS peer.
    pub fn with_dns_hostname(
        hostname: &[u8],
        port: u16,
        enabled: bool,
    ) -> Result<Self, NetworkConfigModelError> {
        Self::from_address(
            OutboundTcpPeerAddress::Dns(DnsHostname::new(hostname)?),
            port,
            enabled,
        )
    }

    /// Construct a peer using Reticulum's conventional TCP port 4242.
    pub fn with_default_port(
        ipv4: [u8; 4],
        enabled: bool,
    ) -> Result<Self, NetworkConfigModelError> {
        Self::new(ipv4, DEFAULT_RETICULUM_TCP_PORT, enabled)
    }

    /// Construct a DNS peer using Reticulum's conventional TCP port 4242.
    pub fn dns_with_default_port(
        hostname: &[u8],
        enabled: bool,
    ) -> Result<Self, NetworkConfigModelError> {
        Self::with_dns_hostname(hostname, DEFAULT_RETICULUM_TCP_PORT, enabled)
    }

    fn from_address(
        address: OutboundTcpPeerAddress,
        port: u16,
        enabled: bool,
    ) -> Result<Self, NetworkConfigModelError> {
        if port == 0 {
            return Err(NetworkConfigModelError::InvalidTcpPort);
        }
        Ok(Self {
            address,
            port,
            enabled,
        })
    }

    /// Configured peer address.
    pub const fn address(&self) -> OutboundTcpPeerAddress {
        self.address
    }

    /// Configured IPv4 address, or `None` for a DNS peer.
    pub const fn ipv4(&self) -> Option<[u8; 4]> {
        match self.address {
            OutboundTcpPeerAddress::Ipv4(ipv4) => Some(ipv4),
            OutboundTcpPeerAddress::Dns(_) => None,
        }
    }

    /// Configured DNS hostname, or `None` for an IPv4 peer.
    pub fn dns_hostname(&self) -> Option<&[u8]> {
        match &self.address {
            OutboundTcpPeerAddress::Dns(hostname) => Some(hostname.as_bytes()),
            OutboundTcpPeerAddress::Ipv4(_) => None,
        }
    }

    /// Configured TCP port.
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// Whether the outbound interface is enabled.
    pub const fn enabled(&self) -> bool {
        self.enabled
    }
}

fn validate_ipv4_peer(ipv4: [u8; 4]) -> Result<(), NetworkConfigModelError> {
    if ipv4[0] == 0 {
        return Err(NetworkConfigModelError::CurrentNetworkTcpPeerAddress);
    }
    if ipv4[0] == 127 {
        return Err(NetworkConfigModelError::LoopbackTcpPeerAddress);
    }
    if (224..=239).contains(&ipv4[0]) {
        return Err(NetworkConfigModelError::MulticastTcpPeerAddress);
    }
    if ipv4 == [255, 255, 255, 255] {
        return Err(NetworkConfigModelError::LimitedBroadcastTcpPeerAddress);
    }
    if ipv4[0] >= 240 {
        return Err(NetworkConfigModelError::ReservedTcpPeerAddress);
    }
    Ok(())
}

/// Phone-supplied position represented as signed millionths of a degree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhoneLocation {
    latitude_e6: i32,
    longitude_e6: i32,
}

impl PhoneLocation {
    /// Validate and construct a phone-supplied position.
    pub fn new(latitude_e6: i32, longitude_e6: i32) -> Result<Self, NetworkConfigModelError> {
        if !(MIN_LATITUDE_E6..=MAX_LATITUDE_E6).contains(&latitude_e6) {
            return Err(NetworkConfigModelError::InvalidLatitude);
        }
        if !(MIN_LONGITUDE_E6..=MAX_LONGITUDE_E6).contains(&longitude_e6) {
            return Err(NetworkConfigModelError::InvalidLongitude);
        }
        Ok(Self {
            latitude_e6,
            longitude_e6,
        })
    }

    /// Latitude in signed millionths of a degree.
    pub const fn latitude_e6(self) -> i32 {
        self.latitude_e6
    }

    /// Longitude in signed millionths of a degree.
    pub const fn longitude_e6(self) -> i32 {
        self.longitude_e6
    }
}

/// Complete bounded network configuration.
///
/// This owner contains WPA2 passphrases and intentionally implements neither
/// `Clone`, `Copy`, nor `Debug`.
pub struct NetworkConfig {
    wifi_profiles: [Option<WifiProfile>; WIFI_PROFILE_CAPACITY],
    tcp_peer: Option<OutboundTcpPeer>,
    wifi_transport_enabled: bool,
    automatic_announces_enabled: bool,
    rmap_discovery_enabled: bool,
    rmap_share_location: bool,
    phone_location: Option<PhoneLocation>,
    lora_profile: LoraRadioProfile,
}

impl NetworkConfig {
    /// Construct an empty configuration using backward-compatible policy defaults.
    pub const fn empty() -> Self {
        Self {
            wifi_profiles: [None, None, None, None],
            tcp_peer: None,
            wifi_transport_enabled: true,
            automatic_announces_enabled: true,
            rmap_discovery_enabled: false,
            rmap_share_location: false,
            phone_location: None,
            lora_profile: LoraRadioProfile::DEFAULT,
        }
    }

    /// Number of retained Wi-Fi profiles.
    pub fn wifi_profile_count(&self) -> usize {
        self.wifi_profiles.iter().flatten().count()
    }

    /// Iterate over secret-bearing Wi-Fi profiles for station integration.
    pub fn wifi_profiles(&self) -> impl Iterator<Item = &WifiProfile> {
        self.wifi_profiles.iter().flatten()
    }

    /// Insert a new profile or replace the profile with the same opaque ID.
    pub fn upsert_wifi_profile(
        &mut self,
        profile: WifiProfile,
    ) -> Result<(), NetworkConfigModelError> {
        if let Some(existing) = self.wifi_profiles.iter_mut().find(|candidate| {
            candidate
                .as_ref()
                .is_some_and(|value| value.id == profile.id)
        }) {
            *existing = Some(profile);
            return Ok(());
        }
        let Some(vacant) = self
            .wifi_profiles
            .iter_mut()
            .find(|candidate| candidate.is_none())
        else {
            return Err(NetworkConfigModelError::WifiProfileCapacityExceeded);
        };
        *vacant = Some(profile);
        Ok(())
    }

    /// Insert a profile only if its opaque ID is not already present.
    pub fn insert_wifi_profile(
        &mut self,
        profile: WifiProfile,
    ) -> Result<(), NetworkConfigModelError> {
        if self
            .wifi_profiles
            .iter()
            .flatten()
            .any(|existing| existing.id == profile.id)
        {
            return Err(NetworkConfigModelError::DuplicateWifiProfileId);
        }
        self.upsert_wifi_profile(profile)
    }

    /// Remove and return one profile by opaque ID.
    pub fn remove_wifi_profile(&mut self, id: WifiProfileId) -> Option<WifiProfile> {
        let slot = self
            .wifi_profiles
            .iter_mut()
            .find(|candidate| candidate.as_ref().is_some_and(|value| value.id == id))?;
        slot.take()
    }

    /// Current outbound Reticulum TCP peer, if configured.
    pub const fn tcp_peer(&self) -> Option<OutboundTcpPeer> {
        self.tcp_peer
    }

    /// Replace or clear the outbound Reticulum TCP peer.
    pub fn set_tcp_peer(&mut self, peer: Option<OutboundTcpPeer>) {
        self.tcp_peer = peer;
    }

    /// Whether the Wi-Fi transport is globally enabled.
    pub const fn wifi_transport_enabled(&self) -> bool {
        self.wifi_transport_enabled
    }

    /// Enable or disable the Wi-Fi transport without deleting saved profiles.
    pub fn set_wifi_transport_enabled(&mut self, enabled: bool) {
        self.wifi_transport_enabled = enabled;
    }

    /// Whether routine Reticulum service announces are enabled.
    pub const fn automatic_announces_enabled(&self) -> bool {
        self.automatic_announces_enabled
    }

    /// Enable or disable routine Reticulum service announces.
    pub fn set_automatic_announces_enabled(&mut self, enabled: bool) {
        self.automatic_announces_enabled = enabled;
    }

    /// Whether opt-in RMAP interface discovery is enabled.
    pub const fn rmap_discovery_enabled(&self) -> bool {
        self.rmap_discovery_enabled
    }

    /// Enable or disable opt-in RMAP interface discovery.
    pub fn set_rmap_discovery_enabled(&mut self, enabled: bool) {
        self.rmap_discovery_enabled = enabled;
    }

    /// Whether RMAP discovery may include the retained phone location.
    pub const fn rmap_share_location(&self) -> bool {
        self.rmap_share_location
    }

    /// Enable or disable including location in RMAP discovery.
    pub fn set_rmap_share_location(&mut self, enabled: bool) {
        self.rmap_share_location = enabled;
    }

    /// Most recently retained phone-supplied location, if any.
    pub const fn phone_location(&self) -> Option<PhoneLocation> {
        self.phone_location
    }

    /// Replace or clear the phone-supplied location.
    pub fn set_phone_location(&mut self, location: Option<PhoneLocation>) {
        self.phone_location = location;
    }

    /// Selected LoRa transmit power.
    pub const fn lora_tx_power(&self) -> LoraTxPower {
        self.lora_profile.tx_power()
    }

    /// Replace the bounded LoRa transmit-power selection.
    pub fn set_lora_tx_power(&mut self, power: LoraTxPower) {
        self.lora_profile = self.lora_profile.with_tx_power(power);
    }

    /// Complete LoRa profile saved for the next boot.
    pub const fn lora_profile(&self) -> LoraRadioProfile {
        self.lora_profile
    }

    /// Atomically replace every LoRa compatibility field.
    pub fn set_lora_profile(&mut self, profile: LoraRadioProfile) {
        self.lora_profile = profile;
    }

    /// Produce a copy-safe projection that excludes every passphrase byte.
    pub fn redacted(&self) -> RedactedNetworkConfig {
        let mut profiles = [None; WIFI_PROFILE_CAPACITY];
        for (destination, source) in profiles.iter_mut().zip(self.wifi_profiles()) {
            *destination = Some(RedactedWifiProfile::from_secret(source));
        }
        RedactedNetworkConfig {
            wifi_profiles: profiles,
            tcp_peer: self.tcp_peer,
            wifi_transport_enabled: self.wifi_transport_enabled,
            automatic_announces_enabled: self.automatic_announces_enabled,
            rmap_discovery_enabled: self.rmap_discovery_enabled,
            rmap_share_location: self.rmap_share_location,
            phone_location: self.phone_location,
            lora_profile: self.lora_profile,
        }
    }
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self::empty()
    }
}

/// One passphrase-free Wi-Fi profile projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RedactedWifiProfile {
    id: WifiProfileId,
    ssid: [u8; MAX_SSID_LENGTH],
    ssid_length: u8,
    enabled: bool,
    priority: u8,
    password_configured: bool,
}

impl RedactedWifiProfile {
    fn from_secret(profile: &WifiProfile) -> Self {
        Self {
            id: profile.id,
            ssid: profile.ssid,
            ssid_length: profile.ssid_length,
            enabled: profile.enabled,
            priority: profile.priority,
            password_configured: profile.password_length != 0,
        }
    }

    /// Opaque stable identifier.
    pub const fn id(&self) -> WifiProfileId {
        self.id
    }

    /// Exact SSID bytes.
    pub fn ssid(&self) -> &[u8] {
        &self.ssid[..usize::from(self.ssid_length)]
    }

    /// Whether a passphrase is retained, without exposing its bytes or length.
    pub const fn password_configured(&self) -> bool {
        self.password_configured
    }

    /// Whether connection attempts are enabled.
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// Caller-defined connection priority.
    pub const fn priority(&self) -> u8 {
        self.priority
    }
}

/// Passphrase-free projection safe for authenticated management responses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RedactedNetworkConfig {
    wifi_profiles: [Option<RedactedWifiProfile>; WIFI_PROFILE_CAPACITY],
    tcp_peer: Option<OutboundTcpPeer>,
    wifi_transport_enabled: bool,
    automatic_announces_enabled: bool,
    rmap_discovery_enabled: bool,
    rmap_share_location: bool,
    phone_location: Option<PhoneLocation>,
    lora_profile: LoraRadioProfile,
}

impl RedactedNetworkConfig {
    /// Iterate over configured passphrase-free access-point projections.
    pub fn wifi_profiles(&self) -> impl Iterator<Item = &RedactedWifiProfile> {
        self.wifi_profiles.iter().flatten()
    }

    /// Current outbound Reticulum TCP peer, if configured.
    pub const fn tcp_peer(&self) -> Option<OutboundTcpPeer> {
        self.tcp_peer
    }

    /// Whether the Wi-Fi transport is globally enabled.
    pub const fn wifi_transport_enabled(&self) -> bool {
        self.wifi_transport_enabled
    }

    /// Whether routine Reticulum service announces are enabled.
    pub const fn automatic_announces_enabled(&self) -> bool {
        self.automatic_announces_enabled
    }

    /// Whether opt-in RMAP interface discovery is enabled.
    pub const fn rmap_discovery_enabled(&self) -> bool {
        self.rmap_discovery_enabled
    }

    /// Whether RMAP discovery may include the retained phone location.
    pub const fn rmap_share_location(&self) -> bool {
        self.rmap_share_location
    }

    /// Most recently retained phone-supplied location, if any.
    pub const fn phone_location(&self) -> Option<PhoneLocation> {
        self.phone_location
    }

    /// Selected LoRa transmit power.
    pub const fn lora_tx_power(&self) -> LoraTxPower {
        self.lora_profile.tx_power()
    }

    /// Complete LoRa profile saved for the next boot.
    pub const fn lora_profile(&self) -> LoraRadioProfile {
        self.lora_profile
    }
}

/// Stable physical identity of the flash device containing configuration.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct NetworkConfigStoreDeviceId([u8; 16]);

impl NetworkConfigStoreDeviceId {
    /// Construct a device identity from exact product-supplied bytes.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Borrow the exact device identity bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Exact physical provenance and format represented by one backend view.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct NetworkConfigStoreBinding {
    device: NetworkConfigStoreDeviceId,
    absolute_offset: usize,
    length: usize,
    format_version: u16,
}

impl NetworkConfigStoreBinding {
    /// Construct one operation-scoped store binding.
    pub const fn new(
        device: NetworkConfigStoreDeviceId,
        absolute_offset: usize,
        length: usize,
        format_version: u16,
    ) -> Self {
        Self {
            device,
            absolute_offset,
            length,
            format_version,
        }
    }

    /// Physical device containing this store.
    pub const fn device(self) -> NetworkConfigStoreDeviceId {
        self.device
    }

    /// Absolute byte offset in the containing device.
    pub const fn absolute_offset(self) -> usize {
        self.absolute_offset
    }

    /// Exact bound range length.
    pub const fn length(self) -> usize {
        self.length
    }

    /// Expected physical format version.
    pub const fn format_version(self) -> u16 {
        self.format_version
    }
}

/// A partition-relative backend carrying exact configuration-store provenance.
pub struct BoundNetworkConfigStore<F> {
    backend: F,
    binding: NetworkConfigStoreBinding,
}

impl<F> BoundNetworkConfigStore<F> {
    /// Attach binding provenance to one range-restricted backend.
    pub const fn new(backend: F, binding: NetworkConfigStoreBinding) -> Self {
        Self { backend, binding }
    }

    /// Binding carried by this view.
    pub const fn binding(&self) -> NetworkConfigStoreBinding {
        self.binding
    }

    /// Borrow the wrapped backend.
    pub const fn backend(&self) -> &F {
        &self.backend
    }

    /// Mutably borrow the wrapped backend.
    pub fn backend_mut(&mut self) -> &mut F {
        &mut self.backend
    }

    /// Consume this view and recover the backend.
    pub fn into_backend(self) -> F {
        self.backend
    }
}

impl<F: ErrorType> ErrorType for BoundNetworkConfigStore<F> {
    type Error = F::Error;
}

impl<F: ReadNorFlash> ReadNorFlash for BoundNetworkConfigStore<F> {
    const READ_SIZE: usize = F::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.backend.read(offset, bytes)
    }

    fn capacity(&self) -> usize {
        self.backend.capacity()
    }
}

impl<F: NorFlash> NorFlash for BoundNetworkConfigStore<F> {
    const WRITE_SIZE: usize = F::WRITE_SIZE;
    const ERASE_SIZE: usize = F::ERASE_SIZE;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.backend.write(offset, bytes)
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.backend.erase(from, to)
    }
}

impl<F: MultiwriteNorFlash> MultiwriteNorFlash for BoundNetworkConfigStore<F> {}

/// Read-only access carrying exact store provenance.
pub trait BoundNetworkConfigStoreReadAccess: ReadNorFlash {
    /// Binding represented by this operation-scoped view.
    fn network_config_store_binding(&self) -> NetworkConfigStoreBinding;
}

impl<F: ReadNorFlash> BoundNetworkConfigStoreReadAccess for BoundNetworkConfigStore<F> {
    fn network_config_store_binding(&self) -> NetworkConfigStoreBinding {
        self.binding
    }
}

/// Writable raw-NOR access carrying exact store provenance.
pub trait BoundNetworkConfigStoreAccess:
    BoundNetworkConfigStoreReadAccess + MultiwriteNorFlash
{
}

impl<F: MultiwriteNorFlash> BoundNetworkConfigStoreAccess for BoundNetworkConfigStore<F> {}

/// Binding validation failure detected before any store I/O.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkConfigStoreBindingError {
    /// A later operation names a different physical device.
    DeviceMismatch {
        /// Device retained by mounted state.
        expected: NetworkConfigStoreDeviceId,
        /// Device supplied by the later view.
        actual: NetworkConfigStoreDeviceId,
    },
    /// A later operation names a different absolute range.
    RangeMismatch {
        /// Retained absolute start.
        expected_offset: usize,
        /// Retained length.
        expected_length: usize,
        /// Supplied absolute start.
        actual_offset: usize,
        /// Supplied length.
        actual_length: usize,
    },
    /// Absolute start plus length cannot be represented.
    RangeOverflow {
        /// Supplied absolute start.
        absolute_offset: usize,
        /// Supplied range length.
        length: usize,
    },
    /// Binding names a different physical format.
    FormatVersionMismatch {
        /// Required version.
        expected: u16,
        /// Supplied version.
        actual: u16,
    },
    /// Bound range is not exactly 8 KiB.
    LengthMismatch {
        /// Required length.
        expected: usize,
        /// Supplied length.
        actual: usize,
    },
    /// Backend capacity differs from the bound range.
    CapacityMismatch {
        /// Required capacity.
        expected: usize,
        /// Backend capacity.
        actual: usize,
    },
    /// Read geometry cannot represent the bound layout.
    ReadAlignmentMismatch {
        /// Backend read granularity.
        read_size: usize,
    },
    /// Program or erase geometry cannot represent the bound layout.
    WriteAlignmentMismatch {
        /// Backend write granularity.
        write_size: usize,
        /// Backend erase granularity.
        erase_size: usize,
    },
}

/// One of the two alternating physical sectors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkConfigStoreSector {
    /// Sector at partition-relative offset zero.
    A,
    /// Sector at partition-relative offset 4096.
    B,
}

impl NetworkConfigStoreSector {
    const fn id(self) -> u8 {
        match self {
            Self::A => 0,
            Self::B => 1,
        }
    }

    const fn offset(self) -> usize {
        match self {
            Self::A => 0,
            Self::B => SECTOR_SIZE,
        }
    }

    const fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }
}

/// Optional physical cleanup reported by a successful read-only mount.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkConfigStoreCleanup {
    /// Inactive sector is already erased.
    Clean,
    /// Exact non-authoritative sector that may be erased.
    EraseInactive {
        /// Sector safe to erase.
        sector: NetworkConfigStoreSector,
    },
}

/// Stable non-secret media or operation fault.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkConfigStoreFault {
    /// Both sectors are exactly erased and require explicit provisioning.
    UnformattedErased,
    /// No valid committed snapshot exists and programmed data remains.
    UnformattedNonErased,
    /// A committed snapshot uses an unsupported physical format.
    UnsupportedPhysicalVersion(u16),
    /// A committed snapshot uses an unsupported semantic format.
    UnsupportedSemanticVersion(u16),
    /// Committed header names a different physical device or range.
    DeviceBindingMismatch {
        /// Sector containing the mismatched binding.
        sector: NetworkConfigStoreSector,
    },
    /// A committed snapshot failed canonical validation.
    CommittedSnapshotCorrupt {
        /// Sector containing the corruption.
        sector: NetworkConfigStoreSector,
    },
    /// Programmed bytes do not follow a recognized commit trajectory.
    UnknownProgrammedData {
        /// Sector containing unknown data.
        sector: NetworkConfigStoreSector,
    },
    /// Two valid snapshots cannot belong to one linear history.
    SnapshotConflict,
    /// Current generation cannot be advanced safely.
    GenerationExhausted,
    /// Explicit first provisioning was requested after a commit already exists.
    AlreadyProvisioned,
    /// Supplied revision token is no longer the mounted authority.
    StaleRevision,
    /// Exact post-write or post-erase readback did not match.
    ReadbackMismatch {
        /// Sector whose operation failed verification.
        sector: NetworkConfigStoreSector,
    },
    /// Completed operation did not remount as its exact intended successor.
    SuccessorVerificationFailed,
}

/// Failure separated into binding, backend, and stable store faults.
#[derive(Debug, Eq, PartialEq)]
pub enum NetworkConfigStoreError<E> {
    /// Binding rejected before store I/O.
    Binding(NetworkConfigStoreBindingError),
    /// Raw NOR backend failure; completion may be ambiguous.
    Backend(E),
    /// Stable non-secret media or operation fault.
    Fault(NetworkConfigStoreFault),
}

/// Copy-safe authority token used to commit or clean one exact revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NetworkConfigRevision {
    binding: NetworkConfigStoreBinding,
    sector: NetworkConfigStoreSector,
    generation: NonZeroU64,
    digest: [u8; DIGEST_SIZE],
}

impl NetworkConfigRevision {
    /// Exact physical binding retained at mount.
    pub const fn binding(self) -> NetworkConfigStoreBinding {
        self.binding
    }

    /// Sector containing this committed revision.
    pub const fn sector(self) -> NetworkConfigStoreSector {
        self.sector
    }

    /// Nonzero monotonically increasing generation.
    pub const fn generation(self) -> NonZeroU64 {
        self.generation
    }
}

/// Successfully mounted secret-bearing authoritative configuration.
///
/// The type intentionally implements neither `Clone`, `Copy`, nor `Debug`.
#[must_use = "mounted configuration must be deliberately retained or dropped"]
pub struct MountedNetworkConfigStore {
    revision: NetworkConfigRevision,
    configuration: NetworkConfig,
    cleanup: NetworkConfigStoreCleanup,
}

impl MountedNetworkConfigStore {
    /// Copy-safe token naming the exact mounted authority.
    pub const fn revision(&self) -> NetworkConfigRevision {
        self.revision
    }

    /// Secret-bearing configuration for the Wi-Fi station integration.
    pub const fn configuration(&self) -> &NetworkConfig {
        &self.configuration
    }

    /// Passphrase-free projection safe for management responses.
    pub fn redacted(&self) -> RedactedNetworkConfig {
        self.configuration.redacted()
    }

    /// Optional physical cleanup safe for this revision.
    pub const fn cleanup(&self) -> NetworkConfigStoreCleanup {
        self.cleanup
    }

    /// Consume mounted ownership into the mutable semantic configuration.
    pub fn into_configuration(self) -> NetworkConfig {
        self.configuration
    }
}

struct ValidSnapshot {
    revision: NetworkConfigRevision,
    predecessor_generation: u64,
    predecessor_digest: [u8; DIGEST_SIZE],
    configuration: NetworkConfig,
}

enum SectorStatus {
    Erased,
    Uncommitted(NetworkConfigStoreSector),
    Valid(ValidSnapshot),
    Invalid {
        sector: NetworkConfigStoreSector,
        kind: InvalidSectorKind,
    },
}

#[derive(Clone, Copy)]
enum InvalidSectorKind {
    UnsupportedPhysical(u16),
    UnsupportedSemantic(u16),
    WrongBinding,
    CommittedCorrupt,
    Unknown,
}

/// Mount the newest valid committed snapshot without modifying flash.
///
/// Exactly erased media reports [`NetworkConfigStoreFault::UnformattedErased`].
/// A recognizable torn inactive successor may coexist with one valid snapshot
/// and is reported as cleanup. Programmed media without a valid snapshot,
/// committed corruption, unknown bytes, and conflicting histories fail closed.
pub fn mount<A>(
    access: &mut A,
) -> Result<MountedNetworkConfigStore, NetworkConfigStoreError<A::Error>>
where
    A: BoundNetworkConfigStoreReadAccess,
{
    validate_read_binding(access).map_err(NetworkConfigStoreError::Binding)?;
    let binding = access.network_config_store_binding();
    let a = scan_sector(access, binding, NetworkConfigStoreSector::A)
        .map_err(NetworkConfigStoreError::Backend)?;
    let b = scan_sector(access, binding, NetworkConfigStoreSector::B)
        .map_err(NetworkConfigStoreError::Backend)?;
    select(binding, a, b).map_err(NetworkConfigStoreError::Fault)
}

/// Explicitly commit generation one to exactly erased media.
///
/// This never erases or reformats media. Existing committed state returns
/// [`NetworkConfigStoreFault::AlreadyProvisioned`], and any non-erased
/// uncommitted or corrupt media retains its mount fault.
pub fn provision_erased<A>(
    access: &mut A,
    configuration: &NetworkConfig,
) -> Result<MountedNetworkConfigStore, NetworkConfigStoreError<A::Error>>
where
    A: BoundNetworkConfigStoreAccess,
{
    validate_write_binding(access).map_err(NetworkConfigStoreError::Binding)?;
    match mount(access) {
        Err(NetworkConfigStoreError::Fault(NetworkConfigStoreFault::UnformattedErased)) => {}
        Ok(_) => {
            return Err(NetworkConfigStoreError::Fault(
                NetworkConfigStoreFault::AlreadyProvisioned,
            ));
        }
        Err(error) => return Err(error),
    }
    let binding = access.network_config_store_binding();
    let generation = NonZeroU64::new(1).expect("one is nonzero");
    write_snapshot(
        access,
        binding,
        NetworkConfigStoreSector::A,
        generation,
        0,
        ZERO_DIGEST,
        configuration,
    )?;
    verify_successor(
        access,
        NetworkConfigStoreSector::A,
        generation,
        configuration,
    )
}

/// Commit a complete successor snapshot to the inactive sector.
///
/// The copy-safe revision token allows callers to consume
/// [`MountedNetworkConfigStore`] into a mutable configuration before this
/// operation. The store remounts before writing and rejects stale tokens.
pub fn commit_successor<A>(
    access: &mut A,
    predecessor: NetworkConfigRevision,
    configuration: &NetworkConfig,
) -> Result<MountedNetworkConfigStore, NetworkConfigStoreError<A::Error>>
where
    A: BoundNetworkConfigStoreAccess,
{
    validate_write_binding(access).map_err(NetworkConfigStoreError::Binding)?;
    validate_same_binding(predecessor.binding, access.network_config_store_binding())
        .map_err(NetworkConfigStoreError::Binding)?;
    let observed = mount(access)?;
    if observed.revision != predecessor {
        return Err(NetworkConfigStoreError::Fault(
            NetworkConfigStoreFault::StaleRevision,
        ));
    }
    let generation = NonZeroU64::new(predecessor.generation.get().checked_add(1).ok_or(
        NetworkConfigStoreError::Fault(NetworkConfigStoreFault::GenerationExhausted),
    )?)
    .expect("checked successor remains nonzero");
    let target = predecessor.sector.other();
    erase_verified(access, target)?;
    write_snapshot(
        access,
        predecessor.binding,
        target,
        generation,
        predecessor.generation.get(),
        predecessor.digest,
        configuration,
    )?;
    verify_successor(access, target, generation, configuration)
}

/// Erase a reported non-authoritative sector and remount.
///
/// The operation remounts first and rejects a stale revision token. It is safe
/// to retry after interruption.
pub fn cleanup<A>(
    access: &mut A,
    revision: NetworkConfigRevision,
) -> Result<MountedNetworkConfigStore, NetworkConfigStoreError<A::Error>>
where
    A: BoundNetworkConfigStoreAccess,
{
    validate_write_binding(access).map_err(NetworkConfigStoreError::Binding)?;
    validate_same_binding(revision.binding, access.network_config_store_binding())
        .map_err(NetworkConfigStoreError::Binding)?;
    let mounted = mount(access)?;
    if mounted.revision != revision {
        return Err(NetworkConfigStoreError::Fault(
            NetworkConfigStoreFault::StaleRevision,
        ));
    }
    let NetworkConfigStoreCleanup::EraseInactive { sector } = mounted.cleanup else {
        return Ok(mounted);
    };
    erase_verified(access, sector)?;
    mount(access)
}

fn write_snapshot<A>(
    access: &mut A,
    binding: NetworkConfigStoreBinding,
    sector: NetworkConfigStoreSector,
    generation: NonZeroU64,
    predecessor_generation: u64,
    predecessor_digest: [u8; DIGEST_SIZE],
    configuration: &NetworkConfig,
) -> Result<(), NetworkConfigStoreError<A::Error>>
where
    A: BoundNetworkConfigStoreAccess,
{
    let encoded = encode_record(
        binding,
        sector,
        generation,
        predecessor_generation,
        predecessor_digest,
        configuration,
    );
    write_verified(access, sector, 0, &encoded[..COMMIT_OFFSET])?;
    write_verified(
        access,
        sector,
        COMMIT_OFFSET,
        &encoded[COMMIT_OFFSET..RECORD_SIZE],
    )
}

fn verify_successor<A>(
    access: &mut A,
    sector: NetworkConfigStoreSector,
    generation: NonZeroU64,
    configuration: &NetworkConfig,
) -> Result<MountedNetworkConfigStore, NetworkConfigStoreError<A::Error>>
where
    A: BoundNetworkConfigStoreReadAccess,
{
    let mounted = mount(access)?;
    if mounted.revision.sector != sector
        || mounted.revision.generation != generation
        || !configuration_eq(&mounted.configuration, configuration)
    {
        return Err(NetworkConfigStoreError::Fault(
            NetworkConfigStoreFault::SuccessorVerificationFailed,
        ));
    }
    Ok(mounted)
}

fn encode_record(
    binding: NetworkConfigStoreBinding,
    sector: NetworkConfigStoreSector,
    generation: NonZeroU64,
    predecessor_generation: u64,
    predecessor_digest: [u8; DIGEST_SIZE],
    configuration: &NetworkConfig,
) -> Zeroizing<[u8; RECORD_SIZE]> {
    let mut record = Zeroizing::new([0_u8; RECORD_SIZE]);
    record[..8].copy_from_slice(MAGIC);
    put_u16(&mut record[..], 8, PHYSICAL_FORMAT_VERSION);
    put_u16(&mut record[..], 10, SEMANTIC_FORMAT_VERSION);
    record[12] = sector.id();
    record[13] = configuration.wifi_profile_count() as u8;
    record[14] = if configuration.tcp_peer.is_some() {
        HEADER_FLAG_TCP_PEER_PRESENT
    } else {
        0
    };
    put_u64(&mut record[..], 16, generation.get());
    put_u64(&mut record[..], 24, predecessor_generation);
    record[32..64].copy_from_slice(&predecessor_digest);
    record[64..80].copy_from_slice(binding.device.as_bytes());
    put_u64(&mut record[..], 80, binding.absolute_offset as u64);
    put_u64(&mut record[..], 88, binding.length as u64);
    put_u32(&mut record[..], 96, WIFI_PROFILE_CAPACITY as u32);
    put_u32(&mut record[..], 100, WIFI_SLOT_SIZE as u32);
    put_u32(&mut record[..], 104, TCP_PEER_SLOT_SIZE as u32);

    for (index, profile) in configuration.wifi_profiles().enumerate() {
        let base = WIFI_SLOTS_OFFSET + index * WIFI_SLOT_SIZE;
        record[base..base + 16].copy_from_slice(profile.id.as_bytes());
        record[base + 16] = u8::from(profile.enabled);
        record[base + 17] = profile.priority;
        record[base + 18] = profile.ssid_length;
        record[base + 19] = profile.password_length;
        record[base + 20..base + 20 + MAX_SSID_LENGTH].copy_from_slice(&profile.ssid);
        record[base + 52..base + 52 + MAX_WPA2_PASSWORD_LENGTH].copy_from_slice(&profile.password);
    }

    if let Some(peer) = configuration.tcp_peer {
        record[TCP_PEER_OFFSET] = 1;
        record[TCP_PEER_OFFSET + 1] = u8::from(peer.enabled);
        put_u16(&mut record[..], TCP_PEER_OFFSET + 8, peer.port);
        match peer.address {
            OutboundTcpPeerAddress::Ipv4(ipv4) => {
                record[TCP_PEER_OFFSET + 2] = TCP_ADDRESS_FAMILY_IPV4;
                record[TCP_PEER_OFFSET + 4..TCP_PEER_OFFSET + 8].copy_from_slice(&ipv4);
            }
            OutboundTcpPeerAddress::Dns(hostname) => {
                record[TCP_PEER_OFFSET + 2] = TCP_ADDRESS_FAMILY_DNS;
                record[V2_HOSTNAME_LENGTH_OFFSET] = hostname.length;
                record[V2_HOSTNAME_OFFSET..V2_HOSTNAME_OFFSET + usize::from(hostname.length)]
                    .copy_from_slice(hostname.as_bytes());
            }
        }
    }

    let mut policy_flags = 0_u8;
    if configuration.wifi_transport_enabled {
        policy_flags |= V2_POLICY_WIFI_TRANSPORT_ENABLED;
    }
    if configuration.automatic_announces_enabled {
        policy_flags |= V2_POLICY_AUTOMATIC_ANNOUNCES_ENABLED;
    }
    if configuration.rmap_discovery_enabled {
        policy_flags |= V2_POLICY_RMAP_DISCOVERY_ENABLED;
    }
    if configuration.rmap_share_location {
        policy_flags |= V2_POLICY_RMAP_SHARE_LOCATION;
    }
    record[V2_POLICY_FLAGS_OFFSET] = policy_flags;
    record[V3_LORA_TX_POWER_DBM_OFFSET] = configuration.lora_tx_power().encoded_dbm();
    put_u32(
        &mut record[..],
        V4_LORA_FREQUENCY_HZ_OFFSET,
        configuration.lora_profile.frequency_hz(),
    );
    put_u32(
        &mut record[..],
        V4_LORA_BANDWIDTH_HZ_OFFSET,
        configuration.lora_profile.bandwidth_hz(),
    );
    record[V4_LORA_SPREADING_FACTOR_OFFSET] = configuration.lora_profile.spreading_factor();
    record[V4_LORA_CODING_RATE_DENOMINATOR_OFFSET] =
        configuration.lora_profile.coding_rate_denominator();
    if let Some(location) = configuration.phone_location {
        record[V2_LOCATION_PRESENT_OFFSET] = 1;
        put_i32(&mut record[..], V2_LATITUDE_OFFSET, location.latitude_e6);
        put_i32(&mut record[..], V2_LONGITUDE_OFFSET, location.longitude_e6);
    }

    let digest = snapshot_digest(&record[..PROTECTED_SIZE]);
    record[DIGEST_OFFSET..COMMIT_OFFSET].copy_from_slice(&digest);
    record[COMMIT_OFFSET..RECORD_SIZE].copy_from_slice(&COMMIT_MARKER);
    record
}

fn scan_sector<A>(
    access: &mut A,
    binding: NetworkConfigStoreBinding,
    sector: NetworkConfigStoreSector,
) -> Result<SectorStatus, A::Error>
where
    A: ReadNorFlash,
{
    let mut record = Zeroizing::new([0_u8; RECORD_SIZE]);
    access.read(sector.offset() as u32, &mut record[..])?;
    let tail_erased = region_is_erased(
        access,
        sector.offset() + RECORD_SIZE,
        SECTOR_SIZE - RECORD_SIZE,
    )?;
    if record.iter().all(|byte| *byte == 0xff) && tail_erased {
        return Ok(SectorStatus::Erased);
    }
    if !tail_erased {
        return Ok(invalid_status(sector, InvalidSectorKind::Unknown));
    }
    let commit = &record[COMMIT_OFFSET..RECORD_SIZE];
    if commit != COMMIT_MARKER {
        return if monotonic_programming_compatible(commit, &COMMIT_MARKER) {
            Ok(SectorStatus::Uncommitted(sector))
        } else {
            Ok(invalid_status(sector, InvalidSectorKind::Unknown))
        };
    }
    if &record[..8] != MAGIC {
        return Ok(invalid_status(sector, InvalidSectorKind::CommittedCorrupt));
    }
    let physical = read_u16(&record[..], 8);
    if physical != PHYSICAL_FORMAT_VERSION {
        return Ok(invalid_status(
            sector,
            InvalidSectorKind::UnsupportedPhysical(physical),
        ));
    }
    let semantic = read_u16(&record[..], 10);
    if !(MIN_SUPPORTED_SEMANTIC_FORMAT_VERSION..=SEMANTIC_FORMAT_VERSION).contains(&semantic) {
        return Ok(invalid_status(
            sector,
            InvalidSectorKind::UnsupportedSemantic(semantic),
        ));
    }
    if record[12] != sector.id()
        || record[15] != 0
        || record[108..HEADER_SIZE].iter().any(|byte| *byte != 0)
        || read_u32(&record[..], 96) != WIFI_PROFILE_CAPACITY as u32
        || read_u32(&record[..], 100) != WIFI_SLOT_SIZE as u32
        || read_u32(&record[..], 104) != TCP_PEER_SLOT_SIZE as u32
    {
        return Ok(invalid_status(sector, InvalidSectorKind::CommittedCorrupt));
    }
    if record[64..80] != binding.device.0
        || read_u64(&record[..], 80) != binding.absolute_offset as u64
        || read_u64(&record[..], 88) != binding.length as u64
    {
        return Ok(invalid_status(sector, InvalidSectorKind::WrongBinding));
    }
    let expected_digest = snapshot_digest(&record[..PROTECTED_SIZE]);
    if record[DIGEST_OFFSET..COMMIT_OFFSET] != expected_digest {
        return Ok(invalid_status(sector, InvalidSectorKind::CommittedCorrupt));
    }
    let generation_raw = read_u64(&record[..], 16);
    let Some(generation) = NonZeroU64::new(generation_raw) else {
        return Ok(invalid_status(sector, InvalidSectorKind::CommittedCorrupt));
    };
    let predecessor_generation = read_u64(&record[..], 24);
    let mut predecessor_digest = [0_u8; DIGEST_SIZE];
    predecessor_digest.copy_from_slice(&record[32..64]);
    if (generation_raw == 1 && (predecessor_generation != 0 || predecessor_digest != ZERO_DIGEST))
        || (generation_raw > 1
            && (predecessor_generation != generation_raw - 1 || predecessor_digest == ZERO_DIGEST))
    {
        return Ok(invalid_status(sector, InvalidSectorKind::CommittedCorrupt));
    }
    let Some(configuration) = decode_configuration(&record[..PROTECTED_SIZE], semantic) else {
        return Ok(invalid_status(sector, InvalidSectorKind::CommittedCorrupt));
    };
    let mut digest = [0_u8; DIGEST_SIZE];
    digest.copy_from_slice(&record[DIGEST_OFFSET..COMMIT_OFFSET]);
    Ok(SectorStatus::Valid(ValidSnapshot {
        revision: NetworkConfigRevision {
            binding,
            sector,
            generation,
            digest,
        },
        predecessor_generation,
        predecessor_digest,
        configuration,
    }))
}

fn decode_configuration(protected: &[u8], semantic_version: u16) -> Option<NetworkConfig> {
    let profile_count = usize::from(protected[13]);
    if profile_count > WIFI_PROFILE_CAPACITY || protected[14] & !HEADER_FLAG_TCP_PEER_PRESENT != 0 {
        return None;
    }
    let mut configuration = NetworkConfig::empty();
    for index in 0..WIFI_PROFILE_CAPACITY {
        let base = WIFI_SLOTS_OFFSET + index * WIFI_SLOT_SIZE;
        let slot = &protected[base..base + WIFI_SLOT_SIZE];
        if index >= profile_count {
            if slot.iter().any(|byte| *byte != 0) {
                return None;
            }
            continue;
        }
        if slot[16] > 1 || slot[115..].iter().any(|byte| *byte != 0) {
            return None;
        }
        let mut id = [0_u8; 16];
        id.copy_from_slice(&slot[..16]);
        let id = WifiProfileId::new(id).ok()?;
        let ssid_length = usize::from(slot[18]);
        let password_length = usize::from(slot[19]);
        if !(1..=MAX_SSID_LENGTH).contains(&ssid_length)
            || !(MIN_WPA2_PASSWORD_LENGTH..=MAX_WPA2_PASSWORD_LENGTH).contains(&password_length)
            || slot[20 + ssid_length..52].iter().any(|byte| *byte != 0)
            || slot[52 + password_length..115]
                .iter()
                .any(|byte| *byte != 0)
        {
            return None;
        }
        let profile = WifiProfile::new(
            id,
            &slot[20..20 + ssid_length],
            &slot[52..52 + password_length],
            slot[16] != 0,
            slot[17],
        )
        .ok()?;
        configuration.insert_wifi_profile(profile).ok()?;
    }

    match semantic_version {
        1 => decode_v1_extension(protected, &mut configuration)?,
        2 => decode_v2_extension(protected, &mut configuration)?,
        3 => decode_v3_extension(protected, &mut configuration)?,
        4 => decode_v4_extension(protected, &mut configuration)?,
        _ => return None,
    }
    Some(configuration)
}

fn decode_v1_extension(protected: &[u8], configuration: &mut NetworkConfig) -> Option<()> {
    let peer_slot = &protected[TCP_PEER_OFFSET..TCP_PEER_OFFSET + TCP_PEER_SLOT_SIZE];
    let peer_present = protected[14] & HEADER_FLAG_TCP_PEER_PRESENT != 0;
    if !peer_present {
        if peer_slot.iter().any(|byte| *byte != 0) {
            return None;
        }
    } else {
        if peer_slot[0] != 1
            || peer_slot[1] > 1
            || peer_slot[2] != TCP_ADDRESS_FAMILY_IPV4
            || peer_slot[3] != 0
            || peer_slot[10..].iter().any(|byte| *byte != 0)
        {
            return None;
        }
        let mut ipv4 = [0_u8; 4];
        ipv4.copy_from_slice(&peer_slot[4..8]);
        let peer = OutboundTcpPeer::new(ipv4, read_u16(peer_slot, 8), peer_slot[1] != 0).ok()?;
        configuration.set_tcp_peer(Some(peer));
    }
    if protected[PAYLOAD_RESERVED_OFFSET..PROTECTED_SIZE]
        .iter()
        .any(|byte| *byte != 0)
    {
        return None;
    }
    Some(())
}

fn decode_v2_extension(protected: &[u8], configuration: &mut NetworkConfig) -> Option<()> {
    if protected[V2_EXTENSION_RESERVED_OFFSET] != 0 {
        return None;
    }
    decode_v2_payload(protected, configuration, V2_HOSTNAME_END)
}

fn decode_v3_extension(protected: &[u8], configuration: &mut NetworkConfig) -> Option<()> {
    let lora_tx_power =
        LoraTxPower::try_from_dbm(i32::from(protected[V3_LORA_TX_POWER_DBM_OFFSET])).ok()?;
    decode_v2_payload(protected, configuration, V2_HOSTNAME_END)?;
    configuration.set_lora_tx_power(lora_tx_power);
    Some(())
}

fn decode_v4_extension(protected: &[u8], configuration: &mut NetworkConfig) -> Option<()> {
    let tx_power =
        LoraTxPower::try_from_dbm(i32::from(protected[V3_LORA_TX_POWER_DBM_OFFSET])).ok()?;
    let profile = LoraRadioProfile::new(
        read_u32(protected, V4_LORA_FREQUENCY_HZ_OFFSET),
        read_u32(protected, V4_LORA_BANDWIDTH_HZ_OFFSET),
        protected[V4_LORA_SPREADING_FACTOR_OFFSET],
        protected[V4_LORA_CODING_RATE_DENOMINATOR_OFFSET],
        tx_power,
    )
    .ok()?;
    decode_v2_payload(protected, configuration, V4_LORA_PROFILE_END)?;
    configuration.set_lora_profile(profile);
    Some(())
}

fn decode_v2_payload(
    protected: &[u8],
    configuration: &mut NetworkConfig,
    extension_end: usize,
) -> Option<()> {
    let policy_flags = protected[V2_POLICY_FLAGS_OFFSET];
    let location_present = protected[V2_LOCATION_PRESENT_OFFSET];
    let hostname_length = usize::from(protected[V2_HOSTNAME_LENGTH_OFFSET]);
    if policy_flags & !V2_POLICY_VALID_MASK != 0
        || location_present > 1
        || hostname_length > MAX_DNS_HOSTNAME_LENGTH
        || protected[extension_end..PROTECTED_SIZE]
            .iter()
            .any(|byte| *byte != 0)
    {
        return None;
    }
    configuration.set_wifi_transport_enabled(policy_flags & V2_POLICY_WIFI_TRANSPORT_ENABLED != 0);
    configuration
        .set_automatic_announces_enabled(policy_flags & V2_POLICY_AUTOMATIC_ANNOUNCES_ENABLED != 0);
    configuration.set_rmap_discovery_enabled(policy_flags & V2_POLICY_RMAP_DISCOVERY_ENABLED != 0);
    configuration.set_rmap_share_location(policy_flags & V2_POLICY_RMAP_SHARE_LOCATION != 0);

    let latitude_e6 = read_i32(protected, V2_LATITUDE_OFFSET);
    let longitude_e6 = read_i32(protected, V2_LONGITUDE_OFFSET);
    if location_present == 0 {
        if latitude_e6 != 0 || longitude_e6 != 0 {
            return None;
        }
    } else {
        configuration.set_phone_location(Some(PhoneLocation::new(latitude_e6, longitude_e6).ok()?));
    }

    let hostname_bytes = &protected[V2_HOSTNAME_OFFSET..V2_HOSTNAME_END];
    if hostname_bytes[hostname_length..]
        .iter()
        .any(|byte| *byte != 0)
    {
        return None;
    }
    let peer_slot = &protected[TCP_PEER_OFFSET..TCP_PEER_OFFSET + TCP_PEER_SLOT_SIZE];
    let peer_present = protected[14] & HEADER_FLAG_TCP_PEER_PRESENT != 0;
    if !peer_present {
        if peer_slot.iter().any(|byte| *byte != 0) || hostname_length != 0 {
            return None;
        }
        return Some(());
    }
    if peer_slot[0] != 1
        || peer_slot[1] > 1
        || peer_slot[3] != 0
        || peer_slot[10..].iter().any(|byte| *byte != 0)
    {
        return None;
    }
    let enabled = peer_slot[1] != 0;
    let port = read_u16(peer_slot, 8);
    let peer = match peer_slot[2] {
        TCP_ADDRESS_FAMILY_IPV4 => {
            if hostname_length != 0 {
                return None;
            }
            let mut ipv4 = [0_u8; 4];
            ipv4.copy_from_slice(&peer_slot[4..8]);
            OutboundTcpPeer::new(ipv4, port, enabled).ok()?
        }
        TCP_ADDRESS_FAMILY_DNS => {
            if peer_slot[4..8].iter().any(|byte| *byte != 0) || hostname_length == 0 {
                return None;
            }
            OutboundTcpPeer::with_dns_hostname(&hostname_bytes[..hostname_length], port, enabled)
                .ok()?
        }
        _ => return None,
    };
    configuration.set_tcp_peer(Some(peer));
    Some(())
}

fn select(
    binding: NetworkConfigStoreBinding,
    a: SectorStatus,
    b: SectorStatus,
) -> Result<MountedNetworkConfigStore, NetworkConfigStoreFault> {
    match (a, b) {
        (SectorStatus::Erased, SectorStatus::Erased) => {
            Err(NetworkConfigStoreFault::UnformattedErased)
        }
        (SectorStatus::Invalid { sector, kind }, _)
        | (_, SectorStatus::Invalid { sector, kind }) => Err(map_invalid(sector, kind)),
        (SectorStatus::Valid(a), SectorStatus::Valid(b)) => select_two_valid(a, b),
        (SectorStatus::Valid(valid), other) | (other, SectorStatus::Valid(valid)) => {
            select_one_valid(binding, valid, other)
        }
        (SectorStatus::Uncommitted(_), SectorStatus::Uncommitted(_))
        | (SectorStatus::Uncommitted(_), SectorStatus::Erased)
        | (SectorStatus::Erased, SectorStatus::Uncommitted(_)) => {
            Err(NetworkConfigStoreFault::UnformattedNonErased)
        }
    }
}

fn select_two_valid(
    a: ValidSnapshot,
    b: ValidSnapshot,
) -> Result<MountedNetworkConfigStore, NetworkConfigStoreFault> {
    let (newer, older) = if a.revision.generation > b.revision.generation {
        (a, b)
    } else {
        (b, a)
    };
    if newer.revision.generation.get()
        != older.revision.generation.get().checked_add(1).unwrap_or(0)
        || newer.predecessor_generation != older.revision.generation.get()
        || newer.predecessor_digest != older.revision.digest
    {
        return Err(NetworkConfigStoreFault::SnapshotConflict);
    }
    Ok(MountedNetworkConfigStore {
        cleanup: NetworkConfigStoreCleanup::EraseInactive {
            sector: older.revision.sector,
        },
        revision: newer.revision,
        configuration: newer.configuration,
    })
}

fn select_one_valid(
    _binding: NetworkConfigStoreBinding,
    valid: ValidSnapshot,
    other: SectorStatus,
) -> Result<MountedNetworkConfigStore, NetworkConfigStoreFault> {
    let cleanup = match other {
        SectorStatus::Erased => NetworkConfigStoreCleanup::Clean,
        SectorStatus::Uncommitted(sector) => {
            if sector == valid.revision.sector {
                return Err(NetworkConfigStoreFault::SnapshotConflict);
            }
            NetworkConfigStoreCleanup::EraseInactive { sector }
        }
        SectorStatus::Valid(_) | SectorStatus::Invalid { .. } => unreachable!("selected earlier"),
    };
    Ok(MountedNetworkConfigStore {
        revision: valid.revision,
        configuration: valid.configuration,
        cleanup,
    })
}

const fn invalid_status(sector: NetworkConfigStoreSector, kind: InvalidSectorKind) -> SectorStatus {
    SectorStatus::Invalid { sector, kind }
}

const fn map_invalid(
    sector: NetworkConfigStoreSector,
    kind: InvalidSectorKind,
) -> NetworkConfigStoreFault {
    match kind {
        InvalidSectorKind::UnsupportedPhysical(version) => {
            NetworkConfigStoreFault::UnsupportedPhysicalVersion(version)
        }
        InvalidSectorKind::UnsupportedSemantic(version) => {
            NetworkConfigStoreFault::UnsupportedSemanticVersion(version)
        }
        InvalidSectorKind::WrongBinding => {
            NetworkConfigStoreFault::DeviceBindingMismatch { sector }
        }
        InvalidSectorKind::CommittedCorrupt => {
            NetworkConfigStoreFault::CommittedSnapshotCorrupt { sector }
        }
        InvalidSectorKind::Unknown => NetworkConfigStoreFault::UnknownProgrammedData { sector },
    }
}

fn validate_read_binding<A>(access: &A) -> Result<(), NetworkConfigStoreBindingError>
where
    A: BoundNetworkConfigStoreReadAccess,
{
    let binding = access.network_config_store_binding();
    validate_binding_shape(binding)?;
    if access.capacity() != PARTITION_SIZE {
        return Err(NetworkConfigStoreBindingError::CapacityMismatch {
            expected: PARTITION_SIZE,
            actual: access.capacity(),
        });
    }
    let read_size = A::READ_SIZE;
    if read_size == 0
        || !binding.absolute_offset.is_multiple_of(read_size)
        || !PARTITION_SIZE.is_multiple_of(read_size)
        || !RECORD_SIZE.is_multiple_of(read_size)
        || !INSPECTION_CHUNK_SIZE.is_multiple_of(read_size)
    {
        return Err(NetworkConfigStoreBindingError::ReadAlignmentMismatch { read_size });
    }
    Ok(())
}

fn validate_write_binding<A>(access: &A) -> Result<(), NetworkConfigStoreBindingError>
where
    A: BoundNetworkConfigStoreAccess,
{
    validate_read_binding(access)?;
    let binding = access.network_config_store_binding();
    let write_size = A::WRITE_SIZE;
    let erase_size = A::ERASE_SIZE;
    if write_size == 0
        || erase_size == 0
        || !binding.absolute_offset.is_multiple_of(write_size)
        || !binding.absolute_offset.is_multiple_of(erase_size)
        || !COMMIT_OFFSET.is_multiple_of(write_size)
        || !COMMIT_SIZE.is_multiple_of(write_size)
        || !SECTOR_SIZE.is_multiple_of(erase_size)
    {
        return Err(NetworkConfigStoreBindingError::WriteAlignmentMismatch {
            write_size,
            erase_size,
        });
    }
    Ok(())
}

fn validate_binding_shape(
    binding: NetworkConfigStoreBinding,
) -> Result<(), NetworkConfigStoreBindingError> {
    if binding.format_version != PHYSICAL_FORMAT_VERSION {
        return Err(NetworkConfigStoreBindingError::FormatVersionMismatch {
            expected: PHYSICAL_FORMAT_VERSION,
            actual: binding.format_version,
        });
    }
    if binding.length != PARTITION_SIZE {
        return Err(NetworkConfigStoreBindingError::LengthMismatch {
            expected: PARTITION_SIZE,
            actual: binding.length,
        });
    }
    if binding
        .absolute_offset
        .checked_add(binding.length)
        .is_none()
        || u64::try_from(binding.absolute_offset).is_err()
    {
        return Err(NetworkConfigStoreBindingError::RangeOverflow {
            absolute_offset: binding.absolute_offset,
            length: binding.length,
        });
    }
    Ok(())
}

fn validate_same_binding(
    expected: NetworkConfigStoreBinding,
    actual: NetworkConfigStoreBinding,
) -> Result<(), NetworkConfigStoreBindingError> {
    if expected.device != actual.device {
        return Err(NetworkConfigStoreBindingError::DeviceMismatch {
            expected: expected.device,
            actual: actual.device,
        });
    }
    if expected.absolute_offset != actual.absolute_offset || expected.length != actual.length {
        return Err(NetworkConfigStoreBindingError::RangeMismatch {
            expected_offset: expected.absolute_offset,
            expected_length: expected.length,
            actual_offset: actual.absolute_offset,
            actual_length: actual.length,
        });
    }
    if expected.format_version != actual.format_version {
        return Err(NetworkConfigStoreBindingError::FormatVersionMismatch {
            expected: expected.format_version,
            actual: actual.format_version,
        });
    }
    Ok(())
}

fn write_verified<A>(
    access: &mut A,
    sector: NetworkConfigStoreSector,
    relative_offset: usize,
    bytes: &[u8],
) -> Result<(), NetworkConfigStoreError<A::Error>>
where
    A: NorFlash,
{
    let offset = sector.offset() + relative_offset;
    access
        .write(offset as u32, bytes)
        .map_err(NetworkConfigStoreError::Backend)?;
    let mut readback = Zeroizing::new([0_u8; INSPECTION_CHUNK_SIZE]);
    for (index, expected) in bytes.chunks(INSPECTION_CHUNK_SIZE).enumerate() {
        let actual = &mut readback[..expected.len()];
        access
            .read((offset + index * INSPECTION_CHUNK_SIZE) as u32, actual)
            .map_err(NetworkConfigStoreError::Backend)?;
        if actual != expected {
            return Err(NetworkConfigStoreError::Fault(
                NetworkConfigStoreFault::ReadbackMismatch { sector },
            ));
        }
    }
    Ok(())
}

fn erase_verified<A>(
    access: &mut A,
    sector: NetworkConfigStoreSector,
) -> Result<(), NetworkConfigStoreError<A::Error>>
where
    A: NorFlash,
{
    access
        .erase(
            sector.offset() as u32,
            (sector.offset() + SECTOR_SIZE) as u32,
        )
        .map_err(NetworkConfigStoreError::Backend)?;
    if !region_is_erased(access, sector.offset(), SECTOR_SIZE)
        .map_err(NetworkConfigStoreError::Backend)?
    {
        return Err(NetworkConfigStoreError::Fault(
            NetworkConfigStoreFault::ReadbackMismatch { sector },
        ));
    }
    Ok(())
}

fn region_is_erased<A>(access: &mut A, offset: usize, length: usize) -> Result<bool, A::Error>
where
    A: ReadNorFlash,
{
    let mut readback = [0_u8; INSPECTION_CHUNK_SIZE];
    let mut cursor = 0_usize;
    while cursor < length {
        let chunk = core::cmp::min(INSPECTION_CHUNK_SIZE, length - cursor);
        access.read((offset + cursor) as u32, &mut readback[..chunk])?;
        if readback[..chunk].iter().any(|byte| *byte != 0xff) {
            return Ok(false);
        }
        cursor += chunk;
    }
    Ok(true)
}

fn snapshot_digest(protected: &[u8]) -> [u8; DIGEST_SIZE] {
    debug_assert_eq!(protected.len(), PROTECTED_SIZE);
    let mut hasher = Sha256::new();
    hasher.update(DIGEST_DOMAIN);
    hasher.update(protected);
    hasher.update(DIGEST_FLUSH_TRAILER);
    hasher.finalize().into()
}

fn monotonic_programming_compatible(observed: &[u8], intended: &[u8]) -> bool {
    observed
        .iter()
        .zip(intended)
        .all(|(observed, intended)| observed | intended == *observed)
}

fn configuration_eq(left: &NetworkConfig, right: &NetworkConfig) -> bool {
    if left.wifi_profile_count() != right.wifi_profile_count()
        || left.tcp_peer != right.tcp_peer
        || left.wifi_transport_enabled != right.wifi_transport_enabled
        || left.automatic_announces_enabled != right.automatic_announces_enabled
        || left.rmap_discovery_enabled != right.rmap_discovery_enabled
        || left.rmap_share_location != right.rmap_share_location
        || left.phone_location != right.phone_location
        || left.lora_profile != right.lora_profile
    {
        return false;
    }
    left.wifi_profiles()
        .zip(right.wifi_profiles())
        .all(|(left, right)| {
            left.id == right.id
                && left.ssid() == right.ssid()
                && constant_time_eq(left.password(), right.password())
                && left.enabled == right.enabled
                && left.priority == right.priority
        })
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().expect("fixed u16"))
}

fn read_i32(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("fixed i32"))
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("fixed u32"))
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("fixed u64"))
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_i32(bytes: &mut [u8], offset: usize, value: i32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
