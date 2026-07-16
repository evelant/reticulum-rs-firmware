//! Pure role, sentinel-frame and semantic-announce policy for the two-board
//! Tracker TX HIL.
//!
//! The default payloads are deliberately shorter than the minimum Reticulum
//! packet header. They prove only LoRa PHY and RNode physical framing
//! interoperability; they must never be reported as valid RNS. The explicit
//! `semantic-announce-hil` feature replaces that initiator payload with one
//! fixed, signed Python-RNS conformance fixture. Its key, time and entropy are
//! test material and must not be used by product firmware.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use reticulum_radio_interface::RnodeFrameHeader;

#[cfg(feature = "semantic-announce-hil")]
use rand_core::{CryptoRng, RngCore};
#[cfg(feature = "semantic-announce-hil")]
use reticulum_rns_rete::{
    DestType, PacketType, build_announce_packet, identity_from_private_key, parse_announce_packet,
};

/// Factory eFuse base MAC of the dedicated Rust HIL initiator.
pub const INITIATOR_BASE_MAC: [u8; 6] = [0x44, 0x1b, 0xf6, 0xf8, 0xe9, 0x44];

/// Factory eFuse base MAC of the dedicated Rust HIL responder.
pub const RESPONDER_BASE_MAC: [u8; 6] = [0x44, 0x1b, 0xf6, 0xf8, 0xe0, 0x40];

/// Fixed sequence carried in the upper nibble of the ping frame header.
pub const HIL_PING_SEQUENCE: u8 = 9;

/// Fixed sequence carried in the upper nibble of the reply frame header.
pub const HIL_REPLY_SEQUENCE: u8 = 10;

/// Minimum byte length of a valid Reticulum HEADER_1 packet.
pub const RNS_MINIMUM_PACKET_LEN: usize = 19;

/// Recognizable HIL ping body, intentionally too short to be valid RNS.
pub const HIL_PING_PAYLOAD: &[u8] = b"RETICULUM-HIL-PING";

/// Recognizable HIL reply body, intentionally too short to be valid RNS.
pub const HIL_REPLY_PAYLOAD: &[u8] = b"RETICULUM-HIL-PONG";

/// Exact byte length of the deterministic signed announce fixture.
#[cfg(feature = "semantic-announce-hil")]
pub const SEMANTIC_ANNOUNCE_PACKET_LEN: usize = 167;

/// Destination hash of the deterministic signed announce fixture.
#[cfg(feature = "semantic-announce-hil")]
pub const SEMANTIC_ANNOUNCE_DESTINATION_HASH: [u8; 16] = [
    0x2b, 0x7f, 0xa6, 0x84, 0x27, 0x83, 0x25, 0x29, 0x74, 0xdc, 0x5f, 0xca, 0xff, 0x22, 0xb8, 0x08,
];

/// Full packet hash of the deterministic signed announce fixture.
#[cfg(feature = "semantic-announce-hil")]
pub const SEMANTIC_ANNOUNCE_PACKET_HASH: [u8; 32] = [
    0xb6, 0x37, 0x05, 0xcf, 0x3e, 0xd5, 0x2d, 0x56, 0xe3, 0x2e, 0x8e, 0x17, 0xfb, 0xd8, 0x6f, 0x51,
    0xf3, 0x91, 0xb9, 0xce, 0x86, 0xa1, 0xa3, 0x8f, 0x0f, 0x36, 0x49, 0xc0, 0x58, 0xe7, 0x4c, 0xae,
];

#[cfg(feature = "semantic-announce-hil")]
const SEMANTIC_ANNOUNCE_PRIVATE_KEY: [u8; 64] = [
    0x40, 0x8b, 0x27, 0xd3, 0x09, 0x7e, 0xea, 0x5a, 0x46, 0xbf, 0x2a, 0xb6, 0x43, 0x3a, 0x72, 0x34,
    0xa3, 0x3d, 0x5e, 0x49, 0x95, 0x7b, 0x13, 0xec, 0x7a, 0xcc, 0x2c, 0xa0, 0x8e, 0x1a, 0x13, 0xc7,
    0x52, 0x72, 0xc9, 0x0c, 0x8d, 0x33, 0x85, 0xd4, 0x7e, 0xde, 0x54, 0x20, 0xa7, 0xa9, 0x62, 0x3a,
    0xad, 0x81, 0x7d, 0x9f, 0x8a, 0x70, 0xbd, 0x10, 0x0a, 0x0a, 0xce, 0xa7, 0x40, 0x0d, 0xaa, 0x59,
];

