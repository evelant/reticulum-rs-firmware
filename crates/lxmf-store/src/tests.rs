use super::*;

extern crate std;

use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
    check_erase, check_read, check_write,
};
use reticulum_lxmf_model::{AuthenticatedMaterialFingerprint, NormalizedWire};
use reticulum_lxmf_wire::{MessageView, WireLimits};
use serde::Deserialize;
use std::{string::String, vec, vec::Vec};

const PARTITION_SIZE: usize = EXTENT_SIZE * 4;
const STORE_OFFSET: usize = 0x73_0000;
const DEVICE: LxmfStoreDeviceId = LxmfStoreDeviceId::new([0x5a; 16]);
const CORPUS_JSON: &str = include_str!("../../../interop/vectors/lxmf-1.0.1-v1.json");

fn index<const N: usize>() -> [LxmfStoreIndexSlot; N] {
    [const { LxmfStoreIndexSlot::new() }; N]
}

#[derive(Deserialize)]
struct Corpus {
    messages: Vec<CorpusMessage>,
}

#[derive(Deserialize)]
struct CorpusMessage {
    name: String,
    destination_hash_hex: String,
    source_hash_hex: String,
    message_id_hex: String,
    full_wire_hex: String,
    ingress: CorpusIngress,
    stamp: Option<CorpusStamp>,
}

#[derive(Deserialize)]
struct CorpusIngress {
    carrier_event: String,
}

#[derive(Deserialize)]
struct CorpusStamp {
    kind: String,
    target_cost: Option<u16>,
    value: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FakeError {
    Bounds,
    Alignment,
    Injected,
}

impl NorFlashError for FakeError {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::Bounds => NorFlashErrorKind::OutOfBounds,
            Self::Alignment => NorFlashErrorKind::NotAligned,
            Self::Injected => NorFlashErrorKind::Other,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum WriteFault {
    Partial(usize),
    Sparse,
    LostReply,
}

#[derive(Clone)]
struct FakeNor<const READ: usize = 4, const WRITE: usize = 4, const ERASE: usize = 4096> {
    bytes: Vec<u8>,
    reads: usize,
    writes: usize,
    erases: usize,
    read_fault: Option<usize>,
    write_fault: Option<(usize, WriteFault)>,
}

type TestNor = FakeNor<4, 4, 4096>;

impl<const READ: usize, const WRITE: usize, const ERASE: usize> FakeNor<READ, WRITE, ERASE> {
    fn erased(capacity: usize) -> Self {
        Self {
            bytes: vec![0xff; capacity],
            reads: 0,
            writes: 0,
            erases: 0,
            read_fault: None,
            write_fault: None,
        }
    }

    fn fail_read_after(&mut self, successful_reads: usize) {
        self.read_fault = Some(successful_reads);
    }

    fn fail_write_after(&mut self, successful_writes: usize, fault: WriteFault) {
        self.write_fault = Some((successful_writes, fault));
    }

    fn program(&mut self, offset: usize, bytes: &[u8]) {
        for (stored, supplied) in self.bytes[offset..offset + bytes.len()]
            .iter_mut()
            .zip(bytes)
        {
            *stored &= *supplied;
        }
    }
}

impl<const READ: usize, const WRITE: usize, const ERASE: usize> ErrorType
    for FakeNor<READ, WRITE, ERASE>
{
    type Error = FakeError;
}

impl<const READ: usize, const WRITE: usize, const ERASE: usize> ReadNorFlash
    for FakeNor<READ, WRITE, ERASE>
{
    const READ_SIZE: usize = READ;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        check_read(self, offset, bytes.len()).map_err(map_check_error)?;
        self.reads += 1;
        if self.read_fault == Some(0) {
            self.read_fault = None;
            return Err(FakeError::Injected);
        }
        if let Some(remaining) = &mut self.read_fault {
            *remaining -= 1;
        }
        let offset = offset as usize;
        bytes.copy_from_slice(&self.bytes[offset..offset + bytes.len()]);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.bytes.len()
    }
}

impl<const READ: usize, const WRITE: usize, const ERASE: usize> NorFlash
    for FakeNor<READ, WRITE, ERASE>
{
    const WRITE_SIZE: usize = WRITE;
    const ERASE_SIZE: usize = ERASE;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        check_write(self, offset, bytes.len()).map_err(map_check_error)?;
        self.writes += 1;
        let offset = offset as usize;
        let trigger = self
            .write_fault
            .is_some_and(|(remaining, _)| remaining == 0);
        if !trigger {
            if let Some((remaining, _)) = &mut self.write_fault {
                *remaining -= 1;
            }
            self.program(offset, bytes);
            return Ok(());
        }
        let (_, fault) = self.write_fault.take().expect("armed write fault");
        match fault {
            WriteFault::Partial(length) => {
                self.program(offset, &bytes[..length.min(bytes.len())]);
                Err(FakeError::Injected)
            }
            WriteFault::Sparse => {
                for index in (0..bytes.len()).step_by(17) {
                    self.bytes[offset + index] &= bytes[index];
                }
                Err(FakeError::Injected)
            }
            WriteFault::LostReply => {
                self.program(offset, bytes);
                Err(FakeError::Injected)
            }
        }
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        check_erase(self, from, to).map_err(map_check_error)?;
        self.erases += 1;
        self.bytes[from as usize..to as usize].fill(0xff);
        Ok(())
    }
}

impl<const READ: usize, const WRITE: usize, const ERASE: usize> MultiwriteNorFlash
    for FakeNor<READ, WRITE, ERASE>
{
}

fn map_check_error(error: NorFlashErrorKind) -> FakeError {
    match error {
        NorFlashErrorKind::OutOfBounds => FakeError::Bounds,
        NorFlashErrorKind::NotAligned => FakeError::Alignment,
        _ => FakeError::Injected,
    }
}

fn binding(length: usize) -> LxmfStoreBinding {
    LxmfStoreBinding::new(DEVICE, STORE_OFFSET, length, PHYSICAL_FORMAT_VERSION)
}

fn bound(flash: TestNor) -> BoundLxmfStore<TestNor> {
    BoundLxmfStore::new(flash, binding(PARTITION_SIZE))
}

#[allow(
    clippy::too_many_arguments,
    reason = "fixture exposes every persisted scalar"
)]
fn metadata(
    id: u8,
    authenticated_material: u8,
    destination: u8,
    source: u8,
    timestamp: u64,
    carrier: CarrierProvenance,
    stamp: StampAdmissionProvenance,
    normalized_len: usize,
    carrier_len: usize,
) -> InboundMessageMetadata {
    InboundMessageMetadata::new(
        MessageId::new([id; 32]),
        AuthenticatedMaterialFingerprint::new([authenticated_material; 32]),
        DestinationHash::new([destination; 16]),
        SourceHash::new([source; 16]),
        timestamp,
        carrier,
        stamp,
        InboundMessageLengths::new(normalized_len, carrier_len, 2, 3, 1).unwrap(),
    )
    .unwrap()
}

