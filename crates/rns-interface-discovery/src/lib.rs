//! Bounded Reticulum on-network interface-discovery announce support.
//!
//! Reticulum's interface discovery surface is distinct from ordinary node,
//! LXMF, and NomadNet announces. A discoverable interface is advertised from
//! the `rnstransport.discovery.interface` destination with application data
//! containing a flags byte, a MessagePack map, and one 32-byte LXMF-compatible
//! proof-of-work stamp.
//!
//! This crate implements the unencrypted `RNodeInterface` subset needed by an
//! embedded LoRa gateway. Encoding is allocation-free. Stamp search is
//! incremental so an embedded product can bound work per cooperative turn.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use hkdf::Hkdf;
use sha2::{Digest, Sha256};

/// Reticulum application name for interface discovery.
pub const DISCOVERY_APPLICATION_NAME: &str = "rnstransport";
/// Reticulum destination aspects for interface discovery.
pub const DISCOVERY_ASPECTS: [&str; 2] = ["discovery", "interface"];
/// Current RNS default interface-discovery proof-of-work cost.
pub const DEFAULT_STAMP_COST: u16 = 16;
/// HKDF expansion rounds used by current RNS interface discovery.
pub const WORKBLOCK_EXPAND_ROUNDS: usize = 20;
/// Bytes in one discovery proof-of-work stamp.
pub const STAMP_BYTES: usize = 32;
/// Bytes in the expanded discovery proof-of-work block.
pub const WORKBLOCK_BYTES: usize = WORKBLOCK_EXPAND_ROUNDS * 256;
/// Maximum UTF-8 byte length of a published discovery name.
pub const MAX_DISCOVERY_NAME_BYTES: usize = 64;
/// Maximum encoded MessagePack bytes for the supported RNode map.
pub const MAX_PACKED_INFO_BYTES: usize = 192;
/// Maximum complete unencrypted discovery announce application data.
pub const MAX_DISCOVERY_APP_DATA_BYTES: usize = 1 + MAX_PACKED_INFO_BYTES + STAMP_BYTES;

const FLAG_UNENCRYPTED_UNSIGNED: u8 = 0;
const RNODE_INTERFACE: &str = "RNodeInterface";

/// Fixed-point phone-provided latitude and longitude.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocationE6 {
    latitude_e6: i32,
    longitude_e6: i32,
}

impl LocationE6 {
    /// Construct a world-bounded coordinate in millionths of one degree.
    pub const fn new(latitude_e6: i32, longitude_e6: i32) -> Result<Self, DiscoveryModelError> {
        if latitude_e6 < -90_000_000 || latitude_e6 > 90_000_000 {
            return Err(DiscoveryModelError::InvalidLatitude);
        }
        if longitude_e6 < -180_000_000 || longitude_e6 > 180_000_000 {
            return Err(DiscoveryModelError::InvalidLongitude);
        }
        Ok(Self {
            latitude_e6,
            longitude_e6,
        })
    }

    /// Latitude in millionths of one degree.
    pub const fn latitude_e6(self) -> i32 {
        self.latitude_e6
    }

    /// Longitude in millionths of one degree.
    pub const fn longitude_e6(self) -> i32 {
        self.longitude_e6
    }
}

/// Validated RNode interface metadata used for one discovery payload.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RnodeDiscoveryInfo<'a> {
    transport_id: [u8; 16],
    name: &'a str,
    transport_enabled: bool,
    location: Option<LocationE6>,
    frequency_hz: u32,
    bandwidth_hz: u32,
    spreading_factor: u8,
    coding_rate: u8,
}

