//! Bounded semantic projection of authenticated LXMF delivery announces.

use std::{fmt, str};

use reticulum_device_api::LxmfDiscoveredPeer;
use reticulum_lxmf_chat_app::LxmfSession;
use reticulum_lxmf_wire::{MessagePackKind, WireLimits, validate_messagepack_value};
use serde::Serialize;
use ts_rs::TS;

use crate::{JsonSafeInteger, MAX_JSON_SAFE_INTEGER, serialize_json_safe_u64};

/// Maximum retained peers returned by one app refresh.
///
/// The first firmware profile retains fewer entries. This independent host
/// ceiling prevents a later or malformed device from turning a refresh into
/// unbounded memory use or authenticated request traffic.
pub const MAX_NEARBY_PEERS: usize = 64;

const MAX_NEARBY_PAGE_REQUESTS: usize = 96;
const MAX_INCARNATION_RESETS: usize = 2;
const ANNOUNCE_LIMITS: WireLimits = WireLimits::new(256, 256, 32, 128, 2_048, 8);
const NOMAD_NODE_EXPANDED_NAME: &str = "nomadnetwork.node";

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
    interface_id: u8,
    interface_name: Option<String>,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    observed_age_ms: u64,
    rssi_dbm: Option<i16>,
    snr_db: Option<i16>,
}

impl NearbyPeerView {
    fn from_peer(peer: &LxmfDiscoveredPeer) -> Self {
        Self {
            destination: hex::encode(peer.destination().0),
            associated_nomad_destination: associated_nomad_destination(
                peer.identity_hash().as_bytes(),
            ),
            display_name: decode_display_name(peer.app_data()),
            hops: peer.hops(),
            identity_hash: hex::encode(peer.identity_hash().as_bytes()),
            interface_id: peer.interface_id(),
            interface_name: interface_name(peer.interface_id()).map(str::to_owned),
            observed_age_ms: peer.observed_age_ms().min(MAX_JSON_SAFE_INTEGER),
            rssi_dbm: peer.rssi_dbm(),
            snr_db: peer.snr_db(),
        }
    }
}

fn associated_nomad_destination(identity_hash: &[u8; 16]) -> String {
    let identity_hash = rete_core::IdentityHash::new(*identity_hash);
    hex::encode(
        rete_core::destination_hash(NOMAD_NODE_EXPANDED_NAME, Some(&identity_hash)).as_bytes(),
    )
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum NearbyReadError {
    Session(String),
    IncarnationResetWithoutGap,
    IncarnationChurn,
    NonAdvancingCursor,
    PeerLimitExceeded { limit: usize },
    PageRequestLimitExceeded { limit: usize },
}

impl fmt::Display for NearbyReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Session(error) => write!(formatter, "nearby peer request failed: {error}"),
            Self::IncarnationResetWithoutGap => {
                formatter.write_str("nearby peer incarnation reset omitted its history-gap marker")
            }
            Self::IncarnationChurn => {
                formatter.write_str("nearby peer incarnation changed repeatedly during one refresh")
            }
            Self::NonAdvancingCursor => {
                formatter.write_str("nearby peer page did not advance its cursor")
            }
            Self::PeerLimitExceeded { limit } => {
                write!(
                    formatter,
                    "nearby peer projection exceeds client limit {limit}"
                )
            }
            Self::PageRequestLimitExceeded { limit } => {
                write!(
                    formatter,
                    "nearby peer refresh exceeds request limit {limit}"
                )
            }
        }
    }
}