fn complete_candidate<'a>(
    wire: &'a [u8],
    id: u8,
    authenticated_material: u8,
    destination: u8,
    timestamp: u64,
    stamp: StampAdmissionProvenance,
) -> InboundMessageCandidate<'a> {
    InboundMessageCandidate::new(
        metadata(
            id,
            authenticated_material,
            destination,
            0x33,
            timestamp,
            CarrierProvenance::Complete,
            stamp,
            wire.len(),
            wire.len(),
        ),
        NormalizedWire::Contiguous(wire),
    )
    .unwrap()
}

fn complete_wire(destination: u8, length: usize, fill: u8) -> Vec<u8> {
    let mut wire = vec![fill; length];
    wire[..16].fill(destination);
    wire
}

fn normal_stamp() -> StampAdmissionProvenance {
    StampAdmissionProvenance::NotRequired {
        stamp_present: false,
    }
}

fn install_incomplete_record_start(
    access: &mut BoundLxmfStore<TestNor>,
    candidate: InboundMessageCandidate<'_>,
    extent_count: u16,
    handle: u64,
) {
    assert_eq!(
        required_extents(candidate.segments().total_len()),
        Some(usize::from(extent_count))
    );
    let header = encode_header(
        binding(PARTITION_SIZE),
        MessageHandle::new(handle).unwrap(),
        0,
        extent_count,
        candidate.metadata(),
        digest_candidate_wire(candidate),
    );
    access.backend_mut().bytes[..EXTENT_HEADER_SIZE].copy_from_slice(&header);
}

fn wire_limits() -> WireLimits {
    WireLimits::new(4096, 2048, 256, 2048, 65_536, 16)
}

fn decode_array<const N: usize>(value: &str) -> [u8; N] {
    hex::decode(value)
        .expect("checked corpus hex")
        .try_into()
        .expect("checked corpus width")
}

#[test]
fn geometry_reports_exact_payload_capacity() {
    assert_eq!(max_normalized_wire_bytes(EXTENT_SIZE), Some(3328));
    assert_eq!(max_normalized_wire_bytes(0x20_0000), Some(1_834_752));
    assert!(max_normalized_wire_bytes(0).is_none());
    assert!(max_normalized_wire_bytes(EXTENT_SIZE + 1).is_none());
    assert!(max_normalized_wire_bytes(0x20_0000).unwrap() >= 1024 * 1024);
    let maximum_extents = u16::MAX as usize;
    assert_eq!(
        max_normalized_wire_bytes((maximum_extents + 1) * EXTENT_SIZE),
        max_normalized_wire_bytes(maximum_extents * EXTENT_SIZE)
    );
}

#[test]
fn commit_mount_and_remount_preserve_receipt_and_metadata() {
    let wire = complete_wire(0x22, 600, 0xa5);
    let candidate = complete_candidate(&wire, 1, 9, 0x22, 7, normal_stamp());
    let mut access = bound(TestNor::erased(PARTITION_SIZE));
    let mut mounted_index = index::<4>();
    let mut mounted = mount(&mut access, &mut mounted_index).unwrap();
    let receipt = match mounted.commit(&mut access, candidate).unwrap() {
        LxmfCommitOutcome::Committed(receipt) => receipt,
        other => panic!("unexpected outcome {other:?}"),
    };
    assert_eq!(receipt.handle().get(), 1);
    assert_eq!(mounted.message_count(), 1);
    assert_eq!(
        mounted.metadata(receipt.handle()),
        Some(candidate.metadata())
    );
    assert_eq!(access.backend().erases, 0);

    let flash = access.into_backend();
    let mut remounted_access = bound(flash);
    let mut remounted_index = index::<4>();
    let remounted = mount(&mut remounted_access, &mut remounted_index).unwrap();
    assert_eq!(remounted.message_count(), 1);
    assert_eq!(remounted.receipt(receipt.handle()), Some(receipt));
    assert_eq!(
        remounted.metadata(receipt.handle()),
        Some(candidate.metadata())
    );
    assert_eq!(remounted_access.backend().erases, 0);
}

#[test]
fn wire_chunks_return_exact_persisted_bytes_across_extent_boundaries() {
    let wire = complete_wire(0x22, 4084, 0xa5);
    let mut access = bound(TestNor::erased(PARTITION_SIZE));
    let mut commit_index = index::<2>();
    let receipt = {
        let mut mounted = mount(&mut access, &mut commit_index).unwrap();
        match mounted
            .commit(
                &mut access,
                complete_candidate(&wire, 1, 9, 0x22, 7, normal_stamp()),
            )
            .unwrap()
        {
            LxmfCommitOutcome::Committed(receipt) => receipt,
            other => panic!("unexpected outcome {other:?}"),
        }
    };

    let mut read_index = index::<2>();
    let mounted = mount(&mut access, &mut read_index).unwrap();
    let mut complete = vec![0_u8; wire.len()];
    let whole = mounted
        .read_wire_chunk(&mut access, receipt.handle(), 0, &mut complete)
        .unwrap();
    assert_eq!(whole.offset(), 0);
    assert_eq!(whole.wire_length(), wire.len() as u32);
    assert_eq!(whole.bytes_read(), wire.len());
    assert_eq!(whole.next_offset(), wire.len() as u32);
    assert!(whole.is_complete());
    assert_eq!(complete, wire);

    let start = EXTENT_PAYLOAD_SIZE - 84;
    let mut crossing = [0xcc; 600];
    let chunk = mounted
        .read_wire_chunk(&mut access, receipt.handle(), start as u32, &mut crossing)
        .unwrap();
    assert_eq!(chunk.bytes_read(), wire.len() - start);
    assert!(chunk.is_complete());
    assert_eq!(&crossing[..chunk.bytes_read()], &wire[start..]);
    assert!(
        crossing[chunk.bytes_read()..]
            .iter()
            .all(|byte| *byte == 0xcc)
    );
}

#[test]
fn wire_chunk_offsets_and_empty_buffers_have_explicit_end_semantics() {
    let wire = complete_wire(0x22, 600, 0x5a);
    let mut access = bound(TestNor::erased(PARTITION_SIZE));
    let mut mounted_index = index::<2>();
    let mut mounted = mount(&mut access, &mut mounted_index).unwrap();
    let receipt = match mounted
        .commit(
            &mut access,
            complete_candidate(&wire, 1, 9, 0x22, 7, normal_stamp()),
        )
        .unwrap()
    {
        LxmfCommitOutcome::Committed(receipt) => receipt,
        other => panic!("unexpected outcome {other:?}"),
    };

    let reads = access.backend().reads;
    let empty_before_end = mounted
        .read_wire_chunk(&mut access, receipt.handle(), 7, &mut [])
        .unwrap();
    assert_eq!(empty_before_end.bytes_read(), 0);
    assert!(!empty_before_end.is_complete());
    assert_eq!(access.backend().reads, reads);

    let at_end = mounted
        .read_wire_chunk(
            &mut access,
            receipt.handle(),
            wire.len() as u32,
            &mut [0; 8],
        )
        .unwrap();
    assert_eq!(at_end.bytes_read(), 0);
    assert!(at_end.is_complete());
    assert_eq!(access.backend().reads, reads);

    assert_eq!(
        mounted.read_wire_chunk(
            &mut access,
            receipt.handle(),
            wire.len() as u32 + 1,
            &mut [0; 8],
        ),
        Err(LxmfWireReadError::OffsetOutOfRange {
            offset: wire.len() as u32 + 1,
            wire_length: wire.len() as u32,
        })
    );
    assert_eq!(access.backend().reads, reads);

    let unknown = MessageHandle::new(receipt.handle().get() + 1).unwrap();
    assert_eq!(
        mounted.read_wire_chunk(&mut access, unknown, 0, &mut [0; 8]),
        Err(LxmfWireReadError::NotFound { handle: unknown })
    );
    assert_eq!(access.backend().reads, reads);
}