impl<'a> RnodeDiscoveryInfo<'a> {
    /// Validate the complete interface-discovery projection.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        transport_id: [u8; 16],
        name: &'a str,
        transport_enabled: bool,
        location: Option<LocationE6>,
        frequency_hz: u32,
        bandwidth_hz: u32,
        spreading_factor: u8,
        coding_rate: u8,
    ) -> Result<Self, DiscoveryModelError> {
        if transport_id == [0; 16] {
            return Err(DiscoveryModelError::ZeroTransportId);
        }
        if name.is_empty() || name.len() > MAX_DISCOVERY_NAME_BYTES {
            return Err(DiscoveryModelError::InvalidNameLength);
        }
        if name.contains(['\n', '\r', '\0']) {
            return Err(DiscoveryModelError::InvalidNameCharacter);
        }
        if frequency_hz == 0 {
            return Err(DiscoveryModelError::ZeroFrequency);
        }
        if bandwidth_hz == 0 {
            return Err(DiscoveryModelError::ZeroBandwidth);
        }
        if !(5..=12).contains(&spreading_factor) {
            return Err(DiscoveryModelError::InvalidSpreadingFactor);
        }
        if !(5..=8).contains(&coding_rate) {
            return Err(DiscoveryModelError::InvalidCodingRate);
        }
        Ok(Self {
            transport_id,
            name,
            transport_enabled,
            location,
            frequency_hz,
            bandwidth_hz,
            spreading_factor,
            coding_rate,
        })
    }

    /// Stable Reticulum transport identity advertised by the appliance.
    pub const fn transport_id(self) -> [u8; 16] {
        self.transport_id
    }

    /// Stable public interface name.
    pub const fn name(self) -> &'a str {
        self.name
    }

    /// Whether the advertising node forwards Reticulum traffic.
    pub const fn transport_enabled(self) -> bool {
        self.transport_enabled
    }

    /// Optional public location.
    pub const fn location(self) -> Option<LocationE6> {
        self.location
    }
}

/// Rejected interface-discovery model input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryModelError {
    /// The all-zero transport identity is not publishable.
    ZeroTransportId,
    /// The public name was empty or exceeded the bounded payload profile.
    InvalidNameLength,
    /// The public name contained a line break or NUL.
    InvalidNameCharacter,
    /// Latitude was outside -90 through +90 degrees.
    InvalidLatitude,
    /// Longitude was outside -180 through +180 degrees.
    InvalidLongitude,
    /// Frequency zero cannot describe an RNode radio.
    ZeroFrequency,
    /// Bandwidth zero cannot describe an RNode radio.
    ZeroBandwidth,
    /// LoRa spreading factor was outside 5 through 12.
    InvalidSpreadingFactor,
    /// LoRa coding-rate denominator was outside 5 through 8.
    InvalidCodingRate,
}

/// Canonical bounded MessagePack map awaiting a proof-of-work stamp.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackedDiscoveryInfo {
    bytes: [u8; MAX_PACKED_INFO_BYTES],
    len: u8,
}

impl PackedDiscoveryInfo {
    /// Exact encoded MessagePack bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }

    /// SHA-256 hash used as discovery stamp material.
    pub fn info_hash(&self) -> [u8; 32] {
        Sha256::digest(self.as_bytes()).into()
    }

    /// Attach a validated or generated 32-byte stamp.
    pub fn with_stamp(&self, stamp: [u8; STAMP_BYTES]) -> DiscoveryAppData {
        let mut bytes = [0_u8; MAX_DISCOVERY_APP_DATA_BYTES];
        bytes[0] = FLAG_UNENCRYPTED_UNSIGNED;
        let packed_len = self.as_bytes().len();
        bytes[1..1 + packed_len].copy_from_slice(self.as_bytes());
        bytes[1 + packed_len..1 + packed_len + STAMP_BYTES].copy_from_slice(&stamp);
        DiscoveryAppData {
            bytes,
            len: (1 + packed_len + STAMP_BYTES) as u8,
        }
    }
}

/// Complete unencrypted RNS interface-discovery announce application data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiscoveryAppData {
    bytes: [u8; MAX_DISCOVERY_APP_DATA_BYTES],
    len: u8,
}

impl DiscoveryAppData {
    /// Exact flags, MessagePack map, and stamp bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }

    /// Encoded MessagePack map, excluding the flags byte and stamp.
    pub fn packed_info(&self) -> &[u8] {
        &self.as_bytes()[1..self.as_bytes().len() - STAMP_BYTES]
    }

    /// Attached 32-byte proof-of-work stamp.
    pub fn stamp(&self) -> &[u8; STAMP_BYTES] {
        self.as_bytes()[self.as_bytes().len() - STAMP_BYTES..]
            .try_into()
            .expect("a constructed discovery payload always ends in one exact stamp")
    }
}

