//! Bounded semantic projection of authenticated LXMF delivery announces.

use std::str;

use reticulum_device_api::LxmfDiscoveredPeer;
use reticulum_lxmf_wire::{MessagePackKind, WireLimits, validate_messagepack_value};
use serde::Serialize;
use ts_rs::TS;

use crate::{JsonSafeInteger, MAX_JSON_SAFE_INTEGER, serialize_json_safe_u64};

/// Maximum app-local accepted LXMF destinations retained by the native node.
pub const MAX_NEARBY_PEERS: usize = 64;

/// Reticulum node whose accepted announce projection supplied an observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum NearbyPeerObserverKind {
    /// The app-owned PRNS node running on this phone.
    Phone,
    /// The currently authenticated appliance PRNS node.
    Appliance,
}

/// One authenticated LXMF delivery announce observed by the app-owned PRNS node.
///
/// This is an internal adapter boundary rather than a persisted product model.
/// PRNS has already authenticated the announce before this value is created.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NearbyPeerObservation {
    /// Complete LXMF delivery destination hash.
    pub destination: [u8; 16],
    /// Identity authenticated by the announce signature.
    pub identity_hash: [u8; 16],
    /// Exact bounded announce application data.
    pub app_data: Vec<u8>,
    /// Reticulum hop count reported by PRNS.
    pub hops: u8,
    /// Complete PRNS identity of the receiving packet interface.
    pub interface_id: [u8; 8],
    /// Elapsed local monotonic time since this process observed the announce.
    pub observed_age_ms: u64,
}

const ANNOUNCE_LIMITS: WireLimits = WireLimits::new(256, 256, 32, 128, 2_048, 8);
/// Display-safe public facts from one authenticated LXMF delivery announce.
///
/// The destination and identity hash are complete lowercase hexadecimal
/// values. Announce application bytes, Reticulum public keys, cursors, and
/// protocol parsing never cross the Rust/application boundary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[allow(missing_docs)]
pub struct NearbyPeerView {
    destination: String,
    associated_nomad_destination: String,
    display_name: Option<String>,
    hops: u8,
    identity_hash: String,
    interface_id: [u8; 8],
    interface_name: Option<String>,
    observer_kind: NearbyPeerObserverKind,
    observer_management_destination: Option<String>,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    observed_age_ms: u64,
}

impl NearbyPeerView {
    /// Project one app-local accepted PRNS announce into display-safe facts.
    #[must_use]
    pub fn from_observation(observation: NearbyPeerObservation) -> Self {
        let interface = personal_rns::interfaces::InterfaceId::new(observation.interface_id);
        Self {
            destination: hex::encode(observation.destination),
            associated_nomad_destination: associated_nomad_destination(&observation.identity_hash),
            display_name: decode_display_name(&observation.app_data),
            hops: observation.hops,
            identity_hash: hex::encode(observation.identity_hash),
            interface_id: observation.interface_id,
            interface_name: interface.kind().map(|kind| kind.name().to_owned()),
            observer_kind: NearbyPeerObserverKind::Phone,
            observer_management_destination: None,
            observed_age_ms: observation.observed_age_ms.min(MAX_JSON_SAFE_INTEGER),
        }
    }

    /// Project one authenticated announce retained by the active appliance.
    #[must_use]
    pub fn from_appliance_peer(peer: &LxmfDiscoveredPeer, management_destination: &str) -> Self {
        let interface_id = *peer.interface_id().as_bytes();
        let interface = personal_rns::interfaces::InterfaceId::new(interface_id);
        Self {
            destination: hex::encode(peer.destination().0),
            associated_nomad_destination: associated_nomad_destination(
                peer.identity_hash().as_bytes(),
            ),
            display_name: decode_display_name(peer.app_data()),
            hops: peer.hops(),
            identity_hash: hex::encode(peer.identity_hash().as_bytes()),
            interface_id,
            interface_name: interface.kind().map(|kind| kind.name().to_owned()),
            observer_kind: NearbyPeerObserverKind::Appliance,
            observer_management_destination: Some(management_destination.to_owned()),
            observed_age_ms: peer.observed_age_ms().min(MAX_JSON_SAFE_INTEGER),
        }
    }
}