pub(crate) fn read_nearby_peers(
    session: &mut (dyn LxmfSession<Error = reticulum_lxmf_chat_app::DeviceSessionError> + Send),
) -> Result<Vec<NearbyPeerView>, NearbyReadError> {
    let mut peers = Vec::new();
    let mut cursor = None;
    let mut incarnation = None;
    let mut incarnation_resets = 0_usize;

    for _ in 0..MAX_NEARBY_PAGE_REQUESTS {
        let requested = cursor;
        let page = session
            .next_nearby_peer(requested)
            .map_err(|error| NearbyReadError::Session(error.to_string()))?;
        let next = page.next_cursor();
        let next_incarnation = next.incarnation();

        if let Some(previous_incarnation) = incarnation
            && previous_incarnation != next_incarnation
        {
            if !page.history_gap() {
                return Err(NearbyReadError::IncarnationResetWithoutGap);
            }
            incarnation_resets += 1;
            if incarnation_resets > MAX_INCARNATION_RESETS {
                return Err(NearbyReadError::IncarnationChurn);
            }
            // The device treats a foreign boot cursor as a reset request and
            // returns the first record from its current incarnation. Discard
            // the obsolete partial view, then accept that record directly.
            peers.clear();
        }
        incarnation = Some(next_incarnation);

        let Some(peer) = page.peer() else {
            return Ok(peers);
        };
        if requested.is_some_and(|requested| {
            requested.incarnation() == next_incarnation
                && next.after_generation() <= requested.after_generation()
        }) {
            return Err(NearbyReadError::NonAdvancingCursor);
        }

        let projected = NearbyPeerView::from_peer(peer);
        if let Some(existing) = peers
            .iter_mut()
            .find(|existing| existing.destination == projected.destination)
        {
            // A same-boot update can move an existing peer beyond our cursor.
            // Keep its newest retained observation without duplicating it.
            *existing = projected;
        } else {
            if peers.len() == MAX_NEARBY_PEERS {
                return Err(NearbyReadError::PeerLimitExceeded {
                    limit: MAX_NEARBY_PEERS,
                });
            }
            peers.push(projected);
        }
        cursor = Some(next);
    }

    Err(NearbyReadError::PageRequestLimitExceeded {
        limit: MAX_NEARBY_PAGE_REQUESTS,
    })
}

