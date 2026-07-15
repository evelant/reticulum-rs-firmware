pub mod generator;

use sha2::{Digest, Sha256};

const BASE_CORPUS: &[u8] = include_bytes!("../../../interop/vectors/rnode-hil-v1.json");

pub use generator::{Generated, Inputs};

pub fn generate_embedded(
    target_public_key: [u8; 64],
    target_destination_hash: [u8; 16],
) -> Result<Generated, String> {
    let source_sha256 = hex::encode(Sha256::digest(generator::SOURCE_BYTES));
    generator::generate(
        target_public_key,
        target_destination_hash,
        Inputs {
            base_corpus: BASE_CORPUS,
            source_sha256: &source_sha256,
        },
    )
}
