//! CLI adapter for the reusable resident live-pairing client.

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

use reticulum_device_pairing_client::{
    AbortSummary, ActivationSummary, DEFAULT_PAIRING_TIMEOUT, PairingClient, PairingProgress,
    PresenceOperation,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Command {
    Pair,
    Resume,
    AbortCurrent,
}

struct Options {
    port: String,
    command: Command,
    state_file: Option<PathBuf>,
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
        Ok((succeeded, line)) => {
            println!("{line}");
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

fn transact(options: &Options) -> Result<(bool, String), String> {
    let client = PairingClient::new(&options.port)
        .with_sequence(options.sequence)
        .with_timeout(options.timeout);
    let mut observer = cli_progress;
    let mut session = client
        .connect(&mut observer)
        .map_err(|error| error.to_string())?;
    match options.command {
        Command::Pair => {
            let state_path = options
                .state_file
                .as_deref()
                .expect("parser requires a state file for pair");
            let summary = session
                .pair(state_path, &mut observer)
                .map_err(|error| error.to_string())?;
            Ok((true, format_activation("pair", state_path, summary)))
        }
        Command::Resume => {
            let state_path = options
                .state_file
                .as_deref()
                .expect("parser requires a state file for resume");
            let summary = session
                .resume(state_path, &mut observer)
                .map_err(|error| error.to_string())?;
            Ok((true, format_activation("resume", state_path, summary)))
        }
        Command::AbortCurrent => {
            let summary = session
                .abort_current(&mut observer)
                .map_err(|error| error.to_string())?;
            Ok((summary.outcome().is_aborted(), format_abort(summary)))
        }
    }
}

fn cli_progress(progress: PairingProgress) {
    match progress {
        PairingProgress::WaitingForPhysicalPresence(PresenceOperation::Begin) => {
            eprintln!(
                "waiting for GPIO21 physical presence: release once, then hold for at least 2 seconds"
            );
        }
        PairingProgress::WaitingForPhysicalPresence(PresenceOperation::ProofStart) => {
            eprintln!(
                "waiting for GPIO21 physical presence for ProofStart: release once, then hold for at least 2 seconds"
            );
        }
        PairingProgress::WaitingForPhysicalPresence(PresenceOperation::AbortCurrent) => {
            eprintln!(
                "waiting for GPIO21 physical presence to abort Pending state: release once, then hold for at least 2 seconds"
            );
        }
        PairingProgress::WaitingForPhysicalPresence(PresenceOperation::Initialize)
        | PairingProgress::OpeningPort
        | PairingProgress::PortReady
        | PairingProgress::CheckingInitialization
        | PairingProgress::Initializing
        | PairingProgress::Initialized
        | PairingProgress::PendingPersisted
        | PairingProgress::ProofChallengeAccepted
        | PairingProgress::ActivationPrepared
        | PairingProgress::Activated
        | PairingProgress::Aborted => {}
    }
}

fn format_activation(command: &str, state_path: &Path, summary: ActivationSummary) -> String {
    let next = summary
        .next_sequence()
        .map_or_else(|| "exhausted".to_owned(), |value| value.to_string());
    format!(
        "command={command} outcome=activated sequence={} device_id={} credential_id={} generation={} state_file={} next_sequence={next}",
        summary.sequence(),
        hex(&summary.device_id()),
        hex(&summary.credential_id()),
        summary.generation(),
        state_path.display()
    )
}

fn format_abort(summary: AbortSummary) -> String {
    let next = summary
        .next_sequence()
        .map_or_else(|| "exhausted".to_owned(), |value| value.to_string());
    format!(
        "command=abort-current sequence={} outcome={} code={} next_sequence={next}",
        summary.sequence(),
        summary.outcome().name(),
        summary.outcome().code()
    )
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut port = None;
    let mut command = None;
    let mut state_file = None;
    let mut sequence = 0_u64;
    let mut timeout_ms = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--port" => {
                index += 1;
                port = Some(required_value(args.get(index), "--port")?.to_owned());
            }
            "--state-file" => {
                index += 1;
                state_file = Some(PathBuf::from(required_value(
                    args.get(index),
                    "--state-file",
                )?));
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
            "pair" if command.is_none() => command = Some(Command::Pair),
            "resume" if command.is_none() => command = Some(Command::Resume),
            "abort-current" if command.is_none() => command = Some(Command::AbortCurrent),
            unknown => return Err(format!("unexpected argument {unknown:?}")),
        }
        index += 1;
    }
    let command = command.ok_or_else(|| "pair, resume, or abort-current is required".to_owned())?;
    if sequence == u64::MAX {
        return Err(format!(
            "--sequence must be less than {}; the firmware refuses the maximum and exhausts the epoch",
            u64::MAX
        ));
    }
    match (command, state_file.as_ref()) {
        (Command::Pair, None) => return Err("pair requires --state-file".to_owned()),
        (Command::Resume, None) => return Err("resume requires --state-file".to_owned()),
        (Command::AbortCurrent, Some(_)) => {
            return Err("abort-current does not accept --state-file".to_owned());
        }
        _ => {}
    }
    Ok(Options {
        port: port.ok_or_else(|| "--port is required".to_owned())?,
        command,
        state_file,
        sequence,
        timeout: Duration::from_millis(
            timeout_ms.unwrap_or(DEFAULT_PAIRING_TIMEOUT.as_millis() as u64),
        ),
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

fn usage() {
    eprintln!(
        "usage:\n  cargo run -p xtask -- e290-pairing-live --port <serial-path> \
         [--sequence <u64>] [--timeout-ms <u64>] --state-file <new-secret-path> pair\n  \
         cargo run -p xtask -- e290-pairing-live --port <serial-path> \
         [--sequence <u64>] [--timeout-ms <u64>] --state-file <pending-secret-path> resume\n  \
         cargo run -p xtask -- e290-pairing-live --port <serial-path> \
         [--sequence <u64>] [--timeout-ms <u64>] abort-current"
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
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn parser_accepts_pairing_options_in_any_order() {
        let parsed = parse(&strings(&[
            "pair",
            "--timeout-ms",
            "9000",
            "--state-file",
            "/tmp/device.key",
            "--sequence",
            "7",
            "--port",
            "/dev/test",
        ]))
        .unwrap();
        assert_eq!(parsed.command, Command::Pair);
        assert_eq!(parsed.port, "/dev/test");
        assert_eq!(parsed.sequence, 7);
        assert_eq!(parsed.timeout, Duration::from_millis(9000));
        assert_eq!(parsed.state_file, Some(PathBuf::from("/tmp/device.key")));
    }

    #[test]
    fn parser_accepts_resume_with_existing_state_path() {
        let parsed = parse(&strings(&[
            "--state-file",
            "/tmp/device.key",
            "resume",
            "--sequence",
            "11",
            "--port",
            "/dev/test",
        ]))
        .unwrap();
        assert_eq!(parsed.command, Command::Resume);
        assert_eq!(parsed.state_file, Some(PathBuf::from("/tmp/device.key")));
        assert_eq!(parsed.sequence, 11);
    }

    #[test]
    fn parser_requires_state_path_for_pair_and_resume() {
        assert_eq!(
            parse(&strings(&["--port", "/dev/test", "pair"]))
                .err()
                .unwrap(),
            "pair requires --state-file"
        );
        assert_eq!(
            parse(&strings(&["--port", "/dev/test", "resume"]))
                .err()
                .unwrap(),
            "resume requires --state-file"
        );
    }

    #[test]
    fn parser_keeps_abort_identifier_free() {
        let parsed = parse(&strings(&["--port", "/dev/test", "abort-current"])).unwrap();
        assert_eq!(parsed.command, Command::AbortCurrent);
        assert!(parsed.state_file.is_none());
        assert_eq!(parsed.sequence, 0);
        assert_eq!(parsed.timeout, DEFAULT_PAIRING_TIMEOUT);
    }

    #[test]
    fn parser_rejects_state_path_for_abort() {
        assert_eq!(
            parse(&strings(&[
                "--port",
                "/dev/test",
                "--state-file",
                "/tmp/device.key",
                "abort-current",
            ]))
            .err()
            .unwrap(),
            "abort-current does not accept --state-file"
        );
    }

    #[test]
    fn parser_rejects_maximum_sequence_and_zero_timeout() {
        assert!(
            parse(&strings(&[
                "--port",
                "/dev/test",
                "--state-file",
                "/tmp/device.key",
                "--sequence",
                "18446744073709551615",
                "pair",
            ]))
            .is_err()
        );
        assert!(
            parse(&strings(&[
                "--port",
                "/dev/test",
                "--timeout-ms",
                "0",
                "abort-current",
            ]))
            .is_err()
        );
    }
}