fn associated_nomad_destination(identity_hash: &[u8; 16]) -> String {
    use personal_rns::identity::IdentityHash;
    use personal_rns::routing::announce::{derive_destination_hash, expand_name};

    let dotted_name_hash = expand_name("nomadnetwork", &["node"])
        .expect("the static NomadNet destination name is valid");
    hex::encode(
        derive_destination_hash(&IdentityHash::new(*identity_hash), &dotted_name_hash).as_bytes(),
    )
}

fn decode_display_name(app_data: &[u8]) -> Option<String> {
    let raw_name = match app_data.first().copied()? {
        0x90..=0x9f | 0xdc | 0xdd => {
            let value = validate_messagepack_value(app_data, ANNOUNCE_LIMITS).ok()?;
            if value.kind() != MessagePackKind::Array {
                return None;
            }
            let (items, first_offset) = array_header(app_data)?;
            if items == 0 {
                return None;
            }
            display_value(&app_data[first_offset..])?
        }
        _ => app_data,
    };

    let decoded = str::from_utf8(raw_name).ok()?;
    sanitize_display_name(decoded)
}

fn sanitize_display_name(decoded: &str) -> Option<String> {
    let mut sanitized = String::with_capacity(decoded.len());
    let mut separator_pending = false;

    for character in decoded.chars() {
        if is_bidi_or_invisible_format_control(character) {
            continue;
        }
        if character.is_whitespace() || character.is_control() {
            separator_pending = !sanitized.is_empty();
            continue;
        }
        if separator_pending {
            sanitized.push(' ');
            separator_pending = false;
        }
        sanitized.push(character);
    }

    (!sanitized.is_empty()).then_some(sanitized)
}

const fn is_bidi_or_invisible_format_control(character: char) -> bool {
    matches!(
        character,
        // Arabic Letter Mark, left/right marks, embeddings, overrides and
        // isolates can reorder a self-asserted name around trusted UI text.
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
            // Strip common zero-width/deprecated formatting controls as well;
            // none carries useful display-name content.
            | '\u{200b}'..='\u{200d}'
            | '\u{2060}'..='\u{2065}'
            | '\u{206a}'..='\u{206f}'
            | '\u{feff}'
    )
}

fn array_header(bytes: &[u8]) -> Option<(usize, usize)> {
    match *bytes.first()? {
        marker @ 0x90..=0x9f => Some((usize::from(marker & 0x0f), 1)),
        0xdc => Some((
            usize::from(u16::from_be_bytes([*bytes.get(1)?, *bytes.get(2)?])),
            3,
        )),
        0xdd => Some((
            usize::try_from(u32::from_be_bytes([
                *bytes.get(1)?,
                *bytes.get(2)?,
                *bytes.get(3)?,
                *bytes.get(4)?,
            ]))
            .ok()?,
            5,
        )),
        _ => None,
    }
}

fn display_value(bytes: &[u8]) -> Option<&[u8]> {
    let marker = *bytes.first()?;
    match marker {
        0xc0 => None,
        0xa0..=0xbf => payload(bytes, 1, usize::from(marker & 0x1f)),
        0xc4 | 0xd9 => payload(bytes, 2, usize::from(*bytes.get(1)?)),
        0xc5 | 0xda => payload(
            bytes,
            3,
            usize::from(u16::from_be_bytes([*bytes.get(1)?, *bytes.get(2)?])),
        ),
        0xc6 | 0xdb => payload(
            bytes,
            5,
            usize::try_from(u32::from_be_bytes([
                *bytes.get(1)?,
                *bytes.get(2)?,
                *bytes.get(3)?,
                *bytes.get(4)?,
            ]))
            .ok()?,
        ),
        _ => None,
    }
}