#[cfg(feature = "semantic-announce-hil")]
const SEMANTIC_ANNOUNCE_NAME_HASH: [u8; 10] =
    [0xfc, 0xa7, 0x09, 0xa4, 0x81, 0x8d, 0x4e, 0x0c, 0x78, 0xa0];

#[cfg(feature = "semantic-announce-hil")]
const SEMANTIC_ANNOUNCE_RANDOM_HASH: [u8; 10] =
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x65, 0x53, 0xf1, 0x00];

const _: () = assert!(HIL_PING_PAYLOAD.len() < RNS_MINIMUM_PACKET_LEN);
const _: () = assert!(HIL_REPLY_PAYLOAD.len() < RNS_MINIMUM_PACKET_LEN);

/// Fail-closed construction or validation stage for the semantic fixture.
#[cfg(feature = "semantic-announce-hil")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticAnnounceError {
    /// Rete rejected the fixed 64-byte private-key fixture.
    Identity,
    /// Rete could not construct the announce in the caller-owned buffer.
    Build,
    /// The constructed packet did not have the committed byte length.
    Length,
    /// Rete could not parse or cryptographically validate its constructed bytes.
    Validation,
    /// Parsed header fields differed from the committed broadcast announce.
    Header,
    /// Parsed destination or full packet hash differed from the committed vector.
    Hash,
    /// Validated identity, name, random hash, ratchet or application data drifted.
    Fields,
}

#[cfg(feature = "semantic-announce-hil")]
struct ZeroRng;

#[cfg(feature = "semantic-announce-hil")]
impl RngCore for ZeroRng {
    fn next_u32(&mut self) -> u32 {
        0
    }

    fn next_u64(&mut self) -> u64 {
        0
    }

    fn fill_bytes(&mut self, destination: &mut [u8]) {
        destination.fill(0);
    }

    fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(destination);
        Ok(())
    }
}

#[cfg(feature = "semantic-announce-hil")]
impl CryptoRng for ZeroRng {}

/// Build and cryptographically validate the exact signed announce fixture.
///
/// This function exists only in the explicit semantic HIL graph. It uses a
/// published test key, zero entropy and a fixed 2023 Unix timestamp so the
/// output is byte-identical to the committed Python-RNS 1.3.8 vector. A caller
/// must provide at least [`reticulum_rns_rete::RNS_MTU`] bytes. Product
/// firmware must instead use a persisted private identity, qualified entropy
/// and nondecreasing wall time.
#[cfg(feature = "semantic-announce-hil")]
pub fn build_semantic_announce_packet(out: &mut [u8]) -> Result<usize, SemanticAnnounceError> {
    let identity = identity_from_private_key(&SEMANTIC_ANNOUNCE_PRIVATE_KEY)
        .map_err(|_| SemanticAnnounceError::Identity)?;
    let packet_len = build_announce_packet(
        &identity,
        "testapp",
        &["aspect1"],
        None,
        &mut ZeroRng,
        1_700_000_000,
        out,
    )
    .map_err(|_| SemanticAnnounceError::Build)?;

    if packet_len != SEMANTIC_ANNOUNCE_PACKET_LEN {
        return Err(SemanticAnnounceError::Length);
    }

    let announce =
        parse_announce_packet(&out[..packet_len]).map_err(|_| SemanticAnnounceError::Validation)?;
    let packet = &announce.packet;
    if packet.flags != 0x01
        || packet.hops != 0
        || packet.packet_type != PacketType::Announce
        || packet.dest_type != DestType::Single
        || packet.context_flag
        || packet.transport_type != 0
        || packet.context != 0
    {
        return Err(SemanticAnnounceError::Header);
    }
    if packet.destination_hash != SEMANTIC_ANNOUNCE_DESTINATION_HASH
        || packet.compute_hash() != SEMANTIC_ANNOUNCE_PACKET_HASH
    {
        return Err(SemanticAnnounceError::Hash);
    }

    let public_key = identity.public_key();
    if announce.fields.identity_hash != identity.hash()
        || announce.fields.pub_key != public_key
        || announce.fields.name_hash != SEMANTIC_ANNOUNCE_NAME_HASH
        || announce.fields.random_hash != SEMANTIC_ANNOUNCE_RANDOM_HASH
        || announce.fields.ratchet.is_some()
        || announce.fields.app_data.is_some()
    {
        return Err(SemanticAnnounceError::Fields);
    }

    Ok(packet_len)
}