#[test]
fn wire_chunk_rejects_wrong_binding_and_reports_backend_failure() {
    let wire = complete_wire(0x22, 600, 0x5a);
    let mut access = bound(TestNor::erased(PARTITION_SIZE));
    let mut mounted_index = index::<2>();
    let mut mounted = mount(&mut access, &mut mounted_index).unwrap();
    let receipt = match mounted
        .commit(
            &mut access,
            complete_candidate(&wire, 1, 9, 0x22, 7, normal_stamp()),
        )
        .unwrap()
    {
        LxmfCommitOutcome::Committed(receipt) => receipt,
        other => panic!("unexpected outcome {other:?}"),
    };

    let wrong_device = LxmfStoreDeviceId::new([0x77; 16]);
    let wrong_binding = LxmfStoreBinding::new(
        wrong_device,
        STORE_OFFSET,
        PARTITION_SIZE,
        PHYSICAL_FORMAT_VERSION,
    );
    let mut wrong_access = BoundLxmfStore::new(access.backend().clone(), wrong_binding);
    let reads = wrong_access.backend().reads;
    assert_eq!(
        mounted.read_wire_chunk(&mut wrong_access, receipt.handle(), 0, &mut [0; 8]),
        Err(LxmfWireReadError::Binding(
            LxmfStoreBindingError::DeviceMismatch {
                expected: DEVICE,
                actual: wrong_device,
            }
        ))
    );
    assert_eq!(wrong_access.backend().reads, reads);

    access.backend_mut().fail_read_after(0);
    assert_eq!(
        mounted.read_wire_chunk(&mut access, receipt.handle(), 0, &mut [0; 8]),
        Err(LxmfWireReadError::Backend(FakeError::Injected))
    );
}

#[test]
fn wire_chunk_fails_closed_when_an_indexed_extent_header_changes() {
    let wire = complete_wire(0x22, 4000, 0x5a);
    let mut access = bound(TestNor::erased(PARTITION_SIZE));
    let mut mounted_index = index::<2>();
    let mut mounted = mount(&mut access, &mut mounted_index).unwrap();
    let receipt = match mounted
        .commit(
            &mut access,
            complete_candidate(&wire, 1, 9, 0x22, 7, normal_stamp()),
        )
        .unwrap()
    {
        LxmfCommitOutcome::Committed(receipt) => receipt,
        other => panic!("unexpected outcome {other:?}"),
    };
    access.backend_mut().bytes[EXTENT_SIZE + HEADER_MAGIC_OFFSET] ^= 0x01;

    assert_eq!(
        mounted.read_wire_chunk(
            &mut access,
            receipt.handle(),
            EXTENT_PAYLOAD_SIZE as u32,
            &mut [0; 8],
        ),
        Err(LxmfWireReadError::Fault(
            LxmfStoreFault::CommittedHeaderCorrupt { extent: 1 }
        ))
    );
}

#[test]
fn opportunistic_391_byte_carrier_exceeds_old_qualification_ceiling() {
    let destination = [0x44; 16];
    let carrier = [0x66; 391];
    let metadata = metadata(
        2,
        8,
        0x44,
        0x55,
        9,
        CarrierProvenance::Opportunistic,
        StampAdmissionProvenance::TrustedPriorTicket,
        407,
        391,
    );
    let candidate = InboundMessageCandidate::new(
        metadata,
        NormalizedWire::Opportunistic {
            implied_destination: &destination,
            carrier_payload: &carrier,
        },
    )
    .unwrap();
    let mut access = bound(TestNor::erased(PARTITION_SIZE));
    let mut mounted_index = index::<4>();
    let mut mounted = mount(&mut access, &mut mounted_index).unwrap();
    assert!(matches!(
        mounted.commit(&mut access, candidate),
        Ok(LxmfCommitOutcome::Committed(_))
    ));
    assert_eq!(
        mount(&mut access, &mut index::<4>())
            .unwrap()
            .message_count(),
        1
    );
}

#[test]
fn released_python_corpus_commits_remounts_and_retains_exact_full_wire() {
    let corpus: Corpus = serde_json::from_str(CORPUS_JSON).expect("checked-in Python corpus");
    for name in [
        "basic_binary",
        "rich_fields",
        "pow_stamp_32",
        "ticket_stamp_16",
        "opportunistic_limit_295",
    ] {
        let fixture = corpus
            .messages
            .iter()
            .find(|fixture| fixture.name == name)
            .expect("required released-Python fixture");
        let full_wire = hex::decode(&fixture.full_wire_hex).expect("checked full wire");
        let destination = decode_array::<16>(&fixture.destination_hash_hex);
        let parsed = MessageView::parse_complete(&full_wire, wire_limits()).unwrap();
        assert_eq!(
            parsed.message_id(),
            decode_array::<32>(&fixture.message_id_hex)
        );
        assert_eq!(parsed.destination_hash(), &destination);
        assert_eq!(
            parsed.source_hash(),
            &decode_array::<16>(&fixture.source_hash_hex)
        );
        let opportunistic = fixture.ingress.carrier_event == "destination_data";
        let carrier = if opportunistic {
            CarrierProvenance::Opportunistic
        } else {
            CarrierProvenance::LinkDataContextNone
        };
        let stamp = match fixture.stamp.as_ref() {
            None => StampAdmissionProvenance::NotRequired {
                stamp_present: false,
            },
            Some(stamp) if stamp.kind == "ticket" => StampAdmissionProvenance::TrustedPriorTicket,
            Some(stamp) if stamp.kind == "proof_of_work" => StampAdmissionProvenance::ProofOfWork {
                target_cost: RequiredStampCost::new(stamp.target_cost.expect("PoW fixture target"))
                    .unwrap(),
                observed_value: stamp.value,
            },
            Some(_) => panic!("unknown corpus stamp"),
        };
        let payload = parsed.payload();
        let carrier_len = if opportunistic {
            full_wire.len() - 16
        } else {
            full_wire.len()
        };
        let metadata = InboundMessageMetadata::new(
            MessageId::new(parsed.message_id()),
            AuthenticatedMaterialFingerprint::new(parsed.authenticated_material_fingerprint()),
            DestinationHash::new(*parsed.destination_hash()),
            SourceHash::new(*parsed.source_hash()),
            payload.timestamp_bits(),
            carrier,
            stamp,
            InboundMessageLengths::new(
                full_wire.len(),
                carrier_len,
                payload.title().as_bytes().len(),
                payload.content().as_bytes().len(),
                payload.fields().raw().len(),
            )
            .unwrap(),
        )
        .unwrap();
        let wire = if opportunistic {
            NormalizedWire::Opportunistic {
                implied_destination: &destination,
                carrier_payload: &full_wire[16..],
            }
        } else {
            NormalizedWire::Contiguous(&full_wire)
        };
        let candidate = InboundMessageCandidate::new(metadata, wire).unwrap();
        let mut access = bound(TestNor::erased(PARTITION_SIZE));
        let mut mounted_index = index::<2>();
        let mut mounted = mount(&mut access, &mut mounted_index).unwrap();
        let receipt = match mounted.commit(&mut access, candidate).unwrap() {
            LxmfCommitOutcome::Committed(receipt) => receipt,
            other => panic!("{name}: unexpected {other:?}"),
        };
        assert_eq!(
            &access.backend().bytes[EXTENT_HEADER_SIZE..EXTENT_HEADER_SIZE + full_wire.len()],
            full_wire.as_slice(),
            "{name}: exact normalized bytes"
        );
        let mut remounted_index = index::<2>();
        let remounted = mount(&mut access, &mut remounted_index).unwrap();
        assert_eq!(remounted.receipt(receipt.handle()), Some(receipt), "{name}");
        assert_eq!(
            remounted.metadata(receipt.handle()),
            Some(metadata),
            "{name}"
        );
    }
}