/// Encode the current RNS `RNodeInterface` discovery-map subset.
pub fn encode_rnode_info(
    info: RnodeDiscoveryInfo<'_>,
) -> Result<PackedDiscoveryInfo, DiscoveryEncodeError> {
    let mut writer = MsgpackWriter::new();
    writer.map(11)?;

    writer.uint(0x00)?;
    writer.string(RNODE_INTERFACE)?;
    writer.uint(0x01)?;
    writer.boolean(info.transport_enabled)?;
    writer.uint(0xfe)?;
    writer.binary(&info.transport_id)?;
    writer.uint(0xff)?;
    writer.string(info.name)?;
    writer.uint(0x03)?;
    writer.optional_f64(
        info.location
            .map(|location| f64::from(location.latitude_e6) / 1_000_000.0),
    )?;
    writer.uint(0x04)?;
    writer.optional_f64(
        info.location
            .map(|location| f64::from(location.longitude_e6) / 1_000_000.0),
    )?;
    writer.uint(0x05)?;
    writer.nil()?;
    writer.uint(0x09)?;
    writer.uint(u64::from(info.frequency_hz))?;
    writer.uint(0x0a)?;
    writer.uint(u64::from(info.bandwidth_hz))?;
    writer.uint(0x0b)?;
    writer.uint(u64::from(info.spreading_factor))?;
    writer.uint(0x0c)?;
    writer.uint(u64::from(info.coding_rate))?;

    Ok(PackedDiscoveryInfo {
        bytes: writer.bytes,
        len: writer.len as u8,
    })
}

/// Encoding exceeded the fixed supported discovery profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiscoveryEncodeError;

/// Incremental proof-of-work search over one immutable discovery map.
///
/// Construction performs the fixed 20-round HKDF expansion. Callers then use
/// [`Self::step`] with a small attempt budget and yield between calls.
pub struct DiscoveryStampSearch {
    info_hash: [u8; 32],
    workblock_hasher: Sha256,
    next_counter: u64,
    target_cost: u16,
}

impl DiscoveryStampSearch {
    /// Build the exact RNS discovery workblock for one packed map.
    pub fn new(
        packed: &PackedDiscoveryInfo,
        target_cost: u16,
    ) -> Result<Self, DiscoveryStampError> {
        if target_cost > 256 {
            return Err(DiscoveryStampError::InvalidTargetCost);
        }
        let info_hash = packed.info_hash();
        let mut workblock_hasher = Sha256::new();
        for round in 0..WORKBLOCK_EXPAND_ROUNDS {
            let mut salt_hash = Sha256::new();
            salt_hash.update(info_hash);
            let (encoded_round, encoded_len) = encode_msgpack_uint(round as u64);
            salt_hash.update(&encoded_round[..encoded_len]);
            let salt: [u8; 32] = salt_hash.finalize().into();
            let hkdf = Hkdf::<Sha256>::new(Some(&salt), &info_hash);
            let mut expanded_round = [0_u8; 256];
            hkdf.expand(&[], &mut expanded_round)
                .map_err(|_| DiscoveryStampError::KdfFailure)?;
            workblock_hasher.update(expanded_round);
        }
        Ok(Self {
            info_hash,
            workblock_hasher,
            next_counter: 0,
            target_cost,
        })
    }

    /// Test at most `attempts` deterministic candidates.
    ///
    /// A zero attempt budget performs no work. Counter exhaustion is reported
    /// without wrapping and reusing candidates.
    pub fn step(&mut self, attempts: u32) -> DiscoveryStampProgress {
        for _ in 0..attempts {
            let counter = self.next_counter;
            let Some(next) = counter.checked_add(1) else {
                return DiscoveryStampProgress::Exhausted;
            };
            self.next_counter = next;
            let mut candidate_hash = Sha256::new();
            candidate_hash.update(self.info_hash);
            candidate_hash.update(counter.to_be_bytes());
            let candidate: [u8; STAMP_BYTES] = candidate_hash.finalize().into();
            let mut proof_hasher = self.workblock_hasher.clone();
            proof_hasher.update(candidate);
            let digest: [u8; 32] = proof_hasher.finalize().into();
            if meets_python_target(&digest, self.target_cost) {
                return DiscoveryStampProgress::Found(candidate);
            }
        }
        DiscoveryStampProgress::Pending
    }

    /// Number of candidates already tested.
    pub const fn attempts(&self) -> u64 {
        self.next_counter
    }
}

/// Result of one bounded proof-of-work search turn.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryStampProgress {
    /// No candidate in this turn met the target.
    Pending,
    /// A compatible 32-byte stamp was found.
    Found([u8; STAMP_BYTES]),
    /// The deterministic candidate counter cannot advance further.
    Exhausted,
}

/// Failure while preparing a discovery stamp search.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryStampError {
    /// Target costs above the 256-bit hash width are invalid.
    InvalidTargetCost,
    /// HKDF rejected a protocol-fixed output block.
    KdfFailure,
}

