//! Bounded authenticated USB client for E290 device-API requests.

use std::{
    fmt::Write as _,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    num::NonZeroU32,
    path::{Path, PathBuf},
    process::ExitCode,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::fs::OpenOptions;

use rand_core::{CryptoRng, RngCore};
use reticulum_device_api::{
    ApiVersion, DestinationHash, DeviceRequest, DeviceResponse, IdempotencyKey, LxmfMessageHandle,
    LxmfMessageSummary, LxmfReadChunk, LxmfReadLength, MAX_LXMF_BASIC_CONTENT_BYTES,
    MAX_LXMF_BASIC_TITLE_BYTES, MAX_LXMF_READ_CHUNK_BYTES, MAX_MESSAGE_BYTES,
    MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES, RequestEnvelope, RequestId, SubmissionFailure, SubmissionId,
    SubmissionState, decode_response, encode_request,
};
use reticulum_device_api_framing::{DecodeEvent, Record, StreamDecoder, TxAdvanceError};
use reticulum_device_api_handoff::{MessageLength, OwnedMessage};
use reticulum_device_api_session::{
    BearerBinding, ClientCredential, ClientHelloFlight, ClientParameters, ClientProofFlight,
    ClientRequestFlight, ClientSession, DeviceId,
};
use reticulum_device_pairing_client::load_activated_credential;
use reticulum_lxmf_wire::{MessageView, WireLimits};
use serde::{Deserialize, Serialize};
use serialport::ClearBuffer;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

const BAUD_RATE: u32 = 115_200;
const DEFAULT_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_SUBMIT_AND_WAIT_TIMEOUT_MS: u64 = 45_000;
const POLL_INTERVAL: Duration = Duration::from_millis(500);
const OPEN_SETTLE_MS: u64 = 250;
const IO_SLICE_MS: u64 = 100;
const READ_BUFFER_CAPACITY: usize = 1_024;
// Exact downloads are streamed without this ceiling; only the optional
// in-memory structural projection is capped.
const MAX_LXMF_HOST_PARSE_BYTES: usize = 16 * 1024 * 1024;
const LXMF_HOST_MAX_NESTING_DEPTH: usize = 16;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
enum EvidenceSchema {
    #[serde(rename = "reticulum.e290-authenticated-usb.evidence.v1")]
    V1,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum EvidenceSubmissionFailure {
    NoPath,
    DeliveryTimeout,
    Rejected,
    Internal,
}

impl From<SubmissionFailure> for EvidenceSubmissionFailure {
    fn from(failure: SubmissionFailure) -> Self {
        match failure {
            SubmissionFailure::NoPath => Self::NoPath,
            SubmissionFailure::DeliveryTimeout => Self::DeliveryTimeout,
            SubmissionFailure::Rejected => Self::Rejected,
            SubmissionFailure::Internal => Self::Internal,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
enum SubmitTerminalEvidence {
    Delivered {
        submission_id: u64,
        packet_len: u16,
        encoded_packet_sha256: String,
    },
    Failed {
        submission_id: u64,
        reason: EvidenceSubmissionFailure,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "command", rename_all = "kebab-case", deny_unknown_fields)]
enum AuthenticatedEvidenceV1 {
    SubmitAndWait {
        schema: EvidenceSchema,
        device_id: String,
        session_id: String,
        terminal: SubmitTerminalEvidence,
    },
    RnsInboxPeek {
        schema: EvidenceSchema,
        device_id: String,
        session_id: String,
        item_id: u64,
        destination: String,
        length: u16,
        payload_sha256: String,
    },
}

#[derive(Debug, Eq, PartialEq)]
enum Command {
    SystemCapabilities,
    IdentitySummary,
    RnsInboxStatus,
    RnsInboxPeek {
        output: PathBuf,
    },
    LxmfList,
    LxmfRead {
        handle: LxmfMessageHandle,
        output: PathBuf,
    },
    LxmfSend {
        destination: DestinationHash,
        timestamp_unix_ms: u64,
        title: Vec<u8>,
        content: Vec<u8>,
        idempotency_key: IdempotencyKey,
    },
    LxmfSendAndWait {
        destination: DestinationHash,
        timestamp_unix_ms: u64,
        title: Vec<u8>,
        content: Vec<u8>,
        idempotency_key: IdempotencyKey,
    },
    SubmissionStatus {
        id: SubmissionId,
    },
    SubmitRnsData {
        destination: DestinationHash,
        payload: Vec<u8>,
        idempotency_key: IdempotencyKey,
    },
    SubmitAndWait {
        destination: DestinationHash,
        payload: Vec<u8>,
        idempotency_key: IdempotencyKey,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommandKind {
    SystemCapabilities,
    IdentitySummary,
    RnsInboxStatus,
    RnsInboxPeek,
    LxmfList,
    LxmfRead,
    LxmfSend,
    LxmfSendAndWait,
    SubmissionStatus,
    SubmitRnsData,
    SubmitAndWait,
}

struct RequestIds {
    next: Option<u64>,
}

impl RequestIds {
    const fn new() -> Self {
        Self { next: Some(1) }
    }

    fn take(&mut self) -> Result<RequestId, String> {
        let next = self
            .next
            .ok_or_else(|| "logical request ID space is exhausted".to_owned())?;
        self.next = next.checked_add(1);
        Ok(RequestId(next))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WaitDecision {
    PollAgain,
    RetryInternal,
    Delivered {
        submission_id: SubmissionId,
        details: reticulum_device_api::PreparedPacketDetails,
    },
    Failed {
        submission_id: SubmissionId,
        failure: SubmissionFailure,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ParsedLxmfMetadata {
    title_sha256: [u8; 32],
    content_sha256: [u8; 32],
    title_utf8: bool,
    content_utf8: bool,
}

#[derive(Clone, Copy)]
struct LxmfSendFields<'a> {
    destination: DestinationHash,
    timestamp_unix_ms: u64,
    title: &'a [u8],
    content: &'a [u8],
    idempotency_key: IdempotencyKey,
}

fn lxmf_send_fields(command: &Command) -> Option<LxmfSendFields<'_>> {
    match command {
        Command::LxmfSend {
            destination,
            timestamp_unix_ms,
            title,
            content,
            idempotency_key,
        }
        | Command::LxmfSendAndWait {
            destination,
            timestamp_unix_ms,
            title,
            content,
            idempotency_key,
        } => Some(LxmfSendFields {
            destination: *destination,
            timestamp_unix_ms: *timestamp_unix_ms,
            title,
            content,
            idempotency_key: *idempotency_key,
        }),
        _ => None,
    }
}

struct Options {
    port: String,
    state_file: PathBuf,
    timeout: Duration,
    command: Command,
    evidence_output: Option<PathBuf>,
}

struct ReservedOutput {
    path: PathBuf,
    file: Option<File>,
    label: &'static str,
    committed: bool,
}

impl ReservedOutput {
    #[cfg(unix)]
    fn create(path: &Path, label: &'static str) -> Result<Self, String> {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        options.mode(0o600);
        let file = options.open(path).map_err(|error| {
            format!(
                "could not create {label} output {} without overwriting: {error}",
                path.display()
            )
        })?;
        let reservation = Self {
            path: path.to_owned(),
            file: Some(file),
            label,
            committed: false,
        };
        reservation
            .file
            .as_ref()
            .expect("new reservation must retain its file")
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|error| {
                format!(
                    "could not restrict {label} output {} to owner-only permissions: {error}",
                    path.display()
                )
            })?;
        Ok(reservation)
    }

    #[cfg(not(unix))]
    fn create(path: &Path, label: &'static str) -> Result<Self, String> {
        Err(format!(
            "could not create {label} output {}: owner-only output reservations require Unix file-permission support",
            path.display()
        ))
    }

    fn write_uncommitted(&mut self, bytes: &[u8]) -> Result<(), String> {
        let file = self
            .file
            .as_mut()
            .expect("uncommitted reservation must retain its file");
        file.write_all(bytes).map_err(|error| {
            format!(
                "could not write {} output {}: {error}",
                self.label,
                self.path.display()
            )
        })
    }

    fn read_back_uncommitted(&mut self, expected_len: usize) -> Result<Vec<u8>, String> {
        let file = self
            .file
            .as_mut()
            .expect("uncommitted reservation must retain its file");
        file.flush().map_err(|error| {
            format!(
                "could not flush {} output {} before validation: {error}",
                self.label,
                self.path.display()
            )
        })?;
        file.seek(SeekFrom::Start(0)).map_err(|error| {
            format!(
                "could not rewind {} output {} for validation: {error}",
                self.label,
                self.path.display()
            )
        })?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(expected_len).map_err(|_| {
            format!(
                "could not reserve {expected_len} bytes to validate {} output {}",
                self.label,
                self.path.display()
            )
        })?;
        bytes.resize(expected_len, 0);
        file.read_exact(&mut bytes).map_err(|error| {
            format!(
                "could not read back complete {} output {} for validation: {error}",
                self.label,
                self.path.display()
            )
        })?;
        let mut trailing = [0_u8; 1];
        match file.read(&mut trailing) {
            Ok(0) => Ok(bytes),
            Ok(_) => Err(format!(
                "{} output {} grew beyond its authenticated length during validation",
                self.label,
                self.path.display()
            )),
            Err(error) => Err(format!(
                "could not validate the end of {} output {}: {error}",
                self.label,
                self.path.display()
            )),
        }
    }

    fn verify_sha256_uncommitted(
        &mut self,
        expected_len: usize,
        expected_sha256: &[u8; 32],
    ) -> Result<(), String> {
        let file = self
            .file
            .as_mut()
            .expect("uncommitted reservation must retain its file");
        file.flush().map_err(|error| {
            format!(
                "could not flush {} output {} before digest verification: {error}",
                self.label,
                self.path.display()
            )
        })?;
        file.seek(SeekFrom::Start(0)).map_err(|error| {
            format!(
                "could not rewind {} output {} for digest verification: {error}",
                self.label,
                self.path.display()
            )
        })?;
        let mut hasher = Sha256::new();
        let mut remaining = expected_len;
        let mut buffer = [0_u8; 8 * 1024];
        while remaining != 0 {
            let requested = remaining.min(buffer.len());
            let read = file.read(&mut buffer[..requested]).map_err(|error| {
                format!(
                    "could not read back {} output {} for digest verification: {error}",
                    self.label,
                    self.path.display()
                )
            })?;
            if read == 0 {
                return Err(format!(
                    "{} output {} ended before its authenticated length of {expected_len} bytes",
                    self.label,
                    self.path.display()
                ));
            }
            hasher.update(&buffer[..read]);
            remaining -= read;
        }
        let mut trailing = [0_u8; 1];
        match file.read(&mut trailing) {
            Ok(0) => {}
            Ok(_) => {
                return Err(format!(
                    "{} output {} grew beyond its authenticated length during digest verification",
                    self.label,
                    self.path.display()
                ));
            }
            Err(error) => {
                return Err(format!(
                    "could not validate the end of {} output {}: {error}",
                    self.label,
                    self.path.display()
                ));
            }
        }
        let actual: [u8; 32] = hasher.finalize().into();
        if actual != *expected_sha256 {
            return Err(format!(
                "{} output {} did not retain the authenticated LXMF SHA-256",
                self.label,
                self.path.display()
            ));
        }
        Ok(())
    }

    fn finish(mut self) -> Result<(), String> {
        let file = self
            .file
            .as_mut()
            .expect("uncommitted reservation must retain its file");
        file.sync_all().map_err(|error| {
            format!(
                "could not sync {} output {}: {error}",
                self.label,
                self.path.display()
            )
        })?;
        sync_output_parent(&self.path).map_err(|error| {
            format!(
                "could not sync {} output parent {}: {error}",
                self.label,
                output_parent(&self.path).display()
            )
        })?;
        self.committed = true;
        Ok(())
    }

    fn commit(mut self, bytes: &[u8]) -> Result<(), String> {
        self.write_uncommitted(bytes)?;
        self.finish()
    }
}

impl Drop for ReservedOutput {
    fn drop(&mut self) {
        if !self.committed {
            self.file.take();
            if std::fs::remove_file(&self.path).is_ok() {
                let _ = sync_output_parent(&self.path);
            }
        }
    }
}

fn output_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn sync_output_parent(path: &Path) -> io::Result<()> {
    File::open(output_parent(path))?.sync_all()
}

enum CommandOutputs {
    None,
    RnsInboxPeek {
        payload: ReservedOutput,
        evidence: Option<ReservedOutput>,
    },
    SubmitAndWait {
        evidence: Option<ReservedOutput>,
    },
    LxmfRead {
        wire: ReservedOutput,
    },
}

impl CommandOutputs {
    fn reserve(command: &Command, evidence_output: Option<&Path>) -> Result<Self, String> {
        match command {
            Command::RnsInboxPeek { output } => {
                let payload = ReservedOutput::create(output, "inbox")?;
                let evidence = evidence_output
                    .map(|path| ReservedOutput::create(path, "evidence"))
                    .transpose()?;
                Ok(Self::RnsInboxPeek { payload, evidence })
            }
            Command::SubmitAndWait { .. } => {
                let evidence = evidence_output
                    .map(|path| ReservedOutput::create(path, "evidence"))
                    .transpose()?;
                Ok(Self::SubmitAndWait { evidence })
            }
            Command::LxmfRead { output, .. } => {
                debug_assert!(evidence_output.is_none());
                Ok(Self::LxmfRead {
                    wire: ReservedOutput::create(output, "LXMF")?,
                })
            }
            _ => {
                debug_assert!(evidence_output.is_none());
                Ok(Self::None)
            }
        }
    }

    fn into_submit_evidence(self) -> Result<Option<ReservedOutput>, String> {
        match self {
            Self::SubmitAndWait { evidence } => Ok(evidence),
            _ => Err("internal output reservation mismatch for submit-and-wait".to_owned()),
        }
    }

    fn into_peek(self) -> Result<(ReservedOutput, Option<ReservedOutput>), String> {
        match self {
            Self::RnsInboxPeek { payload, evidence } => Ok((payload, evidence)),
            _ => Err("internal output reservation mismatch for rns-inbox-peek".to_owned()),
        }
    }

    fn into_lxmf_read(self) -> Result<ReservedOutput, String> {
        match self {
            Self::LxmfRead { wire } => Ok(wire),
            _ => Err("internal output reservation mismatch for lxmf-read".to_owned()),
        }
    }
}

struct HostRng;

impl RngCore for HostRng {
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
        self.try_fill_bytes(destination)
            .unwrap_or_else(|error| panic!("operating-system randomness failed: {error}"));
    }

    fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
        getrandom::fill(destination).map_err(|_| {
            NonZeroU32::new(rand_core::Error::CUSTOM_START)
                .expect("rand_core custom error base is nonzero")
                .into()
        })
    }
}

impl CryptoRng for HostRng {}

trait OutboundFlight {
    fn remaining(&self) -> &[u8];
    fn advance(&mut self, acknowledged: usize) -> Result<(), TxAdvanceError>;
}

impl OutboundFlight for ClientHelloFlight {
    fn remaining(&self) -> &[u8] {
        ClientHelloFlight::remaining(self)
    }

    fn advance(&mut self, acknowledged: usize) -> Result<(), TxAdvanceError> {
        ClientHelloFlight::advance(self, acknowledged)
    }
}

impl OutboundFlight for ClientProofFlight {
    fn remaining(&self) -> &[u8] {
        ClientProofFlight::remaining(self)
    }

    fn advance(&mut self, acknowledged: usize) -> Result<(), TxAdvanceError> {
        ClientProofFlight::advance(self, acknowledged)
    }
}

impl OutboundFlight for ClientRequestFlight {
    fn remaining(&self) -> &[u8] {
        ClientRequestFlight::remaining(self)
    }

    fn advance(&mut self, acknowledged: usize) -> Result<(), TxAdvanceError> {
        ClientRequestFlight::advance(self, acknowledged)
    }
}

struct BufferedRecordReader {
    decoder: StreamDecoder,
    bytes: Zeroizing<[u8; READ_BUFFER_CAPACITY]>,
    next: usize,
    length: usize,
}

impl BufferedRecordReader {
    fn new() -> Self {
        Self {
            decoder: StreamDecoder::new(),
            bytes: Zeroizing::new([0_u8; READ_BUFFER_CAPACITY]),
            next: 0,
            length: 0,
        }
    }

    fn read_record<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
        deadline: Instant,
        stage: &str,
    ) -> Result<Record, String> {
        loop {
            while self.next < self.length {
                let byte = self.bytes[self.next];
                self.next += 1;
                match self.decoder.push(byte) {
                    DecodeEvent::Pending => {}
                    DecodeEvent::Record(record) => return Ok(record),
                    DecodeEvent::MalformedCobs => {
                        return Err(format!("malformed COBS framing while waiting for {stage}"));
                    }
                    DecodeEvent::MalformedRecord(error) => {
                        return Err(format!(
                            "noncanonical device-API record while waiting for {stage}: {error:?}"
                        ));
                    }
                    DecodeEvent::Overflow => {
                        return Err(format!(
                            "oversized device-API record while waiting for {stage}"
                        ));
                    }
                }
            }

            self.next = 0;
            self.length = 0;
            if Instant::now() >= deadline {
                return Err(format!("timed out waiting for {stage}"));
            }
            match reader.read(&mut self.bytes[..]) {
                Ok(0) => thread::yield_now(),
                Ok(length) => self.length = length,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(format!("could not read {stage}: {error}")),
            }
        }
    }
}

pub(crate) fn run(args: Vec<String>) -> ExitCode {
    let options = match parse(&args) {
        Ok(options) => options,
        Err(reason) => {
            eprintln!("{}", cli_error_line(&reason));
            usage();
            return ExitCode::from(2);
        }
    };

    let transaction = {
        let stdout = io::stdout();
        let mut stdout = stdout.lock();
        transact(&options, &mut stdout)
    };

    match transaction {
        Ok(line) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        Err(reason) => {
            eprintln!("{}", cli_error_line(&reason));
            ExitCode::FAILURE
        }
    }
}

fn cli_error_line(reason: &str) -> String {
    format!("error: {reason}")
}

fn transact(options: &Options, accepted_output: &mut dyn Write) -> Result<String, String> {
    let outputs = CommandOutputs::reserve(&options.command, options.evidence_output.as_deref())?;
    let deadline = Instant::now()
        .checked_add(options.timeout)
        .ok_or_else(|| "--timeout-ms is too large for the host monotonic clock".to_owned())?;
    let activated =
        load_activated_credential(&options.state_file).map_err(|error| error.to_string())?;
    let (device_id, credential_id, generation, psk) = activated.into_parts();
    let expected_device_id = DeviceId::new(*device_id.as_bytes());
    let credential = ClientCredential::from_zeroizing(credential_id, generation, psk);

    let mut port = serialport::new(&options.port, BAUD_RATE)
        .timeout(Duration::from_millis(IO_SLICE_MS))
        .open()
        .map_err(|error| format!("could not open {}: {error}", options.port))?;
    port.write_data_terminal_ready(true)
        .map_err(|error| format!("could not assert DTR on {}: {error}", options.port))?;
    port.write_request_to_send(false)
        .map_err(|error| format!("could not clear RTS on {}: {error}", options.port))?;
    thread::sleep(Duration::from_millis(OPEN_SETTLE_MS));
    port.clear(ClearBuffer::Input)
        .map_err(|error| format!("could not clear stale input on {}: {error}", options.port))?;

    let mut reader = BufferedRecordReader::new();
    let mut rng = HostRng;

    let hello = ClientHelloFlight::begin(
        ClientParameters::new(expected_device_id, BearerBinding::UsbSerialJtag),
        credential,
        &mut rng,
    )
    .map_err(|error| format!("could not start authenticated handshake: {error:?}"))?;
    let hello = write_flight(&mut *port, hello, deadline, "client hello")?;
    let awaiting_server_hello = hello
        .try_finish()
        .map_err(|_| "complete client hello did not advance client typestate".to_owned())?;

    let server_hello = reader.read_record(&mut *port, deadline, "server hello")?;
    let awaiting_server_proof = awaiting_server_hello
        .accept(server_hello)
        .map_err(|error| format!("server hello was rejected: {error:?}"))?;
    let server_proof = reader.read_record(&mut *port, deadline, "server proof")?;
    let proof = awaiting_server_proof
        .verify(server_proof)
        .map_err(|error| format!("server proof was rejected: {error:?}"))?;
    let proof = write_flight(&mut *port, proof, deadline, "client proof")?;
    let session = proof
        .try_finish()
        .map_err(|_| "complete client proof did not advance client typestate".to_owned())?;
    let session_id = *session.session_id().as_bytes();
    let mut request_ids = RequestIds::new();
    if let Some(fields) = lxmf_send_fields(&options.command) {
        write_lxmf_send_intent(
            accepted_output,
            command_name(&options.command),
            device_id.as_bytes(),
            &session_id,
            fields,
        )?;
    }
    let request_id = request_ids.take()?;
    let (session, response) = exchange(
        &mut *port,
        &mut reader,
        session,
        request_id,
        command_request(&options.command),
        command_name(&options.command),
        deadline,
    )?;

    if matches!(&options.command, Command::LxmfList) {
        return continue_lxmf_list(
            &mut *port,
            &mut reader,
            session,
            &mut request_ids,
            response,
            deadline,
            device_id.as_bytes(),
            &session_id,
            accepted_output,
        );
    }
    if let Command::LxmfRead { handle, output } = &options.command {
        return continue_lxmf_read(
            &mut *port,
            &mut reader,
            session,
            &mut request_ids,
            response,
            deadline,
            device_id.as_bytes(),
            &session_id,
            *handle,
            output,
            outputs.into_lxmf_read()?,
        );
    }
    if let Some(fields) = lxmf_send_fields(&options.command) {
        let accepted = classify_lxmf_send_response(command_name(&options.command), response)?;
        if matches!(&options.command, Command::LxmfSendAndWait { .. }) {
            write_lxmf_send_accepted(
                accepted_output,
                command_name(&options.command),
                device_id.as_bytes(),
                &session_id,
                accepted,
                fields,
            )?;
            let terminal = wait_for_delivery(
                &mut *port,
                &mut reader,
                session,
                &mut request_ids,
                deadline,
                device_id.as_bytes(),
                &session_id,
                accepted.id,
                None,
                "lxmf-send-and-wait",
            )?;
            return Ok(format!(
                "{terminal} message_id={} {}",
                hex(accepted.message_id()),
                format_lxmf_send_context(fields),
            ));
        }
        drop(session);
        return Ok(format_lxmf_send_accepted(
            command_name(&options.command),
            device_id.as_bytes(),
            &session_id,
            accepted,
            fields,
        ));
    }
    if matches!(&options.command, Command::SubmitAndWait { .. }) {
        return continue_submit_and_wait(
            response,
            device_id.as_bytes(),
            &session_id,
            accepted_output,
            |submission_id| {
                let evidence_output = outputs.into_submit_evidence()?;
                wait_for_delivery(
                    &mut *port,
                    &mut reader,
                    session,
                    &mut request_ids,
                    deadline,
                    device_id.as_bytes(),
                    &session_id,
                    submission_id,
                    evidence_output,
                    "submit-and-wait",
                )
            },
        );
    }
    drop(session);
    format_one_shot_response(
        &options.command,
        device_id.as_bytes(),
        &session_id,
        response,
        outputs,
    )
}

fn classify_lxmf_send_response(
    operation: &str,
    response: DeviceResponse,
) -> Result<reticulum_device_api::LxmfBasicSendAccepted, String> {
    match response {
        DeviceResponse::LxmfBasicSendAccepted(accepted) => Ok(accepted),
        DeviceResponse::Error(error) => Err(format_api_error(operation, error)),
        other => Err(format!(
            "device returned response kind {} instead of {operation}",
            other.kind()
        )),
    }
}

fn format_lxmf_send_context(fields: LxmfSendFields<'_>) -> String {
    format!(
        "destination={} timestamp_unix_ms={} idempotency_key={} title_len={} content_len={}",
        hex(&fields.destination.0),
        fields.timestamp_unix_ms,
        hex(&fields.idempotency_key.0),
        fields.title.len(),
        fields.content.len(),
    )
}

fn write_lxmf_send_intent(
    output: &mut dyn Write,
    command: &str,
    device_id: &[u8; 16],
    session_id: &[u8; 16],
    fields: LxmfSendFields<'_>,
) -> Result<(), String> {
    let record = format!(
        "command={command} outcome=intent device_id={} session_id={} {}\n",
        hex(device_id),
        hex(session_id),
        format_lxmf_send_context(fields),
    );
    output.write_all(record.as_bytes()).map_err(|error| {
        format!("could not write the stable {command} retry material before submission: {error}")
    })?;
    output.flush().map_err(|error| {
        format!("could not flush the stable {command} retry material before submission: {error}")
    })
}

fn format_lxmf_send_accepted(
    command: &str,
    device_id: &[u8; 16],
    session_id: &[u8; 16],
    accepted: reticulum_device_api::LxmfBasicSendAccepted,
    fields: LxmfSendFields<'_>,
) -> String {
    format!(
        "command={command} outcome=accepted device_id={} session_id={} submission_id={} message_id={} {}",
        hex(device_id),
        hex(session_id),
        accepted.id.0,
        hex(accepted.message_id()),
        format_lxmf_send_context(fields),
    )
}

fn write_lxmf_send_accepted(
    output: &mut dyn Write,
    command: &str,
    device_id: &[u8; 16],
    session_id: &[u8; 16],
    accepted: reticulum_device_api::LxmfBasicSendAccepted,
    fields: LxmfSendFields<'_>,
) -> Result<(), String> {
    let record = format!(
        "{}\n",
        format_lxmf_send_accepted(command, device_id, session_id, accepted, fields)
    );
    output.write_all(record.as_bytes()).map_err(|error| {
        format!(
            "device accepted LXMF submission {} but the host could not write its accepted marker: {error}",
            accepted.id.0
        )
    })?;
    output.flush().map_err(|error| {
        format!(
            "device accepted LXMF submission {} but the host could not flush its accepted marker: {error}",
            accepted.id.0
        )
    })
}

fn continue_submit_and_wait<F>(
    response: DeviceResponse,
    device_id: &[u8; 16],
    session_id: &[u8; 16],
    accepted_output: &mut dyn Write,
    wait: F,
) -> Result<String, String>
where
    F: FnOnce(SubmissionId) -> Result<String, String>,
{
    let submission_id = match response {
        DeviceResponse::SubmitRnsDataAccepted(accepted) => accepted.id,
        DeviceResponse::Error(error) => return Err(format_api_error("submit-and-wait", error)),
        other => {
            return Err(format!(
                "device returned response kind {} instead of submit-and-wait",
                other.kind(),
            ));
        }
    };
    write_submit_and_wait_accepted(accepted_output, device_id, session_id, submission_id)?;
    wait(submission_id)
}

fn write_submit_and_wait_accepted(
    output: &mut dyn Write,
    device_id: &[u8; 16],
    session_id: &[u8; 16],
    submission_id: SubmissionId,
) -> Result<(), String> {
    let record = format!(
        "command=submit-and-wait outcome=accepted device_id={} session_id={} submission_id={}\n",
        hex(device_id),
        hex(session_id),
        submission_id.0,
    );
    output.write_all(record.as_bytes()).map_err(|error| {
        format!(
            "device accepted submission {} but the host could not write its accepted marker: {error}",
            submission_id.0
        )
    })?;
    output.flush().map_err(|error| {
        format!(
            "device accepted submission {} but the host could not flush its accepted marker: {error}",
            submission_id.0
        )
    })
}

fn exchange<P: Read + Write + ?Sized>(
    port: &mut P,
    reader: &mut BufferedRecordReader,
    session: ClientSession,
    request_id: RequestId,
    request: DeviceRequest<'_>,
    operation: &str,
    deadline: Instant,
) -> Result<(ClientSession, DeviceResponse), String> {
    let request = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id,
        request,
    };
    let mut request_bytes = [0_u8; MAX_MESSAGE_BYTES];
    let request_length = encode_request(&request, &mut request_bytes)
        .map_err(|error| format!("could not encode {operation} request: {error:?}"))?;
    let request_owner = OwnedMessage::new(
        MessageLength::new(request_length)
            .map_err(|_| format!("{operation} request exceeded the session limit"))?,
        request_bytes,
    );
    let request = session
        .frame_request(request_owner)
        .map_err(|error| format!("could not frame authenticated request: {:?}", error.kind()))?;
    let request = write_flight(&mut *port, request, deadline, "authenticated request")?;
    let awaiting_response = request
        .try_finish()
        .map_err(|_| "complete request did not advance client typestate".to_owned())?;

    let response_record = reader.read_record(&mut *port, deadline, "authenticated response")?;
    let authenticated = awaiting_response
        .authenticate(response_record)
        .map_err(|error| format!("authenticated response was rejected: {error:?}"))?;
    let (session, response_message) = authenticated.into_parts();
    let response = decode_response(response_message.encoded())
        .map_err(|error| format!("could not decode authenticated response: {error:?}"))?;
    validate_response_version(response.version)?;
    if response.request_id != request_id {
        return Err(format!(
            "device returned request ID {} instead of {}",
            response.request_id.0, request_id.0
        ));
    }
    Ok((session, response.response))
}

fn validate_response_version(version: ApiVersion) -> Result<(), String> {
    if version.major == ApiVersion::CURRENT.major {
        Ok(())
    } else {
        Err(format!(
            "device returned incompatible API version {}.{}; client major is {}",
            version.major,
            version.minor,
            ApiVersion::CURRENT.major
        ))
    }
}

fn classify_lxmf_next_response(
    operation: &str,
    response: DeviceResponse,
    after: Option<LxmfMessageHandle>,
) -> Result<Option<LxmfMessageSummary>, String> {
    match response {
        DeviceResponse::LxmfNext(summary) => {
            if let Some(after) = after
                && summary.handle().get() <= after.get()
            {
                return Err(format!(
                    "device returned non-advancing LXMF handle {} after {}",
                    summary.handle().get(),
                    after.get(),
                ));
            }
            Ok(Some(summary))
        }
        DeviceResponse::Error(error)
            if error.code == reticulum_device_api::ApiErrorCode::NotFound =>
        {
            Ok(None)
        }
        DeviceResponse::Error(error) => Err(format_api_error(operation, error)),
        other => Err(format!(
            "device returned response kind {} instead of {operation}",
            other.kind()
        )),
    }
}

fn format_lxmf_summary_line(
    device_id: &[u8; 16],
    session_id: &[u8; 16],
    ordinal: u64,
    summary: LxmfMessageSummary,
) -> String {
    format!(
        "command=lxmf-list outcome=item device_id={} session_id={} ordinal={} handle={} message_id={} destination={} source={} timestamp_bits={:016x} normalized_wire_len={} title_len={} content_len={} fields_encoded_len={} wire_sha256={}",
        hex(device_id),
        hex(session_id),
        ordinal,
        summary.handle().get(),
        hex(summary.message_id()),
        hex(&summary.destination().0),
        hex(&summary.source().0),
        summary.timestamp_bits(),
        summary.normalized_wire_len(),
        summary.title_len(),
        summary.content_len(),
        summary.fields_encoded_len(),
        hex(summary.exact_wire_sha256()),
    )
}

#[allow(clippy::too_many_arguments)]
fn continue_lxmf_list<P: Read + Write + ?Sized>(
    port: &mut P,
    reader: &mut BufferedRecordReader,
    mut session: ClientSession,
    request_ids: &mut RequestIds,
    mut response: DeviceResponse,
    deadline: Instant,
    device_id: &[u8; 16],
    session_id: &[u8; 16],
    output: &mut dyn Write,
) -> Result<String, String> {
    let mut after = None;
    let mut count = 0_u64;
    loop {
        let Some(summary) = classify_lxmf_next_response("lxmf-list", response, after)? else {
            return Ok(format!(
                "command=lxmf-list outcome=ok device_id={} session_id={} count={count}",
                hex(device_id),
                hex(session_id),
            ));
        };
        count = count
            .checked_add(1)
            .ok_or_else(|| "LXMF listing count overflowed u64".to_owned())?;
        let record = format_lxmf_summary_line(device_id, session_id, count, summary);
        output
            .write_all(record.as_bytes())
            .and_then(|()| output.write_all(b"\n"))
            .map_err(|error| {
                format!(
                    "authenticated LXMF summary for handle {} was received but could not be written: {error}",
                    summary.handle().get()
                )
            })?;
        output.flush().map_err(|error| {
            format!(
                "authenticated LXMF summary for handle {} was written but could not be flushed: {error}",
                summary.handle().get()
            )
        })?;
        after = Some(summary.handle());
        let request_id = request_ids.take()?;
        let (restored, next_response) = exchange(
            port,
            reader,
            session,
            request_id,
            DeviceRequest::LxmfNext { after },
            "lxmf-list",
            deadline,
        )?;
        session = restored;
        response = next_response;
    }
}

#[allow(clippy::too_many_arguments)]
fn find_lxmf_summary<P: Read + Write + ?Sized>(
    port: &mut P,
    reader: &mut BufferedRecordReader,
    mut session: ClientSession,
    request_ids: &mut RequestIds,
    mut response: DeviceResponse,
    deadline: Instant,
    target: LxmfMessageHandle,
) -> Result<(ClientSession, LxmfMessageSummary), String> {
    let mut after = None;
    loop {
        let Some(summary) = classify_lxmf_next_response("lxmf-read", response, after)? else {
            return Err(format!(
                "LXMF handle {} was not found; no output file was created",
                target.get()
            ));
        };
        if summary.handle() == target {
            return Ok((session, summary));
        }
        if summary.handle().get() > target.get() {
            return Err(format!(
                "LXMF handle {} was not found; no output file was created",
                target.get()
            ));
        }
        after = Some(summary.handle());
        let request_id = request_ids.take()?;
        let (restored, next_response) = exchange(
            port,
            reader,
            session,
            request_id,
            DeviceRequest::LxmfNext { after },
            "lxmf-read summary lookup",
            deadline,
        )?;
        session = restored;
        response = next_response;
    }
}

fn validate_lxmf_read_chunk(
    summary: LxmfMessageSummary,
    expected_offset: u32,
    requested: LxmfReadLength,
    chunk: &LxmfReadChunk,
) -> Result<u32, String> {
    if chunk.handle() != summary.handle() {
        return Err(format!(
            "device returned LXMF handle {} while reading {}",
            chunk.handle().get(),
            summary.handle().get()
        ));
    }
    if chunk.offset() != expected_offset {
        return Err(format!(
            "device returned LXMF offset {} while {} was required",
            chunk.offset(),
            expected_offset
        ));
    }
    if chunk.total_len() != summary.normalized_wire_len() {
        return Err(format!(
            "device changed LXMF handle {} length from {} to {} during read",
            summary.handle().get(),
            summary.normalized_wire_len(),
            chunk.total_len()
        ));
    }
    if chunk.bytes().is_empty() {
        return Err(format!(
            "device made no progress reading LXMF handle {} at offset {expected_offset}",
            summary.handle().get()
        ));
    }
    if chunk.bytes().len() > usize::from(requested.get()) {
        return Err(format!(
            "device returned {} LXMF bytes after at most {} were requested",
            chunk.bytes().len(),
            requested.get()
        ));
    }
    let length = u32::try_from(chunk.bytes().len())
        .map_err(|_| "LXMF chunk length did not fit u32".to_owned())?;
    let next = expected_offset
        .checked_add(length)
        .ok_or_else(|| "LXMF chunk offset overflowed u32".to_owned())?;
    if next > summary.normalized_wire_len() {
        return Err(format!(
            "device returned LXMF bytes beyond the declared length of {}",
            summary.normalized_wire_len()
        ));
    }
    if chunk.is_final() != (next == summary.normalized_wire_len()) {
        return Err(format!(
            "device returned contradictory final-chunk state for LXMF handle {}",
            summary.handle().get()
        ));
    }
    Ok(next)
}

fn parse_and_validate_lxmf(
    summary: LxmfMessageSummary,
    wire: &[u8],
) -> Result<ParsedLxmfMetadata, String> {
    let scan_steps = wire.len().saturating_mul(16).max(65_536);
    let limits = WireLimits::new(
        wire.len(),
        wire.len(),
        wire.len(),
        wire.len(),
        scan_steps,
        LXMF_HOST_MAX_NESTING_DEPTH,
    );
    let message = MessageView::parse_complete(wire, limits).map_err(|error| {
        format!(
            "downloaded LXMF handle {} failed bounded structural parsing: {error}",
            summary.handle().get()
        )
    })?;
    if message.normalized_wire_len() != summary.normalized_wire_len() as usize {
        return Err("parsed LXMF wire length disagrees with its authenticated summary".to_owned());
    }
    if message.message_id() != *summary.message_id() {
        return Err("parsed LXMF message ID disagrees with its authenticated summary".to_owned());
    }
    if message.destination_hash() != &summary.destination().0 {
        return Err("parsed LXMF destination disagrees with its authenticated summary".to_owned());
    }
    if message.source_hash() != &summary.source().0 {
        return Err("parsed LXMF source disagrees with its authenticated summary".to_owned());
    }
    let payload = message.payload();
    if payload.timestamp_bits() != summary.timestamp_bits() {
        return Err("parsed LXMF timestamp disagrees with its authenticated summary".to_owned());
    }
    let title = payload.title().as_bytes();
    let content = payload.content().as_bytes();
    if title.len() != summary.title_len() as usize {
        return Err("parsed LXMF title length disagrees with its authenticated summary".to_owned());
    }
    if content.len() != summary.content_len() as usize {
        return Err(
            "parsed LXMF content length disagrees with its authenticated summary".to_owned(),
        );
    }
    if payload.fields().raw().len() != summary.fields_encoded_len() as usize {
        return Err(
            "parsed LXMF fields length disagrees with its authenticated summary".to_owned(),
        );
    }
    Ok(ParsedLxmfMetadata {
        title_sha256: Sha256::digest(title).into(),
        content_sha256: Sha256::digest(content).into(),
        title_utf8: std::str::from_utf8(title).is_ok(),
        content_utf8: std::str::from_utf8(content).is_ok(),
    })
}

fn format_lxmf_read_result(
    device_id: &[u8; 16],
    session_id: &[u8; 16],
    summary: LxmfMessageSummary,
    output_path: &Path,
    parsed: Option<ParsedLxmfMetadata>,
) -> String {
    let prefix = format!(
        "command=lxmf-read outcome=ok device_id={} session_id={} handle={} message_id={} destination={} source={} timestamp_bits={:016x} normalized_wire_len={} title_len={} content_len={} fields_encoded_len={} wire_sha256={} output={}",
        hex(device_id),
        hex(session_id),
        summary.handle().get(),
        hex(summary.message_id()),
        hex(&summary.destination().0),
        hex(&summary.source().0),
        summary.timestamp_bits(),
        summary.normalized_wire_len(),
        summary.title_len(),
        summary.content_len(),
        summary.fields_encoded_len(),
        hex(summary.exact_wire_sha256()),
        output_path.display(),
    );
    match parsed {
        Some(parsed) => format!(
            "{prefix} parsed=true title_sha256={} title_utf8={} content_sha256={} content_utf8={}",
            hex(&parsed.title_sha256),
            parsed.title_utf8,
            hex(&parsed.content_sha256),
            parsed.content_utf8,
        ),
        None => format!(
            "{prefix} parsed=false parse_reason=host-size-limit parse_limit_bytes={MAX_LXMF_HOST_PARSE_BYTES}"
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn continue_lxmf_read<P: Read + Write + ?Sized>(
    port: &mut P,
    reader: &mut BufferedRecordReader,
    session: ClientSession,
    request_ids: &mut RequestIds,
    response: DeviceResponse,
    deadline: Instant,
    device_id: &[u8; 16],
    session_id: &[u8; 16],
    target: LxmfMessageHandle,
    output_path: &Path,
    mut output: ReservedOutput,
) -> Result<String, String> {
    let (mut session, summary) = find_lxmf_summary(
        port,
        reader,
        session,
        request_ids,
        response,
        deadline,
        target,
    )?;
    let total_len = summary.normalized_wire_len();
    let mut offset = 0_u32;
    let mut received_hasher = Sha256::new();
    while offset < total_len {
        let remaining = total_len - offset;
        let requested = LxmfReadLength::new(remaining.min(MAX_LXMF_READ_CHUNK_BYTES as u32) as u16)
            .expect("positive remaining LXMF length is bounded to one API chunk");
        let request_id = request_ids.take()?;
        let (restored, response) = exchange(
            port,
            reader,
            session,
            request_id,
            DeviceRequest::LxmfRead {
                handle: target,
                offset,
                max_bytes: requested,
            },
            "lxmf-read",
            deadline,
        )?;
        session = restored;
        let chunk = match response {
            DeviceResponse::LxmfRead(chunk) => chunk,
            DeviceResponse::Error(error) => return Err(format_api_error("lxmf-read", error)),
            other => {
                return Err(format!(
                    "device returned response kind {} instead of lxmf-read",
                    other.kind()
                ));
            }
        };
        let next = validate_lxmf_read_chunk(summary, offset, requested, &chunk)?;
        output.write_uncommitted(chunk.bytes())?;
        received_hasher.update(chunk.bytes());
        offset = next;
    }
    let received_sha256: [u8; 32] = received_hasher.finalize().into();
    if received_sha256 != *summary.exact_wire_sha256() {
        return Err(format!(
            "downloaded LXMF handle {} SHA-256 did not match its authenticated summary; no output file was committed",
            target.get()
        ));
    }
    let total_len_usize = usize::try_from(total_len)
        .map_err(|_| "LXMF normalized wire length does not fit this host".to_owned())?;
    output.verify_sha256_uncommitted(total_len_usize, summary.exact_wire_sha256())?;
    let parsed = if total_len_usize <= MAX_LXMF_HOST_PARSE_BYTES {
        let wire = output.read_back_uncommitted(total_len_usize)?;
        Some(parse_and_validate_lxmf(summary, &wire)?)
    } else {
        None
    };
    output.finish()?;
    Ok(format_lxmf_read_result(
        device_id,
        session_id,
        summary,
        output_path,
        parsed,
    ))
}

fn format_one_shot_response(
    command: &Command,
    device_id: &[u8; 16],
    session_id: &[u8; 16],
    response: DeviceResponse,
    outputs: CommandOutputs,
) -> Result<String, String> {
    match (command, response) {
        (Command::SystemCapabilities, DeviceResponse::SystemCapabilities(capabilities)) => {
            Ok(format!(
                "command=system-capabilities outcome=ok device_id={} session_id={} api={}.{} packet_output={} direct_radio_tx={} experimental_submit_rns_data={} experimental_rns_inbox={} experimental_lxmf={} experimental_lxmf_basic_send={} max_message_bytes={} max_body_bytes={} max_submit_rns_data_payload_bytes={} max_rns_inbox_payload_bytes={} max_lxmf_read_chunk_bytes={} max_lxmf_basic_title_bytes={} max_lxmf_basic_content_bytes={}",
                hex(device_id),
                hex(session_id),
                capabilities.api_version().major,
                capabilities.api_version().minor,
                capabilities.packet_output(),
                capabilities.direct_radio_tx().wire_code(),
                capabilities.experimental_submit_rns_data(),
                capabilities.experimental_rns_inbox().wire_code(),
                capabilities.experimental_lxmf().wire_code(),
                capabilities.experimental_lxmf_basic_send().wire_code(),
                capabilities.max_message_bytes(),
                capabilities.max_body_bytes(),
                capabilities.max_submit_rns_data_payload_bytes(),
                capabilities.max_rns_inbox_payload_bytes(),
                capabilities.max_lxmf_read_chunk_bytes(),
                capabilities.max_lxmf_basic_title_bytes(),
                capabilities.max_lxmf_basic_content_bytes(),
            ))
        }
        (Command::IdentitySummary, DeviceResponse::IdentitySummary(summary)) => Ok(format!(
            "command=identity-summary outcome=ok device_id={} session_id={} primary_destination={} lxmf_delivery_destination={}",
            hex(device_id),
            hex(session_id),
            hex(&summary.primary_destination().0),
            summary
                .lxmf_delivery_destination()
                .map_or_else(|| "none".to_owned(), |destination| hex(&destination.0)),
        )),
        (Command::RnsInboxStatus, DeviceResponse::RnsInboxStatus(status)) => Ok(format!(
            "command=rns-inbox-status outcome=ok device_id={} session_id={} depth={} capacity={} dropped={} max={} durable={}",
            hex(device_id),
            hex(session_id),
            status.depth,
            status.capacity,
            status.dropped_since_boot,
            status.max_payload_bytes,
            status.durable,
        )),
        (Command::RnsInboxPeek { output }, DeviceResponse::RnsInboxPeek(item)) => {
            let payload_sha256: [u8; 32] = Sha256::digest(item.payload()).into();
            let (payload_output, evidence_output) = outputs.into_peek()?;
            payload_output.commit(item.payload())?;
            if let Some(evidence_output) = evidence_output {
                write_authenticated_evidence(
                    evidence_output,
                    &AuthenticatedEvidenceV1::RnsInboxPeek {
                        schema: EvidenceSchema::V1,
                        device_id: hex(device_id),
                        session_id: hex(session_id),
                        item_id: item.id(),
                        destination: hex(&item.destination().0),
                        length: item.payload_len(),
                        payload_sha256: hex(&payload_sha256),
                    },
                    &format!(
                        "authenticated rns-inbox-peek item {} was received and its payload output was committed",
                        item.id()
                    ),
                )?;
            }
            Ok(format!(
                "command=rns-inbox-peek outcome=ok device_id={} session_id={} item_id={} destination={} length={} sha256={} output={}",
                hex(device_id),
                hex(session_id),
                item.id(),
                hex(&item.destination().0),
                item.payload_len(),
                hex(&payload_sha256),
                output.display(),
            ))
        }
        (Command::RnsInboxPeek { .. }, DeviceResponse::Error(error))
            if error.code == reticulum_device_api::ApiErrorCode::NotFound =>
        {
            Err("RNS inbox is empty (NotFound); no output file was created".to_owned())
        }
        (Command::SubmissionStatus { id }, DeviceResponse::SubmissionStatus(status)) => {
            if status.id != *id {
                return Err(format!(
                    "device returned submission ID {} instead of {}",
                    status.id.0, id.0
                ));
            }
            Ok(format_submission_status(
                "submission-status",
                device_id,
                session_id,
                status,
            ))
        }
        (Command::SubmitRnsData { .. }, DeviceResponse::SubmitRnsDataAccepted(accepted)) => {
            Ok(format!(
                "command=submit-rns-data outcome=accepted device_id={} session_id={} submission_id={}",
                hex(device_id),
                hex(session_id),
                accepted.id.0,
            ))
        }
        (_, DeviceResponse::Error(error)) => Err(format_api_error(command_name(command), error)),
        (_, other) => Err(format!(
            "device returned response kind {} instead of {}",
            other.kind(),
            command_name(command),
        )),
    }
}

fn command_request(command: &Command) -> DeviceRequest<'_> {
    match command {
        Command::SystemCapabilities => DeviceRequest::SystemCapabilities,
        Command::IdentitySummary => DeviceRequest::IdentitySummary,
        Command::RnsInboxStatus => DeviceRequest::RnsInboxStatus,
        Command::RnsInboxPeek { .. } => DeviceRequest::RnsInboxPeek,
        Command::LxmfList | Command::LxmfRead { .. } => DeviceRequest::LxmfNext { after: None },
        Command::LxmfSend {
            destination,
            timestamp_unix_ms,
            title,
            content,
            idempotency_key,
        }
        | Command::LxmfSendAndWait {
            destination,
            timestamp_unix_ms,
            title,
            content,
            idempotency_key,
        } => DeviceRequest::LxmfBasicSend {
            destination: *destination,
            timestamp_unix_ms: *timestamp_unix_ms,
            title,
            content,
            location: None,
            idempotency_key: *idempotency_key,
        },
        Command::SubmissionStatus { id } => DeviceRequest::SubmissionStatus { id: *id },
        Command::SubmitRnsData {
            destination,
            payload,
            idempotency_key,
        }
        | Command::SubmitAndWait {
            destination,
            payload,
            idempotency_key,
        } => DeviceRequest::SubmitRnsData {
            destination: *destination,
            payload,
            idempotency_key: *idempotency_key,
        },
    }
}

fn write_authenticated_evidence(
    output: ReservedOutput,
    evidence: &AuthenticatedEvidenceV1,
    authenticated_context: &str,
) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(evidence).map_err(|error| {
        format!("{authenticated_context}; could not serialize evidence output: {error}")
    })?;
    bytes.push(b'\n');
    output
        .commit(&bytes)
        .map_err(|error| format!("{authenticated_context}; {error}"))
}

#[allow(clippy::too_many_arguments)]
fn wait_for_delivery<P: Read + Write + ?Sized>(
    port: &mut P,
    reader: &mut BufferedRecordReader,
    mut session: ClientSession,
    request_ids: &mut RequestIds,
    deadline: Instant,
    device_id: &[u8; 16],
    session_id: &[u8; 16],
    submission_id: SubmissionId,
    mut evidence_output: Option<ReservedOutput>,
    terminal_command: &str,
) -> Result<String, String> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "timed out waiting for submission {} to reach delivered",
                submission_id.0
            ));
        }
        thread::sleep(POLL_INTERVAL.min(remaining));
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for submission {} to reach delivered",
                submission_id.0
            ));
        }

        let request_id = request_ids.take()?;
        let (restored, response) = exchange(
            port,
            reader,
            session,
            request_id,
            DeviceRequest::SubmissionStatus { id: submission_id },
            "submission-status",
            deadline,
        )?;
        session = restored;
        let decision = classify_wait_response(submission_id, response)?;
        match decision {
            WaitDecision::PollAgain | WaitDecision::RetryInternal => {}
            terminal => {
                return finish_wait_decision(
                    terminal,
                    device_id,
                    session_id,
                    evidence_output.take(),
                    terminal_command,
                );
            }
        }
    }
}

