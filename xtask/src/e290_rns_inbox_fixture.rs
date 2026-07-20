//! Deterministic raw-RNS inbox fixtures for E290 power-loss qualification.

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
    check_erase, check_read, check_write,
};
use reticulum_rns_inbox_store::{
    BoundInboxStore, InboxAdmissionOutcome, InboxCandidate, InboxDestination, InboxItemId,
    InboxStoreBinding, InboxStoreDeviceId, PARTITION_SIZE, PHYSICAL_FORMAT_VERSION, RECORD_SIZE,
    mount,
};
use sha2::{Digest, Sha256};

const MESSAGE_STORE_OFFSET: usize = 0x73_0000;
const CLAIM_SIZE: usize = 32;
const DIGEST_OFFSET: usize = 512;
const DIGEST_SIZE: usize = 32;
const COMMIT_SIZE: usize = 32;
const PARTIAL_CLAIM_SIZE: usize = 16;

const FIXTURE_DESTINATION: [u8; 16] = [
    0x6f, 0x7d, 0x91, 0x14, 0xa8, 0x23, 0xce, 0x59, 0x02, 0xb6, 0x48, 0xdd, 0x35, 0xea, 0x70, 0x9c,
];
const FIXTURE_PAYLOAD: &[u8] = b"reticulum-rs-firmware deterministic E290 raw-RNS inbox fixture v1";

const _: () = assert!(PARTITION_SIZE == 0x20_0000);
const _: () = assert!(RECORD_SIZE == 576);
const _: () = assert!(CLAIM_SIZE == 32);
const _: () = assert!(DIGEST_OFFSET + DIGEST_SIZE + COMMIT_SIZE == RECORD_SIZE);
const _: () = assert!(PARTIAL_CLAIM_SIZE < CLAIM_SIZE);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FixtureMode {
    InterruptedClaim,
    InterruptedCommit,
    InvalidDigest,
    Committed,
}

impl FixtureMode {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "interrupted-claim" => Some(Self::InterruptedClaim),
            "interrupted-commit" => Some(Self::InterruptedCommit),
            "invalid-digest" => Some(Self::InvalidDigest),
            "committed" => Some(Self::Committed),
            _ => None,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::InterruptedClaim => "interrupted-claim",
            Self::InterruptedCommit => "interrupted-commit",
            Self::InvalidDigest => "invalid-digest",
            Self::Committed => "committed",
        }
    }
}

struct Options {
    output: PathBuf,
    source_mac: [u8; 6],
    mode: FixtureMode,
}

#[derive(Debug)]
struct FixtureSummary {
    mode: FixtureMode,
    length: usize,
    sha256: String,
}

impl std::fmt::Display for FixtureSummary {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "mode={} length={} sha256={}",
            self.mode.as_str(),
            self.length,
            self.sha256
        )
    }
}

/// Generate one deterministic, board-bound E290 inbox fixture.
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
        Ok(summary) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(reason) => {
            eprintln!("error: {reason}");
            ExitCode::FAILURE
        }
    }
}

fn usage() {
    eprintln!(
        "usage: cargo run -p xtask -- e290-rns-inbox-fixture \
         --output <absent-path> --source-mac <12-lowercase-hex> \
         <interrupted-claim|interrupted-commit|invalid-digest|committed>"
    );
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut output = None;
    let mut source_mac = None;
    let mut mode = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--output" => {
                if output.is_some() {
                    return Err("--output may be supplied only once".to_owned());
                }
                let value = required_flag_value(args, index, "--output")?;
                if value.is_empty() {
                    return Err("--output must not be empty".to_owned());
                }
                output = Some(PathBuf::from(value));
                index += 2;
            }
            "--source-mac" => {
                if source_mac.is_some() {
                    return Err("--source-mac may be supplied only once".to_owned());
                }
                let value = required_flag_value(args, index, "--source-mac")?;
                source_mac = Some(parse_source_mac(value)?);
                index += 2;
            }
            value if value.starts_with('-') => {
                return Err("unknown option".to_owned());
            }
            value => {
                let parsed = FixtureMode::parse(value).ok_or_else(|| {
                    "mode must be one of interrupted-claim, interrupted-commit, invalid-digest, or committed"
                        .to_owned()
                })?;
                if mode.replace(parsed).is_some() {
                    return Err("exactly one fixture mode is required".to_owned());
                }
                index += 1;
            }
        }
    }

    Ok(Options {
        output: output.ok_or_else(|| "--output is required".to_owned())?,
        source_mac: source_mac.ok_or_else(|| "--source-mac is required".to_owned())?,
        mode: mode.ok_or_else(|| "exactly one fixture mode is required".to_owned())?,
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

fn execute(options: &Options) -> Result<FixtureSummary, String> {
    require_absent_output(&options.output)?;
    let bytes = fixture_bytes(options.source_mac, options.mode)?;
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    write_new_secure(&options.output, &bytes)?;
    Ok(FixtureSummary {
        mode: options.mode,
        length: bytes.len(),
        sha256,
    })
}

fn require_absent_output(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("could not inspect --output: {error}")),
        Ok(_) => Err("--output must name an absent path".to_owned()),
    }
}