/// Validate a stamp with Python LXMF's inclusive integer target.
pub fn stamp_valid(workblock: &[u8], stamp: &[u8; STAMP_BYTES], target_cost: u16) -> bool {
    if target_cost > 256 || workblock.len() != WORKBLOCK_BYTES {
        return false;
    }
    let mut hasher = Sha256::new();
    hasher.update(workblock);
    hasher.update(stamp);
    let digest: [u8; 32] = hasher.finalize().into();
    meets_python_target(&digest, target_cost)
}

fn meets_python_target(digest: &[u8; 32], target_cost: u16) -> bool {
    if target_cost == 0 {
        return true;
    }
    let target_bit = usize::from(target_cost - 1);
    let mut target = [0_u8; 32];
    target[target_bit / 8] = 0x80 >> (target_bit % 8);
    digest <= &target
}

fn encode_msgpack_uint(value: u64) -> ([u8; 9], usize) {
    let mut encoded = [0_u8; 9];
    let length = if value < 128 {
        encoded[0] = value as u8;
        1
    } else if value < 256 {
        encoded[0] = 0xcc;
        encoded[1] = value as u8;
        2
    } else if value < 65_536 {
        encoded[0] = 0xcd;
        encoded[1..3].copy_from_slice(&(value as u16).to_be_bytes());
        3
    } else if value < 4_294_967_296 {
        encoded[0] = 0xce;
        encoded[1..5].copy_from_slice(&(value as u32).to_be_bytes());
        5
    } else {
        encoded[0] = 0xcf;
        encoded[1..9].copy_from_slice(&value.to_be_bytes());
        9
    };
    (encoded, length)
}

struct MsgpackWriter {
    bytes: [u8; MAX_PACKED_INFO_BYTES],
    len: usize,
}

impl MsgpackWriter {
    const fn new() -> Self {
        Self {
            bytes: [0; MAX_PACKED_INFO_BYTES],
            len: 0,
        }
    }

    fn push(&mut self, byte: u8) -> Result<(), DiscoveryEncodeError> {
        let destination = self.bytes.get_mut(self.len).ok_or(DiscoveryEncodeError)?;
        *destination = byte;
        self.len += 1;
        Ok(())
    }

    fn extend(&mut self, bytes: &[u8]) -> Result<(), DiscoveryEncodeError> {
        let end = self
            .len
            .checked_add(bytes.len())
            .ok_or(DiscoveryEncodeError)?;
        let destination = self
            .bytes
            .get_mut(self.len..end)
            .ok_or(DiscoveryEncodeError)?;
        destination.copy_from_slice(bytes);
        self.len = end;
        Ok(())
    }

    fn map(&mut self, entries: u8) -> Result<(), DiscoveryEncodeError> {
        if entries <= 15 {
            self.push(0x80 | entries)
        } else {
            Err(DiscoveryEncodeError)
        }
    }

    fn uint(&mut self, value: u64) -> Result<(), DiscoveryEncodeError> {
        let (encoded, len) = encode_msgpack_uint(value);
        self.extend(&encoded[..len])
    }

    fn boolean(&mut self, value: bool) -> Result<(), DiscoveryEncodeError> {
        self.push(if value { 0xc3 } else { 0xc2 })
    }

    fn nil(&mut self) -> Result<(), DiscoveryEncodeError> {
        self.push(0xc0)
    }

    fn optional_f64(&mut self, value: Option<f64>) -> Result<(), DiscoveryEncodeError> {
        match value {
            Some(value) => {
                self.push(0xcb)?;
                self.extend(&value.to_bits().to_be_bytes())
            }
            None => self.nil(),
        }
    }

    fn binary(&mut self, value: &[u8]) -> Result<(), DiscoveryEncodeError> {
        if value.len() > u8::MAX as usize {
            return Err(DiscoveryEncodeError);
        }
        self.push(0xc4)?;
        self.push(value.len() as u8)?;
        self.extend(value)
    }

    fn string(&mut self, value: &str) -> Result<(), DiscoveryEncodeError> {
        let bytes = value.as_bytes();
        if bytes.len() <= 31 {
            self.push(0xa0 | bytes.len() as u8)?;
        } else if bytes.len() <= u8::MAX as usize {
            self.push(0xd9)?;
            self.push(bytes.len() as u8)?;
        } else {
            return Err(DiscoveryEncodeError);
        }
        self.extend(bytes)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    fn info(location: Option<LocationE6>) -> RnodeDiscoveryInfo<'static> {
        RnodeDiscoveryInfo::new(
            [0x42; 16],
            "Metalbeard E290 3f88",
            true,
            location,
            915_000_000,
            125_000,
            7,
            5,
        )
        .unwrap()
    }

