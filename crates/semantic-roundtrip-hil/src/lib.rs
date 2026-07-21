//! Board-independent sentinel, semantic-announce, and semantic round-trip HIL
//! fixtures.
//!
//! The default payloads are deliberately shorter than the minimum Reticulum
//! packet header. They prove only LoRa PHY and RNode physical framing
//! interoperability; they must never be reported as valid RNS. The explicit
//! `semantic-announce-hil` feature replaces that initiator payload with one
//! fixed, signed Python-RNS conformance fixture. `semantic-roundtrip-hil`
//! instead runs a four-packet, two-node announce/DATA/proof exchange with
//! fixed HIL identities and fresh DATA crypto. Physical board authorization is
//! deliberately outside this crate. All identities and deterministic inputs
//! here are test material and must not be used by product firmware.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use reticulum_radio_interface::RnodeFrameHeader;

#[cfg(any(feature = "semantic-announce-hil", feature = "semantic-roundtrip-hil"))]
use rand_core::{CryptoRng, RngCore};
#[cfg(any(feature = "semantic-announce-hil", feature = "semantic-roundtrip-hil"))]
use reticulum_rns_rete::PacketType;
#[cfg(feature = "semantic-roundtrip-hil")]
use reticulum_rns_rete::{
    AnnounceAdmissionError, DestHash, EmbeddedNode, EmbeddedNodeConfig, InboundProofPolicy, Packet,
    RNS_MTU, TxTarget,
};
#[cfg(feature = "semantic-announce-hil")]
use reticulum_rns_rete::{
    DestType, build_announce_packet, identity_from_private_key, parse_announce_packet,
};

/// Stable selector for the semantic initiator's test identity.
///
/// The value preserves the original Tracker fixture bytes for wire-vector
/// continuity. It is not authority to activate any physical board.
pub const SEMANTIC_INITIATOR_SELECTOR: [u8; 6] = [0x44, 0x1b, 0xf6, 0xf8, 0xe9, 0x44];

/// Stable selector for the semantic responder's test identity.
///
/// The value preserves the original Tracker fixture bytes for wire-vector
/// continuity. It is not authority to activate any physical board.
pub const SEMANTIC_RESPONDER_SELECTOR: [u8; 6] = [0x44, 0x1b, 0xf6, 0xf8, 0xe0, 0x40];

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

/// RNode sequence for the initiator's signed announce in the semantic round trip.
#[cfg(feature = "semantic-roundtrip-hil")]
pub const SEMANTIC_ROUNDTRIP_INITIATOR_ANNOUNCE_SEQUENCE: u8 = 9;

/// RNode sequence for the responder's signed announce in the semantic round trip.
#[cfg(feature = "semantic-roundtrip-hil")]
pub const SEMANTIC_ROUNDTRIP_RESPONDER_ANNOUNCE_SEQUENCE: u8 = 10;

/// RNode sequence for the initiator's encrypted destination-DATA packet.
#[cfg(feature = "semantic-roundtrip-hil")]
pub const SEMANTIC_ROUNDTRIP_DATA_SEQUENCE: u8 = 11;

/// RNode sequence for the responder's delivery proof.
#[cfg(feature = "semantic-roundtrip-hil")]
pub const SEMANTIC_ROUNDTRIP_PROOF_SEQUENCE: u8 = 12;

/// Committed destination hash of the fixed E9 initiator identity and name.
#[cfg(feature = "semantic-roundtrip-hil")]
pub const SEMANTIC_ROUNDTRIP_INITIATOR_DESTINATION_HASH: [u8; 16] = [
    0xaa, 0x2d, 0x77, 0xf6, 0x55, 0x18, 0xc7, 0x8a, 0xd1, 0x82, 0x1e, 0xe0, 0x56, 0x97, 0x6b, 0x2a,
];

/// Committed destination hash of the fixed E0 responder identity and name.
#[cfg(feature = "semantic-roundtrip-hil")]
pub const SEMANTIC_ROUNDTRIP_RESPONDER_DESTINATION_HASH: [u8; 16] = [
    0xfd, 0xc1, 0x99, 0x70, 0x55, 0xc1, 0x7c, 0xf3, 0xfb, 0xdb, 0x19, 0x2c, 0x55, 0xce, 0xb3, 0xef,
];

/// Exact application payload size for the semantic round-trip DATA packet.
#[cfg(feature = "semantic-roundtrip-hil")]
pub const SEMANTIC_ROUNDTRIP_PAYLOAD_LEN: usize = 36;

#[cfg(feature = "semantic-roundtrip-hil")]
const SEMANTIC_ROUNDTRIP_PAYLOAD_TAG: [u8; 4] = *b"RRH1";