/// Reset-scoped behavior authorized for an exact factory eFuse base MAC.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HilRole {
    /// Send one ping after the fixed startup delay, then listen for one reply.
    Initiator,
    /// Listen for the exact ping and send at most one exact reply.
    Responder,
    /// Never construct or enable the radio.
    Inert,
}

impl HilRole {
    /// Stable log label for the selected role.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Initiator => "initiator",
            Self::Responder => "responder",
            Self::Inert => "inert-unknown-mac",
        }
    }
}

/// Exact HIL sentinel recognized after RNode physical framing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HilFrameKind {
    /// Exact canonical one-frame ping.
    Ping,
    /// Exact canonical one-frame reply.
    Reply,
    /// Any other physical frame, including otherwise valid RNode traffic.
    Other,
}

/// Select the only role authorized for an exact six-byte eFuse base MAC.
pub fn role_for_base_mac(mac: &[u8]) -> HilRole {
    if mac == INITIATOR_BASE_MAC {
        HilRole::Initiator
    } else if mac == RESPONDER_BASE_MAC {
        HilRole::Responder
    } else {
        HilRole::Inert
    }
}

/// Classify one complete physical LoRa frame against the exact HIL sentinels.
///
/// The one-byte RNode header is part of `frame`. Reserved flags, the split
/// flag, a different sequence, added bytes, or any payload change all reject
/// the sentinel.
pub fn classify_hil_frame(frame: &[u8]) -> HilFrameKind {
    if is_exact_single_frame(frame, HIL_PING_SEQUENCE, HIL_PING_PAYLOAD) {
        HilFrameKind::Ping
    } else if is_exact_single_frame(frame, HIL_REPLY_SEQUENCE, HIL_REPLY_PAYLOAD) {
        HilFrameKind::Reply
    } else {
        HilFrameKind::Other
    }
}

fn is_exact_single_frame(frame: &[u8], sequence: u8, payload: &[u8]) -> bool {
    frame.len() == payload.len() + 1
        && frame[0] == RnodeFrameHeader::encode(sequence, false)
        && frame[1..] == *payload
}

#[cfg(test)]
mod tests {
    use reticulum_radio_interface::{SX1262_FRAME_MTU, frame_rns_packet};
    #[cfg(feature = "semantic-announce-hil")]
    use reticulum_rns_rete::RNS_MTU;

    use super::*;

    #[test]
    fn only_the_two_exact_factory_base_macs_are_active() {
        assert_eq!(role_for_base_mac(&INITIATOR_BASE_MAC), HilRole::Initiator);
        assert_eq!(role_for_base_mac(&RESPONDER_BASE_MAC), HilRole::Responder);

        let mut near_miss = INITIATOR_BASE_MAC;
        near_miss[5] ^= 1;
        assert_eq!(role_for_base_mac(&near_miss), HilRole::Inert);
        assert_eq!(role_for_base_mac(&INITIATOR_BASE_MAC[..5]), HilRole::Inert);
        assert_eq!(role_for_base_mac(&[]), HilRole::Inert);
    }

    #[test]
    fn sentinels_are_provably_too_short_to_be_rns_packets() {
        assert_eq!(HIL_PING_PAYLOAD.len(), 18);
        assert_eq!(HIL_REPLY_PAYLOAD.len(), 18);
        assert!(HIL_PING_PAYLOAD.len() < RNS_MINIMUM_PACKET_LEN);
        assert!(HIL_REPLY_PAYLOAD.len() < RNS_MINIMUM_PACKET_LEN);
    }

    #[test]
    fn canonical_framer_outputs_are_the_only_exact_sentinels() {
        let mut first = [0_u8; SX1262_FRAME_MTU];
        let mut second = [0_u8; SX1262_FRAME_MTU];
        let ping =
            frame_rns_packet(HIL_PING_PAYLOAD, HIL_PING_SEQUENCE, &mut first, &mut second).unwrap();
        assert_eq!(ping.second(), None);
        assert_eq!(classify_hil_frame(ping.first()), HilFrameKind::Ping);

        let mut first = [0_u8; SX1262_FRAME_MTU];
        let mut second = [0_u8; SX1262_FRAME_MTU];
        let reply = frame_rns_packet(
            HIL_REPLY_PAYLOAD,
            HIL_REPLY_SEQUENCE,
            &mut first,
            &mut second,
        )
        .unwrap();
        assert_eq!(reply.second(), None);
        assert_eq!(classify_hil_frame(reply.first()), HilFrameKind::Reply);
    }

