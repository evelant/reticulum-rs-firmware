//! Read-only validation and metadata inspection for a raw E290 LXMF partition.

use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
    process::ExitCode,
};

use embedded_storage::nor_flash::{
    ErrorType, NorFlashError, NorFlashErrorKind, ReadNorFlash, check_read,
};
use reticulum_lxmf_model::{CarrierProvenance, StampAdmissionProvenance};
use reticulum_lxmf_store::{
    BoundLxmfStore, EXTENT_SIZE, LxmfStoreBinding, LxmfStoreDeviceId, LxmfStoreIndexSlot,
    PHYSICAL_FORMAT_VERSION, mount,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

const SCHEMA: &str = "reticulum.e290-lxmf-store-inspection.v1";
const LXMF_STORE_OFFSET: usize = 0x93_0000;
const LXMF_STORE_LENGTH: usize = 0x20_0000;
const LXMF_STORE_LENGTH_U64: u64 = 0x20_0000;
const MAX_RECORDS: usize = LXMF_STORE_LENGTH / EXTENT_SIZE;

const _: () = assert!(LXMF_STORE_LENGTH == 2 * 1024 * 1024);
const _: () = assert!(MAX_RECORDS == 512);

struct Options {
    image: PathBuf,
    source_mac: [u8; 6],
}

#[derive(Debug, Serialize)]
struct InspectionReport {
    schema: &'static str,
    image_bytes: usize,
    image_sha256: String,
    binding: BindingSummary,
    store: StoreSummary,
    records: Vec<RecordSummary>,
}

#[derive(Debug, Serialize)]
struct BindingSummary {
    absolute_offset: usize,
    length: usize,
    physical_format_version: u16,
}

#[derive(Debug, Serialize)]
struct StoreSummary {
    committed_records: usize,
    consumed_extents: usize,
    total_extents: usize,
    record_order: &'static str,
}

#[derive(Debug, Serialize)]
struct RecordSummary {
    handle: u64,
    message_id_hex: String,
    authenticated_material_fingerprint_hex: String,
    destination_hash_hex: String,
    source_hash_hex: String,
    timestamp_bits_hex: String,
    carrier: &'static str,
    stamp_admission: StampSummary,
    lengths: LengthSummary,
    exact_wire_digest_hex: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum StampSummary {
    NotRequired {
        stamp_present: bool,
    },
    TrustedPriorTicket,
    ProofOfWork {
        target_cost: u16,
        observed_value: u16,
    },
}

#[derive(Debug, Serialize)]
struct LengthSummary {
    normalized_wire: u32,
    carrier_payload: u32,
    title: u32,
    content: u32,
    fields_encoded: u32,
}

/// Validate one exact raw partition and print a metadata-only JSON report.
pub fn run(args: Vec<String>) -> ExitCode {
    let options = match parse(&args) {
        Ok(options) => options,
        Err(reason) => {
            eprintln!("error: {reason}");
            usage();
            return ExitCode::from(2);
        }
    };

    match execute(&options) {
        Ok(report) => match serde_json::to_string_pretty(&report) {
            Ok(json) => {
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("error: could not encode inspection report: {error}");
                ExitCode::FAILURE
            }
        },
        Err(reason) => {
            eprintln!("error: {reason}");
            ExitCode::FAILURE
        }
    }
}

fn usage() {
    eprintln!(
        "usage: cargo run -p xtask -- e290-lxmf-store-inspect \
         --image <raw-2MiB-partition.bin> --source-mac <12-lowercase-hex>"
    );
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut image = None;
    let mut source_mac = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--image" => {
                if image.is_some() {
                    return Err("--image may be supplied only once".to_owned());
                }
                let value = required_flag_value(args, index, "--image")?;
                if value.is_empty() {
                    return Err("--image must not be empty".to_owned());
                }
                image = Some(PathBuf::from(value));
                index += 2;
            }
            "--source-mac" => {
                if source_mac.is_some() {
                    return Err("--source-mac may be supplied only once".to_owned());
                }
                source_mac = Some(parse_source_mac(required_flag_value(
                    args,
                    index,
                    "--source-mac",
                )?)?);
                index += 2;
            }
            _ => return Err("unknown argument".to_owned()),
        }
    }

    Ok(Options {
        image: image.ok_or_else(|| "--image is required".to_owned())?,
        source_mac: source_mac.ok_or_else(|| "--source-mac is required".to_owned())?,
    })
}