#[cfg(feature = "semantic-roundtrip-hil")]
const SEMANTIC_ROUNDTRIP_INITIATOR_SEED: [u8; 32] = [0xe9; 32];

#[cfg(feature = "semantic-roundtrip-hil")]
const SEMANTIC_ROUNDTRIP_RESPONDER_SEED: [u8; 32] = [0xe0; 32];

#[cfg(feature = "semantic-roundtrip-hil")]
const SEMANTIC_ROUNDTRIP_APP_NAME: &str = "reticulum-rs-hil";

#[cfg(feature = "semantic-roundtrip-hil")]
const SEMANTIC_ROUNDTRIP_ASPECTS: &[&str] = &["semantic-roundtrip"];

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

/// Fixed-capacity Rete node used by both semantic round-trip HIL roles.
#[cfg(feature = "semantic-roundtrip-hil")]
pub type SemanticRoundtripNode = EmbeddedNode<4, 2, 8, 2>;

/// Construction or bounded announce-preparation failure in semantic round-trip mode.
#[cfg(feature = "semantic-roundtrip-hil")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticRoundtripError {
    /// The supplied MAC is not one of the two dedicated HIL boards.
    UnknownBaseMac,
    /// Rete rejected one of the fixed deterministic identity seeds.
    Identity,
    /// A fixed identity and expanded destination name drifted from its committed hash.
    IdentityBinding,
    /// Rete rejected construction of the fixed endpoint node.
    Node,
    /// The bounded native announce queue rejected the request.
    AnnounceAdmission(AnnounceAdmissionError),
    /// Flushing the announce queue did not produce exactly one broadcast packet.
    AnnounceOutput,
    /// The emitted packet was not the expected local signed announce.
    AnnounceValidation,
}

/// One semantic Reticulum packet in the fixed end-to-end exchange.
#[cfg(feature = "semantic-roundtrip-hil")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticRoundtripStep {
    /// Signed announce sent by the E9 initiator.
    InitiatorAnnounce,
    /// Signed announce sent by the E0 responder.
    ResponderAnnounce,
    /// Encrypted destination-DATA sent by the initiator.
    EncryptedData,
    /// Delivery proof generated by the responder.
    DeliveryProof,
}

#[cfg(feature = "semantic-roundtrip-hil")]
impl SemanticRoundtripStep {
    /// Exact four-bit RNode sequence assigned to this semantic packet.
    pub const fn sequence(self) -> u8 {
        match self {
            Self::InitiatorAnnounce => SEMANTIC_ROUNDTRIP_INITIATOR_ANNOUNCE_SEQUENCE,
            Self::ResponderAnnounce => SEMANTIC_ROUNDTRIP_RESPONDER_ANNOUNCE_SEQUENCE,
            Self::EncryptedData => SEMANTIC_ROUNDTRIP_DATA_SEQUENCE,
            Self::DeliveryProof => SEMANTIC_ROUNDTRIP_PROOF_SEQUENCE,
        }
    }
}

/// Local operation required to complete one semantic round-trip step.
#[cfg(feature = "semantic-roundtrip-hil")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticRoundtripAction {
    /// Construct, validate, frame and transmit this step.
    Transmit(SemanticRoundtripStep),
    /// Receive, reassemble and semantically validate this step.
    Receive(SemanticRoundtripStep),
}

/// Role-specific bounded state in the four-packet semantic round trip.
#[cfg(feature = "semantic-roundtrip-hil")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticRoundtripPhase {
    /// Initiator must transmit its announce.
    InitiatorSendAnnounce,
    /// Initiator must receive the responder announce.
    InitiatorAwaitResponderAnnounce,
    /// Initiator must transmit encrypted DATA.
    InitiatorSendData,
    /// Initiator must receive and correlate the proof.
    InitiatorAwaitProof,
    /// Responder must receive the initiator announce.
    ResponderAwaitInitiatorAnnounce,
    /// Responder must transmit its announce.
    ResponderSendAnnounce,
    /// Responder must receive and decrypt DATA.
    ResponderAwaitData,
    /// Responder must transmit the generated proof.
    ResponderSendProof,
    /// Every role-specific action has completed exactly once.
    Complete,
}

/// Fail-closed semantic state-machine transition error.
#[cfg(feature = "semantic-roundtrip-hil")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticRoundtripStateError {
    /// Unknown boards cannot enter the active semantic exchange.
    InertRole,
    /// The observed action was not the sole action allowed in this phase.
    UnexpectedAction {
        /// Sole action accepted before the rejected call.
        expected: Option<SemanticRoundtripAction>,
        /// Action offered by the caller.
        observed: SemanticRoundtripAction,
    },
}