    #[test]
    fn header_payload_and_length_changes_are_rejected() {
        let mut exact = [0_u8; 1 + HIL_PING_PAYLOAD.len()];
        exact[0] = RnodeFrameHeader::encode(HIL_PING_SEQUENCE, false);
        exact[1..].copy_from_slice(HIL_PING_PAYLOAD);
        assert_eq!(classify_hil_frame(&exact), HilFrameKind::Ping);

        let mut changed = exact;
        changed[0] |= 1;
        assert_eq!(classify_hil_frame(&changed), HilFrameKind::Other);

        let mut changed = exact;
        changed[0] = RnodeFrameHeader::encode(HIL_PING_SEQUENCE + 1, false);
        assert_eq!(classify_hil_frame(&changed), HilFrameKind::Other);

        let mut changed = exact;
        changed[changed.len() - 1] ^= 1;
        assert_eq!(classify_hil_frame(&changed), HilFrameKind::Other);

        assert_eq!(
            classify_hil_frame(&exact[..exact.len() - 1]),
            HilFrameKind::Other
        );
    }

    #[cfg(feature = "semantic-announce-hil")]
    #[test]
    fn semantic_announce_is_the_exact_python_rns_vector_and_one_physical_frame() {
        let expected = decode_hex::<SEMANTIC_ANNOUNCE_PACKET_LEN>(concat!(
            "01002b7fa6842783252974dc5fcaff22b80800",
            "80ffd69d6399c09c790748a2783b9bd5198652b2e14d496eaf4d29ce06a0ea0f",
            "a175c596dc0558fd271c185e89f2c85f8bc490c0e7dd25da0b0142246da9628f",
            "fca709a4818d4e0c78a00000000000006553f100",
            "50fe696f35b4fc3c4e43e2269372ae2b603ac90dd64757c8ac224bb80f0cabd",
            "4e2863f7bc593cd3a785d360ba48485fad67a39617880214dd16086c6e53d8205",
        ));
        let mut raw = [0_u8; RNS_MTU];
        let packet_len = build_semantic_announce_packet(&mut raw).unwrap();
        assert_eq!(packet_len, SEMANTIC_ANNOUNCE_PACKET_LEN);
        assert_eq!(&raw[..packet_len], expected);

        let mut first = [0_u8; SX1262_FRAME_MTU];
        let mut second = [0_u8; SX1262_FRAME_MTU];
        let frames = frame_rns_packet(
            &raw[..packet_len],
            HIL_PING_SEQUENCE,
            &mut first,
            &mut second,
        )
        .unwrap();
        assert_eq!(frames.second(), None);
        assert_eq!(frames.first().len(), SEMANTIC_ANNOUNCE_PACKET_LEN + 1);
        assert_eq!(frames.first()[0], RnodeFrameHeader::encode(9, false));
        assert_eq!(&frames.first()[1..], expected);
    }

    #[cfg(feature = "semantic-announce-hil")]
    #[test]
    fn semantic_announce_construction_fails_closed_on_short_storage() {
        let mut short = [0_u8; SEMANTIC_ANNOUNCE_PACKET_LEN - 1];
        assert_eq!(
            build_semantic_announce_packet(&mut short),
            Err(SemanticAnnounceError::Build)
        );
    }

    #[cfg(feature = "semantic-announce-hil")]
    fn decode_hex<const N: usize>(encoded: &str) -> [u8; N] {
        assert_eq!(encoded.len(), N * 2);
        let encoded = encoded.as_bytes();
        let mut decoded = [0_u8; N];
        let mut index = 0;
        while index < N {
            decoded[index] = (nibble(encoded[index * 2]) << 4) | nibble(encoded[index * 2 + 1]);
            index += 1;
        }
        decoded
    }

    #[cfg(feature = "semantic-announce-hil")]
    fn nibble(byte: u8) -> u8 {
        match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            _ => panic!("fixture contains a non-lowercase-hex byte"),
        }
    }
}