    #[test]
    fn location_world_bounds_are_explicit() {
        assert!(LocationE6::new(-90_000_000, 180_000_000).is_ok());
        assert_eq!(
            LocationE6::new(90_000_001, 0),
            Err(DiscoveryModelError::InvalidLatitude)
        );
        assert_eq!(
            LocationE6::new(0, -180_000_001),
            Err(DiscoveryModelError::InvalidLongitude)
        );
    }

    #[test]
    fn rnode_map_matches_python_umsgpack_shape_without_location() {
        let packed = encode_rnode_info(info(None)).unwrap();
        assert_eq!(
            hex::encode(packed.as_bytes()),
            concat!(
                "8b",
                "00ae524e6f6465496e74657266616365",
                "01c3",
                "ccfec41042424242424242424242424242424242",
                "ccffb44d6574616c626561726420453239302033663838",
                "03c0",
                "04c0",
                "05c0",
                "09ce3689cac0",
                "0ace0001e848",
                "0b07",
                "0c05"
            )
        );
    }

    #[test]
    fn fixed_location_is_encoded_as_float64_and_height_remains_nil() {
        let packed = encode_rnode_info(info(Some(
            LocationE6::new(42_123_456, -71_987_654).unwrap(),
        )))
        .unwrap();
        let bytes = packed.as_bytes();
        assert!(bytes.windows(2).any(|window| window == [0x03, 0xcb]));
        assert!(bytes.windows(2).any(|window| window == [0x04, 0xcb]));
        assert!(bytes.windows(2).any(|window| window == [0x05, 0xc0]));
    }

    #[test]
    fn incremental_stamp_search_finds_and_validates_low_cost_fixture() {
        let packed = encode_rnode_info(info(None)).unwrap();
        let mut search = DiscoveryStampSearch::new(&packed, 6).unwrap();
        let stamp = loop {
            match search.step(32) {
                DiscoveryStampProgress::Pending => {}
                DiscoveryStampProgress::Found(stamp) => break stamp,
                DiscoveryStampProgress::Exhausted => panic!("small test cost must be reachable"),
            }
        };
        let info_hash = packed.info_hash();
        let mut workblock = std::vec![0_u8; WORKBLOCK_BYTES];
        for round in 0..WORKBLOCK_EXPAND_ROUNDS {
            let mut salt_hash = Sha256::new();
            salt_hash.update(info_hash);
            let (encoded_round, encoded_len) = encode_msgpack_uint(round as u64);
            salt_hash.update(&encoded_round[..encoded_len]);
            let salt: [u8; 32] = salt_hash.finalize().into();
            Hkdf::<Sha256>::new(Some(&salt), &info_hash)
                .expand(&[], &mut workblock[round * 256..(round + 1) * 256])
                .unwrap();
        }
        assert!(stamp_valid(&workblock, &stamp, 6));
        let app_data = packed.with_stamp(stamp);
        assert_eq!(app_data.as_bytes()[0], 0);
        assert_eq!(app_data.packed_info(), packed.as_bytes());
        assert_eq!(app_data.stamp(), &stamp);
    }

    #[test]
    fn stamp_search_matches_python_lxmf_golden_vector() {
        // Generated with the checked-in Python Reticulum/LXMF references using
        // umsgpack, Identity.full_hash(), stamp_workblock(..., 20), and
        // LXStamper.stamp_valid(..., 16). The deterministic candidate stream is
        // local to this crate; candidate validity is the Python wire contract.
        let packed = encode_rnode_info(info(None)).unwrap();
        assert_eq!(
            hex::encode(packed.info_hash()),
            "a07e0c87254816ef7b7552b9e443228bbce68c2b28401817115556c61af8e475"
        );

        let mut search = DiscoveryStampSearch::new(&packed, 16).unwrap();
        let stamp = loop {
            match search.step(256) {
                DiscoveryStampProgress::Pending => {}
                DiscoveryStampProgress::Found(stamp) => break stamp,
                DiscoveryStampProgress::Exhausted => {
                    panic!("the Python golden candidate must be reachable")
                }
            }
        };

        assert_eq!(search.attempts(), 8_265);
        assert_eq!(
            hex::encode(stamp),
            "7a1c4e53182a48a5bc6664d71cad947b4ddf5ff26c0566318e4e4439156dae57"
        );
    }

    #[test]
    fn python_target_boundary_is_inclusive() {
        let mut equal_cost_eight = [0_u8; 32];
        equal_cost_eight[0] = 0x01;
        assert!(meets_python_target(&equal_cost_eight, 8));
        assert!(!meets_python_target(&equal_cost_eight, 9));
    }
}