/// Role-specific state machine admitting only the fixed four-packet order.
#[cfg(feature = "semantic-roundtrip-hil")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticRoundtripState {
    phase: SemanticRoundtripPhase,
}

#[cfg(feature = "semantic-roundtrip-hil")]
impl SemanticRoundtripState {
    /// Construct the initial phase for one exact active HIL role.
    pub const fn new(role: HilRole) -> Result<Self, SemanticRoundtripStateError> {
        let phase = match role {
            HilRole::Initiator => SemanticRoundtripPhase::InitiatorSendAnnounce,
            HilRole::Responder => SemanticRoundtripPhase::ResponderAwaitInitiatorAnnounce,
            HilRole::Inert => return Err(SemanticRoundtripStateError::InertRole),
        };
        Ok(Self { phase })
    }

    /// Current bounded phase.
    pub const fn phase(self) -> SemanticRoundtripPhase {
        self.phase
    }

    /// Sole operation accepted in the current phase, or `None` after completion.
    pub const fn expected(self) -> Option<SemanticRoundtripAction> {
        use SemanticRoundtripAction::{Receive, Transmit};
        use SemanticRoundtripPhase::{
            Complete, InitiatorAwaitProof, InitiatorAwaitResponderAnnounce, InitiatorSendAnnounce,
            InitiatorSendData, ResponderAwaitData, ResponderAwaitInitiatorAnnounce,
            ResponderSendAnnounce, ResponderSendProof,
        };
        use SemanticRoundtripStep::{
            DeliveryProof, EncryptedData, InitiatorAnnounce, ResponderAnnounce,
        };

        match self.phase {
            InitiatorSendAnnounce => Some(Transmit(InitiatorAnnounce)),
            InitiatorAwaitResponderAnnounce => Some(Receive(ResponderAnnounce)),
            InitiatorSendData => Some(Transmit(EncryptedData)),
            InitiatorAwaitProof => Some(Receive(DeliveryProof)),
            ResponderAwaitInitiatorAnnounce => Some(Receive(InitiatorAnnounce)),
            ResponderSendAnnounce => Some(Transmit(ResponderAnnounce)),
            ResponderAwaitData => Some(Receive(EncryptedData)),
            ResponderSendProof => Some(Transmit(DeliveryProof)),
            Complete => None,
        }
    }

    /// Commit one successfully transmitted or semantically validated operation.
    ///
    /// A mismatch returns an error without mutating the phase. The caller must
    /// invoke this only after the corresponding radio or Rete operation has
    /// succeeded.
    pub fn advance(
        &mut self,
        completed: SemanticRoundtripAction,
    ) -> Result<(), SemanticRoundtripStateError> {
        let expected = self.expected();
        if expected != Some(completed) {
            return Err(SemanticRoundtripStateError::UnexpectedAction {
                expected,
                observed: completed,
            });
        }

        self.phase = match self.phase {
            SemanticRoundtripPhase::InitiatorSendAnnounce => {
                SemanticRoundtripPhase::InitiatorAwaitResponderAnnounce
            }
            SemanticRoundtripPhase::InitiatorAwaitResponderAnnounce => {
                SemanticRoundtripPhase::InitiatorSendData
            }
            SemanticRoundtripPhase::InitiatorSendData => {
                SemanticRoundtripPhase::InitiatorAwaitProof
            }
            SemanticRoundtripPhase::InitiatorAwaitProof
            | SemanticRoundtripPhase::ResponderSendProof => SemanticRoundtripPhase::Complete,
            SemanticRoundtripPhase::ResponderAwaitInitiatorAnnounce => {
                SemanticRoundtripPhase::ResponderSendAnnounce
            }
            SemanticRoundtripPhase::ResponderSendAnnounce => {
                SemanticRoundtripPhase::ResponderAwaitData
            }
            SemanticRoundtripPhase::ResponderAwaitData => {
                SemanticRoundtripPhase::ResponderSendProof
            }
            SemanticRoundtripPhase::Complete => unreachable!("completed phase rejected above"),
        };
        Ok(())
    }

    /// Whether this role has completed every required operation.
    pub const fn is_complete(self) -> bool {
        matches!(self.phase, SemanticRoundtripPhase::Complete)
    }
}

/// Why a physical frame cannot represent its claimed semantic step.
#[cfg(feature = "semantic-roundtrip-hil")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticRoundtripFrameError {
    /// A physical frame did not contain both an RNode header and valid RNS bytes.
    Length,
    /// The RNode header contained the wrong sequence or any low-nibble flag.
    Header,
}

