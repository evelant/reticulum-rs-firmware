//! Bounded authenticated USB client for E290 device-API requests.

use std::{
    fmt::Write as _,
    fs::OpenOptions,
    io::{self, Read, Write},
    num::NonZeroU32,
    path::{Path, PathBuf},
    process::ExitCode,
    thread,
    time::{Duration, Instant},
};

use rand_core::{CryptoRng, RngCore};
use reticulum_device_api::{
    ApiVersion, DestinationHash, DeviceRequest, DeviceResponse, IdempotencyKey, MAX_MESSAGE_BYTES,
    MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES, RequestEnvelope, RequestId, SubmissionFailure, SubmissionId,
    SubmissionState, decode_response, encode_request,
};
use reticulum_device_api_framing::{DecodeEvent, Record, StreamDecoder, TxAdvanceError};
use reticulum_device_api_handoff::{MessageLength, OwnedMessage};
use reticulum_device_api_session::{
    BearerBinding, ClientCredential, ClientHelloFlight, ClientParameters, ClientProofFlight,
    ClientRequestFlight, ClientSession, DeviceId,
};
use serialport::ClearBuffer;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::e290_pairing_live::load_activated_credential;

const BAUD_RATE: u32 = 115_200;
const DEFAULT_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_SUBMIT_AND_WAIT_TIMEOUT_MS: u64 = 45_000;
const POLL_INTERVAL: Duration = Duration::from_millis(500);
const OPEN_SETTLE_MS: u64 = 250;
const IO_SLICE_MS: u64 = 100;
const READ_BUFFER_CAPACITY: usize = 1_024;

#[derive(Debug, Eq, PartialEq)]
enum Command {
    SystemCapabilities,
    IdentitySummary,
    RnsInboxStatus,
    RnsInboxPeek {
        output: PathBuf,
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
    Delivered(reticulum_device_api::SubmissionStatus),
}

struct Options {
    port: String,
    state_file: PathBuf,
    timeout: Duration,
    command: Command,
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