fn payload(bytes: &[u8], offset: usize, length: usize) -> Option<&[u8]> {
    bytes.get(offset..offset.checked_add(length)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_matches_current_lxmf_and_legacy_formats() {
        assert_eq!(
            decode_display_name(&[
                0x93, 0xc4, 0x0d, b' ', b'F', b'i', b'e', b'l', b'd', 0, b' ', b'n', b'o', b'd',
                b'e', b' ', 0xc0, 0x91, 0x00,
            ])
            .as_deref(),
            Some("Field node")
        );
        assert_eq!(
            decode_display_name(b" Legacy peer ").as_deref(),
            Some("Legacy peer")
        );
        assert_eq!(decode_display_name(&[0x93, 0xc0, 0xc0, 0x90]), None);
    }

    #[test]
    fn associated_nomad_destination_matches_the_reticulum_reference_vector() {
        let identity_hash =
            hex::decode("1234567890abcdef1234567890abcdef").expect("identity vector is hex");
        assert_eq!(
            associated_nomad_destination(identity_hash.as_slice().try_into().unwrap()),
            "02ecdd8cf33b06e43d0eacf26da44162"
        );
    }

    #[test]
    fn malformed_or_non_utf8_display_metadata_is_omitted() {
        assert_eq!(decode_display_name(&[]), None);
        assert_eq!(decode_display_name(&[0x93, 0xc4, 0x02, 0xff]), None);
        assert_eq!(decode_display_name(&[0x93, 0xc4, 0x01, b'a']), None);
        assert_eq!(decode_display_name(&[0x91, 0x01]), None);
    }

    #[test]
    fn hostile_display_formatting_is_removed_before_the_app_boundary() {
        assert_eq!(
            sanitize_display_name(" \u{202e}Alice\u{2066}\r\n\t\u{200f} Relay\u{0007} \u{2069} ")
                .as_deref(),
            Some("Alice Relay")
        );
        assert_eq!(
            sanitize_display_name("\u{202e}\u{2066}\r\n\t\u{0000}\u{2069}"),
            None
        );
        assert_eq!(
            decode_display_name(b"  Ridge\r\n\t relay\x07  ").as_deref(),
            Some("Ridge relay")
        );
    }

    #[test]
    fn semantic_json_contains_no_announce_or_cursor_bytes() {
        let view = NearbyPeerView::from_observation(NearbyPeerObservation {
            destination: [0x61; 16],
            identity_hash: [0x62; 16],
            app_data: vec![0x93, 0xc4, 0x04, b'N', b'o', b'd', b'e', 0xc0, 0x90],
            hops: 1,
            interface_id: [12, 0, 0, 0, 0, 0, 0, 1],
            observed_age_ms: 125,
        });
        assert_eq!(
            serde_json::to_value(view).unwrap(),
            serde_json::json!({
                "associated_nomad_destination": "61755e5fc78bf685c2187ea8253f382c",
                "destination": "61".repeat(16),
                "display_name": "Node",
                "hops": 1,
                "identity_hash": "62".repeat(16),
                "interface_id": [12, 0, 0, 0, 0, 0, 0, 1],
                "interface_name": "bluetooth-auto",
                "observer_kind": "phone",
                "observer_management_destination": null,
                "observed_age_ms": 125,
            })
        );
    }

    #[test]
    fn appliance_projection_preserves_explicit_observer_provenance() {
        let peer = LxmfDiscoveredPeer::new(
            reticulum_device_api::DestinationHash([0x71; 16]),
            reticulum_device_api::IdentityHash::new([0x72; 16]),
            b"Ridge",
            1,
            reticulum_device_api::ReticulumInterfaceId::new([14, 1, 2, 3, 4, 5, 6, 7]),
            Some(-91),
            Some(7),
            500,
            reticulum_device_api::LxmfPeerGeneration::new(1).unwrap(),
        )
        .unwrap();

        let view = NearbyPeerView::from_appliance_peer(&peer, "11111111111111111111111111111111");
        let json = serde_json::to_value(view).unwrap();
        assert_eq!(json["observer_kind"], "appliance");
        assert_eq!(
            json["observer_management_destination"],
            "11111111111111111111111111111111"
        );
        assert_eq!(json["interface_name"], "lora");
    }
}