/// Validate the canonical one-frame RNode envelope for one semantic step.
#[cfg(feature = "semantic-roundtrip-hil")]
pub fn validate_semantic_roundtrip_frame(
    step: SemanticRoundtripStep,
    frame: &[u8],
) -> Result<(), SemanticRoundtripFrameError> {
    if frame.len() < RNS_MINIMUM_PACKET_LEN + 1
        || frame.len() > reticulum_radio_interface::SX1262_FRAME_MTU
    {
        return Err(SemanticRoundtripFrameError::Length);
    }
    if frame[0] != RnodeFrameHeader::encode(step.sequence(), false) {
        return Err(SemanticRoundtripFrameError::Header);
    }
    Ok(())
}

/// Build the compact DATA plaintext binding both exact HIL destinations.
#[cfg(feature = "semantic-roundtrip-hil")]
pub fn build_semantic_roundtrip_payload(
    initiator: &[u8; 16],
    responder: &[u8; 16],
) -> [u8; SEMANTIC_ROUNDTRIP_PAYLOAD_LEN] {
    let mut payload = [0_u8; SEMANTIC_ROUNDTRIP_PAYLOAD_LEN];
    payload[..4].copy_from_slice(&SEMANTIC_ROUNDTRIP_PAYLOAD_TAG);
    payload[4..20].copy_from_slice(initiator);
    payload[20..].copy_from_slice(responder);
    payload
}

/// Validate the exact compact DATA plaintext for both HIL destinations.
#[cfg(feature = "semantic-roundtrip-hil")]
pub fn validate_semantic_roundtrip_payload(
    payload: &[u8],
    initiator: &[u8; 16],
    responder: &[u8; 16],
) -> bool {
    payload == build_semantic_roundtrip_payload(initiator, responder)
}

/// Construct the fixed identity selected by an exact semantic fixture selector.
#[cfg(feature = "semantic-roundtrip-hil")]
pub fn semantic_roundtrip_identity_for_base_mac(
    mac: &[u8],
) -> Result<reticulum_rns_rete::Identity, SemanticRoundtripError> {
    let seed = if mac == SEMANTIC_INITIATOR_SELECTOR {
        &SEMANTIC_ROUNDTRIP_INITIATOR_SEED
    } else if mac == SEMANTIC_RESPONDER_SELECTOR {
        &SEMANTIC_ROUNDTRIP_RESPONDER_SEED
    } else {
        return Err(SemanticRoundtripError::UnknownBaseMac);
    };
    reticulum_rns_rete::Identity::from_seed(seed).map_err(|_| SemanticRoundtripError::Identity)
}

/// Construct the fixed endpoint node selected by a semantic fixture selector.
#[cfg(feature = "semantic-roundtrip-hil")]
pub fn semantic_roundtrip_node_for_base_mac(
    mac: &[u8],
) -> Result<SemanticRoundtripNode, SemanticRoundtripError> {
    let identity = semantic_roundtrip_identity_for_base_mac(mac)?;
    let mut node = SemanticRoundtripNode::new(
        identity,
        SEMANTIC_ROUNDTRIP_APP_NAME,
        SEMANTIC_ROUNDTRIP_ASPECTS,
        EmbeddedNodeConfig::endpoint(),
    )
    .map_err(|_| SemanticRoundtripError::Node)?;
    if node.destination_hash() != semantic_roundtrip_destination_for_base_mac(mac)? {
        return Err(SemanticRoundtripError::IdentityBinding);
    }
    node.set_inbound_proof_policy(InboundProofPolicy::Always);
    Ok(node)
}

/// Return the fixed local destination selected by a semantic fixture selector.
#[cfg(feature = "semantic-roundtrip-hil")]
pub fn semantic_roundtrip_destination_for_base_mac(
    mac: &[u8],
) -> Result<DestHash, SemanticRoundtripError> {
    if mac == SEMANTIC_INITIATOR_SELECTOR {
        Ok(DestHash::new(SEMANTIC_ROUNDTRIP_INITIATOR_DESTINATION_HASH))
    } else if mac == SEMANTIC_RESPONDER_SELECTOR {
        Ok(DestHash::new(SEMANTIC_ROUNDTRIP_RESPONDER_DESTINATION_HASH))
    } else {
        Err(SemanticRoundtripError::UnknownBaseMac)
    }
}

/// Return the other fixture role's fixed semantic HIL destination.
#[cfg(feature = "semantic-roundtrip-hil")]
pub fn semantic_roundtrip_peer_destination_for_base_mac(
    mac: &[u8],
) -> Result<DestHash, SemanticRoundtripError> {
    if mac == SEMANTIC_INITIATOR_SELECTOR {
        Ok(DestHash::new(SEMANTIC_ROUNDTRIP_RESPONDER_DESTINATION_HASH))
    } else if mac == SEMANTIC_RESPONDER_SELECTOR {
        Ok(DestHash::new(SEMANTIC_ROUNDTRIP_INITIATOR_DESTINATION_HASH))
    } else {
        Err(SemanticRoundtripError::UnknownBaseMac)
    }
}

