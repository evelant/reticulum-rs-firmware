//! Host client for the E290 pre-authentication initialization-control bearer.

use std::{
    io,
    process::ExitCode,
    thread,
    time::{Duration, Instant},
};

use reticulum_device_api_framing::{DecodeEvent, FramedRecord, StreamDecoder};
use reticulum_device_api_pairing_control::{
    ControlRequest, ControlResponse, InitializationStatus, InitializeResult,
};
use serialport::ClearBuffer;

const BAUD_RATE: u32 = 115_200;
const DEFAULT_STATUS_TIMEOUT_MS: u64 = 15_000;
const DEFAULT_INITIALIZE_TIMEOUT_MS: u64 = 120_000;
const OPEN_SETTLE_MS: u64 = 250;
const READ_SLICE_MS: u64 = 100;
const WORKFLOW_POLL_MS: u64 = 200;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Command {
    Status,
    Initialize,
}

struct Options {
    port: String,
    command: Command,
    sequence: u64,
    timeout: Duration,
}

struct TransactionResult {
    command: Command,
    response: ControlResponse,
    next_sequence: Option<u64>,
}

impl TransactionResult {
    const fn succeeded(&self) -> bool {
        matches!(
            (self.command, self.response),
            (
                Command::Status,
                ControlResponse::Status {
                    status: InitializationStatus::InitializationRequired
                        | InitializationStatus::InFlight
                        | InitializationStatus::Completed,
                    ..
                },
            ) | (
                Command::Initialize,
                ControlResponse::Status {
                    status: InitializationStatus::Completed,
                    ..
                },
            ) | (
                Command::Initialize,
                ControlResponse::Initialize {
                    result: InitializeResult::Completed,
                    ..
                },
            )
        )
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
        Ok(result) => {
            let succeeded = result.succeeded();
            print_response(result);
            if succeeded {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(reason) => {
            eprintln!("error: {reason}");
            ExitCode::FAILURE
        }
    }
}

fn transact(options: &Options) -> Result<TransactionResult, String> {
    let mut port = serialport::new(&options.port, BAUD_RATE)
        .timeout(Duration::from_millis(READ_SLICE_MS))
        .open()
        .map_err(|error| format!("could not open {}: {error}", options.port))?;
    port.write_data_terminal_ready(true)
        .map_err(|error| format!("could not assert DTR on {}: {error}", options.port))?;
    port.write_request_to_send(false)
        .map_err(|error| format!("could not clear RTS on {}: {error}", options.port))?;

    // Opening native ESP32-S3 CDC may reset a board on some hosts. Let any
    // counted reboot finish and the firmware allocate its connection epoch,
    // then discard only pre-request host input.
    thread::sleep(Duration::from_millis(OPEN_SETTLE_MS));
    port.clear(ClearBuffer::Input)
        .map_err(|error| format!("could not clear stale input on {}: {error}", options.port))?;
    let deadline = Instant::now()
        .checked_add(options.timeout)
        .ok_or_else(|| "--timeout-ms is too large for the host monotonic clock".to_owned())?;
    let mut decoder = StreamDecoder::new();
    let response = match options.command {
        Command::Status => exchange(
            &mut *port,
            &mut decoder,
            ControlRequest::status(options.sequence),
            deadline,
            &options.port,
        )?,
        Command::Initialize => initialize_workflow(
            &mut *port,
            &mut decoder,
            options.sequence,
            deadline,
            &options.port,
        )?,
    };
    let next_sequence = next_usable_sequence(response.sequence());
    Ok(TransactionResult {
        command: options.command,
        response,
        next_sequence,
    })
}

fn initialize_workflow(
    port: &mut dyn serialport::SerialPort,
    decoder: &mut StreamDecoder,
    sequence: u64,
    deadline: Instant,
    port_name: &str,
) -> Result<ControlResponse, String> {
    initialize_workflow_with(
        sequence,
        |request| exchange(port, decoder, request, deadline, port_name),
        |last_sent_sequence| sleep_before_next(deadline, last_sent_sequence),
    )
}

fn initialize_workflow_with(
    mut sequence: u64,
    mut exchange_request: impl FnMut(ControlRequest) -> Result<ControlResponse, String>,
    mut wait_before_next: impl FnMut(u64) -> Result<(), String>,
) -> Result<ControlResponse, String> {
    let mut status = exchange_request(ControlRequest::status(sequence))?;
    let mut announced_presence_wait = false;
    loop {
        match status {
            ControlResponse::Status {
                status:
                    InitializationStatus::Completed
                    | InitializationStatus::Unavailable
                    | InitializationStatus::Blocked,
                ..
            } => return Ok(status),
            ControlResponse::Status {
                status: InitializationStatus::InFlight,
                ..
            } => {
                wait_before_next(sequence)?;
                sequence = require_next_sequence(sequence)?;
                status = exchange_request(ControlRequest::status(sequence))?;
            }
            ControlResponse::Status {
                status: InitializationStatus::InitializationRequired,
                ..
            } => {
                sequence = require_next_sequence(sequence)?;
                let response = exchange_request(ControlRequest::initialize(sequence))?;
                match response {
                    ControlResponse::Initialize {
                        result:
                            InitializeResult::Completed
                            | InitializeResult::Refused
                            | InitializeResult::Blocked
                            | InitializeResult::Unavailable,
                        ..
                    } => return Ok(response),
                    ControlResponse::Initialize {
                        result: InitializeResult::PhysicalPresenceRequired,
                        ..
                    } => {
                        if !announced_presence_wait {
                            eprintln!(
                                "waiting for GPIO21 physical presence: release once, then hold for at least 2 seconds"
                            );
                            announced_presence_wait = true;
                        }
                    }
                    ControlResponse::Initialize {
                        result: InitializeResult::Retrying,
                        ..
                    } => {}
                    ControlResponse::Status { .. } => unreachable!("request kind was checked"),
                }
                wait_before_next(sequence)?;
                sequence = require_next_sequence(sequence)?;
                status = exchange_request(ControlRequest::status(sequence))?;
            }
            ControlResponse::Initialize { .. } => {
                return Err("device returned initialize response to status request".to_owned());
            }
        }
    }
}

fn exchange(
    port: &mut dyn serialport::SerialPort,
    decoder: &mut StreamDecoder,
    request: ControlRequest,
    deadline: Instant,
    port_name: &str,
) -> Result<ControlResponse, String> {
    let sequence = request.sequence();
    if Instant::now() >= deadline {
        return Err(deadline_message(sequence, false));
    }
    let expects_status = matches!(request, ControlRequest::Status { .. });
    let frame = FramedRecord::encode(&request.into_record())
        .map_err(|_| "canonical request did not fit its fixed frame owner".to_owned())?;
    port.write_all(frame.encoded())
        .map_err(|error| post_send_failure("request write", port_name, &error, sequence))?;
    port.flush()
        .map_err(|error| post_send_failure("request flush", port_name, &error, sequence))?;

    let mut bytes = [0_u8; 256];
    while Instant::now() < deadline {
        match port.read(&mut bytes) {
            Ok(0) => {}
            Ok(length) => {
                for byte in &bytes[..length] {
                    let DecodeEvent::Record(record) = decoder.push(*byte) else {
                        continue;
                    };
                    let Ok(response) = ControlResponse::from_record(record) else {
                        continue;
                    };
                    if response.sequence() != sequence {
                        continue;
                    }
                    let matches_command = matches!(
                        (expects_status, response),
                        (true, ControlResponse::Status { .. })
                            | (false, ControlResponse::Initialize { .. })
                    );
                    if matches_command {
                        return Ok(response);
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
            Err(error) => {
                return Err(post_send_failure(
                    "response read",
                    port_name,
                    &error,
                    sequence,
                ));
            }
        }
    }
    Err(format!(
        "timed out waiting for sequence {sequence} on {port_name}; {}",
        sequence_ambiguity_guidance(sequence)
    ))
}

const fn next_usable_sequence(sequence: u64) -> Option<u64> {
    match sequence.checked_add(1) {
        Some(next) if next != u64::MAX => Some(next),
        _ => None,
    }
}

fn require_next_sequence(sequence: u64) -> Result<u64, String> {
    next_usable_sequence(sequence).ok_or_else(|| {
        format!(
            "host request sequence exhausted after sequence {sequence}; the firmware refuses \
             sequence {} and exhausts this USB epoch",
            u64::MAX
        )
    })
}

fn sleep_before_next(deadline: Instant, last_sent_sequence: u64) -> Result<(), String> {
    let now = Instant::now();
    let Some(remaining) = deadline.checked_duration_since(now) else {
        return Err(deadline_message(last_sent_sequence, true));
    };
    if remaining <= Duration::from_millis(WORKFLOW_POLL_MS) {
        return Err(deadline_message(last_sent_sequence, true));
    }
    thread::sleep(Duration::from_millis(WORKFLOW_POLL_MS));
    Ok(())
}

fn deadline_message(last_sent_sequence: u64, response_received: bool) -> String {
    if response_received {
        format!(
            "initialization workflow timed out after last_sent_sequence={last_sent_sequence}; \
             that sequence was consumed because its response was received; {}",
            bus_reset_guidance()
        )
    } else {
        format!(
            "initialization workflow timed out before sending sequence {last_sent_sequence}; {}",
            bus_reset_guidance()
        )
    }
}

fn sequence_ambiguity_guidance(last_sent_sequence: u64) -> String {
    format!(
        "last_sent_sequence={last_sent_sequence} is consumed-or-ambiguous because the device may \
         have accepted it before its response was lost; {}",
        bus_reset_guidance()
    )
}

fn post_send_failure(operation: &str, port_name: &str, error: &io::Error, sequence: u64) -> String {
    format!(
        "{operation} failed on {port_name}: {error}; {}",
        sequence_ambiguity_guidance(sequence)
    )
}

const fn bus_reset_guidance() -> &'static str {
    "opening or closing the serial port does not start a new sequence epoch; confirm a firmware/USB bus reset before restarting at sequence 0"
}

fn print_response(result: TransactionResult) {
    println!("{}", format_response(&result));
}

fn format_response(result: &TransactionResult) -> String {
    let next = result
        .next_sequence
        .map_or_else(|| "exhausted".to_owned(), |value| value.to_string());
    match (result.command, result.response) {
        (Command::Status, ControlResponse::Status { sequence, status }) => format!(
            "command=status sequence={sequence} status={} code={} next_sequence={next}",
            status_name(status),
            status.code()
        ),
        (Command::Initialize, ControlResponse::Status { sequence, status }) => format!(
            "command=initialize response=status sequence={sequence} outcome={} code={} \
             next_sequence={next}",
            status_name(status),
            status.code()
        ),
        (
            Command::Initialize,
            ControlResponse::Initialize {
                sequence,
                result: initialize_result,
            },
        ) => format!(
            "command=initialize response=initialize sequence={sequence} outcome={} code={} \
             next_sequence={next}",
            result_name(initialize_result),
            initialize_result.code()
        ),
        (Command::Status, ControlResponse::Initialize { sequence, .. }) => format!(
            "command=status response=unexpected-initialize sequence={sequence} next_sequence={next}"
        ),
    }
}

const fn status_name(status: InitializationStatus) -> &'static str {
    match status {
        InitializationStatus::Unavailable => "unavailable",
        InitializationStatus::InitializationRequired => "initialization-required",
        InitializationStatus::InFlight => "in-flight",
        InitializationStatus::Completed => "completed",
        InitializationStatus::Blocked => "blocked",
    }
}

const fn result_name(result: InitializeResult) -> &'static str {
    match result {
        InitializeResult::Completed => "completed",
        InitializeResult::Retrying => "retrying",
        InitializeResult::PhysicalPresenceRequired => "physical-presence-required",
        InitializeResult::Refused => "refused",
        InitializeResult::Blocked => "blocked",
        InitializeResult::Unavailable => "unavailable",
    }
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut port = None;
    let mut command = None;
    let mut sequence = 0_u64;
    let mut timeout_ms = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--port" => {
                index += 1;
                port = Some(
                    args.get(index)
                        .ok_or_else(|| "--port requires a path".to_owned())?
                        .clone(),
                );
            }
            "--sequence" => {
                index += 1;
                sequence = parse_u64(args.get(index), "--sequence")?;
            }
            "--timeout-ms" => {
                index += 1;
                let parsed = parse_u64(args.get(index), "--timeout-ms")?;
                if parsed == 0 {
                    return Err("--timeout-ms must be nonzero".to_owned());
                }
                timeout_ms = Some(parsed);
            }
            "status" if command.is_none() => command = Some(Command::Status),
            "initialize" if command.is_none() => command = Some(Command::Initialize),
            unknown => return Err(format!("unexpected argument {unknown:?}")),
        }
        index += 1;
    }
    let command = command.ok_or_else(|| "status or initialize is required".to_owned())?;
    if sequence == u64::MAX {
        return Err(format!(
            "--sequence must be less than {}; the firmware refuses the maximum and exhausts the epoch",
            u64::MAX
        ));
    }
    let timeout_ms = timeout_ms.unwrap_or(match command {
        Command::Status => DEFAULT_STATUS_TIMEOUT_MS,
        Command::Initialize => DEFAULT_INITIALIZE_TIMEOUT_MS,
    });
    Ok(Options {
        port: port.ok_or_else(|| "--port is required".to_owned())?,
        command,
        sequence,
        timeout: Duration::from_millis(timeout_ms),
    })
}