fn required_flag_value<'a>(
    args: &'a [String],
    flag_index: usize,
    flag: &str,
) -> Result<&'a str, String> {
    match args.get(flag_index + 1).map(String::as_str) {
        Some(value) if !value.starts_with('-') => Ok(value),
        _ => Err(format!("{flag} requires a value")),
    }
}

fn parse_source_mac(value: &str) -> Result<[u8; 6], String> {
    if value.len() != 12
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("--source-mac must be exactly 12 lowercase hexadecimal characters".to_owned());
    }

    let bytes = value.as_bytes();
    let mut mac = [0_u8; 6];
    for (index, byte) in mac.iter_mut().enumerate() {
        *byte = (hex_nibble(bytes[index * 2]) << 4) | hex_nibble(bytes[index * 2 + 1]);
    }
    Ok(mac)
}

const fn hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => 0,
    }
}

fn execute(options: &Options) -> Result<InspectionReport, String> {
    let bytes = read_image(&options.image)?;
    inspect_bytes(bytes, options.source_mac)
}

fn read_image(path: &Path) -> Result<Vec<u8>, String> {
    let mut file = File::open(path).map_err(|error| format!("could not open --image: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("could not inspect --image: {error}"))?;
    if !metadata.is_file() {
        return Err("--image must name a regular file".to_owned());
    }
    validate_image_length(metadata.len())?;

    let bytes = read_contract_bytes(&mut file)?;
    let final_length = file
        .metadata()
        .map_err(|error| format!("could not re-inspect --image after reading: {error}"))?
        .len();
    if final_length != LXMF_STORE_LENGTH_U64 {
        return Err(format!(
            "--image changed length while being read; expected {LXMF_STORE_LENGTH} bytes, observed {final_length}"
        ));
    }
    Ok(bytes)
}

fn validate_image_length(observed: u64) -> Result<(), String> {
    if observed != LXMF_STORE_LENGTH_U64 {
        return Err(format!(
            "--image must contain exactly {LXMF_STORE_LENGTH} bytes; observed {observed}"
        ));
    }
    Ok(())
}

fn read_contract_bytes(reader: &mut impl Read) -> Result<Vec<u8>, String> {
    let mut bytes = vec![0_u8; LXMF_STORE_LENGTH];
    reader
        .read_exact(&mut bytes)
        .map_err(|error| format!("could not read exact --image contents: {error}"))?;

    let mut trailing = [0_u8; 1];
    match reader.read_exact(&mut trailing) {
        Ok(()) => Err(format!(
            "--image changed length while being read; expected {LXMF_STORE_LENGTH} bytes, observed additional data"
        )),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(bytes),
        Err(error) => Err(format!(
            "could not verify --image ended at exactly {LXMF_STORE_LENGTH} bytes: {error}"
        )),
    }
}

fn inspect_bytes(bytes: Vec<u8>, source_mac: [u8; 6]) -> Result<InspectionReport, String> {
    if bytes.len() != LXMF_STORE_LENGTH {
        return Err(format!(
            "--image must contain exactly {LXMF_STORE_LENGTH} bytes; observed {}",
            bytes.len()
        ));
    }
    let image_sha256 = format!("{:x}", Sha256::digest(&bytes));
    let binding = binding(source_mac);
    let mut access = BoundLxmfStore::new(ImageNor::new(bytes), binding);
    let mut index = (0..MAX_RECORDS)
        .map(|_| LxmfStoreIndexSlot::new())
        .collect::<Vec<_>>();
    let store = mount(&mut access, &mut index)
        .map_err(|error| format!("LXMF store validation failed: {error:?}"))?;

    let mut records = Vec::with_capacity(store.message_count());
    for receipt in store.receipts() {
        let metadata = store
            .metadata(receipt.handle())
            .ok_or_else(|| "validated store lost indexed metadata".to_owned())?;
        let lengths = metadata.lengths();
        records.push(RecordSummary {
            handle: receipt.handle().get(),
            message_id_hex: hex(metadata.message_id().as_bytes()),
            authenticated_material_fingerprint_hex: hex(metadata
                .authenticated_material()
                .as_bytes()),
            destination_hash_hex: hex(metadata.destination().as_bytes()),
            source_hash_hex: hex(metadata.source().as_bytes()),
            timestamp_bits_hex: format!("{:016x}", metadata.timestamp_bits()),
            carrier: carrier_name(metadata.carrier()),
            stamp_admission: stamp_summary(metadata.stamp_admission()),
            lengths: LengthSummary {
                normalized_wire: lengths.normalized_wire(),
                carrier_payload: lengths.carrier_payload(),
                title: lengths.title(),
                content: lengths.content(),
                fields_encoded: lengths.fields_encoded(),
            },
            exact_wire_digest_hex: hex(receipt.fingerprint().exact_wire_digest().as_bytes()),
        });
    }

    Ok(InspectionReport {
        schema: SCHEMA,
        image_bytes: LXMF_STORE_LENGTH,
        image_sha256,
        binding: BindingSummary {
            absolute_offset: binding.absolute_offset(),
            length: binding.length(),
            physical_format_version: binding.format_version(),
        },
        store: StoreSummary {
            committed_records: store.message_count(),
            consumed_extents: store.consumed_extents(),
            total_extents: MAX_RECORDS,
            record_order: "physical-commit",
        },
        records,
    })
}

const fn binding(source_mac: [u8; 6]) -> LxmfStoreBinding {
    LxmfStoreBinding::new(
        LxmfStoreDeviceId::new([
            b'e',
            b'2',
            b'9',
            b'0',
            b'-',
            b'f',
            b'l',
            b'a',
            b's',
            b'h',
            source_mac[0],
            source_mac[1],
            source_mac[2],
            source_mac[3],
            source_mac[4],
            source_mac[5],
        ]),
        LXMF_STORE_OFFSET,
        LXMF_STORE_LENGTH,
        PHYSICAL_FORMAT_VERSION,
    )
}

const fn carrier_name(carrier: CarrierProvenance) -> &'static str {
    match carrier {
        CarrierProvenance::Complete => "complete",
        CarrierProvenance::Opportunistic => "opportunistic",
        CarrierProvenance::LinkDataContextNone => "link-data-context-none",
        CarrierProvenance::ResourceComplete => "resource-complete",
    }
}

const fn stamp_summary(stamp: StampAdmissionProvenance) -> StampSummary {
    match stamp {
        StampAdmissionProvenance::NotRequired { stamp_present } => {
            StampSummary::NotRequired { stamp_present }
        }
        StampAdmissionProvenance::TrustedPriorTicket => StampSummary::TrustedPriorTicket,
        StampAdmissionProvenance::ProofOfWork {
            target_cost,
            observed_value,
        } => StampSummary::ProofOfWork {
            target_cost: target_cost.get(),
            observed_value,
        },
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ImageNorError {
    OutOfBounds,
    NotAligned,
}

impl NorFlashError for ImageNorError {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::OutOfBounds => NorFlashErrorKind::OutOfBounds,
            Self::NotAligned => NorFlashErrorKind::NotAligned,
        }
    }
}

struct ImageNor {
    bytes: Vec<u8>,
}

impl ImageNor {
    fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    #[cfg(test)]
    fn erased() -> Self {
        Self::new(vec![0xff; LXMF_STORE_LENGTH])
    }

    #[cfg(test)]
    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl ErrorType for ImageNor {
    type Error = ImageNorError;
}

impl ReadNorFlash for ImageNor {
    const READ_SIZE: usize = 4;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        check_read(self, offset, bytes.len()).map_err(map_nor_error)?;
        let offset = offset as usize;
        bytes.copy_from_slice(&self.bytes[offset..offset + bytes.len()]);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.bytes.len()
    }
}

const fn map_nor_error(error: NorFlashErrorKind) -> ImageNorError {
    match error {
        NorFlashErrorKind::OutOfBounds => ImageNorError::OutOfBounds,
        _ => ImageNorError::NotAligned,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_storage::nor_flash::{MultiwriteNorFlash, NorFlash, check_erase, check_write};
    use reticulum_lxmf_model::{
        AuthenticatedMaterialFingerprint, DestinationHash, InboundMessageCandidate,
        InboundMessageLengths, InboundMessageMetadata, MessageId, NormalizedWire, SourceHash,
    };
    use reticulum_lxmf_store::LxmfCommitOutcome;
    use std::{
        fs::{self, OpenOptions},
        io::Cursor,
        sync::atomic::{AtomicU64, Ordering},
    };

    const MAC: [u8; 6] = [0xac, 0xa7, 0x04, 0xe1, 0x3e, 0x88];
    static NEXT_TEMP_IMAGE: AtomicU64 = AtomicU64::new(0);

    struct TempImage {
        path: PathBuf,
    }

    impl TempImage {
        fn sparse(length: u64) -> Self {
            let sequence = NEXT_TEMP_IMAGE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "reticulum-e290-lxmf-inspector-{}-{sequence}.bin",
                std::process::id()
            ));
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .expect("temporary image path is unique");
            file.set_len(length).expect("sparse image length is set");
            Self { path }
        }
    }

    impl Drop for TempImage {
        fn drop(&mut self) {
            fs::remove_file(&self.path).expect("temporary image is removed");
        }
    }

    impl NorFlash for ImageNor {
        const WRITE_SIZE: usize = 4;
        const ERASE_SIZE: usize = EXTENT_SIZE;

        fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
            check_write(self, offset, bytes.len()).map_err(map_nor_error)?;
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
            check_erase(self, from, to).map_err(map_nor_error)?;
            self.bytes[from as usize..to as usize].fill(0xff);
            Ok(())
        }
    }

    impl MultiwriteNorFlash for ImageNor {}

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn synthetic_image() -> Vec<u8> {
        let mut access = BoundLxmfStore::new(ImageNor::erased(), binding(MAC));
        let mut index = (0..MAX_RECORDS)
            .map(|_| LxmfStoreIndexSlot::new())
            .collect::<Vec<_>>();
        let mut store = mount(&mut access, &mut index).expect("erased store mounts");

        for (id, carrier, stamp) in [
            (
                1_u8,
                CarrierProvenance::Complete,
                StampAdmissionProvenance::NotRequired {
                    stamp_present: false,
                },
            ),
            (
                2_u8,
                CarrierProvenance::Opportunistic,
                StampAdmissionProvenance::TrustedPriorTicket,
            ),
        ] {
            let destination = [0x20 + id; 16];
            let mut wire = vec![0x80 + id; 600 + usize::from(id)];
            wire[..16].copy_from_slice(&destination);
            let carrier_len = if carrier.omits_destination_prefix() {
                wire.len() - destination.len()
            } else {
                wire.len()
            };
            let metadata = InboundMessageMetadata::new(
                MessageId::new([id; 32]),
                AuthenticatedMaterialFingerprint::new([0x40 + id; 32]),
                DestinationHash::new(destination),
                SourceHash::new([0x60 + id; 16]),
                0x3ff0_0000_0000_0000 + u64::from(id),
                carrier,
                stamp,
                InboundMessageLengths::new(wire.len(), carrier_len, id as usize, 3, 1).unwrap(),
            )
            .unwrap();
            let normalized = if carrier.omits_destination_prefix() {
                NormalizedWire::Opportunistic {
                    implied_destination: &destination,
                    carrier_payload: &wire[destination.len()..],
                }
            } else {
                NormalizedWire::Contiguous(&wire)
            };
            let candidate = InboundMessageCandidate::new(metadata, normalized).unwrap();
            assert!(matches!(
                store.commit(&mut access, candidate).unwrap(),
                LxmfCommitOutcome::Committed(_)
            ));
        }
        drop(store);
        access.into_backend().into_bytes()
    }

    #[test]
    fn parser_requires_exact_image_and_lowercase_mac() {
        let options = parse(&strings(&[
            "--image",
            "store.bin",
            "--source-mac",
            "aca704e13e88",
        ]))
        .unwrap();
        assert_eq!(options.image, PathBuf::from("store.bin"));
        assert_eq!(options.source_mac, MAC);

        for invalid in [
            strings(&[]),
            strings(&["--image", "store.bin"]),
            strings(&["--source-mac", "aca704e13e88"]),
            strings(&[
                "--image",
                "a.bin",
                "--image",
                "b.bin",
                "--source-mac",
                "aca704e13e88",
            ]),
            strings(&["--image", "store.bin", "--source-mac", "AC:A7:04:E1:3E:88"]),
            strings(&[
                "--image",
                "store.bin",
                "--source-mac",
                "aca704e13e88",
                "extra",
            ]),
        ] {
            assert!(parse(&invalid).is_err());
        }
    }

    #[test]
    fn binding_matches_the_product_partition_contract() {
        let actual = binding(MAC);
        assert_eq!(
            actual.device().as_bytes(),
            b"e290-flash\xac\xa7\x04\xe1\x3e\x88"
        );
        assert_eq!(actual.absolute_offset(), 0x93_0000);
        assert_eq!(actual.length(), 0x20_0000);
        assert_eq!(actual.format_version(), PHYSICAL_FORMAT_VERSION);
    }

    #[test]
    fn erased_image_validates_as_an_empty_store() {
        let report = inspect_bytes(vec![0xff; LXMF_STORE_LENGTH], MAC).unwrap();
        assert_eq!(report.store.committed_records, 0);
        assert_eq!(report.store.consumed_extents, 0);
        assert_eq!(report.records.len(), 0);
    }

    #[test]
    fn synthetic_committed_image_reports_exact_metadata_without_wire_bytes() {
        let bytes = synthetic_image();
        let report = inspect_bytes(bytes, MAC).expect("real store image validates");
        assert_eq!(report.store.committed_records, 2);
        assert_eq!(report.store.consumed_extents, 2);
        assert_eq!(report.records.len(), 2);
        assert_eq!(report.records[0].handle, 1);
        assert_eq!(report.records[1].handle, 2);
        assert_eq!(report.records[0].message_id_hex, "01".repeat(32));
        assert_eq!(report.records[1].message_id_hex, "02".repeat(32));
        assert_eq!(report.records[0].destination_hash_hex, "21".repeat(16));
        assert_eq!(report.records[1].source_hash_hex, "62".repeat(16));
        assert_eq!(report.records[0].carrier, "complete");
        assert_eq!(report.records[1].carrier, "opportunistic");
        assert!(matches!(
            report.records[0].stamp_admission,
            StampSummary::NotRequired {
                stamp_present: false
            }
        ));
        assert!(matches!(
            report.records[1].stamp_admission,
            StampSummary::TrustedPriorTicket
        ));
        assert_eq!(report.records[0].lengths.normalized_wire, 601);
        assert_eq!(report.records[1].lengths.normalized_wire, 602);
        assert_eq!(report.records[1].lengths.carrier_payload, 586);

        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("aca704e13e88"));
        assert!(!json.contains(&"81".repeat(64)));
        assert!(!json.contains(&"82".repeat(64)));
    }

    #[test]
    fn wrong_board_and_corrupt_wire_fail_closed() {
        let image = synthetic_image();
        let wrong_mac = [0xac, 0xa7, 0x04, 0xe1, 0x3f, 0x88];
        assert!(
            inspect_bytes(image.clone(), wrong_mac)
                .unwrap_err()
                .contains("validation failed")
        );

        let mut corrupt = image;
        corrupt[512] ^= 1;
        let error = inspect_bytes(corrupt, MAC).unwrap_err();
        assert!(error.contains("CommittedWireDigestMismatch"));
    }

    #[test]
    fn exact_partition_length_is_required() {
        for length in [0, LXMF_STORE_LENGTH - 1, LXMF_STORE_LENGTH + 1] {
            let error = inspect_bytes(vec![0xff; length], MAC).unwrap_err();
            assert!(error.contains("exactly 2097152 bytes"));
        }
    }

    #[test]
    fn file_reader_rejects_oversized_image_from_handle_metadata() {
        let image = TempImage::sparse(LXMF_STORE_LENGTH_U64 + 1);
        let error = read_image(&image.path).unwrap_err();
        assert_eq!(
            error,
            "--image must contain exactly 2097152 bytes; observed 2097153"
        );
    }

    #[test]
    fn contract_reader_requires_eof_after_exact_partition() {
        let mut input = Cursor::new(vec![0_u8; LXMF_STORE_LENGTH + 1]);
        let error = read_contract_bytes(&mut input).unwrap_err();
        assert!(error.contains("changed length while being read"));
        assert!(error.contains("observed additional data"));
    }

    #[test]
    fn contract_reader_fails_if_partition_shrinks_during_read() {
        let mut input = Cursor::new(vec![0_u8; LXMF_STORE_LENGTH - 1]);
        let error = read_contract_bytes(&mut input).unwrap_err();
        assert!(error.contains("could not read exact --image contents"));
    }
}
