use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
    check_erase, check_read, check_write,
};
use reticulum_lxmf_durable_ingress::{
    DurableCarrierOutcome, DurableIngressCommitKind, commit_parsed_carrier,
};
use reticulum_lxmf_ingress::{CarrierIngress, WireLimits};
use reticulum_lxmf_model::SignatureVerification;
use reticulum_lxmf_store::{
    BoundLxmfStore, LxmfStoreBinding, LxmfStoreDeviceId, LxmfStoreIndexSlot,
    PHYSICAL_FORMAT_VERSION, mount,
};
use serde::Deserialize;

const CORPUS_JSON: &str = include_str!("../../../interop/vectors/lxmf-1.0.1-v1.json");
const PARTITION_SIZE: usize = 4 * reticulum_lxmf_store::EXTENT_SIZE;

#[derive(Deserialize)]
struct Corpus {
    messages: Vec<MessageFixture>,
}

#[derive(Deserialize)]
struct MessageFixture {
    name: String,
    destination_hash_hex: String,
    ingress: IngressFixture,
}

#[derive(Deserialize)]
struct IngressFixture {
    carrier_event: String,
    payload_hex: String,
}

fn decode(value: &str) -> Vec<u8> {
    hex::decode(value).expect("fixture hex")
}

fn array<const N: usize>(value: &str) -> [u8; N] {
    decode(value).try_into().expect("fixed fixture width")
}

fn limits() -> WireLimits {
    WireLimits::new(4_096, 2_048, 256, 2_048, 65_536, 16)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FakeError {
    Bounds,
    Alignment,
}

impl NorFlashError for FakeError {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::Bounds => NorFlashErrorKind::OutOfBounds,
            Self::Alignment => NorFlashErrorKind::NotAligned,
        }
    }
}

struct FakeNor {
    bytes: Vec<u8>,
}

impl FakeNor {
    fn erased() -> Self {
        Self {
            bytes: vec![0xff; PARTITION_SIZE],
        }
    }
}

impl ErrorType for FakeNor {
    type Error = FakeError;
}

impl ReadNorFlash for FakeNor {
    const READ_SIZE: usize = 4;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        check_read(self, offset, bytes.len()).map_err(map_check_error)?;
        let offset = offset as usize;
        bytes.copy_from_slice(&self.bytes[offset..offset + bytes.len()]);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.bytes.len()
    }
}

impl NorFlash for FakeNor {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = reticulum_lxmf_store::EXTENT_SIZE;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        check_write(self, offset, bytes.len()).map_err(map_check_error)?;
        let offset = offset as usize;
        for (stored, supplied) in self.bytes[offset..offset + bytes.len()]
            .iter_mut()
            .zip(bytes)
        {
            *stored &= *supplied;
        }
        Ok(())
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        check_erase(self, from, to).map_err(map_check_error)?;
        self.bytes[from as usize..to as usize].fill(0xff);
        Ok(())
    }
}

impl MultiwriteNorFlash for FakeNor {}

fn map_check_error(error: NorFlashErrorKind) -> FakeError {
    match error {
        NorFlashErrorKind::OutOfBounds => FakeError::Bounds,
        _ => FakeError::Alignment,
    }
}

#[test]
fn default_durable_handoff_commits_unknown_source_without_retry() {
    let corpus: Corpus = serde_json::from_str(CORPUS_JSON).expect("checked-in LXMF corpus");
    let fixture = corpus
        .messages
        .iter()
        .find(|fixture| fixture.name == "basic_binary")
        .expect("basic fixture");
    assert_eq!(fixture.ingress.carrier_event, "destination_data");
    let destination = array::<16>(&fixture.destination_hash_hex);
    let payload = decode(&fixture.ingress.payload_hex);
    let resolver = |_: &[u8; 16]| None::<[u8; 64]>;
    let binding = LxmfStoreBinding::new(
        LxmfStoreDeviceId::new([0x5a; 16]),
        0x73_0000,
        PARTITION_SIZE,
        PHYSICAL_FORMAT_VERSION,
    );
    let mut access = BoundLxmfStore::new(FakeNor::erased(), binding);
    let mut index = [LxmfStoreIndexSlot::new()];
    let mut store = mount(&mut access, &mut index).expect("empty store mounts");

    let DurableCarrierOutcome::Durable(committed) = commit_parsed_carrier(
        CarrierIngress::Opportunistic {
            implied_destination: &destination,
            payload: &payload,
        },
        None,
        limits(),
        &resolver,
        &mut store,
        &mut access,
    ) else {
        panic!("source-unknown is metadata, not retained admission")
    };
    assert_eq!(committed.kind(), DurableIngressCommitKind::New);
    assert_eq!(
        store
            .metadata(committed.receipt().handle())
            .expect("committed metadata")
            .signature_verification(),
        SignatureVerification::SourceUnknown
    );

    let DurableCarrierOutcome::Durable(replay) = commit_parsed_carrier(
        CarrierIngress::Opportunistic {
            implied_destination: &destination,
            payload: &payload,
        },
        None,
        limits(),
        &resolver,
        &mut store,
        &mut access,
    ) else {
        panic!("exact replay reconciles without admission state")
    };
    assert_eq!(replay.kind(), DurableIngressCommitKind::Replay);
    assert_eq!(replay.receipt(), committed.receipt());
}