fn fixture_bytes(source_mac: [u8; 6], mode: FixtureMode) -> Result<Vec<u8>, String> {
    let binding = binding(source_mac);
    let mut access = BoundInboxStore::new(MemoryNor::erased(), binding);
    let mut mounted = mount(&mut access)
        .map_err(|_| "internal fixture store did not mount as erased".to_owned())?;
    let candidate =
        InboxCandidate::new(InboxDestination::new(FIXTURE_DESTINATION), FIXTURE_PAYLOAD)
            .map_err(|_| "fixed fixture payload exceeds the store capacity".to_owned())?;
    match mounted.accept(&mut access, candidate) {
        Ok(InboxAdmissionOutcome::Accepted(id)) if id == InboxItemId::FIRST => {}
        Ok(_) => return Err("internal fixture record was not accepted".to_owned()),
        Err(_) => return Err("internal fixture record could not be committed".to_owned()),
    }

    let mut bytes = access.into_backend().into_bytes();
    match mode {
        FixtureMode::InterruptedClaim => {
            let partial_claim: [u8; PARTIAL_CLAIM_SIZE] = bytes[..PARTIAL_CLAIM_SIZE]
                .try_into()
                .expect("fixed partial claim range");
            bytes.fill(0xff);
            bytes[..PARTIAL_CLAIM_SIZE].copy_from_slice(&partial_claim);
        }
        FixtureMode::InterruptedCommit => {
            bytes[RECORD_SIZE - COMMIT_SIZE..RECORD_SIZE].fill(0xff);
        }
        FixtureMode::InvalidDigest => {
            let byte = bytes[DIGEST_OFFSET..DIGEST_OFFSET + DIGEST_SIZE]
                .iter_mut()
                .find(|byte| **byte != 0)
                .ok_or_else(|| {
                    "internal fixture digest cannot be corrupted monotonically".to_owned()
                })?;
            *byte &= (*byte).wrapping_sub(1);
        }
        FixtureMode::Committed => {}
    }
    Ok(bytes)
}

const fn binding(source_mac: [u8; 6]) -> InboxStoreBinding {
    InboxStoreBinding::new(
        InboxStoreDeviceId::new([
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
        MESSAGE_STORE_OFFSET,
        PARTITION_SIZE,
        PHYSICAL_FORMAT_VERSION,
    )
}

fn write_new_secure(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);

    let mut file = options
        .open(path)
        .map_err(|error| format!("could not securely create output: {error}"))?;
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("could not restrict output permissions: {error}"))?;
    file.write_all(bytes)
        .map_err(|error| format!("could not write output: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("could not sync output: {error}"))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MemoryNorError {
    OutOfBounds,
    NotAligned,
    NotWritable,
}

impl NorFlashError for MemoryNorError {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::OutOfBounds => NorFlashErrorKind::OutOfBounds,
            Self::NotAligned => NorFlashErrorKind::NotAligned,
            Self::NotWritable => NorFlashErrorKind::Other,
        }
    }
}

struct MemoryNor {
    bytes: Vec<u8>,
}

impl MemoryNor {
    fn erased() -> Self {
        Self {
            bytes: vec![0xff; PARTITION_SIZE],
        }
    }

