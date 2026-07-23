//! CLI adapter for reusable pre-auth status and initialization control.

use std::{process::ExitCode, time::Duration};

use reticulum_device_pairing_client::{
    InitializationResponse, InitializationState, InitializationSummary, PairingClient,
    PairingProgress, PresenceOperation,
};

const DEFAULT_STATUS_TIMEOUT_MS: u64 = 15_000;
const DEFAULT_INITIALIZE_TIMEOUT_MS: u64 = 120_000;

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
        Ok(summary) => {
            println!("{}", format_response(options.command, summary));
            if succeeded(options.command, summary) {
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

fn transact(options: &Options) -> Result<InitializationSummary, String> {
    let client = PairingClient::new(&options.port)
        .with_sequence(options.sequence)
        .with_timeout(options.timeout);
    let mut observer = cli_progress;
    let mut session = client
        .connect(&mut observer)
        .map_err(|error| error.to_string())?;
    match options.command {
        Command::Status => session
            .initialization_status(&mut observer)
            .map_err(|error| error.to_string()),
        Command::Initialize => session
            .ensure_initialized(&mut observer)
            .map_err(|error| error.to_string()),
    }
}

fn cli_progress(progress: PairingProgress) {
    if progress == PairingProgress::WaitingForPhysicalPresence(PresenceOperation::Initialize) {
        eprintln!(
            "waiting for GPIO21 physical presence: release once, then hold for at least 2 seconds"
        );
    }
}

const fn succeeded(command: Command, summary: InitializationSummary) -> bool {
    match (command, summary.response()) {
        (
            Command::Status,
            InitializationResponse::Status(
                InitializationState::Required
                | InitializationState::InFlight
                | InitializationState::Completed,
            ),
        ) => true,
        (Command::Initialize, _) => summary.is_completed(),
        _ => false,
    }
}

fn format_response(command: Command, summary: InitializationSummary) -> String {
    let next = summary
        .next_sequence()
        .map_or_else(|| "exhausted".to_owned(), |value| value.to_string());
    match (command, summary.response()) {
        (Command::Status, InitializationResponse::Status(status)) => format!(
            "command=status sequence={} status={} code={} next_sequence={next}",
            summary.sequence(),
            status.name(),
            status.code()
        ),
        (Command::Initialize, InitializationResponse::Status(status)) => format!(
            "command=initialize response=status sequence={} outcome={} code={} next_sequence={next}",
            summary.sequence(),
            status.name(),
            status.code()
        ),
        (Command::Initialize, InitializationResponse::Initialize(outcome)) => format!(
            "command=initialize response=initialize sequence={} outcome={} code={} next_sequence={next}",
            summary.sequence(),
            outcome.name(),
            outcome.code()
        ),
        (Command::Status, InitializationResponse::Initialize(_)) => format!(
            "command=status response=unexpected-initialize sequence={} next_sequence={next}",
            summary.sequence()
        ),
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
    use reticulum_device_pairing_client::{InitializationResponse, InitializeOutcome};

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
    fn parser_rejects_missing_ambiguous_or_exhausted_commands() {
        assert!(parse(&strings(&["status"])).is_err());
        assert!(parse(&strings(&["--port", "/dev/test"])).is_err());
        assert!(parse(&strings(&["--port", "/dev/test", "status", "initialize"])).is_err());
        assert!(
            parse(&strings(&[
                "--port",
                "/dev/test",
                "--timeout-ms",
                "0",
                "status",
            ]))
            .is_err()
        );
        assert!(
            parse(&strings(&[
                "--port",
                "/dev/test",
                "--sequence",
                "18446744073709551615",
                "status",
            ]))
            .is_err()
        );
    }

    #[test]
    fn terminal_formatting_and_success_preserve_cli_contract() {
        let completed = InitializationSummary::from_response(
            7,
            InitializationResponse::Status(InitializationState::Completed),
        );
        assert!(succeeded(Command::Initialize, completed));
        assert_eq!(
            format_response(Command::Initialize, completed),
            "command=initialize response=status sequence=7 outcome=completed code=3 next_sequence=8"
        );

        let refused = InitializationSummary::from_response(
            9,
            InitializationResponse::Initialize(InitializeOutcome::Refused),
        );
        assert!(!succeeded(Command::Initialize, refused));
    }
}