/// Queue, flush and validate exactly one local signed announce into caller storage.
#[cfg(feature = "semantic-roundtrip-hil")]
pub fn prepare_semantic_roundtrip_announce<R: RngCore + CryptoRng>(
    node: &mut SemanticRoundtripNode,
    now: u64,
    rng: &mut R,
    output: &mut [u8; RNS_MTU],
) -> Result<usize, SemanticRoundtripError> {
    node.queue_announce(None, now, rng)
        .map_err(SemanticRoundtripError::AnnounceAdmission)?;
    let mut packets = node.flush_announces(now, rng);
    if packets.len() != 1 {
        return Err(SemanticRoundtripError::AnnounceOutput);
    }
    let packet = packets
        .pop()
        .ok_or(SemanticRoundtripError::AnnounceOutput)?;
    if packet.target() != TxTarget::All || packet.bytes().len() > output.len() {
        return Err(SemanticRoundtripError::AnnounceOutput);
    }
    let parsed =
        Packet::parse(packet.bytes()).map_err(|_| SemanticRoundtripError::AnnounceValidation)?;
    if parsed.packet_type != PacketType::Announce
        || parsed.destination_hash != node.destination_hash().as_ref()
    {
        return Err(SemanticRoundtripError::AnnounceValidation);
    }
    let packet_len = packet.bytes().len();
    output[..packet_len].copy_from_slice(packet.bytes());
    Ok(packet_len)
}