#[test]
fn multiple_variable_extent_records_append_and_remount() {
    let first_wire = complete_wire(0x21, 100, 1);
    let second_wire = complete_wire(0x22, 4000, 2);
    let mut access = bound(TestNor::erased(PARTITION_SIZE));
    let mut mounted_index = index::<4>();
    let mut mounted = mount(&mut access, &mut mounted_index).unwrap();
    let first = mounted
        .commit(
            &mut access,
            complete_candidate(&first_wire, 1, 1, 0x21, 1, normal_stamp()),
        )
        .unwrap();
    let second = mounted
        .commit(
            &mut access,
            complete_candidate(&second_wire, 2, 2, 0x22, 2, normal_stamp()),
        )
        .unwrap();
    assert!(matches!(first, LxmfCommitOutcome::Committed(_)));
    assert!(matches!(second, LxmfCommitOutcome::Committed(_)));
    assert_eq!(mounted.message_count(), 2);
    assert_eq!(mounted.consumed_extents(), 3);
    assert_eq!(
        mount(&mut access, &mut index::<4>())
            .unwrap()
            .message_count(),
        2
    );
}

#[test]
fn opportunistic_segments_cross_segment_and_extent_boundaries_without_coalescing() {
    let destination = [0x44; 16];
    let carrier = vec![0x66; 4000];
    let metadata = metadata(
        3,
        9,
        0x44,
        0x55,
        10,
        CarrierProvenance::Opportunistic,
        normal_stamp(),
        destination.len() + carrier.len(),
        carrier.len(),
    );
    let candidate = InboundMessageCandidate::new(
        metadata,
        NormalizedWire::Opportunistic {
            implied_destination: &destination,
            carrier_payload: &carrier,
        },
    )
    .unwrap();
    let mut access = bound(TestNor::erased(PARTITION_SIZE));
    let mut mounted_index = index::<4>();
    let mut mounted = mount(&mut access, &mut mounted_index).unwrap();
    mounted.commit(&mut access, candidate).unwrap();
    assert_eq!(mounted.consumed_extents(), 2);
    assert_eq!(
        &access.backend().bytes[EXTENT_HEADER_SIZE..EXTENT_HEADER_SIZE + 16],
        &destination
    );
    assert_eq!(
        &access.backend().bytes[EXTENT_HEADER_SIZE + 16..EXTENT_HEADER_SIZE + EXTENT_PAYLOAD_SIZE],
        &carrier[..EXTENT_PAYLOAD_SIZE - 16]
    );
    assert_eq!(
        &access.backend().bytes[EXTENT_SIZE + EXTENT_HEADER_SIZE
            ..EXTENT_SIZE + EXTENT_HEADER_SIZE + carrier.len() - (EXTENT_PAYLOAD_SIZE - 16)],
        &carrier[EXTENT_PAYLOAD_SIZE - 16..]
    );
    assert_eq!(
        mount(&mut access, &mut index::<4>())
            .unwrap()
            .message_count(),
        1
    );
}

#[test]
fn exact_and_alternative_stamp_replays_do_not_write() {
    let first_wire = complete_wire(0x22, 400, 0x10);
    let alternative_wire = complete_wire(0x22, 400, 0x20);
    let mut access = bound(TestNor::erased(PARTITION_SIZE));
    let mut mounted_index = index::<4>();
    let mut mounted = mount(&mut access, &mut mounted_index).unwrap();
    let first_candidate = complete_candidate(
        &first_wire,
        7,
        8,
        0x22,
        4,
        StampAdmissionProvenance::TrustedPriorTicket,
    );
    let receipt = match mounted.commit(&mut access, first_candidate).unwrap() {
        LxmfCommitOutcome::Committed(receipt) => receipt,
        other => panic!("unexpected outcome {other:?}"),
    };
    let writes = access.backend().writes;
    assert_eq!(
        mounted.commit(&mut access, first_candidate),
        Ok(LxmfCommitOutcome::AlreadyDurable(receipt))
    );
    let alternative = complete_candidate(
        &alternative_wire,
        7,
        8,
        0x22,
        4,
        StampAdmissionProvenance::ProofOfWork {
            target_cost: RequiredStampCost::new(8).unwrap(),
            observed_value: 11,
        },
    );
    assert_eq!(
        mounted.commit(&mut access, alternative),
        Ok(LxmfCommitOutcome::AlreadyDurable(receipt))
    );
    assert_eq!(access.backend().writes, writes);
    assert_eq!(mounted.message_count(), 1);
}

#[test]
fn same_id_with_conflicting_authenticated_material_fails_closed() {
    let wire = complete_wire(0x22, 100, 1);
    let mut access = bound(TestNor::erased(PARTITION_SIZE));
    let mut mounted_index = index::<4>();
    let mut mounted = mount(&mut access, &mut mounted_index).unwrap();
    mounted
        .commit(
            &mut access,
            complete_candidate(&wire, 7, 1, 0x22, 4, normal_stamp()),
        )
        .unwrap();
    let writes = access.backend().writes;
    let collision = complete_candidate(&wire, 7, 2, 0x22, 4, normal_stamp());
    assert!(matches!(
        mounted.commit(&mut access, collision).unwrap_err().error(),
        LxmfCommitError::HashCollision { message_id } if *message_id == MessageId::new([7; 32])
    ));
    assert_eq!(access.backend().writes, writes);
}