    match transact(&options) {
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

fn transact(options: &Options) -> Result<String, String> {
    let deadline = Instant::now()
        .checked_add(options.timeout)
        .ok_or_else(|| "--timeout-ms is too large for the host monotonic clock".to_owned())?;
    let activated = load_activated_credential(&options.state_file)?;
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

    if matches!(&options.command, Command::SubmitAndWait { .. }) {
        let submission_id = match response {
            DeviceResponse::SubmitRnsDataAccepted(accepted) => accepted.id,
            DeviceResponse::Error(error) => {
                return Err(format_api_error(command_name(&options.command), error));
            }
            other => {
                return Err(format!(
                    "device returned response kind {} instead of {}",
                    other.kind(),
                    command_name(&options.command),
                ));
            }
        };
        return wait_for_delivery(
            &mut *port,
            &mut reader,
            session,
            &mut request_ids,
            deadline,
            device_id.as_bytes(),
            &session_id,
            submission_id,
        );
    }
    drop(session);
    format_one_shot_response(
        &options.command,
        device_id.as_bytes(),
        &session_id,
        response,
    )
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

fn format_one_shot_response(
    command: &Command,
    device_id: &[u8; 16],
    session_id: &[u8; 16],
    response: DeviceResponse,
) -> Result<String, String> {
    match (command, response) {
        (Command::SystemCapabilities, DeviceResponse::SystemCapabilities(capabilities)) => {
            Ok(format!(
                "command=system-capabilities outcome=ok device_id={} session_id={} api={}.{} packet_output={} direct_radio_tx={} experimental_submit_rns_data={} experimental_rns_inbox={} max_message_bytes={} max_body_bytes={} max_submit_rns_data_payload_bytes={} max_rns_inbox_payload_bytes={}",
                hex(device_id),
                hex(session_id),
                capabilities.api_version().major,
                capabilities.api_version().minor,
                capabilities.packet_output(),
                capabilities.direct_radio_tx().wire_code(),
                capabilities.experimental_submit_rns_data(),
                capabilities.experimental_rns_inbox().wire_code(),
                capabilities.max_message_bytes(),
                capabilities.max_body_bytes(),
                capabilities.max_submit_rns_data_payload_bytes(),
                capabilities.max_rns_inbox_payload_bytes(),
            ))
        }
        (Command::IdentitySummary, DeviceResponse::IdentitySummary(summary)) => Ok(format!(
            "command=identity-summary outcome=ok device_id={} session_id={} primary_destination={}",
            hex(device_id),
            hex(session_id),
            hex(&summary.primary_destination().0),
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
            let payload_sha256 = write_rns_inbox_payload(output, item.payload())?;
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

fn write_rns_inbox_payload(path: &Path, payload: &[u8]) -> Result<[u8; 32], String> {
    #[cfg(unix)]
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path).map_err(|error| {
        format!(
            "could not create inbox output {} without overwriting: {error}",
            path.display()
        )
    })?;
    #[cfg(unix)]
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|error| {
            format!(
                "could not restrict inbox output {} to owner-only permissions: {error}",
                path.display()
            )
        })?;
    file.write_all(payload)
        .map_err(|error| format!("could not write inbox output {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("could not sync inbox output {}: {error}", path.display()))?;
    Ok(Sha256::digest(payload).into())
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
        match classify_wait_response(submission_id, response)? {
            WaitDecision::PollAgain | WaitDecision::RetryInternal => {}
            WaitDecision::Delivered(status) => {
                return Ok(format_submission_status(
                    "submit-and-wait",
                    device_id,
                    session_id,
                    status,
                ));
            }
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
                SubmissionState::Delivered(_) => Ok(WaitDecision::Delivered(status)),
                SubmissionState::Failed(failure) => Err(format!(
                    "submission {} reached state=failed failure={}",
                    status.id.0,
                    failure_name(failure)
                )),
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
    let mut idempotency_key = None;
    let mut submission_id = None;
    let mut output = None;
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
            "--output" if output.is_none() => {
                index += 1;
                output = Some(PathBuf::from(required_value(args.get(index), "--output")?));
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
            "submission-status" if command.is_none() => {
                command = Some(CommandKind::SubmissionStatus);
            }
            "submit-rns-data" if command.is_none() => command = Some(CommandKind::SubmitRnsData),
            "submit-and-wait" if command.is_none() => command = Some(CommandKind::SubmitAndWait),
            option @ ("--port" | "--state-file" | "--timeout-ms" | "--destination-hash"
            | "--payload-hex" | "--idempotency-key" | "--submission-id" | "--output") => {
                return Err(format!("duplicate option {option}"));
            }
            command_name @ ("system-capabilities"
            | "identity-summary"
            | "rns-inbox-status"
            | "rns-inbox-peek"
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
        | CommandKind::RnsInboxStatus) => {
            if destination.is_some()
                || payload.is_some()
                || idempotency_key.is_some()
                || submission_id.is_some()
                || output.is_some()
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
                _ => unreachable!("combined match admits only argument-free commands"),
            }
        }
        CommandKind::RnsInboxPeek => {
            if destination.is_some()
                || payload.is_some()
                || idempotency_key.is_some()
                || submission_id.is_some()
            {
                return Err(
                    "rns-inbox-peek accepts only the operation-specific --output argument"
                        .to_owned(),
                );
            }
            Command::RnsInboxPeek {
                output: output.ok_or_else(|| "rns-inbox-peek requires --output".to_owned())?,
            }
        }
        CommandKind::SubmissionStatus => {
            if destination.is_some() || payload.is_some() || idempotency_key.is_some() {
                return Err(
                    "submission-status does not accept submit-rns-data arguments".to_owned(),
                );
            }
            if output.is_some() {
                return Err("submission-status does not accept --output".to_owned());
            }
            Command::SubmissionStatus {
                id: submission_id
                    .ok_or_else(|| "submission-status requires --submission-id".to_owned())?,
            }
        }
        kind @ (CommandKind::SubmitRnsData | CommandKind::SubmitAndWait) => {
            if submission_id.is_some() {
                return Err(format!(
                    "{} does not accept --submission-id",
                    command_kind_name(kind)
                ));
            }
            if output.is_some() {
                return Err(format!(
                    "{} does not accept --output",
                    command_kind_name(kind)
                ));
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
    let default_timeout_ms = if matches!(&command, Command::SubmitAndWait { .. }) {
        DEFAULT_SUBMIT_AND_WAIT_TIMEOUT_MS
    } else {
        DEFAULT_TIMEOUT_MS
    };
    Ok(Options {
        port: port.ok_or_else(|| "--port is required".to_owned())?,
        state_file: state_file.ok_or_else(|| "--state-file is required".to_owned())?,
        timeout: Duration::from_millis(timeout_ms.unwrap_or(default_timeout_ms)),
        command,
    })
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
    if !value.len().is_multiple_of(2) {
        return Err("--payload-hex requires an even number of hexadecimal digits".to_owned());
    }
    let length = value.len() / 2;
    if length > MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES {
        return Err(format!(
            "--payload-hex decodes to {length} bytes; maximum is {MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES}"
        ));
    }
    let mut bytes = vec![0_u8; length];
    decode_hex_into(value, &mut bytes, "--payload-hex")?;
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
        "usage:\n  cargo run -p xtask -- e290-authenticated-usb --port <serial-path> --state-file <active-secret-path> [--timeout-ms <u64>] [system-capabilities]\n  cargo run -p xtask -- e290-authenticated-usb --port <serial-path> --state-file <active-secret-path> [--timeout-ms <u64>] identity-summary\n  cargo run -p xtask -- e290-authenticated-usb --port <serial-path> --state-file <active-secret-path> [--timeout-ms <u64>] rns-inbox-status\n  cargo run -p xtask -- e290-authenticated-usb --port <serial-path> --state-file <active-secret-path> [--timeout-ms <u64>] rns-inbox-peek --output <path>\n  cargo run -p xtask -- e290-authenticated-usb --port <serial-path> --state-file <active-secret-path> [--timeout-ms <u64>] --submission-id <u64> submission-status\n  cargo run -p xtask -- e290-authenticated-usb --port <serial-path> --state-file <active-secret-path> [--timeout-ms <u64>] --destination-hash <32-hex> --payload-hex <0-to-766-hex> --idempotency-key <32-hex> submit-rns-data\n  cargo run -p xtask -- e290-authenticated-usb --port <serial-path> --state-file <active-secret-path> [--timeout-ms <u64, default 45000>] --destination-hash <32-hex> --payload-hex <0-to-766-hex> --idempotency-key <32-hex> submit-and-wait"
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
        fs,
        io::Cursor,
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
            "rns-inbox-peek accepts only the operation-specific --output argument"
        );
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
        )
        .unwrap();
        assert_eq!(
            output,
            format!(
                "command=identity-summary outcome=ok device_id={} session_id={} primary_destination=000102030405060708090a0b0c0d0e0f",
                "ab".repeat(16),
                "cd".repeat(16),
            )
        );
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
        assert!(matches!(waiting.command, Command::SubmitAndWait { .. }));

        let mut overridden = strings(&common);
        overridden.insert(0, "12000".to_owned());
        overridden.insert(0, "--timeout-ms".to_owned());
        assert_eq!(
            parse(&overridden).unwrap().timeout,
            Duration::from_millis(12_000)
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
    fn capabilities_output_includes_inbox_availability_and_limit() {
        let output = format_one_shot_response(
            &Command::SystemCapabilities,
            &[0x11; 16],
            &[0x22; 16],
            DeviceResponse::SystemCapabilities(reticulum_device_api::CapabilitySnapshot::current()),
        )
        .unwrap();
        assert!(output.contains("api=1.2"));
        assert!(output.contains("experimental_rns_inbox=2"));
        assert!(output.contains("max_rns_inbox_payload_bytes=383"));
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
        let output = format_one_shot_response(
            &command,
            &[0x11; 16],
            &[0x22; 16],
            DeviceResponse::RnsInboxPeek(item),
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
        }
    }

    #[test]
    fn inbox_peek_never_overwrites_an_existing_output() {
        let output_file = TempOutput::new("existing");
        fs::write(&output_file.path, b"keep this").unwrap();
        let command = Command::RnsInboxPeek {
            output: output_file.path.clone(),
        };
        let item = reticulum_device_api::RnsInboxItem::new(
            core::num::NonZeroU64::MIN,
            DestinationHash([0x44; 16]),
            b"replacement",
        )
        .unwrap();
        let error = format_one_shot_response(
            &command,
            &[0x11; 16],
            &[0x22; 16],
            DeviceResponse::RnsInboxPeek(item),
        )
        .expect_err("create-new output must reject an existing path");
        assert!(error.contains("without overwriting"));
        assert_eq!(fs::read(&output_file.path).unwrap(), b"keep this");
    }

    #[test]
    fn empty_inbox_is_clear_and_does_not_create_an_output() {
        let output_file = TempOutput::new("empty");
        let command = Command::RnsInboxPeek {
            output: output_file.path.clone(),
        };
        let error = format_one_shot_response(
            &command,
            &[0x11; 16],
            &[0x22; 16],
            DeviceResponse::Error(reticulum_device_api::ApiErrorResponse {
                code: reticulum_device_api::ApiErrorCode::NotFound,
                operation: Some(reticulum_device_api::OP_EXPERIMENTAL_RNS_INBOX_PEEK),
            }),
        )
        .expect_err("an empty inbox must be an explicit error");
        assert_eq!(
            error,
            "RNS inbox is empty (NotFound); no output file was created"
        );
        assert!(!output_file.path.exists());
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
            WaitDecision::Delivered(_)
        ));
        assert!(
            classify_wait_response(
                id,
                status(SubmissionState::Failed(SubmissionFailure::NoPath))
            )
            .is_err()
        );
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