fn interface_name(interface_id: u8) -> Option<&'static str> {
    match interface_id {
        // Interface slot one is the primary LoRa/RNode actor in the current
        // E290 product registry. Unknown future slots remain explicit scalars
        // until the device API exports the registry's human-readable label.
        1 => Some("LoRa"),
        _ => None,
    }
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
    use std::collections::VecDeque;

    use reticulum_device_api::{
        DestinationHash, IdentityHash, LxmfDiscoveredPeer, LxmfPeerDiscoveryCursor,
        LxmfPeerDiscoveryIncarnation, LxmfPeerDiscoveryPage, LxmfPeerGeneration,
    };
    use reticulum_lxmf_chat_app::{DeviceSessionError, InboxCursor, InboxSummary};
    use reticulum_lxmf_chat_core::{
        AcceptanceIds, DeviceBinding, InboundMessage, OutboxMaterial, SubmissionId, SubmissionState,
    };

    use super::*;

    const fn incarnation(tag: u8) -> LxmfPeerDiscoveryIncarnation {
        LxmfPeerDiscoveryIncarnation::new([tag; 8])
    }

    fn peer(tag: u8, generation: u64, app_data: &[u8]) -> LxmfDiscoveredPeer {
        LxmfDiscoveredPeer::new(
            DestinationHash([tag; 16]),
            IdentityHash::new([tag.wrapping_add(1); 16]),
            app_data,
            tag,
            1,
            Some(-100 + i16::from(tag)),
            Some(i16::from(tag)),
            125,
            LxmfPeerGeneration::new(generation).unwrap(),
        )
        .unwrap()
    }

    fn page(
        incarnation: LxmfPeerDiscoveryIncarnation,
        generation: u64,
        history_gap: bool,
        peer: Option<LxmfDiscoveredPeer>,
    ) -> LxmfPeerDiscoveryPage {
        LxmfPeerDiscoveryPage::new(
            LxmfPeerDiscoveryCursor::new(incarnation, generation),
            LxmfPeerGeneration::new(generation).ok(),
            peer.as_ref().map(LxmfDiscoveredPeer::generation),
            history_gap,
            peer,
        )
    }

    struct ScriptedSession {
        pages: VecDeque<LxmfPeerDiscoveryPage>,
        requested: Vec<Option<LxmfPeerDiscoveryCursor>>,
    }

    impl ScriptedSession {
        fn new(pages: impl IntoIterator<Item = LxmfPeerDiscoveryPage>) -> Self {
            Self {
                pages: pages.into_iter().collect(),
                requested: Vec::new(),
            }
        }
    }

    impl LxmfSession for ScriptedSession {
        type Error = DeviceSessionError;

        fn binding(&mut self) -> Result<DeviceBinding, Self::Error> {
            unreachable!()
        }

        fn submit(&mut self, _material: &OutboxMaterial) -> Result<AcceptanceIds, Self::Error> {
            unreachable!()
        }

        fn submission_status(&mut self, _id: SubmissionId) -> Result<SubmissionState, Self::Error> {
            unreachable!()
        }

        fn next_inbox(
            &mut self,
            _after: Option<InboxCursor>,
        ) -> Result<Option<InboxSummary>, Self::Error> {
            unreachable!()
        }

        fn read_inbox(&mut self, _summary: InboxSummary) -> Result<InboundMessage, Self::Error> {
            unreachable!()
        }

        fn next_nearby_peer(
            &mut self,
            after: Option<LxmfPeerDiscoveryCursor>,
        ) -> Result<LxmfPeerDiscoveryPage, Self::Error> {
            self.requested.push(after);
            Ok(self.pages.pop_front().expect("scripted page remains"))
        }

        fn nomad_fetch_start(
            &mut self,
            _request: reticulum_device_api::NomadFetchStartRequest<'_>,
        ) -> Result<reticulum_device_api::NomadFetchStartAccepted, Self::Error> {
            unreachable!("nearby projection does not perform NomadNet fetches")
        }

        fn nomad_fetch_poll(
            &mut self,
            _id: reticulum_device_api::NomadFetchId,
        ) -> Result<reticulum_device_api::NomadFetchPollResponse, Self::Error> {
            unreachable!("nearby projection does not perform NomadNet fetches")
        }

        fn is_usable(&self) -> bool {
            true
        }
    }

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
    fn paging_keeps_same_incarnation_history_gaps_and_decodes_semantics() {
        let boot = incarnation(1);
        let mut session = ScriptedSession::new([
            page(
                boot,
                2,
                true,
                Some(peer(
                    0x21,
                    2,
                    &[0x93, 0xc4, 0x04, b'R', b'i', b'd', b'g', 0xc0, 0x90],
                )),
            ),
            page(boot, 4, true, Some(peer(0x22, 4, b"Legacy"))),
            page(boot, 4, false, None),
        ]);

        let peers = read_nearby_peers(&mut session).unwrap();
        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0].destination, "21".repeat(16));
        assert_eq!(peers[0].identity_hash, "22".repeat(16));
        assert_eq!(
            peers[0].associated_nomad_destination,
            associated_nomad_destination(&[0x22; 16])
        );
        assert_eq!(peers[0].display_name.as_deref(), Some("Ridg"));
        assert_eq!(peers[0].interface_name.as_deref(), Some("LoRa"));
        assert_eq!(peers[1].display_name.as_deref(), Some("Legacy"));
        assert_eq!(
            session.requested,
            vec![
                None,
                Some(LxmfPeerDiscoveryCursor::new(boot, 2)),
                Some(LxmfPeerDiscoveryCursor::new(boot, 4)),
            ]
        );
    }

    #[test]
    fn boot_incarnation_change_discards_the_obsolete_partial_view() {
        let old_boot = incarnation(1);
        let new_boot = incarnation(2);
        let mut session = ScriptedSession::new([
            page(old_boot, 7, false, Some(peer(0x31, 7, b"old"))),
            page(new_boot, 1, true, Some(peer(0x41, 1, b"new"))),
            page(new_boot, 1, false, None),
        ]);

        let peers = read_nearby_peers(&mut session).unwrap();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].destination, "41".repeat(16));
        assert_eq!(peers[0].display_name.as_deref(), Some("new"));
    }

    #[test]
    fn a_same_boot_peer_update_replaces_instead_of_duplicating() {
        let boot = incarnation(1);
        let mut updated = peer(0x51, 3, b"new");
        // Make the replacement observably different beyond its generation.
        updated = LxmfDiscoveredPeer::new(
            updated.destination(),
            updated.identity_hash(),
            b"new",
            9,
            7,
            None,
            None,
            1,
            updated.generation(),
        )
        .unwrap();
        let mut session = ScriptedSession::new([
            page(boot, 1, false, Some(peer(0x51, 1, b"old"))),
            page(boot, 3, true, Some(updated)),
            page(boot, 3, false, None),
        ]);

        let peers = read_nearby_peers(&mut session).unwrap();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].display_name.as_deref(), Some("new"));
        assert_eq!(peers[0].hops, 9);
        assert_eq!(peers[0].interface_name, None);
    }

    #[test]
    fn semantic_json_contains_no_announce_or_cursor_bytes() {
        let view = NearbyPeerView::from_peer(&peer(
            0x61,
            1,
            &[0x93, 0xc4, 0x04, b'N', b'o', b'd', b'e', 0xc0, 0x90],
        ));
        assert_eq!(
            serde_json::to_value(view).unwrap(),
            serde_json::json!({
                "associated_nomad_destination": "61755e5fc78bf685c2187ea8253f382c",
                "destination": "61".repeat(16),
                "display_name": "Node",
                "hops": 97,
                "identity_hash": "62".repeat(16),
                "interface_id": 1,
                "interface_name": "LoRa",
                "observed_age_ms": 125,
                "rssi_dbm": -3,
                "snr_db": 97,
            })
        );
    }
}