#[test]
fn physical_full_and_ram_index_full_are_distinct_and_nonmutating() {
    let too_large_wire = complete_wire(0x22, FINAL_EXTENT_PAYLOAD_SIZE + 1, 1);
    let mut one_extent = BoundLxmfStore::new(TestNor::erased(EXTENT_SIZE), binding(EXTENT_SIZE));
    let mut mounted_index = index::<2>();
    let mut mounted = mount(&mut one_extent, &mut mounted_index).unwrap();
    assert!(matches!(
        mounted
            .commit(
                &mut one_extent,
                complete_candidate(&too_large_wire, 1, 1, 0x22, 1, normal_stamp())
            )
            .unwrap_err()
            .error(),
        LxmfCommitError::Full {
            required_extents: 2,
            remaining_extents: 1
        }
    ));
    assert_eq!(one_extent.backend().writes, 0);

    let first_wire = complete_wire(0x22, 100, 1);
    let second_wire = complete_wire(0x23, 100, 2);
    let mut access = bound(TestNor::erased(PARTITION_SIZE));
    let mut single_index = index::<1>();
    let mut indexed = mount(&mut access, &mut single_index).unwrap();
    indexed
        .commit(
            &mut access,
            complete_candidate(&first_wire, 1, 1, 0x22, 1, normal_stamp()),
        )
        .unwrap();
    let writes = access.backend().writes;
    assert!(matches!(
        indexed
            .commit(
                &mut access,
                complete_candidate(&second_wire, 2, 2, 0x23, 2, normal_stamp())
            )
            .unwrap_err()
            .error(),
        LxmfCommitError::IndexFull { capacity: 1 }
    ));
    assert_eq!(access.backend().writes, writes);
    assert!(matches!(
        mount(&mut access, &mut index::<0>()),
        Err(LxmfStoreMountError::IndexCapacityExceeded {
            required: 1,
            capacity: 0
        })
    ));
}

#[test]
fn caller_index_capacity_is_exact_for_zero_one_and_multiple_slots() {
    let wires = [
        complete_wire(0x21, 100, 1),
        complete_wire(0x22, 100, 2),
        complete_wire(0x23, 100, 3),
        complete_wire(0x24, 100, 4),
    ];

    let mut zero_access = bound(TestNor::erased(PARTITION_SIZE));
    let mut zero_index = index::<0>();
    let mut zero = mount(&mut zero_access, &mut zero_index).unwrap();
    assert_eq!(zero.message_count(), 0);
    assert!(matches!(
        zero.commit(
            &mut zero_access,
            complete_candidate(&wires[0], 1, 1, 0x21, 1, normal_stamp()),
        )
        .unwrap_err()
        .error(),
        LxmfCommitError::IndexFull { capacity: 0 }
    ));
    assert_eq!(zero_access.backend().writes, 0);

    let mut one_access = bound(TestNor::erased(PARTITION_SIZE));
    let mut one_index = index::<1>();
    let mut one = mount(&mut one_access, &mut one_index).unwrap();
    one.commit(
        &mut one_access,
        complete_candidate(&wires[0], 1, 1, 0x21, 1, normal_stamp()),
    )
    .unwrap();
    let one_writes = one_access.backend().writes;
    assert!(matches!(
        one.commit(
            &mut one_access,
            complete_candidate(&wires[1], 2, 2, 0x22, 2, normal_stamp()),
        )
        .unwrap_err()
        .error(),
        LxmfCommitError::IndexFull { capacity: 1 }
    ));
    assert_eq!(one.message_count(), 1);
    assert_eq!(one_access.backend().writes, one_writes);

    let mut multiple_access = bound(TestNor::erased(PARTITION_SIZE));
    let mut multiple_index = index::<3>();
    let mut multiple = mount(&mut multiple_access, &mut multiple_index).unwrap();
    for (index, wire) in wires[..3].iter().enumerate() {
        let tag = (index + 1) as u8;
        multiple
            .commit(
                &mut multiple_access,
                complete_candidate(wire, tag, tag, 0x20 + tag, u64::from(tag), normal_stamp()),
            )
            .unwrap();
    }
    let multiple_writes = multiple_access.backend().writes;
    assert_eq!(multiple.message_count(), 3);
    assert_eq!(multiple.receipts().count(), 3);
    assert!(matches!(
        multiple
            .commit(
                &mut multiple_access,
                complete_candidate(&wires[3], 4, 4, 0x24, 4, normal_stamp()),
            )
            .unwrap_err()
            .error(),
        LxmfCommitError::IndexFull { capacity: 3 }
    ));
    assert_eq!(multiple_access.backend().writes, multiple_writes);
}

#[test]
fn remount_resets_and_reconstructs_the_complete_caller_index() {
    let wire = complete_wire(0x22, 100, 1);
    let candidate = complete_candidate(&wire, 1, 1, 0x22, 1, normal_stamp());
    let mut access = bound(TestNor::erased(PARTITION_SIZE));
    let mut caller_index = index::<2>();
    let receipt = {
        let mut mounted = mount(&mut access, &mut caller_index).unwrap();
        match mounted.commit(&mut access, candidate).unwrap() {
            LxmfCommitOutcome::Committed(receipt) => receipt,
            other => panic!("unexpected outcome {other:?}"),
        }
    };
    assert!(caller_index[0].entry.is_some());
    caller_index[1].entry = caller_index[0].entry;

    {
        let remounted = mount(&mut access, &mut caller_index).unwrap();
        assert_eq!(remounted.message_count(), 1);
        assert_eq!(remounted.receipt(receipt.handle()), Some(receipt));
    }
    assert!(caller_index[0].entry.is_some());
    assert!(caller_index[1].entry.is_none());

    access.backend_mut().bytes.fill(0xff);
    let empty = mount(&mut access, &mut caller_index).unwrap();
    assert_eq!(empty.message_count(), 0);
    drop(empty);
    assert!(caller_index.iter().all(|slot| slot.entry.is_none()));
}

#[test]
fn wrong_operation_binding_is_rejected_with_zero_io() {
    let wire = complete_wire(0x22, 100, 1);
    let candidate = complete_candidate(&wire, 1, 1, 0x22, 1, normal_stamp());
    let mut access = bound(TestNor::erased(PARTITION_SIZE));
    let mut mounted_index = index::<2>();
    let mut mounted = mount(&mut access, &mut mounted_index).unwrap();
    let mut wrong = BoundLxmfStore::new(
        TestNor::erased(PARTITION_SIZE),
        LxmfStoreBinding::new(
            LxmfStoreDeviceId::new([0x99; 16]),
            STORE_OFFSET,
            PARTITION_SIZE,
            PHYSICAL_FORMAT_VERSION,
        ),
    );
    assert!(matches!(
        mounted.commit(&mut wrong, candidate).unwrap_err().error(),
        LxmfCommitError::Binding(LxmfStoreBindingError::DeviceMismatch { .. })
    ));
    assert_eq!(wrong.backend().reads, 0);
    assert_eq!(wrong.backend().writes, 0);
    assert_eq!(wrong.backend().erases, 0);
}