fn parse_u64(value: Option<&String>, flag: &str) -> Result<u64, String> {
    value
        .ok_or_else(|| format!("{flag} requires an integer"))?
        .parse()
        .map_err(|_| format!("{flag} requires an unsigned 64-bit integer"))
}

fn usage() {
    eprintln!(
        "usage: cargo run -p xtask -- e290-pairing-control \
         --port <serial-path> [--sequence <u64>] [--timeout-ms <u64>] \
         <status|initialize>"
    );
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn parser_defaults_status_to_sequence_zero_and_bounded_timeout() {
        let parsed = parse(&strings(&["--port", "/dev/test", "status"])).unwrap();
        assert_eq!(parsed.port, "/dev/test");
        assert_eq!(parsed.command, Command::Status);
        assert_eq!(parsed.sequence, 0);
        assert_eq!(
            parsed.timeout,
            Duration::from_millis(DEFAULT_STATUS_TIMEOUT_MS)
        );
    }

    #[test]
    fn parser_gives_initialize_enough_time_for_physical_presence_by_default() {
        let parsed = parse(&strings(&["--port", "/dev/test", "initialize"])).unwrap();
        assert_eq!(parsed.command, Command::Initialize);
        assert_eq!(
            parsed.timeout,
            Duration::from_millis(DEFAULT_INITIALIZE_TIMEOUT_MS)
        );
        assert!(parsed.timeout >= Duration::from_millis(120_000));
    }

    #[test]
    fn parser_accepts_explicit_initialize_options_in_any_order() {
        let parsed = parse(&strings(&[
            "initialize",
            "--timeout-ms",
            "9000",
            "--sequence",
            "7",
            "--port",
            "/dev/test",
        ]))
        .unwrap();
        assert_eq!(parsed.command, Command::Initialize);
        assert_eq!(parsed.sequence, 7);
        assert_eq!(parsed.timeout, Duration::from_millis(9_000));
    }

    #[test]
    fn parser_rejects_missing_or_ambiguous_commands() {
        assert!(parse(&strings(&["status"])).is_err());
        assert!(parse(&strings(&["--port", "/dev/test"])).is_err());
        assert!(parse(&strings(&["--port", "/dev/test", "status", "initialize"])).is_err());
        assert!(
            parse(&strings(&[
                "--port",
                "/dev/test",
                "--timeout-ms",
                "0",
                "status"
            ]))
            .is_err()
        );
        let maximum = u64::MAX.to_string();
        assert!(
            parse(&strings(&[
                "--port",
                "/dev/test",
                "--sequence",
                &maximum,
                "status"
            ]))
            .is_err()
        );
    }

    #[test]
    fn stable_public_names_cover_every_wire_code() {
        assert_eq!(status_name(InitializationStatus::Blocked), "blocked");
        assert_eq!(
            result_name(InitializeResult::PhysicalPresenceRequired),
            "physical-presence-required"
        );
    }

    #[test]
    fn initialize_workflow_advances_one_sequence_across_presence_and_retry_polling() {
        let mut scripted = VecDeque::from([
            (
                ControlRequest::status(10),
                ControlResponse::status(10, InitializationStatus::InitializationRequired),
            ),
            (
                ControlRequest::initialize(11),
                ControlResponse::initialize(11, InitializeResult::PhysicalPresenceRequired),
            ),
            (
                ControlRequest::status(12),
                ControlResponse::status(12, InitializationStatus::InitializationRequired),
            ),
            (
                ControlRequest::initialize(13),
                ControlResponse::initialize(13, InitializeResult::Retrying),
            ),
            (
                ControlRequest::status(14),
                ControlResponse::status(14, InitializationStatus::InFlight),
            ),
            (
                ControlRequest::status(15),
                ControlResponse::status(15, InitializationStatus::Completed),
            ),
        ]);
        let mut waits = Vec::new();
        let response = initialize_workflow_with(
            10,
            |request| {
                let (expected, response) = scripted.pop_front().expect("unexpected request");
                assert_eq!(request, expected);
                Ok(response)
            },
            |last_sent_sequence| {
                waits.push(last_sent_sequence);
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(
            response,
            ControlResponse::status(15, InitializationStatus::Completed)
        );
        assert_eq!(waits, [11, 13, 14]);
        assert!(scripted.is_empty());
    }

    #[test]
    fn maximum_sequence_is_refused_and_maximum_minus_one_has_no_usable_successor() {
        assert_eq!(next_usable_sequence(u64::MAX - 2), Some(u64::MAX - 1));
        assert_eq!(next_usable_sequence(u64::MAX - 1), None);
        assert_eq!(next_usable_sequence(u64::MAX), None);
        assert!(require_next_sequence(u64::MAX - 1).is_err());

        let maximum_minus_one = (u64::MAX - 1).to_string();
        let parsed = parse(&strings(&[
            "--port",
            "/dev/test",
            "--sequence",
            &maximum_minus_one,
            "status",
        ]))
        .unwrap();
        assert_eq!(parsed.sequence, u64::MAX - 1);
    }

    #[test]
    fn terminal_maximum_minus_one_response_succeeds_but_reports_exhaustion() {
        let response = initialize_workflow_with(
            u64::MAX - 1,
            |request| {
                assert_eq!(request, ControlRequest::status(u64::MAX - 1));
                Ok(ControlResponse::status(
                    u64::MAX - 1,
                    InitializationStatus::Completed,
                ))
            },
            |_| panic!("terminal status must not wait"),
        )
        .unwrap();
        let result = TransactionResult {
            command: Command::Initialize,
            response,
            next_sequence: next_usable_sequence(response.sequence()),
        };
        assert!(result.succeeded());
        assert!(format_response(&result).contains("next_sequence=exhausted"));
    }

    #[test]
    fn workflow_fails_before_sending_firmware_refused_maximum_sequence() {
        let mut requests = 0;
        let error = initialize_workflow_with(
            u64::MAX - 1,
            |request| {
                requests += 1;
                assert_eq!(request, ControlRequest::status(u64::MAX - 1));
                Ok(ControlResponse::status(
                    u64::MAX - 1,
                    InitializationStatus::InitializationRequired,
                ))
            },
            |_| Ok(()),
        )
        .unwrap_err();
        assert_eq!(requests, 1);
        assert!(error.contains("firmware refuses"));
    }

    #[test]
    fn terminal_failures_produce_failure_while_completed_initialize_succeeds() {
        for response in [
            ControlResponse::status(0, InitializationStatus::Blocked),
            ControlResponse::status(0, InitializationStatus::Unavailable),
            ControlResponse::initialize(0, InitializeResult::Refused),
            ControlResponse::initialize(0, InitializeResult::Blocked),
            ControlResponse::initialize(0, InitializeResult::Unavailable),
        ] {
            let result = TransactionResult {
                command: Command::Initialize,
                response,
                next_sequence: Some(1),
            };
            assert!(!result.succeeded());
        }

        let completed = TransactionResult {
            command: Command::Initialize,
            response: ControlResponse::status(7, InitializationStatus::Completed),
            next_sequence: Some(8),
        };
        assert!(completed.succeeded());
        assert_eq!(
            format_response(&completed),
            "command=initialize response=status sequence=7 outcome=completed code=3 next_sequence=8"
        );
    }

    #[test]
    fn timeout_guidance_names_sequence_ambiguity_and_required_reset_boundary() {
        let ambiguous = sequence_ambiguity_guidance(42);
        assert!(ambiguous.contains("last_sent_sequence=42"));
        assert!(ambiguous.contains("consumed-or-ambiguous"));
        assert!(ambiguous.contains("USB bus reset"));
        assert!(ambiguous.contains("opening or closing"));

        let consumed = deadline_message(43, true);
        assert!(consumed.contains("last_sent_sequence=43"));
        assert!(consumed.contains("was consumed"));
        assert!(consumed.contains("USB bus reset"));
    }

    #[test]
    fn every_post_send_io_failure_reports_sequence_ambiguity() {
        let error = io::Error::new(io::ErrorKind::BrokenPipe, "fixture");
        for operation in ["request write", "request flush", "response read"] {
            let message = post_send_failure(operation, "/dev/test", &error, 41);
            assert!(message.contains("last_sent_sequence=41 is consumed-or-ambiguous"));
            assert!(message.contains("confirm a firmware/USB bus reset"));
        }
    }
}