/// Role in a bounded two-peer HIL exchange.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HilRole {
    /// The selected fixture initiates the exchange after its startup delay.
    Initiator,
    /// The selected fixture listens first and responds within the exchange.
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
    #[cfg(feature = "semantic-roundtrip-hil")]
    use core::num::NonZeroU64;

    #[cfg(feature = "semantic-roundtrip-hil")]
    use reticulum_radio_interface::{FrameSignal, RNODE_HW_MTU, TimedReceiveOutcome, TimedRnodeRx};
    use reticulum_radio_interface::{SX1262_FRAME_MTU, frame_rns_packet};
    #[cfg(any(feature = "semantic-announce-hil", feature = "semantic-roundtrip-hil"))]
    use reticulum_rns_rete::RNS_MTU;
    #[cfg(feature = "semantic-roundtrip-hil")]
    use reticulum_rns_rete::{
        ApplicationEvent, IngressDisposition, InterfaceId, ReceiptCandidate,
        ReceiptReservationUnavailable, ReceiptTerminal, ReceiptTerminalReservation,
        ReceiptTerminalSink, TxTarget,
    };

    use super::*;

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

    #[cfg(feature = "semantic-roundtrip-hil")]
    #[derive(Default)]
    struct CounterRng(u8);

    #[cfg(feature = "semantic-roundtrip-hil")]
    impl RngCore for CounterRng {
        fn next_u32(&mut self) -> u32 {
            let mut bytes = [0_u8; 4];
            self.fill_bytes(&mut bytes);
            u32::from_le_bytes(bytes)
        }

        fn next_u64(&mut self) -> u64 {
            let mut bytes = [0_u8; 8];
            self.fill_bytes(&mut bytes);
            u64::from_le_bytes(bytes)
        }

        fn fill_bytes(&mut self, destination: &mut [u8]) {
            for byte in destination {
                self.0 = self.0.wrapping_add(1);
                *byte = self.0;
            }
        }

        fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
            self.fill_bytes(destination);
            Ok(())
        }
    }

    #[cfg(feature = "semantic-roundtrip-hil")]
    impl CryptoRng for CounterRng {}

    #[cfg(feature = "semantic-roundtrip-hil")]
    #[derive(Default)]
    struct OneReceiptSink {
        terminal: Option<ReceiptTerminal>,
    }

    #[cfg(feature = "semantic-roundtrip-hil")]
    struct OneReceiptReservation<'a> {
        candidate: ReceiptCandidate,
        terminal: &'a mut Option<ReceiptTerminal>,
    }

    #[cfg(feature = "semantic-roundtrip-hil")]
    impl ReceiptTerminalSink for OneReceiptSink {
        type Reservation<'a> = OneReceiptReservation<'a>;

        fn try_reserve(
            &mut self,
            candidate: ReceiptCandidate,
        ) -> Result<Self::Reservation<'_>, ReceiptReservationUnavailable> {
            if self.terminal.is_some() {
                return Err(ReceiptReservationUnavailable);
            }
            Ok(OneReceiptReservation {
                candidate,
                terminal: &mut self.terminal,
            })
        }
    }

    #[cfg(feature = "semantic-roundtrip-hil")]
    impl ReceiptTerminalReservation for OneReceiptReservation<'_> {
        fn commit(self, terminal: ReceiptTerminal) {
            assert_eq!(terminal.candidate(), self.candidate);
            *self.terminal = Some(terminal);
        }
    }

    #[cfg(feature = "semantic-roundtrip-hil")]
    fn pump_single_frame(
        packet: &[u8],
        step: SemanticRoundtripStep,
        now: u64,
        receiver: &mut TimedRnodeRx,
        output: &mut [u8; RNODE_HW_MTU],
    ) -> usize {
        let mut first = [0_u8; SX1262_FRAME_MTU];
        let mut second = [0_u8; SX1262_FRAME_MTU];
        let frames = frame_rns_packet(packet, step.sequence(), &mut first, &mut second).unwrap();
        assert_eq!(frames.second(), None, "{step:?} unexpectedly split");
        validate_semantic_roundtrip_frame(step, frames.first()).unwrap();
        match receiver
            .feed(frames.first(), now, FrameSignal::new(-70, 8), output)
            .unwrap()
        {
            TimedReceiveOutcome::Packet {
                packet_len,
                discarded_pending,
                ..
            } => {
                assert!(!discarded_pending);
                assert_eq!(&output[..packet_len], packet);
                packet_len
            }
            TimedReceiveOutcome::AwaitingContinuation { .. } => {
                panic!("{step:?} unexpectedly awaited a continuation")
            }
        }
    }

    #[cfg(feature = "semantic-roundtrip-hil")]
    #[test]
    fn semantic_roundtrip_pumps_two_real_nodes_through_four_single_rnode_frames() {
        let mut initiator =
            semantic_roundtrip_node_for_base_mac(&SEMANTIC_INITIATOR_SELECTOR).unwrap();
        let mut responder =
            semantic_roundtrip_node_for_base_mac(&SEMANTIC_RESPONDER_SELECTOR).unwrap();
        let initiator_destination = initiator.destination_hash();
        let responder_destination = responder.destination_hash();
        assert_eq!(
            *initiator_destination.as_bytes(),
            SEMANTIC_ROUNDTRIP_INITIATOR_DESTINATION_HASH
        );
        assert_eq!(
            *responder_destination.as_bytes(),
            SEMANTIC_ROUNDTRIP_RESPONDER_DESTINATION_HASH
        );
        assert_eq!(
            semantic_roundtrip_destination_for_base_mac(&SEMANTIC_INITIATOR_SELECTOR).unwrap(),
            initiator_destination
        );
        assert_eq!(
            semantic_roundtrip_destination_for_base_mac(&SEMANTIC_RESPONDER_SELECTOR).unwrap(),
            responder_destination
        );
        assert_eq!(
            semantic_roundtrip_peer_destination_for_base_mac(&SEMANTIC_INITIATOR_SELECTOR).unwrap(),
            responder_destination
        );
        assert_eq!(
            semantic_roundtrip_peer_destination_for_base_mac(&SEMANTIC_RESPONDER_SELECTOR).unwrap(),
            initiator_destination
        );
        assert_ne!(initiator_destination, responder_destination);

        let payload = build_semantic_roundtrip_payload(
            initiator_destination.as_bytes(),
            responder_destination.as_bytes(),
        );
        assert!(validate_semantic_roundtrip_payload(
            &payload,
            initiator_destination.as_bytes(),
            responder_destination.as_bytes(),
        ));

        let mut initiator_state = SemanticRoundtripState::new(HilRole::Initiator).unwrap();
        let mut responder_state = SemanticRoundtripState::new(HilRole::Responder).unwrap();
        let timeout = NonZeroU64::new(10).unwrap();
        let mut initiator_rx = TimedRnodeRx::new(timeout);
        let mut responder_rx = TimedRnodeRx::new(timeout);
        let mut packet = [0_u8; RNS_MTU];
        let mut reassembled = [0_u8; RNODE_HW_MTU];
        let mut rng = CounterRng::default();

        assert_eq!(
            initiator_state.expected(),
            Some(SemanticRoundtripAction::Transmit(
                SemanticRoundtripStep::InitiatorAnnounce
            ))
        );
        let announce_len =
            prepare_semantic_roundtrip_announce(&mut initiator, 100, &mut rng, &mut packet)
                .unwrap();
        initiator_state
            .advance(SemanticRoundtripAction::Transmit(
                SemanticRoundtripStep::InitiatorAnnounce,
            ))
            .unwrap();
        let packet_len = pump_single_frame(
            &packet[..announce_len],
            SemanticRoundtripStep::InitiatorAnnounce,
            100,
            &mut responder_rx,
            &mut reassembled,
        );
        let report = responder.ingest(&reassembled[..packet_len], 100, InterfaceId(1), &mut rng);
        assert_eq!(report.disposition, IngressDisposition::Processed);
        assert_eq!(report.actions.events.len(), 1);
        assert!(matches!(
            report.actions.events.first(),
            Some(ApplicationEvent::AnnounceReceived { destination, .. })
                if *destination == *initiator_destination.as_bytes()
        ));
        assert!(report.actions.packets.is_empty());
        assert_eq!(report.actions.unroutable_packets, 0);
        assert!(responder.route(&initiator_destination).is_some());
        responder_state
            .advance(SemanticRoundtripAction::Receive(
                SemanticRoundtripStep::InitiatorAnnounce,
            ))
            .unwrap();

        let announce_len =
            prepare_semantic_roundtrip_announce(&mut responder, 101, &mut rng, &mut packet)
                .unwrap();
        responder_state
            .advance(SemanticRoundtripAction::Transmit(
                SemanticRoundtripStep::ResponderAnnounce,
            ))
            .unwrap();
        let packet_len = pump_single_frame(
            &packet[..announce_len],
            SemanticRoundtripStep::ResponderAnnounce,
            101,
            &mut initiator_rx,
            &mut reassembled,
        );
        let report = initiator.ingest(&reassembled[..packet_len], 101, InterfaceId(1), &mut rng);
        assert_eq!(report.disposition, IngressDisposition::Processed);
        assert_eq!(report.actions.events.len(), 1);
        assert!(matches!(
            report.actions.events.first(),
            Some(ApplicationEvent::AnnounceReceived { destination, .. })
                if *destination == *responder_destination.as_bytes()
        ));
        assert!(report.actions.packets.is_empty());
        assert_eq!(report.actions.unroutable_packets, 0);
        assert!(initiator.route(&responder_destination).is_some());
        initiator_state
            .advance(SemanticRoundtripAction::Receive(
                SemanticRoundtripStep::ResponderAnnounce,
            ))
            .unwrap();

        let prepared = initiator
            .prepare_data_into(&responder_destination, &payload, 102, &mut rng, &mut packet)
            .unwrap();
        let data_len = usize::from(prepared.packet_len());
        initiator_state
            .advance(SemanticRoundtripAction::Transmit(
                SemanticRoundtripStep::EncryptedData,
            ))
            .unwrap();
        let packet_len = pump_single_frame(
            &packet[..data_len],
            SemanticRoundtripStep::EncryptedData,
            102,
            &mut responder_rx,
            &mut reassembled,
        );
        let received = responder.ingest(&reassembled[..packet_len], 102, InterfaceId(1), &mut rng);
        assert_eq!(received.disposition, IngressDisposition::Processed);
        assert_eq!(received.actions.events.len(), 1);
        assert!(matches!(
            received.actions.events.first(),
            Some(ApplicationEvent::DataReceived { destination, payload: received })
                if *destination == *responder_destination.as_bytes() && received == &payload
        ));
        assert_eq!(received.actions.packets.len(), 1);
        assert_eq!(received.actions.unroutable_packets, 0);
        responder_state
            .advance(SemanticRoundtripAction::Receive(
                SemanticRoundtripStep::EncryptedData,
            ))
            .unwrap();
        let proof = &received.actions.packets[0];
        assert_eq!(proof.target(), TxTarget::Only(InterfaceId(1)));
        assert_eq!(
            Packet::parse(proof.bytes()).unwrap().packet_type,
            PacketType::Proof
        );
        responder_state
            .advance(SemanticRoundtripAction::Transmit(
                SemanticRoundtripStep::DeliveryProof,
            ))
            .unwrap();

        let packet_len = pump_single_frame(
            proof.bytes(),
            SemanticRoundtripStep::DeliveryProof,
            103,
            &mut initiator_rx,
            &mut reassembled,
        );
        let mut sink = OneReceiptSink::default();
        let proof_report = initiator
            .ingest_with_receipt_sink(
                &reassembled[..packet_len],
                103,
                InterfaceId(1),
                &mut rng,
                &mut sink,
            )
            .unwrap();
        assert_eq!(proof_report.disposition, IngressDisposition::Processed);
        assert!(proof_report.actions.events.is_empty());
        assert!(proof_report.actions.packets.is_empty());
        assert_eq!(proof_report.actions.unroutable_packets, 0);
        let terminal = sink.terminal.expect("valid proof must deliver the receipt");
        assert!(matches!(
            terminal,
            ReceiptTerminal::Delivered(candidate) if candidate.receipt() == prepared.receipt()
        ));
        assert_eq!(initiator.metrics().capacity.receipts.used, 0);
        initiator_state
            .advance(SemanticRoundtripAction::Receive(
                SemanticRoundtripStep::DeliveryProof,
            ))
            .unwrap();
        assert!(initiator_state.is_complete());
        assert!(responder_state.is_complete());
        assert_eq!(initiator.metrics().capacity.receipts.used, 0);
    }

    #[cfg(feature = "semantic-roundtrip-hil")]
    #[test]
    fn semantic_roundtrip_policy_rejects_unknown_roles_and_out_of_order_actions() {
        assert_eq!(
            SemanticRoundtripState::new(HilRole::Inert),
            Err(SemanticRoundtripStateError::InertRole)
        );
        assert!(matches!(
            semantic_roundtrip_node_for_base_mac(&[0_u8; 6]),
            Err(SemanticRoundtripError::UnknownBaseMac)
        ));
        assert!(matches!(
            semantic_roundtrip_destination_for_base_mac(&[0_u8; 6]),
            Err(SemanticRoundtripError::UnknownBaseMac)
        ));
        assert!(matches!(
            semantic_roundtrip_peer_destination_for_base_mac(&[0_u8; 6]),
            Err(SemanticRoundtripError::UnknownBaseMac)
        ));
        assert_eq!(
            semantic_roundtrip_destination_for_base_mac(&SEMANTIC_INITIATOR_SELECTOR)
                .unwrap()
                .as_bytes(),
            &SEMANTIC_ROUNDTRIP_INITIATOR_DESTINATION_HASH
        );
        assert_eq!(
            semantic_roundtrip_peer_destination_for_base_mac(&SEMANTIC_INITIATOR_SELECTOR)
                .unwrap()
                .as_bytes(),
            &SEMANTIC_ROUNDTRIP_RESPONDER_DESTINATION_HASH
        );

        let expected_payload = build_semantic_roundtrip_payload(
            &SEMANTIC_ROUNDTRIP_INITIATOR_DESTINATION_HASH,
            &SEMANTIC_ROUNDTRIP_RESPONDER_DESTINATION_HASH,
        );
        let mut changed_payload = expected_payload;
        changed_payload[SEMANTIC_ROUNDTRIP_PAYLOAD_LEN - 1] ^= 1;
        assert!(!validate_semantic_roundtrip_payload(
            &changed_payload,
            &SEMANTIC_ROUNDTRIP_INITIATOR_DESTINATION_HASH,
            &SEMANTIC_ROUNDTRIP_RESPONDER_DESTINATION_HASH,
        ));

        let mut envelope = [0_u8; RNS_MINIMUM_PACKET_LEN + 1];
        envelope[0] = RnodeFrameHeader::encode(SEMANTIC_ROUNDTRIP_DATA_SEQUENCE, false);
        validate_semantic_roundtrip_frame(SemanticRoundtripStep::EncryptedData, &envelope).unwrap();
        envelope[0] = RnodeFrameHeader::encode(SEMANTIC_ROUNDTRIP_DATA_SEQUENCE, true);
        assert_eq!(
            validate_semantic_roundtrip_frame(SemanticRoundtripStep::EncryptedData, &envelope),
            Err(SemanticRoundtripFrameError::Header)
        );
        assert_eq!(
            validate_semantic_roundtrip_frame(
                SemanticRoundtripStep::EncryptedData,
                &envelope[..RNS_MINIMUM_PACKET_LEN]
            ),
            Err(SemanticRoundtripFrameError::Length)
        );

        let mut state = SemanticRoundtripState::new(HilRole::Initiator).unwrap();
        let before = state;
        let wrong = SemanticRoundtripAction::Receive(SemanticRoundtripStep::DeliveryProof);
        assert!(matches!(
            state.advance(wrong),
            Err(SemanticRoundtripStateError::UnexpectedAction { observed, .. })
                if observed == wrong
        ));
        assert_eq!(state, before);

        let exact = [
            SemanticRoundtripAction::Transmit(SemanticRoundtripStep::InitiatorAnnounce),
            SemanticRoundtripAction::Receive(SemanticRoundtripStep::ResponderAnnounce),
            SemanticRoundtripAction::Transmit(SemanticRoundtripStep::EncryptedData),
            SemanticRoundtripAction::Receive(SemanticRoundtripStep::DeliveryProof),
        ];
        for action in exact {
            state.advance(action).unwrap();
        }
        assert!(state.is_complete());
        assert!(state.advance(exact[3]).is_err());
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