#[test]
fn ambiguous_write_blocks_unrelated_mutation_and_exact_retry_reconciles() {
    let wire = complete_wire(0x22, 600, 1);
    let other_wire = complete_wire(0x23, 100, 2);
    let candidate = complete_candidate(&wire, 1, 1, 0x22, 1, normal_stamp());
    let other = complete_candidate(&other_wire, 2, 2, 0x23, 2, normal_stamp());
    let mut access = bound(TestNor::erased(PARTITION_SIZE));
    let mut mounted_index = index::<4>();
    let mut mounted = mount(&mut access, &mut mounted_index).unwrap();
    let message_id = candidate.metadata().message_id();
    assert!(!mounted.has_pending_mutation());
    assert_eq!(mounted.pending_message_id(), None);
    access
        .backend_mut()
        .fail_write_after(3, WriteFault::Partial(73));
    assert!(matches!(
        mounted.commit(&mut access, candidate).unwrap_err().error(),
        LxmfCommitError::Backend {
            stage: LxmfProgramStage::Wire,
            ..
        }
    ));
    assert!(mounted.has_pending_mutation());
    assert_eq!(mounted.pending_message_id(), Some(message_id));
    let writes = access.backend().writes;
    assert!(matches!(
        mounted.commit(&mut access, other).unwrap_err().error(),
        LxmfCommitError::AmbiguousMutationPending { .. }
    ));
    assert!(mounted.has_pending_mutation());
    assert_eq!(mounted.pending_message_id(), Some(message_id));
    assert_eq!(access.backend().writes, writes);
    assert!(matches!(
        mounted.commit(&mut access, candidate),
        Ok(LxmfCommitOutcome::Committed(_))
    ));
    assert_eq!(mounted.message_count(), 1);
    assert!(!mounted.has_pending_mutation());
    assert_eq!(mounted.pending_message_id(), None);
}

#[test]
fn readback_fault_latches_and_retry_completes_same_candidate() {
    let wire = complete_wire(0x22, 600, 1);
    let candidate = complete_candidate(&wire, 1, 1, 0x22, 1, normal_stamp());
    let mut access = bound(TestNor::erased(PARTITION_SIZE));
    let mut mounted_index = index::<4>();
    let mut mounted = mount(&mut access, &mut mounted_index).unwrap();
    access.backend_mut().fail_read_after(16);
    assert!(matches!(
        mounted.commit(&mut access, candidate).unwrap_err().error(),
        LxmfCommitError::Backend {
            stage: LxmfProgramStage::Claim,
            ..
        }
    ));
    assert!(matches!(
        mounted.commit(&mut access, candidate),
        Ok(LxmfCommitOutcome::Committed(_))
    ));
}

#[test]
fn every_write_prefix_is_invisible_after_power_loss_and_retryable_in_place() {
    let wire = complete_wire(0x22, 600, 1);
    let candidate = complete_candidate(&wire, 1, 1, 0x22, 1, normal_stamp());
    let mut baseline = bound(TestNor::erased(PARTITION_SIZE));
    let mut baseline_index = index::<4>();
    let mut mounted = mount(&mut baseline, &mut baseline_index).unwrap();
    mounted.commit(&mut baseline, candidate).unwrap();
    let total_writes = baseline.backend().writes;
    assert!(total_writes >= 6);

    for successful_writes in 0..total_writes {
        let mut access = bound(TestNor::erased(PARTITION_SIZE));
        let mut mounted_index = index::<4>();
        let mut mounted = mount(&mut access, &mut mounted_index).unwrap();
        access
            .backend_mut()
            .fail_write_after(successful_writes, WriteFault::Partial(1));
        assert!(
            mounted.commit(&mut access, candidate).is_err(),
            "prefix {successful_writes}"
        );
        let interrupted_flash = access.backend().clone();
        let mut remount_access = bound(interrupted_flash);
        let mut remounted_index = index::<4>();
        let remounted = mount(&mut remount_access, &mut remounted_index).unwrap();
        assert_eq!(remounted.message_count(), 0, "prefix {successful_writes}");
        assert!(matches!(
            mounted.commit(&mut access, candidate),
            Ok(LxmfCommitOutcome::Committed(_))
        ));
    }
}

#[test]
fn every_multi_extent_power_cut_reboots_retires_and_appends_exactly_once() {
    const SIX_EXTENTS: usize = EXTENT_SIZE * 6;
    let wire = complete_wire(0x22, 7_000, 1);
    let candidate = complete_candidate(&wire, 1, 1, 0x22, 1, normal_stamp());
    let mut baseline = BoundLxmfStore::new(TestNor::erased(SIX_EXTENTS), binding(SIX_EXTENTS));
    let mut baseline_index = index::<4>();
    let mut baseline_mounted = mount(&mut baseline, &mut baseline_index).unwrap();
    baseline_mounted.commit(&mut baseline, candidate).unwrap();
    let total_writes = baseline.backend().writes;

    for successful_writes in 0..total_writes {
        let mut interrupted =
            BoundLxmfStore::new(TestNor::erased(SIX_EXTENTS), binding(SIX_EXTENTS));
        let mut before_reset_index = index::<4>();
        let mut before_reset = mount(&mut interrupted, &mut before_reset_index).unwrap();
        interrupted
            .backend_mut()
            .fail_write_after(successful_writes, WriteFault::Partial(1));
        assert!(before_reset.commit(&mut interrupted, candidate).is_err());

        let interrupted_flash = interrupted.into_backend();
        let mut rebooted_access = BoundLxmfStore::new(interrupted_flash, binding(SIX_EXTENTS));
        let mut rebooted_index = index::<4>();
        let mut rebooted = mount(&mut rebooted_access, &mut rebooted_index).unwrap();
        assert_eq!(
            rebooted.message_count(),
            0,
            "cut at write {successful_writes}"
        );
        let receipt = match rebooted.commit(&mut rebooted_access, candidate).unwrap() {
            LxmfCommitOutcome::Committed(receipt) => receipt,
            other => panic!("cut at write {successful_writes}: {other:?}"),
        };
        let mut final_index = index::<4>();
        let final_mount = mount(&mut rebooted_access, &mut final_index).unwrap();
        assert_eq!(final_mount.message_count(), 1, "cut at {successful_writes}");
        assert_eq!(
            final_mount
                .receipts()
                .filter(|value| *value == receipt)
                .count(),
            1,
            "cut at write {successful_writes}"
        );
    }
}

#[test]
fn sparse_torn_claim_and_header_are_recognized_but_never_visible() {
    let wire = complete_wire(0x22, 600, 1);
    let candidate = complete_candidate(&wire, 1, 1, 0x22, 1, normal_stamp());
    for failed_write in [0, 1] {
        let mut access = bound(TestNor::erased(PARTITION_SIZE));
        let mut mounted_index = index::<4>();
        let mut mounted = mount(&mut access, &mut mounted_index).unwrap();
        access
            .backend_mut()
            .fail_write_after(failed_write, WriteFault::Sparse);
        assert!(mounted.commit(&mut access, candidate).is_err());
        assert_eq!(
            mount(&mut access, &mut index::<4>())
                .unwrap()
                .message_count(),
            0,
            "sparse failure at program call {failed_write}"
        );
    }
}