fn finish_wait_decision(
    decision: WaitDecision,
    device_id: &[u8; 16],
    session_id: &[u8; 16],
    evidence_output: Option<ReservedOutput>,
    terminal_command: &str,
) -> Result<String, String> {
    match decision {
        WaitDecision::PollAgain | WaitDecision::RetryInternal => {
            Err("internal nonterminal wait decision reached terminal handler".to_owned())
        }
        WaitDecision::Delivered {
            submission_id,
            details,
        } => {
            if let Some(evidence_output) = evidence_output {
                let terminal_context = format!(
                    "authenticated submission {} reached state=delivered",
                    submission_id.0
                );
                write_authenticated_evidence(
                    evidence_output,
                    &AuthenticatedEvidenceV1::SubmitAndWait {
                        schema: EvidenceSchema::V1,
                        device_id: hex(device_id),
                        session_id: hex(session_id),
                        terminal: SubmitTerminalEvidence::Delivered {
                            submission_id: submission_id.0,
                            packet_len: details.packet_len,
                            encoded_packet_sha256: hex(details.encoded_packet_sha256.as_bytes()),
                        },
                    },
                    &terminal_context,
                )?;
            }
            Ok(format_submission_status(
                terminal_command,
                device_id,
                session_id,
                reticulum_device_api::SubmissionStatus {
                    id: submission_id,
                    state: SubmissionState::Delivered(details),
                },
            ))
        }
        WaitDecision::Failed {
            submission_id,
            failure,
        } => {
            let terminal_error = format!(
                "submission {} reached state=failed failure={}",
                submission_id.0,
                failure_name(failure)
            );
            if let Some(evidence_output) = evidence_output {
                let terminal_context = format!("authenticated {terminal_error}");
                write_authenticated_evidence(
                    evidence_output,
                    &AuthenticatedEvidenceV1::SubmitAndWait {
                        schema: EvidenceSchema::V1,
                        device_id: hex(device_id),
                        session_id: hex(session_id),
                        terminal: SubmitTerminalEvidence::Failed {
                            submission_id: submission_id.0,
                            reason: failure.into(),
                        },
                    },
                    &terminal_context,
                )?;
            }
            Err(terminal_error)
        }
    }
}