    #[cfg(test)]
    fn from_bytes(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl ErrorType for MemoryNor {
    type Error = MemoryNorError;
}

impl ReadNorFlash for MemoryNor {
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

impl NorFlash for MemoryNor {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = 4096;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        check_write(self, offset, bytes.len()).map_err(map_nor_error)?;
        let offset = offset as usize;
        let stored = &mut self.bytes[offset..offset + bytes.len()];
        if stored
            .iter()
            .zip(bytes)
            .any(|(stored, supplied)| *stored & *supplied != *supplied)
        {
            return Err(MemoryNorError::NotWritable);
        }
        for (stored, supplied) in stored.iter_mut().zip(bytes) {
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

impl MultiwriteNorFlash for MemoryNor {}

const fn map_nor_error(error: NorFlashErrorKind) -> MemoryNorError {
    match error {
        NorFlashErrorKind::OutOfBounds => MemoryNorError::OutOfBounds,
        NorFlashErrorKind::NotAligned => MemoryNorError::NotAligned,
        _ => MemoryNorError::NotWritable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reticulum_rns_inbox_store::{
        InboxRecordCorruption, InboxStoreFault, InboxStoreMountError, InboxStoreState,
    };
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);
    const MAC: [u8; 6] = [0xac, 0xa7, 0x04, 0xe1, 0x3e, 0x88];

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    struct TemporaryDirectory {
        path: PathBuf,
    }

    impl TemporaryDirectory {
        fn new() -> Self {
            let nonce = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "reticulum-e290-inbox-fixture-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("unique temporary directory is created");
            Self { path }
        }
    }

    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn parser_accepts_each_mode_and_flexible_order() {
        for mode in [
            "interrupted-claim",
            "interrupted-commit",
            "invalid-digest",
            "committed",
        ] {
            let args = strings(&[
                mode,
                "--source-mac",
                "aca704e13e88",
                "--output",
                "fixture.bin",
            ]);
            let options = parse(&args).expect("complete invocation parses");
            assert_eq!(options.mode.as_str(), mode);
            assert_eq!(options.source_mac, MAC);
            assert_eq!(options.output, PathBuf::from("fixture.bin"));
        }
    }

    #[test]
    fn parser_rejects_missing_duplicate_unknown_and_extra_arguments() {
        let invalid = [
            strings(&[]),
            strings(&["committed", "--source-mac", "aca704e13e88"]),
            strings(&["committed", "--output", "fixture.bin"]),
            strings(&["--output", "fixture.bin", "--source-mac", "aca704e13e88"]),
            strings(&[
                "committed",
                "--output",
                "first.bin",
                "--output",
                "second.bin",
                "--source-mac",
                "aca704e13e88",
            ]),
            strings(&[
                "committed",
                "--output",
                "fixture.bin",
                "--source-mac",
                "aca704e13e88",
                "--source-mac",
                "000000000000",
            ]),
            strings(&[
                "committed",
                "invalid-digest",
                "--output",
                "fixture.bin",
                "--source-mac",
                "aca704e13e88",
            ]),
            strings(&[
                "unknown-mode",
                "--output",
                "fixture.bin",
                "--source-mac",
                "aca704e13e88",
            ]),
            strings(&[
                "committed",
                "--unknown",
                "value",
                "--output",
                "fixture.bin",
                "--source-mac",
                "aca704e13e88",
            ]),
            strings(&[
                "committed",
                "--output=fixture.bin",
                "--source-mac",
                "aca704e13e88",
            ]),
            strings(&["committed", "--output", "--source-mac", "aca704e13e88"]),
        ];

        for args in invalid {
            assert!(
                parse(&args).is_err(),
                "invalid arguments unexpectedly parsed"
            );
        }
    }

    #[test]
    fn source_mac_is_exact_lowercase_hex_and_errors_are_redacted() {
        for invalid in [
            "aca704e13e8",
            "aca704e13e888",
            "ACA704E13E88",
            "ac:a7:04:e1:3e:88",
            "aca704e13e8g",
            "aca704e13e8-",
        ] {
            let error = parse_source_mac(invalid).expect_err("invalid MAC must fail");
            assert!(!error.contains(invalid));
            assert!(!error.contains("aca704e13e88"));
        }
        assert_eq!(parse_source_mac("000000000000").unwrap(), [0; 6]);
        assert_eq!(parse_source_mac("ffffffffffff").unwrap(), [0xff; 6]);
    }

    #[test]
    fn binding_is_exact_e290_message_store_identity_and_range() {
        let actual = binding(MAC);
        assert_eq!(
            actual.device().as_bytes(),
            b"e290-flash\xac\xa7\x04\xe1\x3e\x88"
        );
        assert_eq!(actual.absolute_offset(), 0x73_0000);
        assert_eq!(actual.length(), 0x20_0000);
        assert_eq!(actual.format_version(), PHYSICAL_FORMAT_VERSION);
    }

    #[test]
    fn every_fixture_remounts_with_the_expected_real_store_classification() {
        for mode in [
            FixtureMode::InterruptedClaim,
            FixtureMode::InterruptedCommit,
            FixtureMode::InvalidDigest,
            FixtureMode::Committed,
        ] {
            let bytes = fixture_bytes(MAC, mode).expect("fixture generation succeeds");
            assert_eq!(bytes.len(), PARTITION_SIZE);
            assert!(bytes[RECORD_SIZE..].iter().all(|byte| *byte == 0xff));
            let mut access = BoundInboxStore::new(MemoryNor::from_bytes(bytes), binding(MAC));
            match mode {
                FixtureMode::InterruptedClaim => assert!(matches!(
                    mount(&mut access),
                    Err(InboxStoreMountError::Fault(
                        InboxStoreFault::InterruptedClaim
                    ))
                )),
                FixtureMode::InterruptedCommit => assert!(matches!(
                    mount(&mut access),
                    Err(InboxStoreMountError::Fault(
                        InboxStoreFault::InterruptedRecord
                    ))
                )),
                FixtureMode::InvalidDigest => assert!(matches!(
                    mount(&mut access),
                    Err(InboxStoreMountError::Fault(
                        InboxStoreFault::CommittedRecordCorrupt(InboxRecordCorruption::Digest)
                    ))
                )),
                FixtureMode::Committed => {
                    let mounted = mount(&mut access).expect("committed fixture mounts");
                    let InboxStoreState::Occupied(item) = mounted.state() else {
                        panic!("committed fixture must be occupied");
                    };
                    assert_eq!(item.id(), InboxItemId::FIRST);
                    assert_eq!(item.destination().as_bytes(), &FIXTURE_DESTINATION);
                    assert_eq!(item.payload(), FIXTURE_PAYLOAD);
                }
            }
        }
    }

    #[test]
    fn fixtures_are_deterministic_distinct_and_board_bound() {
        let mut digests = std::collections::BTreeSet::new();
        for mode in [
            FixtureMode::InterruptedClaim,
            FixtureMode::InterruptedCommit,
            FixtureMode::InvalidDigest,
            FixtureMode::Committed,
        ] {
            let first = fixture_bytes(MAC, mode).unwrap();
            let second = fixture_bytes(MAC, mode).unwrap();
            assert_eq!(first, second);
            digests.insert(format!("{:x}", Sha256::digest(first)));
        }
        assert_eq!(digests.len(), 4);

        let committed = fixture_bytes(MAC, FixtureMode::Committed).unwrap();
        let wrong_mac = [0xac, 0xa7, 0x04, 0xe1, 0x3f, 0x88];
        let mut access = BoundInboxStore::new(MemoryNor::from_bytes(committed), binding(wrong_mac));
        assert!(matches!(
            mount(&mut access),
            Err(InboxStoreMountError::Fault(
                InboxStoreFault::RecordBindingMismatch { .. }
            ))
        ));
    }

    #[test]
    fn fixture_hashes_are_pinned_for_the_reviewed_board_identity() {
        for (mode, expected) in [
            (
                FixtureMode::InterruptedClaim,
                "4b9e6dad1415850588c001b17053e893ab1316aaa1b6d584082170d049f871f0",
            ),
            (
                FixtureMode::InterruptedCommit,
                "a8a8d40f63a69c7e3df59f4af1960f241f464566a5ae9251c12209eb3334c66a",
            ),
            (
                FixtureMode::InvalidDigest,
                "bb24e892d435a0b6888cc16f8733f096015a36f0f19dcd8a22e0978602e55ad5",
            ),
            (
                FixtureMode::Committed,
                "dee21d3c72a914ac00627c49a119631999dc9e986ce18897b9a171254c79561b",
            ),
        ] {
            let bytes = fixture_bytes(MAC, mode).unwrap();
            assert_eq!(format!("{:x}", Sha256::digest(bytes)), expected);
        }
    }

    #[test]
    fn transformations_represent_monotonic_partial_or_corrupt_programming() {
        let committed = fixture_bytes(MAC, FixtureMode::Committed).unwrap();
        let claim = fixture_bytes(MAC, FixtureMode::InterruptedClaim).unwrap();
        assert_eq!(
            &claim[..PARTIAL_CLAIM_SIZE],
            &committed[..PARTIAL_CLAIM_SIZE]
        );
        assert!(claim[PARTIAL_CLAIM_SIZE..].iter().all(|byte| *byte == 0xff));

        let interrupted_commit = fixture_bytes(MAC, FixtureMode::InterruptedCommit).unwrap();
        let erased_commit_tail = RECORD_SIZE - COMMIT_SIZE;
        assert_eq!(
            &interrupted_commit[..erased_commit_tail],
            &committed[..erased_commit_tail]
        );
        assert!(
            interrupted_commit[erased_commit_tail..]
                .iter()
                .all(|byte| *byte == 0xff)
        );

        let invalid_digest = fixture_bytes(MAC, FixtureMode::InvalidDigest).unwrap();
        let differences = committed
            .iter()
            .zip(&invalid_digest)
            .filter(|(before, after)| before != after)
            .count();
        assert_eq!(differences, 1);
        assert!(
            committed
                .iter()
                .zip(&invalid_digest)
                .all(|(before, after)| *before & *after == *after)
        );
    }

    #[test]
    fn output_is_create_new_exact_synced_content_with_redacted_summary() {
        let temporary = TemporaryDirectory::new();
        let output = temporary.path.join("fixture.bin");
        let options = Options {
            output: output.clone(),
            source_mac: MAC,
            mode: FixtureMode::Committed,
        };
        let expected = fixture_bytes(MAC, FixtureMode::Committed).unwrap();
        let summary = execute(&options).expect("new fixture is written");
        let summary = summary.to_string();

        assert_eq!(fs::read(&output).unwrap(), expected);
        assert_eq!(fs::metadata(&output).unwrap().len(), PARTITION_SIZE as u64);
        assert!(summary.starts_with("mode=committed length=2097152 sha256="));
        assert_eq!(summary.split_whitespace().count(), 3);
        assert!(!summary.contains("aca704e13e88"));
        assert!(!summary.contains(output.to_string_lossy().as_ref()));
        assert!(!summary.contains(std::str::from_utf8(FIXTURE_PAYLOAD).unwrap()));
        assert!(!summary.contains("6f7d9114a823ce5902b648dd35ea709c"));

        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&output).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn existing_output_is_never_overwritten() {
        let temporary = TemporaryDirectory::new();
        let output = temporary.path.join("fixture.bin");
        fs::write(&output, b"keep this exact content").unwrap();
        let options = Options {
            output: output.clone(),
            source_mac: MAC,
            mode: FixtureMode::Committed,
        };
        let error = execute(&options).expect_err("existing output must fail");
        assert_eq!(error, "--output must name an absent path");
        assert_eq!(fs::read(&output).unwrap(), b"keep this exact content");
    }

    #[cfg(unix)]
    #[test]
    fn output_symlink_is_not_followed() {
        use std::os::unix::fs::symlink;

        let temporary = TemporaryDirectory::new();
        let target = temporary.path.join("target.bin");
        let output = temporary.path.join("fixture.bin");
        fs::write(&target, b"target remains unchanged").unwrap();
        symlink(&target, &output).unwrap();
        let options = Options {
            output,
            source_mac: MAC,
            mode: FixtureMode::InvalidDigest,
        };
        assert!(execute(&options).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"target remains unchanged");
    }
}
