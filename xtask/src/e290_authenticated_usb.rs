//! Minimal authenticated USB client for one E290 device-API request.

use std::{
    fmt::Write as _,
    io::{self, Read, Write},
    num::NonZeroU32,
    path::PathBuf,
    process::ExitCode,
    thread,
    time::{Duration, Instant},
};

use rand_core::{CryptoRng, RngCore};
use reticulum_device_api::{
    ApiVersion, DeviceRequest, DeviceResponse, MAX_MESSAGE_BYTES, RequestEnvelope, RequestId,
    decode_response, encode_request,
};
use reticulum_device_api_framing::{DecodeEvent, Record, StreamDecoder, TxAdvanceError};
use reticulum_device_api_handoff::{MessageLength, OwnedMessage};
use reticulum_device_api_session::{
    BearerBinding, ClientCredential, ClientHelloFlight, ClientParameters, ClientProofFlight,
    ClientRequestFlight, DeviceId,
};
use serialport::ClearBuffer;
use zeroize::Zeroizing;

use crate::e290_pairing_live::load_activated_credential;

const BAUD_RATE: u32 = 115_200;
const DEFAULT_TIMEOUT_MS: u64 = 5_000;
const OPEN_SETTLE_MS: u64 = 250;
const IO_SLICE_MS: u64 = 100;
const READ_BUFFER_CAPACITY: usize = 1_024;
const CAPABILITIES_REQUEST_ID: RequestId = RequestId(1);

struct Options {
    port: String,
    state_file: PathBuf,
    timeout: Duration,
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
            eprintln!("error: {reason}");
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
            eprintln!("error: {reason}");
            ExitCode::FAILURE
        }
    }
}

fn transact(options: &Options) -> Result<String, String> {
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

    let deadline = Instant::now()
        .checked_add(options.timeout)
        .ok_or_else(|| "--timeout-ms is too large for the host monotonic clock".to_owned())?;
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

    let request = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: CAPABILITIES_REQUEST_ID,
        request: DeviceRequest::SystemCapabilities,
    };
    let mut request_bytes = [0_u8; MAX_MESSAGE_BYTES];
    let request_length = encode_request(&request, &mut request_bytes)
        .map_err(|error| format!("could not encode system.capabilities request: {error:?}"))?;
    let request_owner = OwnedMessage::new(
        MessageLength::new(request_length)
            .map_err(|_| "system.capabilities request exceeded the session limit".to_owned())?,
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
    let (_, response_message) = authenticated.into_parts();
    let response = decode_response(response_message.encoded())
        .map_err(|error| format!("could not decode authenticated response: {error:?}"))?;
    if response.version != ApiVersion::CURRENT {
        return Err(format!(
            "device returned API version {}.{}",
            response.version.major, response.version.minor
        ));
    }
    if response.request_id != CAPABILITIES_REQUEST_ID {
        return Err(format!(
            "device returned request ID {} instead of {}",
            response.request_id.0, CAPABILITIES_REQUEST_ID.0
        ));
    }

    let DeviceResponse::SystemCapabilities(capabilities) = response.response else {
        return Err(format!(
            "device returned response kind {} instead of system.capabilities",
            response.response.kind()
        ));
    };
    Ok(format!(
        "command=system-capabilities outcome=ok device_id={} session_id={} api={}.{} packet_output={} direct_radio_tx={} experimental_submit_rns_data={} max_message_bytes={} max_body_bytes={} max_submit_rns_data_payload_bytes={}",
        hex(device_id.as_bytes()),
        hex(&session_id),
        capabilities.api_version().major,
        capabilities.api_version().minor,
        capabilities.packet_output(),
        capabilities.direct_radio_tx().wire_code(),
        capabilities.experimental_submit_rns_data(),
        capabilities.max_message_bytes(),
        capabilities.max_body_bytes(),
        capabilities.max_submit_rns_data_payload_bytes(),
    ))
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
            unknown => return Err(format!("unexpected or duplicate argument {unknown:?}")),
        }
        index += 1;
    }
    Ok(Options {
        port: port.ok_or_else(|| "--port is required".to_owned())?,
        state_file: state_file.ok_or_else(|| "--state-file is required".to_owned())?,
        timeout: Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS)),
    })
}

fn required_value<'a>(value: Option<&'a String>, flag: &str) -> Result<&'a str, String> {
    value
        .map(String::as_str)
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn usage() {
    eprintln!(
        "usage: cargo run -p xtask -- e290-authenticated-usb --port <serial-path> --state-file <active-secret-path> [--timeout-ms <u64>]"
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
    use std::io::Cursor;

    use reticulum_device_api_framing::{
        AUTH_TAG_LENGTH, FramedRecord, PAYLOAD_CAPACITY, PayloadLength,
    };

    use super::*;

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