fn classify_wait_response(
    expected_id: SubmissionId,
    response: DeviceResponse,
) -> Result<WaitDecision, String> {
    match response {
        DeviceResponse::SubmissionStatus(status) => {
            if status.id != expected_id {
                return Err(format!(
                    "device returned submission ID {} instead of {}",
                    status.id.0, expected_id.0
                ));
            }
            match status.state {
                SubmissionState::Queued
                | SubmissionState::Preparing
                | SubmissionState::AwaitingDelivery(_) => Ok(WaitDecision::PollAgain),
                SubmissionState::Delivered(details) => Ok(WaitDecision::Delivered {
                    submission_id: status.id,
                    details,
                }),
                SubmissionState::Failed(failure) => Ok(WaitDecision::Failed {
                    submission_id: status.id,
                    failure,
                }),
                SubmissionState::Cancelled => Err(format!(
                    "submission {} reached state=cancelled",
                    status.id.0
                )),
            }
        }
        DeviceResponse::Error(error)
            if error.code == reticulum_device_api::ApiErrorCode::Internal =>
        {
            Ok(WaitDecision::RetryInternal)
        }
        DeviceResponse::Error(error) => Err(format_api_error("submission-status", error)),
        other => Err(format!(
            "device returned response kind {} instead of submission-status",
            other.kind()
        )),
    }
}