#[test]
fn lost_success_at_every_program_call_is_reconciled_by_readback() {
    let wire = complete_wire(0x22, 600, 1);
    let candidate = complete_candidate(&wire, 1, 1, 0x22, 1, normal_stamp());
    let mut baseline = bound(TestNor::erased(PARTITION_SIZE));
    let mut baseline_index = index::<4>();
    let mut mounted = mount(&mut baseline, &mut baseline_index).unwrap();
    mounted.commit(&mut baseline, candidate).unwrap();
    let total_writes = baseline.backend().writes;

    for lost_call in 0..total_writes {
        let mut access = bound(TestNor::erased(PARTITION_SIZE));
        let mut mounted_index = index::<4>();
        let mut mounted = mount(&mut access, &mut mounted_index).unwrap();
        access
            .backend_mut()
            .fail_write_after(lost_call, WriteFault::LostReply);
        assert!(matches!(
            mounted.commit(&mut access, candidate),
            Ok(LxmfCommitOutcome::Committed(_))
        ));
        assert_eq!(
            mount(&mut access, &mut index::<4>())
                .unwrap()
                .message_count(),
            1
        );
    }
}

#[test]
fn durable_commit_with_lost_readback_retries_as_already_durable_without_writing() {
    let wire = complete_wire(0x22, 600, 1);
    let candidate = complete_candidate(&wire, 1, 1, 0x22, 1, normal_stamp());
    for (successful_reads, expected_stage) in [
        (23, LxmfProgramStage::Commit),
        (24, LxmfProgramStage::Verification),
    ] {
        let mut access = bound(TestNor::erased(PARTITION_SIZE));
        let mut mounted_index = index::<4>();
        let mut mounted = mount(&mut access, &mut mounted_index).unwrap();
        access.backend_mut().fail_read_after(successful_reads);
        assert!(matches!(
            mounted.commit(&mut access, candidate).unwrap_err().error(),
            LxmfCommitError::Backend { stage, .. } if *stage == expected_stage
        ));
        assert!(mounted.has_pending_mutation());
        assert_eq!(
            mounted.pending_message_id(),
            Some(candidate.metadata().message_id())
        );

        let mut reset_access = bound(access.backend().clone());
        let mut reset_index = index::<4>();
        let reset = mount(&mut reset_access, &mut reset_index).unwrap();
        assert_eq!(reset.message_count(), 1);
        assert!(!reset.has_pending_mutation());
        assert_eq!(reset.pending_message_id(), None);

        let writes = access.backend().writes;
        assert!(matches!(
            mounted.commit(&mut access, candidate),
            Ok(LxmfCommitOutcome::AlreadyDurable(_))
        ));
        assert_eq!(access.backend().writes, writes);
        assert_eq!(mounted.message_count(), 1);
        assert!(!mounted.has_pending_mutation());
        assert_eq!(mounted.pending_message_id(), None);
    }
}

#[test]
fn partially_programmed_terminal_marker_is_completed_by_same_candidate_retry() {
    let wire = complete_wire(0x22, 600, 1);
    let candidate = complete_candidate(&wire, 1, 1, 0x22, 1, normal_stamp());
    let mut baseline = bound(TestNor::erased(PARTITION_SIZE));
    let mut baseline_index = index::<4>();
    let mut baseline_mounted = mount(&mut baseline, &mut baseline_index).unwrap();
    baseline_mounted.commit(&mut baseline, candidate).unwrap();
    let writes_before_terminal = baseline.backend().writes - 1;

    let mut access = bound(TestNor::erased(PARTITION_SIZE));
    let mut mounted_index = index::<4>();
    let mut mounted = mount(&mut access, &mut mounted_index).unwrap();
    access
        .backend_mut()
        .fail_write_after(writes_before_terminal, WriteFault::Partial(240));
    assert!(matches!(
        mounted.commit(&mut access, candidate).unwrap_err().error(),
        LxmfCommitError::Backend {
            stage: LxmfProgramStage::Commit,
            ..
        }
    ));
    assert_eq!(
        mount(&mut access, &mut index::<4>())
            .unwrap()
            .message_count(),
        0
    );
    assert!(matches!(
        mounted.commit(&mut access, candidate),
        Ok(LxmfCommitOutcome::Committed(_))
    ));
}

#[test]
fn exact_commit_marker_with_corrupt_header_fails_closed() {
    let wire = complete_wire(0x22, 600, 1);
    let candidate = complete_candidate(&wire, 1, 1, 0x22, 1, normal_stamp());
    let mut access = bound(TestNor::erased(PARTITION_SIZE));
    let mut mounted_index = index::<4>();
    let mut mounted = mount(&mut access, &mut mounted_index).unwrap();
    mounted.commit(&mut access, candidate).unwrap();
    access.backend_mut().bytes[HEADER_TIMESTAMP_OFFSET] &= 0xfe;
    assert!(matches!(
        mount(&mut access, &mut index::<4>()),
        Err(LxmfStoreMountError::Fault(
            LxmfStoreFault::CommittedHeaderCorrupt { extent: 0 }
        ))
    ));
}

#[test]
fn committed_programmed_wire_padding_fails_closed() {
    let wire = complete_wire(0x22, 600, 1);
    let candidate = complete_candidate(&wire, 1, 1, 0x22, 1, normal_stamp());
    let mut access = bound(TestNor::erased(PARTITION_SIZE));
    let mut mounted_index = index::<4>();
    let mut mounted = mount(&mut access, &mut mounted_index).unwrap();
    mounted.commit(&mut access, candidate).unwrap();
    access.backend_mut().bytes[EXTENT_HEADER_SIZE + wire.len()] = 0x7f;
    assert!(matches!(
        mount(&mut access, &mut index::<4>()),
        Err(LxmfStoreMountError::Fault(
            LxmfStoreFault::CommittedWirePaddingProgrammed { extent: 0 }
        ))
    ));
}

#[test]
fn unclaimed_programmed_media_fails_closed() {
    let mut access = bound(TestNor::erased(PARTITION_SIZE));
    access.backend_mut().bytes[EXTENT_HEADER_SIZE + 17] = 0x7f;
    assert!(matches!(
        mount(&mut access, &mut index::<4>()),
        Err(LxmfStoreMountError::Fault(
            LxmfStoreFault::UnknownProgrammedExtent { extent: 0 }
        ))
    ));
}

#[test]
fn incomplete_multi_extent_header_cannot_hide_unknown_programmed_interior_media() {
    let wire = complete_wire(0x22, 7_000, 1);
    let candidate = complete_candidate(&wire, 1, 1, 0x22, 1, normal_stamp());
    let mut access = bound(TestNor::erased(PARTITION_SIZE));
    install_incomplete_record_start(&mut access, candidate, 3, 9);
    let expected_continuation = encode_header(
        binding(PARTITION_SIZE),
        MessageHandle::new(9).unwrap(),
        1,
        3,
        candidate.metadata(),
        digest_candidate_wire(candidate),
    );
    access.backend_mut().bytes
        [EXTENT_SIZE + HEADER_MAGIC_OFFSET..EXTENT_SIZE + HEADER_VERSION_OFFSET]
        .copy_from_slice(&expected_continuation[HEADER_MAGIC_OFFSET..HEADER_VERSION_OFFSET]);

    assert!(matches!(
        mount(&mut access, &mut index::<4>()),
        Err(LxmfStoreMountError::Fault(
            LxmfStoreFault::UnknownProgrammedExtent { extent: 1 }
        ))
    ));
}

