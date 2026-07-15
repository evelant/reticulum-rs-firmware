use rand_core::{CryptoRng, RngCore};
use reticulum_rns_rete::{
    DestHash, EmbeddedNodeConfig, Identity, InitialEmbeddedNode, Packet, PacketType, TxTarget,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const SOURCE_PATH: &str = "tools/phase1-rx-local-data/src/generator.rs";
pub const SOURCE_BYTES: &[u8] = include_bytes!("generator.rs");

const APP_NAME: &str = "reticulum-rs-firmware";
const ASPECTS: &[&str] = &["heltec-tracker-v2", "lab-rx"];
const PLAINTEXT: &[u8] = b"phase1 boot-bound local DATA action suppression";
const NOW_SECONDS: u64 = 1;

#[derive(Clone, Copy, Debug)]
pub struct Inputs<'a> {
    pub base_corpus: &'a [u8],
    pub source_sha256: &'a str,
}

#[derive(Clone, Debug)]
pub struct Generated {
    pub corpus_bytes: Vec<u8>,
    pub packet: Vec<u8>,
}

struct HilDeterministicRng {
    key: [u8; 32],
    counter: u64,
    block: [u8; 32],
    cursor: usize,
}

impl HilDeterministicRng {
    fn new(target_public_key: &[u8; 64]) -> Self {
        let key: [u8; 32] = Sha256::new()
            .chain_update(b"reticulum-phase1-local-data/rng/v1")
            .chain_update(target_public_key)
            .finalize()
            .into();
        Self {
            key,
            counter: 0,
            block: [0; 32],
            cursor: 32,
        }
    }

    fn refill(&mut self) {
        self.block = Sha256::new()
            .chain_update(b"reticulum-phase1-local-data/block/v1")
            .chain_update(self.key)
            .chain_update(self.counter.to_be_bytes())
            .finalize()
            .into();
        self.counter = self.counter.wrapping_add(1);
        self.cursor = 0;
    }
}

impl RngCore for HilDeterministicRng {
    fn next_u32(&mut self) -> u32 {
        let mut bytes = [0; 4];
        self.fill_bytes(&mut bytes);
        u32::from_le_bytes(bytes)
    }

    fn next_u64(&mut self) -> u64 {
        let mut bytes = [0; 8];
        self.fill_bytes(&mut bytes);
        u64::from_le_bytes(bytes)
    }

    fn fill_bytes(&mut self, destination: &mut [u8]) {
        let mut written = 0;
        while written < destination.len() {
            if self.cursor == self.block.len() {
                self.refill();
            }
            let available = self.block.len() - self.cursor;
            let count = available.min(destination.len() - written);
            destination[written..written + count]
                .copy_from_slice(&self.block[self.cursor..self.cursor + count]);
            self.cursor += count;
            written += count;
        }
    }

    fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(destination);
        Ok(())
    }
}

impl CryptoRng for HilDeterministicRng {}