fn format_api_error(operation: &str, error: reticulum_device_api::ApiErrorResponse) -> String {
    format!(
        "device rejected {operation} request: code={} operation={}",
        error.code.wire_code(),
        error
            .operation
            .map_or_else(|| "none".to_owned(), |operation| operation.to_string()),
    )
}

fn write_flight<W: Write + ?Sized, F: OutboundFlight>(
    writer: &mut W,
    mut flight: F,
    deadline: Instant,
    stage: &str,
) -> Result<F, String> {
    while !flight.remaining().is_empty() {
        if Instant::now() >= deadline {
            return Err(format!("timed out writing {stage}"));
        }
        match writer.write(flight.remaining()) {
            Ok(0) => return Err(format!("device made no progress writing {stage}")),
            Ok(written) => flight
                .advance(written)
                .map_err(|_| format!("backend over-acknowledged {stage}"))?,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(format!("could not write {stage}: {error}")),
        }
    }
    loop {
        if Instant::now() >= deadline {
            return Err(format!("timed out flushing {stage}"));
        }
        match writer.flush() {
            Ok(()) => return Ok(flight),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(format!("could not flush {stage}: {error}")),
        }
    }
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut port = None;
    let mut state_file = None;
    let mut timeout_ms = None;
    let mut command = None;
    let mut destination = None;
    let mut payload = None;
    let mut lxmf_title = None;
    let mut lxmf_content = None;
    let mut lxmf_timestamp_unix_ms = None;
    let mut idempotency_key = None;
    let mut submission_id = None;
    let mut lxmf_handle = None;
    let mut output = None;
    let mut evidence_output = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--port" if port.is_none() => {
                index += 1;
                port = Some(required_value(args.get(index), "--port")?.to_owned());
            }
            "--state-file" if state_file.is_none() => {
                index += 1;
                state_file = Some(PathBuf::from(required_value(
                    args.get(index),
                    "--state-file",
                )?));
            }
            "--timeout-ms" if timeout_ms.is_none() => {
                index += 1;
                let parsed = required_value(args.get(index), "--timeout-ms")?
                    .parse::<u64>()
                    .map_err(|_| "--timeout-ms requires an unsigned 64-bit integer".to_owned())?;
                if parsed == 0 {
                    return Err("--timeout-ms must be nonzero".to_owned());
                }
                timeout_ms = Some(parsed);
            }
            "--destination-hash" if destination.is_none() => {
                index += 1;
                destination = Some(DestinationHash(parse_fixed_hex(
                    required_value(args.get(index), "--destination-hash")?,
                    "--destination-hash",
                )?));
            }
            "--payload-hex" if payload.is_none() => {
                index += 1;
                payload = Some(parse_payload_hex(required_value(
                    args.get(index),
                    "--payload-hex",
                )?)?);
            }
            "--title-hex" if lxmf_title.is_none() => {
                index += 1;
                lxmf_title = Some(parse_bounded_hex(
                    required_value(args.get(index), "--title-hex")?,
                    "--title-hex",
                    MAX_LXMF_BASIC_TITLE_BYTES,
                )?);
            }
            "--content-hex" if lxmf_content.is_none() => {
                index += 1;
                lxmf_content = Some(parse_bounded_hex(
                    required_value(args.get(index), "--content-hex")?,
                    "--content-hex",
                    MAX_LXMF_BASIC_CONTENT_BYTES,
                )?);
            }
            "--timestamp-ms" if lxmf_timestamp_unix_ms.is_none() => {
                index += 1;
                lxmf_timestamp_unix_ms = Some(parse_u64(args.get(index), "--timestamp-ms")?);
            }
            "--idempotency-key" if idempotency_key.is_none() => {
                index += 1;
                idempotency_key = Some(IdempotencyKey(parse_fixed_hex(
                    required_value(args.get(index), "--idempotency-key")?,
                    "--idempotency-key",
                )?));
            }
            "--submission-id" if submission_id.is_none() => {
                index += 1;
                submission_id = Some(SubmissionId(parse_u64(args.get(index), "--submission-id")?));
            }
            "--handle" if lxmf_handle.is_none() => {
                index += 1;
                let value = parse_u64(args.get(index), "--handle")?;
                lxmf_handle = Some(LxmfMessageHandle::new(value).map_err(|_| {
                    "--handle must be a nonzero unsigned 64-bit integer".to_owned()
                })?);
            }
            "--output" if output.is_none() => {
                index += 1;
                output = Some(PathBuf::from(required_value(args.get(index), "--output")?));
            }
            "--evidence-output" if evidence_output.is_none() => {
                index += 1;
                evidence_output = Some(PathBuf::from(required_value(
                    args.get(index),
                    "--evidence-output",
                )?));
            }
            "system-capabilities" if command.is_none() => {
                command = Some(CommandKind::SystemCapabilities);
            }
            "identity-summary" if command.is_none() => {
                command = Some(CommandKind::IdentitySummary);
            }
            "rns-inbox-status" if command.is_none() => {
                command = Some(CommandKind::RnsInboxStatus);
            }
            "rns-inbox-peek" if command.is_none() => {
                command = Some(CommandKind::RnsInboxPeek);
            }
            "lxmf-list" if command.is_none() => {
                command = Some(CommandKind::LxmfList);
            }
            "lxmf-read" if command.is_none() => {
                command = Some(CommandKind::LxmfRead);
            }
            "lxmf-send" if command.is_none() => {
                command = Some(CommandKind::LxmfSend);
            }
            "lxmf-send-and-wait" if command.is_none() => {
                command = Some(CommandKind::LxmfSendAndWait);
            }
            "submission-status" if command.is_none() => {
                command = Some(CommandKind::SubmissionStatus);
            }
            "submit-rns-data" if command.is_none() => command = Some(CommandKind::SubmitRnsData),
            "submit-and-wait" if command.is_none() => command = Some(CommandKind::SubmitAndWait),
            option @ ("--port" | "--state-file" | "--timeout-ms" | "--destination-hash"
            | "--payload-hex" | "--title-hex" | "--content-hex" | "--timestamp-ms"
            | "--idempotency-key" | "--submission-id" | "--output"
            | "--evidence-output" | "--handle") => {
                return Err(format!("duplicate option {option}"));
            }
            command_name @ ("system-capabilities"
            | "identity-summary"
            | "rns-inbox-status"
            | "rns-inbox-peek"
            | "lxmf-list"
            | "lxmf-read"
            | "lxmf-send"
            | "lxmf-send-and-wait"
            | "submission-status"
            | "submit-rns-data"
            | "submit-and-wait") => {
                return Err(format!("unexpected or duplicate command {command_name}"));
            }
            unknown if unknown.starts_with('-') => {
                return Err(format!("unexpected option at argument {}", index + 1));
            }
            _ => {
                return Err(format!(
                    "unexpected positional argument at argument {}",
                    index + 1
                ));
            }
        }
        index += 1;
    }
    let command = match command.unwrap_or(CommandKind::SystemCapabilities) {
        kind @ (CommandKind::SystemCapabilities
        | CommandKind::IdentitySummary
        | CommandKind::RnsInboxStatus
        | CommandKind::LxmfList) => {
            if destination.is_some()
                || payload.is_some()
                || lxmf_title.is_some()
                || lxmf_content.is_some()
                || lxmf_timestamp_unix_ms.is_some()
                || idempotency_key.is_some()
                || submission_id.is_some()
                || lxmf_handle.is_some()
                || output.is_some()
                || evidence_output.is_some()
            {
                return Err(format!(
                    "{} does not accept operation-specific arguments",
                    command_kind_name(kind)
                ));
            }
            match kind {
                CommandKind::SystemCapabilities => Command::SystemCapabilities,
                CommandKind::IdentitySummary => Command::IdentitySummary,
                CommandKind::RnsInboxStatus => Command::RnsInboxStatus,
                CommandKind::LxmfList => Command::LxmfList,
                _ => unreachable!("combined match admits only argument-free commands"),
            }
        }
        CommandKind::RnsInboxPeek => {
            if destination.is_some()
                || payload.is_some()
                || lxmf_title.is_some()
                || lxmf_content.is_some()
                || lxmf_timestamp_unix_ms.is_some()
                || idempotency_key.is_some()
                || submission_id.is_some()
                || lxmf_handle.is_some()
            {
                return Err(
                    "rns-inbox-peek accepts only the operation-specific --output and optional --evidence-output arguments"
                        .to_owned(),
                );
            }
            Command::RnsInboxPeek {
                output: output.ok_or_else(|| "rns-inbox-peek requires --output".to_owned())?,
            }
        }
        CommandKind::SubmissionStatus => {
            if destination.is_some()
                || payload.is_some()
                || lxmf_title.is_some()
                || lxmf_content.is_some()
                || lxmf_timestamp_unix_ms.is_some()
                || idempotency_key.is_some()
            {
                return Err(
                    "submission-status does not accept submit-rns-data arguments".to_owned(),
                );
            }
            if lxmf_handle.is_some() {
                return Err("submission-status does not accept --handle".to_owned());
            }
            if output.is_some() {
                return Err("submission-status does not accept --output".to_owned());
            }
            if evidence_output.is_some() {
                return Err("submission-status does not accept --evidence-output".to_owned());
            }
            Command::SubmissionStatus {
                id: submission_id
                    .ok_or_else(|| "submission-status requires --submission-id".to_owned())?,
            }
        }
        CommandKind::LxmfRead => {
            if destination.is_some()
                || payload.is_some()
                || lxmf_title.is_some()
                || lxmf_content.is_some()
                || lxmf_timestamp_unix_ms.is_some()
                || idempotency_key.is_some()
                || submission_id.is_some()
                || evidence_output.is_some()
            {
                return Err(
                    "lxmf-read accepts only the operation-specific --handle and --output arguments"
                        .to_owned(),
                );
            }
            Command::LxmfRead {
                handle: lxmf_handle.ok_or_else(|| "lxmf-read requires --handle".to_owned())?,
                output: output.ok_or_else(|| "lxmf-read requires --output".to_owned())?,
            }
        }
        kind @ (CommandKind::LxmfSend | CommandKind::LxmfSendAndWait) => {
            if payload.is_some()
                || submission_id.is_some()
                || lxmf_handle.is_some()
                || output.is_some()
                || evidence_output.is_some()
            {
                return Err(format!(
                    "{} accepts only --destination-hash, --title-hex, --content-hex, optional --timestamp-ms, and optional --idempotency-key",
                    command_kind_name(kind)
                ));
            }
            let operation = command_kind_name(kind);
            let destination =
                destination.ok_or_else(|| format!("{operation} requires --destination-hash"))?;
            let title = lxmf_title.ok_or_else(|| format!("{operation} requires --title-hex"))?;
            let content =
                lxmf_content.ok_or_else(|| format!("{operation} requires --content-hex"))?;
            let timestamp_unix_ms = match lxmf_timestamp_unix_ms {
                Some(timestamp) => timestamp,
                None => current_unix_timestamp_ms()?,
            };
            let idempotency_key = match idempotency_key {
                Some(key) => key,
                None => generate_idempotency_key()?,
            };
            match kind {
                CommandKind::LxmfSend => Command::LxmfSend {
                    destination,
                    timestamp_unix_ms,
                    title,
                    content,
                    idempotency_key,
                },
                CommandKind::LxmfSendAndWait => Command::LxmfSendAndWait {
                    destination,
                    timestamp_unix_ms,
                    title,
                    content,
                    idempotency_key,
                },
                _ => unreachable!("combined match admits only LXMF send commands"),
            }
        }
        kind @ (CommandKind::SubmitRnsData | CommandKind::SubmitAndWait) => {
            if lxmf_title.is_some() || lxmf_content.is_some() || lxmf_timestamp_unix_ms.is_some() {
                return Err(format!(
                    "{} does not accept LXMF title, content, or timestamp arguments",
                    command_kind_name(kind)
                ));
            }
            if submission_id.is_some() {
                return Err(format!(
                    "{} does not accept --submission-id",
                    command_kind_name(kind)
                ));
            }
            if lxmf_handle.is_some() {
                return Err(format!(
                    "{} does not accept --handle",
                    command_kind_name(kind)
                ));
            }
            if output.is_some() {
                return Err(format!(
                    "{} does not accept --output",
                    command_kind_name(kind)
                ));
            }
            if matches!(kind, CommandKind::SubmitRnsData) && evidence_output.is_some() {
                return Err("submit-rns-data does not accept --evidence-output".to_owned());
            }
            let operation = command_kind_name(kind);
            let destination =
                destination.ok_or_else(|| format!("{operation} requires --destination-hash"))?;
            let payload = payload.ok_or_else(|| format!("{operation} requires --payload-hex"))?;
            let idempotency_key =
                idempotency_key.ok_or_else(|| format!("{operation} requires --idempotency-key"))?;
            match kind {
                CommandKind::SubmitRnsData => Command::SubmitRnsData {
                    destination,
                    payload,
                    idempotency_key,
                },
                CommandKind::SubmitAndWait => Command::SubmitAndWait {
                    destination,
                    payload,
                    idempotency_key,
                },
                _ => unreachable!("combined match admits only submission commands"),
            }
        }
    };
    validate_lxmf_send_encoding(&command)?;
    let default_timeout_ms = if matches!(
        &command,
        Command::SubmitAndWait { .. } | Command::LxmfSendAndWait { .. }
    ) {
        DEFAULT_SUBMIT_AND_WAIT_TIMEOUT_MS
    } else {
        DEFAULT_TIMEOUT_MS
    };
    Ok(Options {
        port: port.ok_or_else(|| "--port is required".to_owned())?,
        state_file: state_file.ok_or_else(|| "--state-file is required".to_owned())?,
        timeout: Duration::from_millis(timeout_ms.unwrap_or(default_timeout_ms)),
        command,
        evidence_output,
    })
}

fn validate_lxmf_send_encoding(command: &Command) -> Result<(), String> {
    if lxmf_send_fields(command).is_none() {
        return Ok(());
    }
    let envelope = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(u64::MAX),
        request: command_request(command),
    };
    let mut encoded = [0_u8; MAX_MESSAGE_BYTES];
    encode_request(&envelope, &mut encoded).map_err(|error| {
        format!(
            "{} title/content combination cannot fit one bounded device-API request: {error:?}",
            command_name(command)
        )
    })?;
    Ok(())
}

fn required_value<'a>(value: Option<&'a String>, flag: &str) -> Result<&'a str, String> {
    value
        .map(String::as_str)
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn parse_u64(value: Option<&String>, flag: &str) -> Result<u64, String> {
    required_value(value, flag)?
        .parse()
        .map_err(|_| format!("{flag} requires an unsigned 64-bit integer"))
}

fn current_unix_timestamp_ms() -> Result<u64, String> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "host clock is before the Unix epoch; supply --timestamp-ms".to_owned())?;
    u64::try_from(elapsed.as_millis()).map_err(|_| {
        "host Unix timestamp does not fit u64 milliseconds; supply --timestamp-ms".to_owned()
    })
}

fn generate_idempotency_key() -> Result<IdempotencyKey, String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| {
        "operating-system randomness failed while generating --idempotency-key".to_owned()
    })?;
    Ok(IdempotencyKey(bytes))
}

fn parse_fixed_hex<const N: usize>(value: &str, flag: &str) -> Result<[u8; N], String> {
    if value.len() != N * 2 {
        return Err(format!(
            "{flag} requires exactly {} hexadecimal digits",
            N * 2
        ));
    }
    let mut bytes = [0_u8; N];
    decode_hex_into(value, &mut bytes, flag)?;
    Ok(bytes)
}

fn parse_payload_hex(value: &str) -> Result<Vec<u8>, String> {
    parse_bounded_hex(value, "--payload-hex", MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES)
}

fn parse_bounded_hex(value: &str, flag: &str, maximum: usize) -> Result<Vec<u8>, String> {
    if !value.len().is_multiple_of(2) {
        return Err(format!(
            "{flag} requires an even number of hexadecimal digits"
        ));
    }
    let length = value.len() / 2;
    if length > maximum {
        return Err(format!(
            "{flag} decodes to {length} bytes; maximum is {maximum}"
        ));
    }
    let mut bytes = vec![0_u8; length];
    decode_hex_into(value, &mut bytes, flag)?;
    Ok(bytes)
}

fn decode_hex_into(value: &str, output: &mut [u8], flag: &str) -> Result<(), String> {
    for (index, byte) in output.iter_mut().enumerate() {
        let offset = index * 2;
        let high = hex_nibble(value.as_bytes()[offset]);
        let low = hex_nibble(value.as_bytes()[offset + 1]);
        let (Some(high), Some(low)) = (high, low) else {
            return Err(format!("{flag} requires hexadecimal digits"));
        };
        *byte = (high << 4) | low;
    }
    Ok(())
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

const fn command_name(command: &Command) -> &'static str {
    match command {
        Command::SystemCapabilities => "system-capabilities",
        Command::IdentitySummary => "identity-summary",
        Command::RnsInboxStatus => "rns-inbox-status",
        Command::RnsInboxPeek { .. } => "rns-inbox-peek",
        Command::LxmfList => "lxmf-list",
        Command::LxmfRead { .. } => "lxmf-read",
        Command::LxmfSend { .. } => "lxmf-send",
        Command::LxmfSendAndWait { .. } => "lxmf-send-and-wait",
        Command::SubmissionStatus { .. } => "submission-status",
        Command::SubmitRnsData { .. } => "submit-rns-data",
        Command::SubmitAndWait { .. } => "submit-and-wait",
    }
}