#[test]
fn incomplete_multi_extent_header_cannot_hide_committed_interior_record() {
    let inner_wire = complete_wire(0x33, 100, 2);
    let inner_candidate = complete_candidate(&inner_wire, 2, 2, 0x33, 2, normal_stamp());
    let mut inner = bound(TestNor::erased(PARTITION_SIZE));
    let mut inner_index = index::<4>();
    let mut inner_mounted = mount(&mut inner, &mut inner_index).unwrap();
    inner_mounted.commit(&mut inner, inner_candidate).unwrap();

    let outer_wire = complete_wire(0x22, 7_000, 1);
    let outer_candidate = complete_candidate(&outer_wire, 1, 1, 0x22, 1, normal_stamp());
    let mut access = bound(TestNor::erased(PARTITION_SIZE));
    install_incomplete_record_start(&mut access, outer_candidate, 3, 9);
    access.backend_mut().bytes[EXTENT_SIZE..EXTENT_SIZE * 2]
        .copy_from_slice(&inner.backend().bytes[..EXTENT_SIZE]);

    assert!(matches!(
        mount(&mut access, &mut index::<4>()),
        Err(LxmfStoreMountError::Fault(
            LxmfStoreFault::UnknownCommittedExtent { extent: 1 }
        ))
    ));
}

#[test]
fn pending_retry_cannot_hide_committed_interior_record() {
    let inner_wire = complete_wire(0x33, 100, 2);
    let inner_candidate = complete_candidate(&inner_wire, 2, 2, 0x33, 2, normal_stamp());
    let mut inner = bound(TestNor::erased(PARTITION_SIZE));
    let mut inner_index = index::<4>();
    let mut inner_mounted = mount(&mut inner, &mut inner_index).unwrap();
    inner_mounted.commit(&mut inner, inner_candidate).unwrap();

    let wire = complete_wire(0x22, 7_000, 1);
    let candidate = complete_candidate(&wire, 1, 1, 0x22, 1, normal_stamp());
    let mut access = bound(TestNor::erased(PARTITION_SIZE));
    let mut mounted_index = index::<4>();
    let mut mounted = mount(&mut access, &mut mounted_index).unwrap();
    access.backend_mut().fail_read_after(0);
    assert!(matches!(
        mounted.commit(&mut access, candidate).unwrap_err().error(),
        LxmfCommitError::Backend {
            stage: LxmfProgramStage::Verification,
            ..
        }
    ));

    install_incomplete_record_start(&mut access, candidate, 3, 1);
    access.backend_mut().bytes[EXTENT_SIZE..EXTENT_SIZE * 2]
        .copy_from_slice(&inner.backend().bytes[..EXTENT_SIZE]);
    let writes_before_retry = access.backend().writes;
    assert!(matches!(
        mounted.commit(&mut access, candidate).unwrap_err().error(),
        LxmfCommitError::Fault(LxmfStoreFault::UnknownCommittedExtent { extent: 1 })
    ));
    assert_eq!(access.backend().writes, writes_before_retry);
}

#[test]
fn standalone_continuation_with_incomplete_marker_fails_closed() {
    let wire = complete_wire(0x22, 7_000, 1);
    let candidate = complete_candidate(&wire, 1, 1, 0x22, 1, normal_stamp());
    let header = encode_header(
        binding(PARTITION_SIZE),
        MessageHandle::new(9).unwrap(),
        1,
        3,
        candidate.metadata(),
        digest_candidate_wire(candidate),
    );
    let mut access = bound(TestNor::erased(PARTITION_SIZE));
    access.backend_mut().bytes[..EXTENT_HEADER_SIZE].copy_from_slice(&header);

    assert!(matches!(
        mount(&mut access, &mut index::<4>()),
        Err(LxmfStoreMountError::Fault(
            LxmfStoreFault::UnknownProgrammedExtent { extent: 0 }
        ))
    ));
}

#[test]
fn decoded_record_start_extending_beyond_bound_range_fails_closed() {
    let wire = complete_wire(0x22, 7_000, 1);
    let candidate = complete_candidate(&wire, 1, 1, 0x22, 1, normal_stamp());
    let header = encode_header(
        binding(PARTITION_SIZE),
        MessageHandle::new(9).unwrap(),
        0,
        3,
        candidate.metadata(),
        digest_candidate_wire(candidate),
    );
    let mut access = bound(TestNor::erased(PARTITION_SIZE));
    access.backend_mut().bytes[EXTENT_SIZE * 2..EXTENT_SIZE * 2 + EXTENT_HEADER_SIZE]
        .copy_from_slice(&header);

    assert!(matches!(
        mount(&mut access, &mut index::<4>()),
        Err(LxmfStoreMountError::Fault(
            LxmfStoreFault::UnknownProgrammedExtent { extent: 2 }
        ))
    ));
}

#[test]
fn duplicate_committed_handle_fails_closed() {
    let first_wire = complete_wire(0x22, 100, 1);
    let second_wire = complete_wire(0x23, 100, 2);
    let mut first = bound(TestNor::erased(PARTITION_SIZE));
    let mut first_index = index::<4>();
    let mut first_mounted = mount(&mut first, &mut first_index).unwrap();
    first_mounted
        .commit(
            &mut first,
            complete_candidate(&first_wire, 1, 1, 0x22, 1, normal_stamp()),
        )
        .unwrap();
    let mut second = bound(TestNor::erased(PARTITION_SIZE));
    let mut second_index = index::<4>();
    let mut second_mounted = mount(&mut second, &mut second_index).unwrap();
    second_mounted
        .commit(
            &mut second,
            complete_candidate(&second_wire, 2, 2, 0x23, 2, normal_stamp()),
        )
        .unwrap();
    first.backend_mut().bytes[EXTENT_SIZE..EXTENT_SIZE * 2]
        .copy_from_slice(&second.backend().bytes[..EXTENT_SIZE]);
    assert!(matches!(
        mount(&mut first, &mut index::<4>()),
        Err(LxmfStoreMountError::Fault(
            LxmfStoreFault::DuplicateCommittedHandle { handle }
        )) if handle.get() == 1
    ));
}

#[test]
fn runtime_state_and_candidate_never_contain_a_whole_message_buffer() {
    assert!(core::mem::size_of::<InboundMessageCandidate<'_>>() < 256);
    let mounted_bytes = core::mem::size_of::<MountedLxmfStore<'static>>();
    let slot_bytes = core::mem::size_of::<LxmfStoreIndexSlot>();
    assert!(mounted_bytes < EXTENT_SIZE);
    assert!(slot_bytes > 0);
    assert_eq!(
        core::mem::size_of::<[LxmfStoreIndexSlot; 512]>(),
        slot_bytes * 512
    );
    assert!(mounted_bytes < core::mem::size_of::<[LxmfStoreIndexSlot; 4]>());
}