pub fn generate(
    target_public_key: [u8; 64],
    target_destination_hash: [u8; 16],
    inputs: Inputs<'_>,
) -> Result<Generated, String> {
    validate_sha256(inputs.source_sha256)?;
    let target = Identity::from_public_key(&target_public_key)
        .map_err(|error| format!("invalid target public identity: {error}"))?;
    let sender_seed: [u8; 32] = Sha256::new()
        .chain_update(b"reticulum-phase1-local-data/sender/v1")
        .chain_update(target_public_key)
        .finalize()
        .into();
    let sender_identity = Identity::from_seed(&sender_seed)
        .map_err(|error| format!("could not construct HIL sender identity: {error}"))?;
    let mut sender = InitialEmbeddedNode::new(
        sender_identity,
        APP_NAME,
        &["phase1-local-data-sender"],
        EmbeddedNodeConfig::endpoint(),
    )
    .map_err(|error| format!("could not construct HIL sender node: {error}"))?;
    sender
        .register_peer(&target, APP_NAME, ASPECTS, NOW_SECONDS)
        .map_err(|error| format!("could not register target peer: {error}"))?;

    let destination = DestHash::from(target_destination_hash);
    if sender.route(&destination).is_none() {
        return Err(format!(
            "target destination hash {} does not match the public key and {}.{}",
            hex::encode(target_destination_hash),
            APP_NAME,
            ASPECTS.join(".")
        ));
    }

    let mut rng = HilDeterministicRng::new(&target_public_key);
    let packet = sender
        .send_data(&destination, PLAINTEXT, NOW_SECONDS, &mut rng)
        .map_err(|error| format!("could not build local DATA packet: {error}"))?;
    if packet.target() != TxTarget::All {
        return Err("local DATA fixture unexpectedly acquired non-broadcast routing".to_owned());
    }
    let packet = packet.bytes().to_vec();
    let parsed = Packet::parse(&packet)
        .map_err(|error| format!("generated packet did not parse: {error}"))?;
    if parsed.packet_type != PacketType::Data || parsed.destination_hash != target_destination_hash
    {
        return Err("generated packet does not address the requested DATA destination".to_owned());
    }

    let base: Value = serde_json::from_slice(inputs.base_corpus)
        .map_err(|error| format!("committed base RNode corpus is invalid: {error}"))?;
    if base["schema"] != json!(3) {
        return Err("committed base RNode corpus has an unsupported schema".to_owned());
    }
    let packet_sha256 = sha256_hex(&packet);
    let corpus = json!({
        "schema": 3,
        "protocol": "RNode LoRa framing",
        "lane": "phase-1-rx-hil",
        "peer": base["peer"].clone(),
        "generator": {
            "tool": SOURCE_PATH,
            "source_sha256": inputs.source_sha256,
            "rete_revision": reticulum_rns_rete::SOURCE_REVISION,
            "deterministic": true,
            "security": "HIL-only predictable entropy; packet payload is non-secret"
        },
        "wire_contract": base["wire_contract"].clone(),
        "target_boot": {
            "public_key_hex": hex::encode(target_public_key),
            "destination_hash_hex": hex::encode(target_destination_hash),
            "destination_name": format!("{APP_NAME}.{}", ASPECTS.join("."))
        },
        "scenarios": [{
            "name": "boot-local-data",
            "description": "Deterministic encrypted DATA addressed to the ephemeral identity from one exact Tracker boot.",
            "steps": [{
                "mode": "rnode_packet",
                "payload_hex": hex::encode(&packet),
                "payload_len": packet.len(),
                "payload_sha256": packet_sha256,
                "wait_after": {"kind": "fixed", "milliseconds": 250}
            }],
            "unstalled_reference_deltas": {
                "completed_packets": [{
                    "packet_len": packet.len(),
                    "packet_sha256": sha256_hex(&packet),
                    "rns_admitted": true,
                    "rete_disposition": "processed"
                }],
                "pending_started": u64::from(packet.len() > 254),
                "pending_replaced": 0,
                "pending_discarded": 0,
                "pending_expired": 0,
                "packets_too_long": 0,
                "suppressed_events": 1
            },
            "plaintext_hex": hex::encode(PLAINTEXT)
        }]
    });
    let mut corpus_bytes = serde_json::to_vec_pretty(&corpus)
        .map_err(|error| format!("could not encode boot-local corpus: {error}"))?;
    corpus_bytes.push(b'\n');
    Ok(Generated {
        corpus_bytes,
        packet,
    })
}

fn validate_sha256(value: &str) -> Result<(), String> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(format!("invalid canonical SHA-256 digest {value:?}"))
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use reticulum_rns_rete::{
        EmbeddedNodeConfig, Identity, IngressDisposition, InterfaceId, NodeEvent,
    };

    use super::*;

    fn embedded_inputs() -> Inputs<'static> {
        Inputs {
            base_corpus: include_bytes!("../../../interop/vectors/rnode-hil-v1.json"),
            source_sha256: "0000000000000000000000000000000000000000000000000000000000000000",
        }
    }

    #[test]
    fn boot_bound_packet_is_deterministic_and_decrypts_only_for_target() {
        let target_seed = b"phase1 local data test receiver";
        let receiver_identity = Identity::from_seed(target_seed).unwrap();
        let public_key = receiver_identity.public_key();
        let mut receiver = InitialEmbeddedNode::new(
            Identity::from_seed(target_seed).unwrap(),
            APP_NAME,
            ASPECTS,
            EmbeddedNodeConfig::endpoint(),
        )
        .unwrap();
        let destination: [u8; 16] = *receiver.destination_hash().as_bytes();

        let first = generate(public_key, destination, embedded_inputs()).unwrap();
        let second = generate(public_key, destination, embedded_inputs()).unwrap();
        assert_eq!(first.packet, second.packet);
        assert_eq!(first.corpus_bytes, second.corpus_bytes);
        let corpus: Value = serde_json::from_slice(&first.corpus_bytes).unwrap();
        assert_eq!(corpus["schema"], json!(3));
        assert!(corpus["scenarios"][0]["expected_deltas"].is_null());
        assert!(corpus["scenarios"][0]["unstalled_reference_deltas"].is_object());

        let mut rng = HilDeterministicRng::new(&public_key);
        let report = receiver.ingest(&first.packet, 2, InterfaceId(7), &mut rng);
        assert_eq!(report.disposition, IngressDisposition::Processed);
        assert!(matches!(
            report.actions.events.as_slice(),
            [NodeEvent::DataReceived { dest_hash, payload }]
                if *dest_hash == DestHash::from(destination) && payload == PLAINTEXT
        ));
    }

    #[test]
    fn destination_mismatch_is_rejected() {
        let target = Identity::from_seed(b"phase1 mismatched target").unwrap();
        let error = generate(target.public_key(), [0xA5; 16], embedded_inputs()).unwrap_err();
        assert!(error.contains("does not match"));
    }
}