const fn command_kind_name(command: CommandKind) -> &'static str {
    match command {
        CommandKind::SystemCapabilities => "system-capabilities",
        CommandKind::IdentitySummary => "identity-summary",
        CommandKind::RnsInboxStatus => "rns-inbox-status",
        CommandKind::RnsInboxPeek => "rns-inbox-peek",
        CommandKind::LxmfList => "lxmf-list",
        CommandKind::LxmfRead => "lxmf-read",
        CommandKind::LxmfSend => "lxmf-send",
        CommandKind::LxmfSendAndWait => "lxmf-send-and-wait",
        CommandKind::SubmissionStatus => "submission-status",
        CommandKind::SubmitRnsData => "submit-rns-data",
        CommandKind::SubmitAndWait => "submit-and-wait",
    }
}

fn format_submission_status(
    command: &str,
    device_id: &[u8; 16],
    session_id: &[u8; 16],
    status: reticulum_device_api::SubmissionStatus,
) -> String {
    let prefix = format!(
        "command={command} outcome=ok device_id={} session_id={} submission_id={}",
        hex(device_id),
        hex(session_id),
        status.id.0,
    );
    match status.state {
        SubmissionState::Queued => format!("{prefix} state=queued"),
        SubmissionState::Preparing => format!("{prefix} state=preparing"),
        SubmissionState::AwaitingDelivery(details) => format!(
            "{prefix} state=awaiting-delivery packet_len={} encoded_packet_sha256={}",
            details.packet_len,
            hex(details.encoded_packet_sha256.as_bytes()),
        ),
        SubmissionState::Delivered(details) => format!(
            "{prefix} state=delivered packet_len={} encoded_packet_sha256={}",
            details.packet_len,
            hex(details.encoded_packet_sha256.as_bytes()),
        ),
        SubmissionState::Failed(failure) => {
            format!("{prefix} state=failed failure={}", failure_name(failure))
        }
        SubmissionState::Cancelled => format!("{prefix} state=cancelled"),
    }
}

const fn failure_name(failure: SubmissionFailure) -> &'static str {
    match failure {
        SubmissionFailure::NoPath => "no-path",
        SubmissionFailure::DeliveryTimeout => "delivery-timeout",
        SubmissionFailure::Rejected => "rejected",
        SubmissionFailure::Internal => "internal",
    }
}

fn usage() {
    eprintln!(
        "usage:\n  cargo run -p xtask -- e290-authenticated-usb --port <serial-path> --state-file <active-secret-path> [--timeout-ms <u64>] [system-capabilities]\n  cargo run -p xtask -- e290-authenticated-usb --port <serial-path> --state-file <active-secret-path> [--timeout-ms <u64>] identity-summary\n  cargo run -p xtask -- e290-authenticated-usb --port <serial-path> --state-file <active-secret-path> [--timeout-ms <u64>] rns-inbox-status\n  cargo run -p xtask -- e290-authenticated-usb --port <serial-path> --state-file <active-secret-path> [--timeout-ms <u64>] rns-inbox-peek --output <path> [--evidence-output <absent-json>]\n  cargo run -p xtask -- e290-authenticated-usb --port <serial-path> --state-file <active-secret-path> [--timeout-ms <u64>] lxmf-list\n  cargo run -p xtask -- e290-authenticated-usb --port <serial-path> --state-file <active-secret-path> [--timeout-ms <u64>] lxmf-read --handle <nonzero-u64> --output <absent-path>\n  cargo run -p xtask -- e290-authenticated-usb --port <serial-path> --state-file <active-secret-path> [--timeout-ms <u64>] lxmf-send --destination-hash <32-hex> --title-hex <0-to-590-hex> --content-hex <0-to-590-hex> [--timestamp-ms <u64>] [--idempotency-key <32-hex>]\n  cargo run -p xtask -- e290-authenticated-usb --port <serial-path> --state-file <active-secret-path> [--timeout-ms <u64, default 45000>] lxmf-send-and-wait --destination-hash <32-hex> --title-hex <0-to-590-hex> --content-hex <0-to-590-hex> [--timestamp-ms <u64>] [--idempotency-key <32-hex>]\n  cargo run -p xtask -- e290-authenticated-usb --port <serial-path> --state-file <active-secret-path> [--timeout-ms <u64>] --submission-id <u64> submission-status\n  cargo run -p xtask -- e290-authenticated-usb --port <serial-path> --state-file <active-secret-path> [--timeout-ms <u64>] --destination-hash <32-hex> --payload-hex <0-to-766-hex> --idempotency-key <32-hex> submit-rns-data\n  cargo run -p xtask -- e290-authenticated-usb --port <serial-path> --state-file <active-secret-path> [--timeout-ms <u64, default 45000>] --destination-hash <32-hex> --payload-hex <0-to-766-hex> --idempotency-key <32-hex> submit-and-wait [--evidence-output <absent-json>]"
    );
}

fn hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

#[cfg(test)]
mod tests {
    use std::{
        cell::{Cell, RefCell},
        fs,
        io::Cursor,
        rc::Rc,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use reticulum_device_api_framing::{
        AUTH_TAG_LENGTH, FramedRecord, PAYLOAD_CAPACITY, PayloadLength,
    };

    use super::*;

    static NEXT_TEMP_OUTPUT: AtomicU64 = AtomicU64::new(0);

    struct TempOutput {
        path: PathBuf,
    }

    impl TempOutput {
        fn new(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("host clock must be after the Unix epoch")
                .as_nanos();
            let sequence = NEXT_TEMP_OUTPUT.fetch_add(1, Ordering::Relaxed);
            Self {
                path: std::env::temp_dir().join(format!(
                    "reticulum-e290-authenticated-usb-{label}-{}-{nonce}-{sequence}",
                    std::process::id(),
                )),
            }
        }

        #[cfg(unix)]
        fn bare(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("host clock must be after the Unix epoch")
                .as_nanos();
            let sequence = NEXT_TEMP_OUTPUT.fetch_add(1, Ordering::Relaxed);
            Self {
                path: PathBuf::from(format!(
                    "reticulum-e290-authenticated-usb-{label}-{}-{nonce}-{sequence}",
                    std::process::id(),
                )),
            }
        }
    }

    impl Drop for TempOutput {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn record(kind: u8, sequence: u64) -> Record {
        Record::new(
            kind,
            [sequence as u8; 16],
            sequence,
            PayloadLength::new(1).unwrap(),
            {
                let mut payload = [0_u8; PAYLOAD_CAPACITY];
                payload[0] = kind;
                payload
            },
            [0_u8; AUTH_TAG_LENGTH],
        )
    }

    fn submit_and_wait_command() -> Command {
        submit_and_wait_command_with_payload(b"private submission payload")
    }

    fn submit_and_wait_command_with_payload(payload: &[u8]) -> Command {
        Command::SubmitAndWait {
            destination: DestinationHash([
                0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
                0x0e, 0x0f,
            ]),
            payload: payload.to_vec(),
            idempotency_key: IdempotencyKey([
                0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc, 0xfd,
                0xfe, 0xff,
            ]),
        }
    }

    fn sample_lxmf_wire(title: &[u8], content: &[u8]) -> Vec<u8> {
        assert!(title.len() <= u8::MAX as usize);
        assert!(content.len() <= u8::MAX as usize);
        let mut wire = Vec::new();
        wire.extend_from_slice(&[0x10; 16]);
        wire.extend_from_slice(&[0x20; 16]);
        wire.extend_from_slice(&[0x30; 64]);
        wire.push(0x94);
        wire.push(0xcb);
        wire.extend_from_slice(&1.25_f64.to_bits().to_be_bytes());
        wire.extend_from_slice(&[0xc4, title.len() as u8]);
        wire.extend_from_slice(title);
        wire.extend_from_slice(&[0xc4, content.len() as u8]);
        wire.extend_from_slice(content);
        wire.push(0x80);
        wire
    }

    fn sample_lxmf_summary(handle: u64, wire: &[u8]) -> LxmfMessageSummary {
        let message = MessageView::parse_complete(
            wire,
            WireLimits::new(wire.len(), wire.len(), wire.len(), wire.len(), 65_536, 16),
        )
        .unwrap();
        let payload = message.payload();
        LxmfMessageSummary::new(
            LxmfMessageHandle::new(handle).unwrap(),
            message.message_id(),
            DestinationHash(*message.destination_hash()),
            DestinationHash(*message.source_hash()),
            payload.timestamp_bits(),
            u32::try_from(wire.len()).unwrap(),
            u32::try_from(payload.title().as_bytes().len()).unwrap(),
            u32::try_from(payload.content().as_bytes().len()).unwrap(),
            u32::try_from(payload.fields().raw().len()).unwrap(),
            Sha256::digest(wire).into(),
        )
        .unwrap()
    }

    struct TracedOutput {
        bytes: Vec<u8>,
        events: Rc<RefCell<Vec<&'static str>>>,
    }

    impl Write for TracedOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.events.borrow_mut().push("accepted-write");
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.events.borrow_mut().push("accepted-flush");
            Ok(())
        }
    }

    #[derive(Clone, Copy)]
    enum AcceptedOutputFault {
        Write,
        Flush,
    }

    struct FaultingAcceptedOutput {
        fault: AcceptedOutputFault,
        bytes: Vec<u8>,
    }

    impl Write for FaultingAcceptedOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if matches!(self.fault, AcceptedOutputFault::Write) {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "marker write fault",
                ));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            if matches!(self.fault, AcceptedOutputFault::Flush) {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "marker flush fault",
                ));
            }
            Ok(())
        }
    }

    #[test]
    fn parser_requires_one_port_and_active_state_file() {
        let parsed = parse(&strings(&[
            "--state-file",
            "/tmp/e290.key",
            "--timeout-ms",
            "7000",
            "--port",
            "/dev/test",
        ]))
        .unwrap();
        assert_eq!(parsed.port, "/dev/test");
        assert_eq!(parsed.state_file, PathBuf::from("/tmp/e290.key"));
        assert_eq!(parsed.timeout, Duration::from_millis(7000));
        assert_eq!(parsed.command, Command::SystemCapabilities);
        assert_eq!(parsed.evidence_output, None);

        assert!(parse(&strings(&["--port", "/dev/test"])).is_err());
        assert!(parse(&strings(&["--state-file", "/tmp/e290.key"])).is_err());
        assert!(
            parse(&strings(&[
                "--port",
                "/dev/one",
                "--port",
                "/dev/two",
                "--state-file",
                "/tmp/e290.key",
            ]))
            .is_err()
        );
        assert!(
            parse(&strings(&[
                "--port",
                "/dev/test",
                "--state-file",
                "/tmp/e290.key",
                "--timeout-ms",
                "0",
            ]))
            .is_err()
        );
    }

    #[test]
    fn parser_error_output_redacts_unrecognized_submission_material() {
        const PAYLOAD: &str = "48656c6c6f2070726976617465206d657373616765";
        const IDEMPOTENCY_KEY: &str = "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff";

        let payload_option = format!("--payload-hex={PAYLOAD}");
        let payload_args = vec![
            "--port".to_owned(),
            "/dev/test".to_owned(),
            "--state-file".to_owned(),
            "/tmp/e290.key".to_owned(),
            payload_option,
        ];
        let payload_reason = parse(&payload_args)
            .err()
            .expect("joined payload option must be rejected");
        let payload_output = cli_error_line(&payload_reason);
        assert_eq!(payload_output, "error: unexpected option at argument 5");
        assert!(!payload_output.contains(PAYLOAD));

        let idempotency_args = vec![
            "--port".to_owned(),
            "/dev/test".to_owned(),
            "--state-file".to_owned(),
            "/tmp/e290.key".to_owned(),
            "submit-rns-data".to_owned(),
            IDEMPOTENCY_KEY.to_owned(),
        ];
        let idempotency_reason = parse(&idempotency_args)
            .err()
            .expect("bare idempotency material must be rejected");
        let idempotency_output = cli_error_line(&idempotency_reason);
        assert_eq!(
            idempotency_output,
            "error: unexpected positional argument at argument 6"
        );
        assert!(!idempotency_output.contains(IDEMPOTENCY_KEY));
    }

    #[test]
    fn parser_names_only_whitelisted_duplicate_options_and_commands() {
        let duplicate_option = parse(&strings(&[
            "--port",
            "/dev/one",
            "--port",
            "/dev/two",
            "--state-file",
            "/tmp/e290.key",
        ]))
        .err()
        .expect("duplicate option must be rejected");
        assert_eq!(duplicate_option, "duplicate option --port");

        let duplicate_command = parse(&strings(&[
            "--port",
            "/dev/test",
            "--state-file",
            "/tmp/e290.key",
            "system-capabilities",
            "submit-and-wait",
        ]))
        .err()
        .expect("multiple commands must be rejected");
        assert_eq!(
            duplicate_command,
            "unexpected or duplicate command submit-and-wait"
        );

        let duplicate_output = parse(&strings(&[
            "--port",
            "/dev/test",
            "--state-file",
            "/tmp/e290.key",
            "rns-inbox-peek",
            "--output",
            "/tmp/one",
            "--output",
            "/tmp/two",
        ]))
        .err()
        .expect("duplicate output option must be rejected");
        assert_eq!(duplicate_output, "duplicate option --output");
    }

    #[test]
    fn parser_accepts_only_the_inbox_command_arguments() {
        let status = parse(&strings(&[
            "--port",
            "/dev/test",
            "rns-inbox-status",
            "--state-file",
            "/tmp/e290.key",
        ]))
        .unwrap();
        assert_eq!(status.command, Command::RnsInboxStatus);
        assert_eq!(
            command_request(&status.command),
            DeviceRequest::RnsInboxStatus
        );

        let peek = parse(&strings(&[
            "--output",
            "/tmp/inbox.bin",
            "--state-file",
            "/tmp/e290.key",
            "rns-inbox-peek",
            "--port",
            "/dev/test",
        ]))
        .unwrap();
        assert_eq!(
            peek.command,
            Command::RnsInboxPeek {
                output: PathBuf::from("/tmp/inbox.bin"),
            }
        );
        assert_eq!(command_request(&peek.command), DeviceRequest::RnsInboxPeek);

        assert_eq!(
            parse(&strings(&[
                "--port",
                "/dev/test",
                "--state-file",
                "/tmp/e290.key",
                "rns-inbox-peek",
            ]))
            .err()
            .unwrap(),
            "rns-inbox-peek requires --output"
        );
        assert_eq!(
            parse(&strings(&[
                "--port",
                "/dev/test",
                "--state-file",
                "/tmp/e290.key",
                "rns-inbox-status",
                "--output",
                "/tmp/inbox.bin",
            ]))
            .err()
            .unwrap(),
            "rns-inbox-status does not accept operation-specific arguments"
        );
        assert_eq!(
            parse(&strings(&[
                "--port",
                "/dev/test",
                "--state-file",
                "/tmp/e290.key",
                "rns-inbox-peek",
                "--output",
                "/tmp/inbox.bin",
                "--submission-id",
                "7",
            ]))
            .err()
            .unwrap(),
            "rns-inbox-peek accepts only the operation-specific --output and optional --evidence-output arguments"
        );
    }

    #[test]
    fn parser_accepts_only_bounded_lxmf_list_and_read_arguments() {
        let list = parse(&strings(&[
            "--port",
            "/dev/test",
            "lxmf-list",
            "--state-file",
            "/tmp/e290.key",
        ]))
        .unwrap();
        assert_eq!(list.command, Command::LxmfList);
        assert_eq!(
            command_request(&list.command),
            DeviceRequest::LxmfNext { after: None }
        );

        let read = parse(&strings(&[
            "--output",
            "/tmp/message.lxmf",
            "--handle",
            "18446744073709551615",
            "--state-file",
            "/tmp/e290.key",
            "lxmf-read",
            "--port",
            "/dev/test",
        ]))
        .unwrap();
        let handle = LxmfMessageHandle::new(u64::MAX).unwrap();
        assert_eq!(
            read.command,
            Command::LxmfRead {
                handle,
                output: PathBuf::from("/tmp/message.lxmf"),
            }
        );
        assert_eq!(
            command_request(&read.command),
            DeviceRequest::LxmfNext { after: None }
        );

        for (arguments, expected) in [
            (
                strings(&[
                    "--port",
                    "/dev/test",
                    "--state-file",
                    "/tmp/e290.key",
                    "lxmf-read",
                    "--output",
                    "/tmp/message.lxmf",
                ]),
                "lxmf-read requires --handle",
            ),
            (
                strings(&[
                    "--port",
                    "/dev/test",
                    "--state-file",
                    "/tmp/e290.key",
                    "lxmf-read",
                    "--handle",
                    "1",
                ]),
                "lxmf-read requires --output",
            ),
            (
                strings(&[
                    "--port",
                    "/dev/test",
                    "--state-file",
                    "/tmp/e290.key",
                    "lxmf-list",
                    "--output",
                    "/tmp/message.lxmf",
                ]),
                "lxmf-list does not accept operation-specific arguments",
            ),
        ] {
            assert_eq!(parse(&arguments).err().unwrap(), expected);
        }

        assert_eq!(
            parse(&strings(&[
                "--port",
                "/dev/test",
                "--state-file",
                "/tmp/e290.key",
                "lxmf-read",
                "--handle",
                "0",
                "--output",
                "/tmp/message.lxmf",
            ]))
            .err()
            .unwrap(),
            "--handle must be a nonzero unsigned 64-bit integer"
        );
        assert_eq!(
            parse(&strings(&[
                "--port",
                "/dev/test",
                "--state-file",
                "/tmp/e290.key",
                "lxmf-read",
                "--handle",
                "1",
                "--handle",
                "2",
                "--output",
                "/tmp/message.lxmf",
            ]))
            .err()
            .unwrap(),
            "duplicate option --handle"
        );
        assert_eq!(
            parse(&strings(&[
                "--port",
                "/dev/test",
                "--state-file",
                "/tmp/e290.key",
                "lxmf-read",
                "--handle",
                "1",
                "--output",
                "/tmp/message.lxmf",
                "--evidence-output",
                "/tmp/private.json",
            ]))
            .err()
            .unwrap(),
            "lxmf-read accepts only the operation-specific --handle and --output arguments"
        );
    }

    #[test]
    fn parser_builds_replayable_lxmf_send_requests_with_explicit_material() {
        const DESTINATION: &str = "00112233445566778899aabbccddeeff";
        const TITLE_HEX: &str = "70726976617465207469746c65";
        const CONTENT_HEX: &str = "7072697661746520636f6e74656e74";
        const KEY: &str = "101112131415161718191a1b1c1d1e1f";

        for (name, waits) in [("lxmf-send", false), ("lxmf-send-and-wait", true)] {
            let options = parse(&strings(&[
                "--port",
                "/dev/test",
                "--state-file",
                "/tmp/e290.key",
                name,
                "--destination-hash",
                DESTINATION,
                "--title-hex",
                TITLE_HEX,
                "--content-hex",
                CONTENT_HEX,
                "--timestamp-ms",
                "1720123456789",
                "--idempotency-key",
                KEY,
            ]))
            .unwrap();
            let fields = lxmf_send_fields(&options.command).unwrap();
            assert_eq!(
                fields.destination,
                DestinationHash(parse_fixed_hex(DESTINATION, "d").unwrap())
            );
            assert_eq!(fields.timestamp_unix_ms, 1_720_123_456_789);
            assert_eq!(fields.title, b"private title");
            assert_eq!(fields.content, b"private content");
            assert_eq!(
                fields.idempotency_key,
                IdempotencyKey(parse_fixed_hex(KEY, "k").unwrap())
            );
            assert_eq!(
                options.timeout,
                if waits {
                    Duration::from_millis(DEFAULT_SUBMIT_AND_WAIT_TIMEOUT_MS)
                } else {
                    Duration::from_millis(DEFAULT_TIMEOUT_MS)
                }
            );

            let first = command_request(&options.command);
            let second = command_request(&options.command);
            assert_eq!(first, second, "retry material must remain stable");
            assert!(matches!(first, DeviceRequest::LxmfBasicSend { .. }));
            let mut encoded = [0_u8; MAX_MESSAGE_BYTES];
            encode_request(
                &RequestEnvelope {
                    version: ApiVersion::CURRENT,
                    request_id: RequestId(u64::MAX),
                    request: first,
                },
                &mut encoded,
            )
            .unwrap();
        }
    }

    #[test]
    fn lxmf_send_generates_missing_retry_material_once_and_prints_it_before_submission() {
        let before = current_unix_timestamp_ms().unwrap();
        let options = parse(&strings(&[
            "--port",
            "/dev/test",
            "--state-file",
            "/tmp/e290.key",
            "lxmf-send",
            "--destination-hash",
            "00112233445566778899aabbccddeeff",
            "--title-hex",
            "70726976617465207469746c65",
            "--content-hex",
            "7072697661746520636f6e74656e74",
        ]))
        .unwrap();
        let after = current_unix_timestamp_ms().unwrap();
        let fields = lxmf_send_fields(&options.command).unwrap();
        assert!((before..=after).contains(&fields.timestamp_unix_ms));
        assert_eq!(
            command_request(&options.command),
            command_request(&options.command)
        );

        let events = Rc::new(RefCell::new(Vec::new()));
        let mut output = TracedOutput {
            bytes: Vec::new(),
            events: Rc::clone(&events),
        };
        write_lxmf_send_intent(&mut output, "lxmf-send", &[0x11; 16], &[0x22; 16], fields).unwrap();
        assert_eq!(*events.borrow(), ["accepted-write", "accepted-flush"]);
        let intent = String::from_utf8(output.bytes).unwrap();
        assert!(intent.contains("command=lxmf-send outcome=intent"));
        assert!(intent.contains(&format!("timestamp_unix_ms={}", fields.timestamp_unix_ms)));
        assert!(intent.contains(&format!(
            "idempotency_key={}",
            hex(&fields.idempotency_key.0)
        )));
        assert!(intent.contains("title_len=13 content_len=15"));
        assert!(!intent.contains("private title"));
        assert!(!intent.contains("private content"));
        assert!(!intent.contains("70726976617465207469746c65"));
        assert!(!intent.contains("7072697661746520636f6e74656e74"));
    }

    #[test]
    fn lxmf_send_parser_enforces_individual_and_combined_bounds_without_echoing_content() {
        assert_eq!(
            parse(&strings(&[
                "--port",
                "/dev/test",
                "--state-file",
                "/tmp/e290.key",
                "lxmf-send",
                "--destination-hash",
                "00112233445566778899aabbccddeeff",
                "--content-hex",
                "00",
            ]))
            .err()
            .unwrap(),
            "lxmf-send requires --title-hex"
        );
        let oversized_title = "74".repeat(MAX_LXMF_BASIC_TITLE_BYTES + 1);
        let args = vec![
            "--port".to_owned(),
            "/dev/test".to_owned(),
            "--state-file".to_owned(),
            "/tmp/e290.key".to_owned(),
            "lxmf-send".to_owned(),
            "--destination-hash".to_owned(),
            "00112233445566778899aabbccddeeff".to_owned(),
            "--title-hex".to_owned(),
            oversized_title.clone(),
            "--content-hex".to_owned(),
            String::new(),
        ];
        let error = parse(&args).err().unwrap();
        assert!(error.contains("maximum is 295"));
        assert!(!error.contains(&oversized_title));

        let title = "74".repeat(220);
        let content = "63".repeat(220);
        let combined = vec![
            "--port".to_owned(),
            "/dev/test".to_owned(),
            "--state-file".to_owned(),
            "/tmp/e290.key".to_owned(),
            "lxmf-send".to_owned(),
            "--destination-hash".to_owned(),
            "00112233445566778899aabbccddeeff".to_owned(),
            "--title-hex".to_owned(),
            title.clone(),
            "--content-hex".to_owned(),
            content.clone(),
        ];
        let error = parse(&combined).err().unwrap();
        assert!(error.contains("cannot fit one bounded device-API request"));
        assert!(!error.contains(&title));
        assert!(!error.contains(&content));
    }

    #[test]
    fn lxmf_send_acceptance_reports_ids_and_retry_metadata_but_not_message_bodies() {
        let command = Command::LxmfSend {
            destination: DestinationHash([0x10; 16]),
            timestamp_unix_ms: 1_720_123_456_789,
            title: b"private title".to_vec(),
            content: b"private content".to_vec(),
            idempotency_key: IdempotencyKey([0x20; 16]),
        };
        let fields = lxmf_send_fields(&command).unwrap();
        let accepted =
            reticulum_device_api::LxmfBasicSendAccepted::new(SubmissionId(42), [0x33; 32]);
        assert_eq!(
            classify_lxmf_send_response(
                "lxmf-send",
                DeviceResponse::LxmfBasicSendAccepted(accepted),
            )
            .unwrap(),
            accepted
        );
        let output =
            format_lxmf_send_accepted("lxmf-send", &[0x11; 16], &[0x22; 16], accepted, fields);
        assert!(output.contains("submission_id=42"));
        assert!(output.contains(&format!("message_id={}", "33".repeat(32))));
        assert!(output.contains("timestamp_unix_ms=1720123456789"));
        assert!(output.contains(&format!("idempotency_key={}", "20".repeat(16))));
        assert!(output.contains("title_len=13 content_len=15"));
        assert!(!output.contains("private title"));
        assert!(!output.contains("private content"));
        assert!(!output.contains("70726976617465207469746c65"));
        assert!(!output.contains("7072697661746520636f6e74656e74"));

        let terminal = finish_wait_decision(
            WaitDecision::Delivered {
                submission_id: accepted.id,
                details: reticulum_device_api::PreparedPacketDetails {
                    packet_len: 123,
                    encoded_packet_sha256: reticulum_device_api::EncodedPacketSha256::new(
                        [0x44; 32],
                    ),
                },
            },
            &[0x11; 16],
            &[0x22; 16],
            None,
            "lxmf-send-and-wait",
        )
        .unwrap();
        assert!(terminal.starts_with("command=lxmf-send-and-wait outcome=ok"));
        assert!(terminal.contains("submission_id=42 state=delivered"));
    }

    #[test]
    fn parser_redacts_an_unrecognized_joined_output_path() {
        const PRIVATE_PATH: &str = "/tmp/private-inbox-location";
        let reason = parse(&[
            "--port".to_owned(),
            "/dev/test".to_owned(),
            "--state-file".to_owned(),
            "/tmp/e290.key".to_owned(),
            format!("--output={PRIVATE_PATH}"),
            "rns-inbox-peek".to_owned(),
        ])
        .err()
        .expect("joined output option must be rejected");
        let output = cli_error_line(&reason);
        assert_eq!(output, "error: unexpected option at argument 5");
        assert!(!output.contains(PRIVATE_PATH));
    }

    #[test]
    fn parser_accepts_explicit_capabilities_and_complete_submission() {
        let capabilities = parse(&strings(&[
            "--port",
            "/dev/test",
            "system-capabilities",
            "--state-file",
            "/tmp/e290.key",
        ]))
        .unwrap();
        assert_eq!(capabilities.command, Command::SystemCapabilities);

        let submitted = parse(&strings(&[
            "--payload-hex",
            "48656c6c6f",
            "submit-rns-data",
            "--idempotency-key",
            "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff",
            "--state-file",
            "/tmp/e290.key",
            "--destination-hash",
            "000102030405060708090a0b0c0d0e0f",
            "--port",
            "/dev/test",
        ]))
        .unwrap();
        assert_eq!(
            submitted.command,
            Command::SubmitRnsData {
                destination: DestinationHash([
                    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
                    0x0d, 0x0e, 0x0f,
                ]),
                payload: b"Hello".to_vec(),
                idempotency_key: IdempotencyKey([
                    0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc,
                    0xfd, 0xfe, 0xff,
                ]),
            }
        );
    }

    #[test]
    fn identity_summary_is_argument_free_and_outputs_only_public_hashes() {
        let parsed = parse(&strings(&[
            "--port",
            "/dev/test",
            "identity-summary",
            "--state-file",
            "/tmp/e290.key",
        ]))
        .unwrap();
        assert_eq!(parsed.command, Command::IdentitySummary);
        assert_eq!(parsed.timeout, Duration::from_millis(DEFAULT_TIMEOUT_MS));
        assert_eq!(
            command_request(&parsed.command),
            DeviceRequest::IdentitySummary
        );

        assert_eq!(
            parse(&strings(&[
                "--port",
                "/dev/test",
                "identity-summary",
                "--submission-id",
                "42",
                "--state-file",
                "/tmp/e290.key",
            ]))
            .err()
            .unwrap(),
            "identity-summary does not accept operation-specific arguments"
        );

        let output = format_one_shot_response(
            &Command::IdentitySummary,
            &[0xab; 16],
            &[0xcd; 16],
            DeviceResponse::IdentitySummary(reticulum_device_api::IdentitySummary::new(
                DestinationHash([
                    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
                    0x0d, 0x0e, 0x0f,
                ]),
            )),
            CommandOutputs::None,
        )
        .unwrap();
        assert_eq!(
            output,
            format!(
                "command=identity-summary outcome=ok device_id={} session_id={} primary_destination=000102030405060708090a0b0c0d0e0f lxmf_delivery_destination=none",
                "ab".repeat(16),
                "cd".repeat(16),
            )
        );

        let with_lxmf = format_one_shot_response(
            &Command::IdentitySummary,
            &[0xab; 16],
            &[0xcd; 16],
            DeviceResponse::IdentitySummary(
                reticulum_device_api::IdentitySummary::with_lxmf_delivery_destination(
                    DestinationHash([0x10; 16]),
                    DestinationHash([0x20; 16]),
                ),
            ),
            CommandOutputs::None,
        )
        .unwrap();
        assert!(with_lxmf.contains(&format!(
            "primary_destination={} lxmf_delivery_destination={}",
            "10".repeat(16),
            "20".repeat(16)
        )));
    }

    #[test]
    fn parser_gives_submit_and_wait_a_bounded_default_and_accepts_override() {
        let common = [
            "--destination-hash",
            "000102030405060708090a0b0c0d0e0f",
            "--payload-hex",
            "48656c6c6f",
            "--idempotency-key",
            "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff",
            "--state-file",
            "/tmp/e290.key",
            "--port",
            "/dev/test",
            "submit-and-wait",
        ];
        let waiting = parse(&strings(&common)).unwrap();
        assert_eq!(
            waiting.timeout,
            Duration::from_millis(DEFAULT_SUBMIT_AND_WAIT_TIMEOUT_MS)
        );
        assert_eq!(
            waiting.command,
            submit_and_wait_command_with_payload(b"Hello")
        );

        let mut overridden = strings(&common);
        overridden.insert(0, "12000".to_owned());
        overridden.insert(0, "--timeout-ms".to_owned());
        assert_eq!(
            parse(&overridden).unwrap().timeout,
            Duration::from_millis(12_000)
        );
    }

    #[test]
    fn parser_accepts_evidence_only_for_submit_and_wait_and_inbox_peek() {
        let submit = parse(&strings(&[
            "--destination-hash",
            "000102030405060708090a0b0c0d0e0f",
            "--payload-hex",
            "48656c6c6f",
            "--idempotency-key",
            "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff",
            "--state-file",
            "/tmp/e290.key",
            "--evidence-output",
            "/tmp/submit-evidence.json",
            "--port",
            "/dev/test",
            "submit-and-wait",
        ]))
        .unwrap();
        assert_eq!(
            submit.evidence_output,
            Some(PathBuf::from("/tmp/submit-evidence.json"))
        );

        let peek = parse(&strings(&[
            "--output",
            "/tmp/inbox.bin",
            "rns-inbox-peek",
            "--evidence-output",
            "/tmp/peek-evidence.json",
            "--state-file",
            "/tmp/e290.key",
            "--port",
            "/dev/test",
        ]))
        .unwrap();
        assert_eq!(
            peek.evidence_output,
            Some(PathBuf::from("/tmp/peek-evidence.json"))
        );

        for command in [
            "system-capabilities",
            "submission-status",
            "submit-rns-data",
            "lxmf-send",
            "lxmf-send-and-wait",
        ] {
            let mut arguments = strings(&[
                "--port",
                "/dev/test",
                "--state-file",
                "/tmp/e290.key",
                "--evidence-output",
                "/tmp/evidence.json",
            ]);
            match command {
                "submission-status" => {
                    arguments.extend(strings(&["--submission-id", "7"]));
                }
                "submit-rns-data" => {
                    arguments.extend(strings(&[
                        "--destination-hash",
                        "000102030405060708090a0b0c0d0e0f",
                        "--payload-hex",
                        "00",
                        "--idempotency-key",
                        "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff",
                    ]));
                }
                "lxmf-send" | "lxmf-send-and-wait" => {
                    arguments.extend(strings(&[
                        "--destination-hash",
                        "000102030405060708090a0b0c0d0e0f",
                        "--title-hex",
                        "00",
                        "--content-hex",
                        "01",
                    ]));
                }
                _ => {}
            }
            arguments.push(command.to_owned());
            assert!(parse(&arguments).is_err(), "{command} accepted evidence");
        }

        assert_eq!(
            parse(&strings(&[
                "--port",
                "/dev/test",
                "--state-file",
                "/tmp/e290.key",
                "rns-inbox-peek",
                "--output",
                "/tmp/inbox.bin",
                "--evidence-output",
            ]))
            .err()
            .unwrap(),
            "--evidence-output requires a value"
        );
        assert_eq!(
            parse(&strings(&[
                "--port",
                "/dev/test",
                "--state-file",
                "/tmp/e290.key",
                "rns-inbox-peek",
                "--output",
                "/tmp/inbox.bin",
                "--evidence-output",
                "/tmp/one.json",
                "--evidence-output",
                "/tmp/two.json",
            ]))
            .err()
            .unwrap(),
            "duplicate option --evidence-output"
        );
    }

    #[test]
    fn parser_accepts_status_and_rejects_missing_or_mixed_status_arguments() {
        let status = parse(&strings(&[
            "submission-status",
            "--state-file",
            "/tmp/e290.key",
            "--submission-id",
            "18446744073709551615",
            "--port",
            "/dev/test",
        ]))
        .unwrap();
        assert_eq!(
            status.command,
            Command::SubmissionStatus {
                id: SubmissionId(u64::MAX)
            }
        );

        assert_eq!(
            parse(&strings(&[
                "submission-status",
                "--state-file",
                "/tmp/e290.key",
                "--port",
                "/dev/test",
            ]))
            .err()
            .unwrap(),
            "submission-status requires --submission-id"
        );
        assert_eq!(
            parse(&strings(&[
                "submission-status",
                "--state-file",
                "/tmp/e290.key",
                "--submission-id",
                "7",
                "--payload-hex",
                "00",
                "--port",
                "/dev/test",
            ]))
            .err()
            .unwrap(),
            "submission-status does not accept submit-rns-data arguments"
        );
        assert!(
            parse(&strings(&[
                "submission-status",
                "--state-file",
                "/tmp/e290.key",
                "--submission-id",
                "not-a-number",
                "--port",
                "/dev/test",
            ]))
            .is_err()
        );
    }

    #[test]
    fn parser_rejects_incomplete_or_malformed_submission_material() {
        let base = [
            "--port",
            "/dev/test",
            "--state-file",
            "/tmp/e290.key",
            "submit-rns-data",
        ];
        assert_eq!(
            parse(&strings(&base)).err().unwrap(),
            "submit-rns-data requires --destination-hash"
        );
        assert!(
            parse(&strings(&[
                "--port",
                "/dev/test",
                "--state-file",
                "/tmp/e290.key",
                "--destination-hash",
                "00",
                "--payload-hex",
                "00",
                "--idempotency-key",
                "00000000000000000000000000000000",
                "submit-rns-data",
            ]))
            .is_err()
        );
        assert!(
            parse(&strings(&[
                "--port",
                "/dev/test",
                "--state-file",
                "/tmp/e290.key",
                "--destination-hash",
                "0000000000000000000000000000000g",
                "--payload-hex",
                "0",
                "--idempotency-key",
                "00000000000000000000000000000000",
                "submit-rns-data",
            ]))
            .is_err()
        );
        let oversized = "00".repeat(MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES + 1);
        let args = vec![
            "--port".to_owned(),
            "/dev/test".to_owned(),
            "--state-file".to_owned(),
            "/tmp/e290.key".to_owned(),
            "--destination-hash".to_owned(),
            "00000000000000000000000000000000".to_owned(),
            "--payload-hex".to_owned(),
            oversized,
            "--idempotency-key".to_owned(),
            "00000000000000000000000000000000".to_owned(),
            "submit-rns-data".to_owned(),
        ];
        assert!(parse(&args).is_err());
    }

    #[test]
    fn parser_rejects_submission_arguments_without_submission_command() {
        assert_eq!(
            parse(&strings(&[
                "--port",
                "/dev/test",
                "--state-file",
                "/tmp/e290.key",
                "--payload-hex",
                "00",
            ]))
            .err()
            .unwrap(),
            "system-capabilities does not accept operation-specific arguments"
        );
    }

    #[test]
    fn capabilities_output_includes_inbox_and_lxmf_availability_and_limits() {
        let output = format_one_shot_response(
            &Command::SystemCapabilities,
            &[0x11; 16],
            &[0x22; 16],
            DeviceResponse::SystemCapabilities(reticulum_device_api::CapabilitySnapshot::current()),
            CommandOutputs::None,
        )
        .unwrap();
        assert!(output.contains(&format!(
            "api={}.{}",
            ApiVersion::CURRENT.major,
            ApiVersion::CURRENT.minor
        )));
        assert!(output.contains("experimental_rns_inbox=2"));
        assert!(output.contains("max_rns_inbox_payload_bytes=383"));
        assert!(output.contains("experimental_lxmf=2"));
        assert!(output.contains("max_lxmf_read_chunk_bytes=416"));
        assert!(output.contains("experimental_lxmf_basic_send=2"));
        assert!(output.contains("max_lxmf_basic_title_bytes=295"));
        assert!(output.contains("max_lxmf_basic_content_bytes=295"));
    }

    #[test]
    fn lxmf_summary_iteration_is_monotonic_and_not_found_is_clean_completion() {
        let wire = sample_lxmf_wire(b"private title", b"private content");
        let first = sample_lxmf_summary(1, &wire);
        let second = sample_lxmf_summary(2, &wire);
        assert_eq!(
            classify_lxmf_next_response("lxmf-list", DeviceResponse::LxmfNext(first), None,)
                .unwrap(),
            Some(first)
        );
        assert_eq!(
            classify_lxmf_next_response(
                "lxmf-list",
                DeviceResponse::LxmfNext(second),
                Some(first.handle()),
            )
            .unwrap(),
            Some(second)
        );
        assert!(
            classify_lxmf_next_response(
                "lxmf-list",
                DeviceResponse::LxmfNext(first),
                Some(first.handle()),
            )
            .unwrap_err()
            .contains("non-advancing")
        );
        assert_eq!(
            classify_lxmf_next_response(
                "lxmf-list",
                DeviceResponse::Error(reticulum_device_api::ApiErrorResponse {
                    code: reticulum_device_api::ApiErrorCode::NotFound,
                    operation: Some(reticulum_device_api::OP_EXPERIMENTAL_LXMF_NEXT),
                }),
                Some(second.handle()),
            )
            .unwrap(),
            None
        );

        let line = format_lxmf_summary_line(&[0x11; 16], &[0x22; 16], 1, first);
        assert!(line.contains("command=lxmf-list outcome=item"));
        assert!(line.contains("ordinal=1 handle=1"));
        assert!(line.contains(&format!("message_id={}", hex(first.message_id()))));
        assert!(!line.contains("private title"));
        assert!(!line.contains("private content"));
        assert!(!line.contains(&hex(b"private content")));
    }

    #[test]
    fn lxmf_chunk_validation_requires_exact_handle_offset_length_and_progress() {
        let wire = sample_lxmf_wire(b"title", b"content");
        let summary = sample_lxmf_summary(7, &wire);
        let requested = LxmfReadLength::new(64).unwrap();
        let first = LxmfReadChunk::new(
            summary.handle(),
            0,
            summary.normalized_wire_len(),
            &wire[..64],
        )
        .unwrap();
        assert_eq!(
            validate_lxmf_read_chunk(summary, 0, requested, &first).unwrap(),
            64
        );

        let wrong_handle = LxmfReadChunk::new(
            LxmfMessageHandle::new(8).unwrap(),
            0,
            summary.normalized_wire_len(),
            &wire[..1],
        )
        .unwrap();
        assert!(validate_lxmf_read_chunk(summary, 0, requested, &wrong_handle).is_err());

        let wrong_offset = LxmfReadChunk::new(
            summary.handle(),
            1,
            summary.normalized_wire_len(),
            &wire[1..2],
        )
        .unwrap();
        assert!(validate_lxmf_read_chunk(summary, 0, requested, &wrong_offset).is_err());

        let changed_length = LxmfReadChunk::new(
            summary.handle(),
            0,
            summary.normalized_wire_len() + 1,
            &wire[..1],
        )
        .unwrap();
        assert!(validate_lxmf_read_chunk(summary, 0, requested, &changed_length).is_err());
        assert!(
            validate_lxmf_read_chunk(summary, 0, LxmfReadLength::new(1).unwrap(), &first,).is_err()
        );
    }

    #[test]
    fn lxmf_wire_parser_cross_checks_summary_and_redacts_title_and_content() {
        let title = b"private title";
        let content = b"private content";
        let wire = sample_lxmf_wire(title, content);
        let summary = sample_lxmf_summary(7, &wire);
        let parsed = parse_and_validate_lxmf(summary, &wire).unwrap();
        let expected_title_sha256: [u8; 32] = Sha256::digest(title).into();
        let expected_content_sha256: [u8; 32] = Sha256::digest(content).into();
        assert_eq!(parsed.title_sha256, expected_title_sha256);
        assert_eq!(parsed.content_sha256, expected_content_sha256);
        assert!(parsed.title_utf8);
        assert!(parsed.content_utf8);

        let line = format_lxmf_read_result(
            &[0x11; 16],
            &[0x22; 16],
            summary,
            Path::new("/tmp/message.lxmf"),
            Some(parsed),
        );
        assert!(line.contains("parsed=true"));
        assert!(line.contains(&format!("title_sha256={}", hex(&parsed.title_sha256))));
        assert!(line.contains(&format!("content_sha256={}", hex(&parsed.content_sha256))));
        assert!(!line.contains("private title"));
        assert!(!line.contains("private content"));
        assert!(!line.contains(&hex(content)));

        let mut tampered = wire.clone();
        let last_content_byte = tampered
            .windows(content.len())
            .position(|window| window == content)
            .unwrap()
            + content.len()
            - 1;
        tampered[last_content_byte] ^= 1;
        assert!(parse_and_validate_lxmf(summary, &tampered).is_err());
    }

    #[test]
    fn lxmf_output_is_reserved_streamed_verified_and_never_overwritten() {
        let wire = sample_lxmf_wire(b"private title", b"private content");
        let summary = sample_lxmf_summary(7, &wire);
        let output_file = TempOutput::new("lxmf-output");
        let command = Command::LxmfRead {
            handle: summary.handle(),
            output: output_file.path.clone(),
        };
        let mut output = CommandOutputs::reserve(&command, None)
            .unwrap()
            .into_lxmf_read()
            .unwrap();
        output.write_uncommitted(&wire[..64]).unwrap();
        output.write_uncommitted(&wire[64..]).unwrap();
        output
            .verify_sha256_uncommitted(wire.len(), summary.exact_wire_sha256())
            .unwrap();
        let read_back = output.read_back_uncommitted(wire.len()).unwrap();
        assert_eq!(read_back, wire);
        assert!(parse_and_validate_lxmf(summary, &read_back).is_ok());
        output.finish().unwrap();
        assert_eq!(fs::read(&output_file.path).unwrap(), wire);

        let error = CommandOutputs::reserve(&command, None)
            .err()
            .expect("an existing exact LXMF output must not be overwritten");
        assert!(error.contains("without overwriting"));
        assert_eq!(fs::read(&output_file.path).unwrap(), wire);

        let rejected_file = TempOutput::new("lxmf-digest-rejected");
        let rejected_command = Command::LxmfRead {
            handle: summary.handle(),
            output: rejected_file.path.clone(),
        };
        let mut rejected = CommandOutputs::reserve(&rejected_command, None)
            .unwrap()
            .into_lxmf_read()
            .unwrap();
        rejected.write_uncommitted(&wire).unwrap();
        assert!(
            rejected
                .verify_sha256_uncommitted(wire.len(), &[0; 32])
                .is_err()
        );
        drop(rejected);
        assert!(!rejected_file.path.exists());
    }

    #[test]
    fn inbox_status_output_has_authenticated_context_and_only_five_state_scalars() {
        let output = format_one_shot_response(
            &Command::RnsInboxStatus,
            &[0x11; 16],
            &[0x22; 16],
            DeviceResponse::RnsInboxStatus(reticulum_device_api::RnsInboxStatus {
                depth: 1,
                capacity: 1,
                dropped_since_boot: 42,
                max_payload_bytes: 383,
                durable: true,
            }),
            CommandOutputs::None,
        )
        .unwrap();
        assert_eq!(
            output,
            format!(
                "command=rns-inbox-status outcome=ok device_id={} session_id={} depth=1 capacity=1 dropped=42 max=383 durable=true",
                "11".repeat(16),
                "22".repeat(16),
            )
        );
        assert_eq!(output.split_whitespace().count(), 9);
    }

    #[test]
    fn inbox_peek_creates_private_exact_synced_output_and_reports_only_metadata() {
        const PAYLOAD: &[u8] = b"private inbox payload";
        let output_file = TempOutput::new("peek");
        let evidence_file = TempOutput::new("peek-evidence");
        let command = Command::RnsInboxPeek {
            output: output_file.path.clone(),
        };
        let item = reticulum_device_api::RnsInboxItem::new(
            core::num::NonZeroU64::new(7).unwrap(),
            DestinationHash([
                0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
                0x0e, 0x0f,
            ]),
            PAYLOAD,
        )
        .unwrap();
        let outputs = CommandOutputs::reserve(&command, Some(&evidence_file.path)).unwrap();
        let output = format_one_shot_response(
            &command,
            &[0x11; 16],
            &[0x22; 16],
            DeviceResponse::RnsInboxPeek(item),
            outputs,
        )
        .unwrap();

        assert_eq!(fs::read(&output_file.path).unwrap(), PAYLOAD);
        let expected_sha256: [u8; 32] = Sha256::digest(PAYLOAD).into();
        assert_eq!(
            output,
            format!(
                "command=rns-inbox-peek outcome=ok device_id={} session_id={} item_id=7 destination=000102030405060708090a0b0c0d0e0f length={} sha256={} output={}",
                "11".repeat(16),
                "22".repeat(16),
                PAYLOAD.len(),
                hex(&expected_sha256),
                output_file.path.display(),
            )
        );
        assert_eq!(output.split_whitespace().count(), 9);
        assert!(!output.contains("private inbox payload"));
        assert!(!output.contains(&hex(PAYLOAD)));

        let evidence_bytes = fs::read(&evidence_file.path).unwrap();
        assert_eq!(evidence_bytes.last(), Some(&b'\n'));
        let evidence: AuthenticatedEvidenceV1 = serde_json::from_slice(&evidence_bytes).unwrap();
        assert_eq!(
            evidence,
            AuthenticatedEvidenceV1::RnsInboxPeek {
                schema: EvidenceSchema::V1,
                device_id: "11".repeat(16),
                session_id: "22".repeat(16),
                item_id: 7,
                destination: "000102030405060708090a0b0c0d0e0f".to_owned(),
                length: PAYLOAD.len() as u16,
                payload_sha256: hex(&expected_sha256),
            }
        );
        let evidence_json = String::from_utf8(evidence_bytes).unwrap();
        assert!(!evidence_json.contains("private inbox payload"));
        assert!(!evidence_json.contains(&hex(PAYLOAD)));
        assert!(!evidence_json.contains(&output_file.path.display().to_string()));
        assert!(!evidence_json.contains(&evidence_file.path.display().to_string()));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&output_file.path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(&evidence_file.path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn inbox_peek_never_overwrites_an_existing_output() {
        let output_file = TempOutput::new("existing");
        fs::write(&output_file.path, b"keep this").unwrap();
        let command = Command::RnsInboxPeek {
            output: output_file.path.clone(),
        };
        let error = CommandOutputs::reserve(&command, None)
            .err()
            .expect("create-new output must reject an existing path");
        assert!(error.contains("without overwriting"));
        assert_eq!(fs::read(&output_file.path).unwrap(), b"keep this");
    }

    #[test]
    fn inbox_preflight_rejects_existing_evidence_and_output_aliases_without_residue() {
        let output_file = TempOutput::new("preflight-payload");
        let evidence_file = TempOutput::new("preflight-evidence");
        fs::write(&evidence_file.path, b"keep evidence").unwrap();
        let command = Command::RnsInboxPeek {
            output: output_file.path.clone(),
        };

        let existing_error = CommandOutputs::reserve(&command, Some(&evidence_file.path))
            .err()
            .expect("an existing evidence file must fail preflight");
        assert!(existing_error.contains("could not create evidence output"));
        assert_eq!(fs::read(&evidence_file.path).unwrap(), b"keep evidence");
        assert!(
            !output_file.path.exists(),
            "failed evidence reservation must remove the payload reservation"
        );

        let alias = output_file
            .path
            .parent()
            .unwrap()
            .join(".")
            .join(output_file.path.file_name().unwrap());
        let alias_error = CommandOutputs::reserve(&command, Some(&alias))
            .err()
            .expect("payload and evidence filesystem aliases must be rejected");
        assert!(alias_error.contains("could not create evidence output"));
        assert!(
            !output_file.path.exists(),
            "an alias collision must remove the payload reservation"
        );
    }

    #[cfg(unix)]
    #[test]
    fn reserved_output_commit_and_cleanup_sync_the_parent_for_bare_relative_paths() {
        use std::os::unix::fs::PermissionsExt as _;

        let committed = TempOutput::bare("bare-commit");
        assert_eq!(output_parent(&committed.path), Path::new("."));
        ReservedOutput::create(&committed.path, "evidence")
            .unwrap()
            .commit(b"synced evidence\n")
            .unwrap();
        assert_eq!(fs::read(&committed.path).unwrap(), b"synced evidence\n");
        assert_eq!(
            fs::metadata(&committed.path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let uncommitted = TempOutput::bare("bare-cleanup");
        let reservation = ReservedOutput::create(&uncommitted.path, "evidence").unwrap();
        assert!(uncommitted.path.exists());
        drop(reservation);
        assert!(!uncommitted.path.exists());
    }

    #[cfg(not(unix))]
    #[test]
    fn secret_bearing_reservations_fail_closed_without_unix_permissions() {
        let output = TempOutput::new("non-unix-permissions");
        let error = ReservedOutput::create(&output.path, "evidence")
            .err()
            .expect("non-Unix hosts must not claim owner-only output");
        assert!(error.contains("owner-only output reservations require Unix"));
        assert!(!output.path.exists());
    }

    #[test]
    fn evidence_preflight_precedes_credentials_and_serial_and_uncommitted_guard_cleans_up() {
        let evidence_file = TempOutput::new("preflight-ordering");
        fs::write(&evidence_file.path, b"existing evidence").unwrap();
        let options = Options {
            port: "/dev/this-port-must-not-be-opened".to_owned(),
            state_file: PathBuf::from("/this/credential/must-not-be-read"),
            timeout: Duration::from_millis(1),
            command: submit_and_wait_command(),
            evidence_output: Some(evidence_file.path.clone()),
        };
        let mut accepted_output = Vec::new();
        let error = transact(&options, &mut accepted_output)
            .expect_err("occupied evidence must fail before host I/O");
        assert!(error.contains("could not create evidence output"));
        assert!(!error.contains("credential"));
        assert!(!error.contains("could not open"));
        assert!(accepted_output.is_empty());
        assert_eq!(fs::read(&evidence_file.path).unwrap(), b"existing evidence");

        let reserved_file = TempOutput::new("uncommitted-evidence");
        let outputs =
            CommandOutputs::reserve(&submit_and_wait_command(), Some(&reserved_file.path)).unwrap();
        assert_eq!(fs::metadata(&reserved_file.path).unwrap().len(), 0);
        drop(outputs);
        assert!(
            !reserved_file.path.exists(),
            "host, transport, and deadline exits must remove an empty reservation"
        );
    }

    #[test]
    fn empty_inbox_is_clear_and_does_not_create_an_output() {
        let output_file = TempOutput::new("empty");
        let command = Command::RnsInboxPeek {
            output: output_file.path.clone(),
        };
        let outputs = CommandOutputs::reserve(&command, None).unwrap();
        let error = format_one_shot_response(
            &command,
            &[0x11; 16],
            &[0x22; 16],
            DeviceResponse::Error(reticulum_device_api::ApiErrorResponse {
                code: reticulum_device_api::ApiErrorCode::NotFound,
                operation: Some(reticulum_device_api::OP_EXPERIMENTAL_RNS_INBOX_PEEK),
            }),
            outputs,
        )
        .expect_err("an empty inbox must be an explicit error");
        assert_eq!(
            error,
            "RNS inbox is empty (NotFound); no output file was created"
        );
        assert!(!output_file.path.exists());
    }

    #[test]
    fn submit_and_wait_flushes_machine_accepted_marker_before_polling() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut output = TracedOutput {
            bytes: Vec::new(),
            events: Rc::clone(&events),
        };
        let poll_events = Rc::clone(&events);

        let terminal = continue_submit_and_wait(
            DeviceResponse::SubmitRnsDataAccepted(reticulum_device_api::SubmissionAccepted {
                id: SubmissionId(42),
            }),
            &[0x11; 16],
            &[0x22; 16],
            &mut output,
            move |submission_id| {
                assert_eq!(submission_id, SubmissionId(42));
                poll_events.borrow_mut().push("poll");
                Ok("terminal-output".to_owned())
            },
        )
        .unwrap();

        assert_eq!(terminal, "terminal-output");
        assert_eq!(
            events.borrow().as_slice(),
            &["accepted-write", "accepted-flush", "poll"]
        );
        let marker = String::from_utf8(output.bytes).unwrap();
        assert_eq!(
            marker,
            format!(
                "command=submit-and-wait outcome=accepted device_id={} session_id={} submission_id=42\n",
                "11".repeat(16),
                "22".repeat(16),
            )
        );
        let mut fields = marker.split_whitespace();
        assert_eq!(fields.next(), Some("command=submit-and-wait"));
        assert_eq!(fields.next(), Some("outcome=accepted"));
        assert_eq!(
            fields.next(),
            Some(format!("device_id={}", "11".repeat(16)).as_str())
        );
        assert_eq!(
            fields.next(),
            Some(format!("session_id={}", "22".repeat(16)).as_str())
        );
        assert_eq!(fields.next(), Some("submission_id=42"));
        assert_eq!(fields.next(), None);
        assert!(!marker.contains("000102030405060708090a0b0c0d0e0f"));
        assert!(!marker.contains(&hex(b"private submission payload")));
        assert!(!marker.contains("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff"));
    }

    #[test]
    fn submit_and_wait_emits_no_marker_or_poll_before_device_acceptance() {
        let mut output = Vec::new();
        let polled = Cell::new(false);
        let error = continue_submit_and_wait(
            DeviceResponse::Error(reticulum_device_api::ApiErrorResponse {
                code: reticulum_device_api::ApiErrorCode::CapacityExhausted,
                operation: Some(reticulum_device_api::OP_EXPERIMENTAL_SUBMIT_RNS_DATA),
            }),
            &[0x11; 16],
            &[0x22; 16],
            &mut output,
            |_| {
                polled.set(true);
                Ok("must not run".to_owned())
            },
        )
        .expect_err("a rejected submission must remain a command error");

        assert_eq!(
            error,
            format_api_error(
                "submit-and-wait",
                reticulum_device_api::ApiErrorResponse {
                    code: reticulum_device_api::ApiErrorCode::CapacityExhausted,
                    operation: Some(reticulum_device_api::OP_EXPERIMENTAL_SUBMIT_RNS_DATA),
                },
            )
        );
        assert!(output.is_empty());
        assert!(!polled.get());
    }

    #[test]
    fn accepted_marker_output_failure_stops_before_poll_and_cleans_evidence() {
        for (fault, expected_error, bytes_were_written) in [
            (AcceptedOutputFault::Write, "could not write", false),
            (AcceptedOutputFault::Flush, "could not flush", true),
        ] {
            let evidence_file = TempOutput::new("accepted-output-fault");
            let outputs =
                CommandOutputs::reserve(&submit_and_wait_command(), Some(&evidence_file.path))
                    .unwrap();
            let polled = Rc::new(Cell::new(false));
            let closure_polled = Rc::clone(&polled);
            let mut output = FaultingAcceptedOutput {
                fault,
                bytes: Vec::new(),
            };

            let error = continue_submit_and_wait(
                DeviceResponse::SubmitRnsDataAccepted(reticulum_device_api::SubmissionAccepted {
                    id: SubmissionId(42),
                }),
                &[0x11; 16],
                &[0x22; 16],
                &mut output,
                move |_| {
                    closure_polled.set(true);
                    drop(outputs);
                    Ok("must not poll".to_owned())
                },
            )
            .expect_err("an unobservable accepted marker must stop terminal polling");

            assert!(error.contains(expected_error), "unexpected error: {error}");
            assert!(error.contains("submission 42"));
            assert!(!polled.get());
            assert_eq!(!output.bytes.is_empty(), bytes_were_written);
            assert!(
                !evidence_file.path.exists(),
                "an uncommitted evidence reservation must be removed"
            );
        }
    }

    #[test]
    fn post_acceptance_wait_error_preserves_marker_and_cleans_evidence() {
        let evidence_file = TempOutput::new("accepted-then-wait-fault");
        let outputs =
            CommandOutputs::reserve(&submit_and_wait_command(), Some(&evidence_file.path)).unwrap();
        let mut output = Vec::new();

        let error = continue_submit_and_wait(
            DeviceResponse::SubmitRnsDataAccepted(reticulum_device_api::SubmissionAccepted {
                id: SubmissionId(42),
            }),
            &[0x11; 16],
            &[0x22; 16],
            &mut output,
            move |_| {
                let evidence = outputs.into_submit_evidence()?;
                drop(evidence);
                Err("terminal wait transport fault".to_owned())
            },
        )
        .expect_err("a wait failure must remain a command failure");

        assert_eq!(error, "terminal wait transport fault");
        assert_eq!(
            String::from_utf8(output).unwrap(),
            format!(
                "command=submit-and-wait outcome=accepted device_id={} session_id={} submission_id=42\n",
                "11".repeat(16),
                "22".repeat(16),
            )
        );
        assert!(
            !evidence_file.path.exists(),
            "a nonterminal wait failure must not leave empty evidence"
        );
    }

    #[test]
    fn status_output_exposes_only_scalar_state_and_prepared_packet_diagnostics() {
        let device_id = [0x11; 16];
        let session_id = [0x22; 16];
        let details = reticulum_device_api::PreparedPacketDetails {
            packet_len: 97,
            encoded_packet_sha256: reticulum_device_api::EncodedPacketSha256::new([0x33; 32]),
        };
        let awaiting = format_submission_status(
            "submission-status",
            &device_id,
            &session_id,
            reticulum_device_api::SubmissionStatus {
                id: SubmissionId(42),
                state: SubmissionState::AwaitingDelivery(details),
            },
        );
        assert_eq!(
            awaiting,
            format!(
                "command=submission-status outcome=ok device_id={} session_id={} submission_id=42 state=awaiting-delivery packet_len=97 encoded_packet_sha256={}",
                "11".repeat(16),
                "22".repeat(16),
                "33".repeat(32),
            )
        );

        let failed = format_submission_status(
            "submission-status",
            &device_id,
            &session_id,
            reticulum_device_api::SubmissionStatus {
                id: SubmissionId(42),
                state: SubmissionState::Failed(SubmissionFailure::DeliveryTimeout),
            },
        );
        assert!(failed.ends_with("submission_id=42 state=failed failure=delivery-timeout"));
        assert!(!failed.contains("packet_len"));
        assert!(!failed.contains("sha256"));
    }

    #[test]
    fn submit_delivered_evidence_is_strict_private_and_excludes_submission_inputs() {
        let evidence_file = TempOutput::new("submit-delivered-evidence");
        let evidence_output =
            CommandOutputs::reserve(&submit_and_wait_command(), Some(&evidence_file.path))
                .unwrap()
                .into_submit_evidence()
                .unwrap();
        let details = reticulum_device_api::PreparedPacketDetails {
            packet_len: 97,
            encoded_packet_sha256: reticulum_device_api::EncodedPacketSha256::new([0x33; 32]),
        };

        let line = finish_wait_decision(
            WaitDecision::Delivered {
                submission_id: SubmissionId(42),
                details,
            },
            &[0x11; 16],
            &[0x22; 16],
            evidence_output,
            "submit-and-wait",
        )
        .unwrap();
        assert_eq!(
            line,
            format!(
                "command=submit-and-wait outcome=ok device_id={} session_id={} submission_id=42 state=delivered packet_len=97 encoded_packet_sha256={}",
                "11".repeat(16),
                "22".repeat(16),
                "33".repeat(32),
            )
        );

        let evidence_bytes = fs::read(&evidence_file.path).unwrap();
        assert_eq!(evidence_bytes.last(), Some(&b'\n'));
        let evidence: AuthenticatedEvidenceV1 = serde_json::from_slice(&evidence_bytes).unwrap();
        let expected = AuthenticatedEvidenceV1::SubmitAndWait {
            schema: EvidenceSchema::V1,
            device_id: "11".repeat(16),
            session_id: "22".repeat(16),
            terminal: SubmitTerminalEvidence::Delivered {
                submission_id: 42,
                packet_len: 97,
                encoded_packet_sha256: "33".repeat(32),
            },
        };
        assert_eq!(evidence, expected);

        let evidence_json = String::from_utf8(evidence_bytes).unwrap();
        assert!(!evidence_json.contains("000102030405060708090a0b0c0d0e0f"));
        assert!(!evidence_json.contains(&hex(b"private submission payload")));
        assert!(!evidence_json.contains("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff"));
        assert!(!evidence_json.contains(&evidence_file.path.display().to_string()));
        assert!(!evidence_json.contains("state-file"));

        let mut unknown_top_level = serde_json::to_value(&expected).unwrap();
        unknown_top_level
            .as_object_mut()
            .unwrap()
            .insert("unexpected".to_owned(), serde_json::json!(true));
        assert!(
            serde_json::from_value::<AuthenticatedEvidenceV1>(unknown_top_level).is_err(),
            "v1 evidence must reject unknown top-level fields"
        );
        let mut unknown_terminal = serde_json::to_value(&expected).unwrap();
        unknown_terminal["terminal"]
            .as_object_mut()
            .unwrap()
            .insert("unexpected".to_owned(), serde_json::json!(true));
        assert!(
            serde_json::from_value::<AuthenticatedEvidenceV1>(unknown_terminal).is_err(),
            "v1 evidence must reject unknown terminal fields"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&evidence_file.path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn device_failed_delivery_timeout_writes_evidence_then_preserves_terminal_error() {
        let evidence_file = TempOutput::new("submit-failed-evidence");
        let evidence_output =
            CommandOutputs::reserve(&submit_and_wait_command(), Some(&evidence_file.path))
                .unwrap()
                .into_submit_evidence()
                .unwrap();
        let error = finish_wait_decision(
            WaitDecision::Failed {
                submission_id: SubmissionId(42),
                failure: SubmissionFailure::DeliveryTimeout,
            },
            &[0x11; 16],
            &[0x22; 16],
            evidence_output,
            "submit-and-wait",
        )
        .expect_err("device failure remains a command failure after evidence is committed");
        assert_eq!(
            error,
            "submission 42 reached state=failed failure=delivery-timeout"
        );
        assert_eq!(
            serde_json::from_slice::<AuthenticatedEvidenceV1>(
                &fs::read(&evidence_file.path).unwrap()
            )
            .unwrap(),
            AuthenticatedEvidenceV1::SubmitAndWait {
                schema: EvidenceSchema::V1,
                device_id: "11".repeat(16),
                session_id: "22".repeat(16),
                terminal: SubmitTerminalEvidence::Failed {
                    submission_id: 42,
                    reason: EvidenceSubmissionFailure::DeliveryTimeout,
                },
            }
        );
    }

    #[test]
    fn terminal_evidence_write_error_retains_authenticated_terminal_context() {
        let evidence_file = TempOutput::new("submit-evidence-write-error");
        let mut evidence_output =
            CommandOutputs::reserve(&submit_and_wait_command(), Some(&evidence_file.path))
                .unwrap()
                .into_submit_evidence()
                .unwrap()
                .unwrap();
        evidence_output.file.take();
        evidence_output.file = Some(File::open(&evidence_file.path).unwrap());

        let error = finish_wait_decision(
            WaitDecision::Delivered {
                submission_id: SubmissionId(42),
                details: reticulum_device_api::PreparedPacketDetails {
                    packet_len: 97,
                    encoded_packet_sha256: reticulum_device_api::EncodedPacketSha256::new(
                        [0x33; 32],
                    ),
                },
            },
            &[0x11; 16],
            &[0x22; 16],
            Some(evidence_output),
            "submit-and-wait",
        )
        .expect_err("read-only reservation handle must fail evidence commit");
        assert!(error.contains("authenticated submission 42 reached state=delivered"));
        assert!(error.contains("could not write evidence output"));
        assert!(
            !evidence_file.path.exists(),
            "failed evidence commit must remove its incomplete reservation"
        );
    }

    #[test]
    fn wait_state_machine_retries_only_internal_and_requires_delivered() {
        let id = SubmissionId(42);
        let status = |state| {
            DeviceResponse::SubmissionStatus(reticulum_device_api::SubmissionStatus { id, state })
        };
        let details = reticulum_device_api::PreparedPacketDetails {
            packet_len: 97,
            encoded_packet_sha256: reticulum_device_api::EncodedPacketSha256::new([0x33; 32]),
        };
        assert_eq!(
            classify_wait_response(id, status(SubmissionState::Queued)).unwrap(),
            WaitDecision::PollAgain
        );
        assert_eq!(
            classify_wait_response(id, status(SubmissionState::Preparing)).unwrap(),
            WaitDecision::PollAgain
        );
        assert_eq!(
            classify_wait_response(id, status(SubmissionState::AwaitingDelivery(details))).unwrap(),
            WaitDecision::PollAgain
        );
        assert!(matches!(
            classify_wait_response(id, status(SubmissionState::Delivered(details))).unwrap(),
            WaitDecision::Delivered { .. }
        ));
        assert!(matches!(
            classify_wait_response(
                id,
                status(SubmissionState::Failed(SubmissionFailure::NoPath))
            )
            .unwrap(),
            WaitDecision::Failed {
                failure: SubmissionFailure::NoPath,
                ..
            }
        ));
        assert!(classify_wait_response(id, status(SubmissionState::Cancelled)).is_err());

        let api_error = |code| {
            DeviceResponse::Error(reticulum_device_api::ApiErrorResponse {
                code,
                operation: Some(reticulum_device_api::OP_SUBMISSION_STATUS),
            })
        };
        assert_eq!(
            classify_wait_response(id, api_error(reticulum_device_api::ApiErrorCode::Internal))
                .unwrap(),
            WaitDecision::RetryInternal
        );
        assert!(
            classify_wait_response(id, api_error(reticulum_device_api::ApiErrorCode::NotFound))
                .is_err()
        );
        assert!(
            classify_wait_response(
                id,
                DeviceResponse::SubmissionStatus(reticulum_device_api::SubmissionStatus {
                    id: SubmissionId(43),
                    state: SubmissionState::Queued,
                })
            )
            .is_err()
        );
    }

    #[test]
    fn repeated_session_request_ids_are_strictly_increasing_and_never_wrap() {
        let mut ids = RequestIds::new();
        assert_eq!(ids.take().unwrap(), RequestId(1));
        assert_eq!(ids.take().unwrap(), RequestId(2));
        ids.next = Some(u64::MAX);
        assert_eq!(ids.take().unwrap(), RequestId(u64::MAX));
        assert!(ids.take().is_err());
    }

    #[test]
    fn response_version_accepts_any_current_major_minor_only() {
        assert!(
            validate_response_version(ApiVersion {
                major: ApiVersion::CURRENT.major,
                minor: u16::MAX,
            })
            .is_ok()
        );
        assert!(
            validate_response_version(ApiVersion {
                major: ApiVersion::CURRENT.major + 1,
                minor: 0,
            })
            .is_err()
        );
    }

    #[test]
    fn submit_and_wait_output_contains_no_submission_input_material() {
        let line = format_submission_status(
            "submit-and-wait",
            &[0x11; 16],
            &[0x22; 16],
            reticulum_device_api::SubmissionStatus {
                id: SubmissionId(42),
                state: SubmissionState::Delivered(reticulum_device_api::PreparedPacketDetails {
                    packet_len: 97,
                    encoded_packet_sha256: reticulum_device_api::EncodedPacketSha256::new(
                        [0x33; 32],
                    ),
                }),
            },
        );
        assert!(line.contains("command=submit-and-wait outcome=ok"));
        assert!(line.contains("state=delivered packet_len=97 encoded_packet_sha256="));
        assert!(!line.contains("48656c6c6f"));
        assert!(!line.contains("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff"));
    }

    #[test]
    fn buffered_reader_preserves_a_coalesced_second_record() {
        let first = FramedRecord::encode(&record(0x02, 1)).unwrap();
        let second = FramedRecord::encode(&record(0x03, 2)).unwrap();
        let mut wire = Vec::from(first.encoded());
        wire.extend_from_slice(&second.encoded()[1..]);
        let mut source = Cursor::new(wire);
        let mut reader = BufferedRecordReader::new();
        let deadline = Instant::now() + Duration::from_secs(1);

        let decoded_first = reader.read_record(&mut source, deadline, "first").unwrap();
        assert_eq!(decoded_first.kind(), 0x02);
        assert_eq!(decoded_first.sequence(), 1);
        let decoded_second = reader.read_record(&mut source, deadline, "second").unwrap();
        assert_eq!(decoded_second.kind(), 0x03);
        assert_eq!(decoded_second.sequence(), 2);
    }
}
