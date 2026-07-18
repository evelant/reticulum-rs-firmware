//! Host client for the E290 resident live-pairing lifecycle.

use std::{
    ffi::OsString,
    fmt::Write as _,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use reticulum_device_api_framing::{DecodeEvent, FramedRecord, StreamDecoder};
use reticulum_device_api_pairing::{
    AbortCurrentRequest, AbortResult, ActivateRequest, ActivateResponse, BeginOffer, BeginRequest,
    BeginResponse, ClientProof, CredentialGeneration, CredentialId, DeviceId, PairingFailure,
    PairingPsk, PairingRequest, PairingResponse, PairingTranscript, ProofChallenge,
    ProofStartRequest, ProofStartResponse,
};
use serialport::ClearBuffer;
use zeroize::Zeroizing;

const BAUD_RATE: u32 = 115_200;
const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const OPEN_SETTLE_MS: u64 = 250;
const READ_SLICE_MS: u64 = 100;
const PRESENCE_POLL_MS: u64 = 200;

const STATE_MAGIC: [u8; 8] = *b"RDPKEY1\0";
const STATE_FORMAT_VERSION: u16 = 1;
const STATE_RESERVED: u8 = 0;
const STATE_PENDING: u8 = 1;
const STATE_ACTIVE: u8 = 2;
const STATE_STATUS_OFFSET: u64 = 10;
const STATE_FILE_LENGTH: usize = 96;
const STAGING_ATTEMPTS: u64 = 16;

static NEXT_STAGING_FILE: AtomicU64 = AtomicU64::new(0);

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

#[derive(Clone, Copy)]
enum ExpectedResponse {
    Begin,
    ProofStart,
    Activate,
    AbortCurrent,
}

struct CommandResult {
    succeeded: bool,
    line: String,
}

struct PendingStateFile {
    file: File,
    path: PathBuf,
}

struct ReservedStateFile {
    file: File,
    path: PathBuf,
    staging_file: File,
    staging_path: PathBuf,
}

struct PersistedPendingState {
    file: PendingStateFile,
    device_id: DeviceId,
    credential_id: CredentialId,
    generation: CredentialGeneration,
    psk: PairingPsk,
}

pub(crate) struct ActivatedCredential {
    device_id: DeviceId,
    credential_id: CredentialId,
    generation: CredentialGeneration,
    psk: Zeroizing<[u8; 32]>,
}

impl ActivatedCredential {
    pub(crate) fn into_parts(
        self,
    ) -> (
        DeviceId,
        CredentialId,
        CredentialGeneration,
        Zeroizing<[u8; 32]>,
    ) {
        (
            self.device_id,
            self.credential_id,
            self.generation,
            self.psk,
        )
    }
}

struct BeginWorkflowError {
    message: String,
    pending_may_exist: bool,
}

struct ExchangeError {
    message: String,
    request_was_or_may_have_been_accepted: bool,
}

struct SecureCreateError {
    message: String,
    already_exists: bool,
}

impl ExchangeError {
    fn before_send(message: String) -> Self {
        Self {
            message,
            request_was_or_may_have_been_accepted: false,
        }
    }

    fn after_send(message: String) -> Self {
        Self {
            message,
            request_was_or_may_have_been_accepted: true,
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
        Ok(result) => {
            println!("{}", result.line);
            if result.succeeded {
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

fn transact(options: &Options) -> Result<CommandResult, String> {
    match options.command {
        Command::Pair => {
            ensure_secure_persistence_host()?;
            ensure_pair_headroom(options.sequence)?;
        }
        Command::Resume => {
            ensure_secure_persistence_host()?;
            ensure_resume_headroom(options.sequence)?;
        }
        Command::AbortCurrent => {}
    }
    let mut port = serialport::new(&options.port, BAUD_RATE)
        .timeout(Duration::from_millis(READ_SLICE_MS))
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
    let mut decoder = StreamDecoder::new();
    match options.command {
        Command::Pair => pair_workflow(
            &mut *port,
            &mut decoder,
            options,
            deadline,
            options
                .state_file
                .as_deref()
                .expect("parser requires a state file for pairing"),
        ),
        Command::Resume => resume_workflow(
            &mut *port,
            &mut decoder,
            options,
            deadline,
            options
                .state_file
                .as_deref()
                .expect("parser requires a state file for resume"),
        ),
        Command::AbortCurrent => abort_workflow(
            &mut *port,
            &mut decoder,
            options.sequence,
            deadline,
            &options.port,
        ),
    }
}

fn pair_workflow(
    port: &mut dyn serialport::SerialPort,
    decoder: &mut StreamDecoder,
    options: &Options,
    deadline: Instant,
    state_path: &Path,
) -> Result<CommandResult, String> {
    let client_nonce = random_nonzero_nonce()?;
    let reservation = ReservedStateFile::reserve(state_path)?;
    let (begin_sequence, offer) = match begin_until_offered(
        port,
        decoder,
        options.sequence,
        deadline,
        &options.port,
    ) {
        Ok(offered) => offered,
        Err(error) if error.pending_may_exist => {
            let cleanup = match reservation.retain_ambiguous_begin_marker() {
                Ok(()) => String::new(),
                Err(cleanup) => format!("; staging cleanup failed: {cleanup}"),
            };
            return Err(format!(
                "{}; an owner-only reserved state marker remains at {} because a lost Begin offer may have committed Pending state{cleanup}; use a fresh confirmed USB epoch and physically confirmed abort-current before removing it",
                error.message,
                state_path.display()
            ));
        }
        Err(error) => {
            reservation.discard().map_err(|cleanup| {
                format!(
                    "{}; no Pending offer was observed, but reserved state cleanup failed: {cleanup}",
                    error.message
                )
            })?;
            return Err(error.message);
        }
    };
    let (device_id, credential_id, generation, psk) = offer.into_parts();
    let state = reservation
        .commit_pending(device_id, credential_id, generation, &psk)
        .map_err(|error| {
            format!(
                "Pending credential was durably offered at Begin sequence {begin_sequence}, but host persistence did not complete cleanly: {error}; {}; keep every state artifact and assess it: resume only from a canonical complete Pending file, otherwise run physically confirmed abort-current before removing artifacts",
                next_sequence_guidance(begin_sequence)
            )
        })?;

    let proof_sequence = require_next_sequence(begin_sequence)?;
    complete_pairing(
        port,
        decoder,
        deadline,
        &options.port,
        state,
        device_id,
        credential_id,
        generation,
        psk,
        client_nonce,
        proof_sequence,
        false,
    )
}

fn resume_workflow(
    port: &mut dyn serialport::SerialPort,
    decoder: &mut StreamDecoder,
    options: &Options,
    deadline: Instant,
    state_path: &Path,
) -> Result<CommandResult, String> {
    ensure_resume_headroom(options.sequence)?;
    let persisted = PendingStateFile::open_pending(state_path)?;
    let client_nonce = random_nonzero_nonce()?;
    complete_pairing(
        port,
        decoder,
        deadline,
        &options.port,
        persisted.file,
        persisted.device_id,
        persisted.credential_id,
        persisted.generation,
        persisted.psk,
        client_nonce,
        options.sequence,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn complete_pairing(
    port: &mut dyn serialport::SerialPort,
    decoder: &mut StreamDecoder,
    deadline: Instant,
    port_name: &str,
    state: PendingStateFile,
    device_id: DeviceId,
    credential_id: CredentialId,
    generation: CredentialGeneration,
    psk: PairingPsk,
    client_nonce: [u8; 32],
    proof_sequence: u64,
    resumed: bool,
) -> Result<CommandResult, String> {
    let (proof_sequence, proof_request, challenge) = proof_until_challenged(
        port,
        decoder,
        deadline,
        port_name,
        credential_id,
        generation,
        client_nonce,
        proof_sequence,
        &state.path,
        resumed,
    )?;
    validate_challenge_device(device_id, challenge.device_id()).map_err(|reason| {
        format!(
            "{reason}; Pending state remains at {}; {}",
            state.path.display(),
            next_sequence_guidance(proof_sequence)
        )
    })?;
    let transcript = PairingTranscript::new(&proof_request, &challenge).map_err(|_| {
        format!(
            "device returned a mismatched ProofStart transcript; Pending state remains at {}; {}",
            state.path.display(),
            next_sequence_guidance(proof_sequence)
        )
    })?;

    let activate_sequence = require_next_sequence(proof_sequence)?;
    let client_proof = ClientProof::calculate(&psk, &transcript);
    let activate_request =
        ActivateRequest::new(activate_sequence, credential_id, generation, client_proof).map_err(
            |error| format!("could not construct canonical Activate request: {error:?}"),
        )?;
    let response = exchange(
        port,
        decoder,
        PairingRequest::Activate(activate_request),
        ExpectedResponse::Activate,
        deadline,
        port_name,
    )
    .map_err(|error| {
        if error.request_was_or_may_have_been_accepted {
            format!(
                "{}; Pending host state remains at {}; activation may have committed, and authenticated-session reconciliation is deferred—do not guess Active or abort until that state is assessed",
                error.message,
                state.path.display()
            )
        } else {
            format!(
                "{}; Activate was not sent, and Pending state remains at {}; retry with resume from a confirmed sequence epoch",
                error.message,
                state.path.display()
            )
        }
    })?;
    let activate = match response {
        PairingResponse::Activate(response) => response,
        _ => unreachable!("exchange enforces the response family"),
    };
    let activated_generation = match &activate {
        ActivateResponse::Activated { .. } => {
            if !activate.verify_confirmation(&psk, &transcript) {
                return Err(format!(
                    "device activation confirmation was invalid; Pending state remains at {}; {}",
                    state.path.display(),
                    next_sequence_guidance(activate_sequence)
                ));
            }
            activate
                .generation()
                .expect("the Activated response always carries its durable generation")
        }
        ActivateResponse::Failure { failure, .. } => {
            return Err(format!(
                "Activate failed outcome={} code={}; Pending state remains at {}; {}",
                activate_failure_name(*failure),
                failure.code(),
                state.path.display(),
                next_sequence_guidance(activate_sequence)
            ));
        }
    };

    let state_path = state.path.clone();
    state.mark_active(device_id, credential_id, activated_generation, &psk)?;
    let next = next_usable_sequence(activate_sequence)
        .map_or_else(|| "exhausted".to_owned(), |value| value.to_string());
    Ok(CommandResult {
        succeeded: true,
        line: format!(
            "command={} outcome=activated sequence={activate_sequence} device_id={} \
             credential_id={} generation={} state_file={} next_sequence={next}",
            if resumed { "resume" } else { "pair" },
            hex(device_id.as_bytes()),
            hex(credential_id.as_bytes()),
            activated_generation.get(),
            state_path.display()
        ),
    })
}

#[allow(clippy::too_many_arguments)]
fn proof_until_challenged(
    port: &mut dyn serialport::SerialPort,
    decoder: &mut StreamDecoder,
    deadline: Instant,
    port_name: &str,
    credential_id: CredentialId,
    generation: CredentialGeneration,
    mut client_nonce: [u8; 32],
    mut sequence: u64,
    state_path: &Path,
    resumed: bool,
) -> Result<(u64, ProofStartRequest, ProofChallenge), String> {
    let mut announced_presence = false;
    loop {
        ensure_resume_headroom(sequence).map_err(|error| {
            format!("{error}; Pending state remains at {}", state_path.display())
        })?;
        let proof_request =
            ProofStartRequest::new(sequence, credential_id, generation, client_nonce).map_err(
                |error| format!("could not construct canonical ProofStart request: {error:?}"),
            )?;
        let response = exchange(
            port,
            decoder,
            PairingRequest::ProofStart(proof_request),
            ExpectedResponse::ProofStart,
            deadline,
            port_name,
        )
        .map_err(|error| {
            format!(
                "{}; Pending state remains at {}; retry with resume from a confirmed sequence epoch",
                error.message,
                state_path.display()
            )
        })?;
        match response {
            PairingResponse::ProofStart(ProofStartResponse::Challenge { challenge, .. }) => {
                return Ok((sequence, proof_request, challenge));
            }
            PairingResponse::ProofStart(ProofStartResponse::Failure {
                failure: PairingFailure::PhysicalPresenceRequired,
                ..
            }) => {
                if !announced_presence {
                    eprintln!(
                        "waiting for GPIO21 physical presence for ProofStart: release once, then hold for at least 2 seconds"
                    );
                    announced_presence = true;
                }
                wait_before_next(deadline, sequence, "pairing ProofStart").map_err(|error| {
                    format!("{error}; Pending state remains at {}", state_path.display())
                })?;
                let next = require_next_sequence(sequence).map_err(|error| {
                    format!("{error}; Pending state remains at {}", state_path.display())
                })?;
                ensure_resume_headroom(next).map_err(|error| {
                    format!("{error}; Pending state remains at {}", state_path.display())
                })?;
                client_nonce = random_nonzero_nonce().map_err(|error| {
                    format!(
                        "{error}; Pending state remains at {}; {}",
                        state_path.display(),
                        next_sequence_guidance(sequence)
                    )
                })?;
                sequence = next;
            }
            PairingResponse::ProofStart(ProofStartResponse::Failure { failure, .. }) => {
                let reconciliation = if resumed {
                    " this may represent an already-Active credential after an ambiguous prior Activate; authenticated-session reconciliation is deferred, so do not guess or abort;"
                } else {
                    ""
                };
                return Err(format!(
                    "ProofStart failed outcome={} code={};{reconciliation} Pending state remains at {}; {}",
                    pairing_failure_name(failure),
                    failure.code(),
                    state_path.display(),
                    next_sequence_guidance(sequence)
                ));
            }
            _ => unreachable!("exchange enforces the response family"),
        }
    }
}

fn begin_until_offered(
    port: &mut dyn serialport::SerialPort,
    decoder: &mut StreamDecoder,
    mut sequence: u64,
    deadline: Instant,
    port_name: &str,
) -> Result<(u64, BeginOffer), BeginWorkflowError> {
    let mut announced_presence = false;
    loop {
        ensure_pair_headroom(sequence).map_err(|message| BeginWorkflowError {
            message,
            pending_may_exist: false,
        })?;
        let response = exchange(
            port,
            decoder,
            PairingRequest::Begin(BeginRequest::new(sequence)),
            ExpectedResponse::Begin,
            deadline,
            port_name,
        )
        .map_err(|error| BeginWorkflowError {
            message: error.message,
            pending_may_exist: error.request_was_or_may_have_been_accepted,
        })?;
        match response {
            PairingResponse::Begin(BeginResponse::Offered { offer, .. }) => {
                return Ok((sequence, offer));
            }
            PairingResponse::Begin(BeginResponse::Failure {
                failure: PairingFailure::PhysicalPresenceRequired,
                ..
            }) => {
                if !announced_presence {
                    eprintln!(
                        "waiting for GPIO21 physical presence: release once, then hold for at least 2 seconds"
                    );
                    announced_presence = true;
                }
                wait_before_next(deadline, sequence, "pairing Begin").map_err(|message| {
                    BeginWorkflowError {
                        message,
                        pending_may_exist: false,
                    }
                })?;
                sequence =
                    require_next_sequence(sequence).map_err(|message| BeginWorkflowError {
                        message,
                        pending_may_exist: false,
                    })?;
            }
            PairingResponse::Begin(BeginResponse::Failure { failure, .. }) => {
                let next = next_usable_sequence(sequence)
                    .map_or_else(|| "exhausted".to_owned(), |value| value.to_string());
                return Err(BeginWorkflowError {
                    message: format!(
                        "Begin failed outcome={} code={} next_sequence={next}",
                        pairing_failure_name(failure),
                        failure.code()
                    ),
                    pending_may_exist: false,
                });
            }
            _ => unreachable!("exchange enforces the response family"),
        }
    }
}

fn abort_workflow(
    port: &mut dyn serialport::SerialPort,
    decoder: &mut StreamDecoder,
    mut sequence: u64,
    deadline: Instant,
    port_name: &str,
) -> Result<CommandResult, String> {
    let mut announced_presence = false;
    loop {
        let response = exchange(
            port,
            decoder,
            PairingRequest::AbortCurrent(AbortCurrentRequest::new(sequence)),
            ExpectedResponse::AbortCurrent,
            deadline,
            port_name,
        )
        .map_err(|error| error.message)?;
        let PairingResponse::AbortCurrent(response) = response else {
            unreachable!("exchange enforces the response family");
        };
        let result = response.result();
        if result == AbortResult::PhysicalPresenceRequired {
            if !announced_presence {
                eprintln!(
                    "waiting for GPIO21 physical presence to abort Pending state: release once, then hold for at least 2 seconds"
                );
                announced_presence = true;
            }
            wait_before_next(deadline, sequence, "AbortCurrent")?;
            sequence = require_next_sequence(sequence)?;
            continue;
        }
        let next = next_usable_sequence(sequence)
            .map_or_else(|| "exhausted".to_owned(), |value| value.to_string());
        return Ok(CommandResult {
            succeeded: result == AbortResult::Aborted,
            line: format!(
                "command=abort-current sequence={sequence} outcome={} code={} next_sequence={next}",
                abort_result_name(result),
                result.code()
            ),
        });
    }
}

fn exchange(
    port: &mut dyn serialport::SerialPort,
    decoder: &mut StreamDecoder,
    request: PairingRequest,
    expected: ExpectedResponse,
    deadline: Instant,
    port_name: &str,
) -> Result<PairingResponse, ExchangeError> {
    let sequence = request.sequence();
    if Instant::now() >= deadline {
        return Err(ExchangeError::before_send(deadline_message(
            sequence,
            false,
            "live-pairing workflow",
        )));
    }
    let frame = FramedRecord::encode(&request.into_record()).map_err(|_| {
        ExchangeError::before_send("canonical request did not fit its fixed frame owner".to_owned())
    })?;
    port.write_all(frame.encoded()).map_err(|error| {
        ExchangeError::after_send(post_send_failure(
            "request write",
            port_name,
            &error,
            sequence,
        ))
    })?;
    port.flush().map_err(|error| {
        ExchangeError::after_send(post_send_failure(
            "request flush",
            port_name,
            &error,
            sequence,
        ))
    })?;

    let mut bytes = Zeroizing::new([0_u8; 256]);
    while Instant::now() < deadline {
        match port.read(&mut bytes[..]) {
            Ok(0) => {}
            Ok(length) => {
                for byte in &bytes[..length] {
                    let DecodeEvent::Record(record) = decoder.push(*byte) else {
                        continue;
                    };
                    let Ok(response) = PairingResponse::from_record(record) else {
                        continue;
                    };
                    if response.sequence() != sequence {
                        continue;
                    }
                    if response_matches(expected, &response) {
                        return Ok(response);
                    }
                    return Err(ExchangeError::after_send(format!(
                        "device returned the wrong live-pairing response family for sequence {sequence}"
                    )));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
            Err(error) => {
                return Err(ExchangeError::after_send(post_send_failure(
                    "response read",
                    port_name,
                    &error,
                    sequence,
                )));
            }
        }
    }
    Err(ExchangeError::after_send(format!(
        "timed out waiting for sequence {sequence} on {port_name}; {}",
        sequence_ambiguity_guidance(sequence)
    )))
}

const fn response_matches(expected: ExpectedResponse, response: &PairingResponse) -> bool {
    matches!(
        (expected, response),
        (ExpectedResponse::Begin, PairingResponse::Begin(_))
            | (ExpectedResponse::ProofStart, PairingResponse::ProofStart(_))
            | (ExpectedResponse::Activate, PairingResponse::Activate(_))
            | (
                ExpectedResponse::AbortCurrent,
                PairingResponse::AbortCurrent(_)
            )
    )
}

fn validate_challenge_device(expected: DeviceId, observed: DeviceId) -> Result<(), &'static str> {
    if expected == observed {
        Ok(())
    } else {
        Err("device returned a ProofStart challenge for a different device ID")
    }
}

fn random_nonzero_nonce() -> Result<[u8; 32], String> {
    loop {
        let mut nonce = [0_u8; 32];
        getrandom::fill(&mut nonce)
            .map_err(|error| format!("operating-system randomness failed: {error}"))?;
        if nonce.iter().any(|byte| *byte != 0) {
            return Ok(nonce);
        }
    }
}

impl ReservedStateFile {
    fn reserve(path: &Path) -> Result<Self, String> {
        ensure_secure_persistence_host()?;
        let mut file =
            secure_create_new(path, "reserved state file").map_err(|error| error.message)?;
        let bytes = encode_state(STATE_RESERVED, None);
        if let Err(error) = write_sync_and_verify(&mut file, path, &bytes) {
            drop(file);
            return Err(clean_pre_begin_file(path, error));
        }
        let (staging_file, staging_path) = match create_staging_file(path) {
            Ok(staging) => staging,
            Err(error) => {
                drop(file);
                return Err(clean_pre_begin_file(path, error));
            }
        };
        let mut reservation = Self {
            file,
            path: path.to_path_buf(),
            staging_file,
            staging_path,
        };
        let prepared = write_sync_and_verify(
            &mut reservation.staging_file,
            &reservation.staging_path,
            &bytes,
        )
        .and_then(|()| sync_parent(path));
        match prepared {
            Ok(()) => Ok(reservation),
            Err(error) => match reservation.discard() {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!(
                    "{error}; pre-Begin reservation cleanup also failed: {cleanup}"
                )),
            },
        }
    }

    fn discard(self) -> Result<(), String> {
        let Self {
            file,
            path,
            staging_file,
            staging_path,
        } = self;
        drop(file);
        drop(staging_file);
        let mut errors = Vec::new();
        if let Err(error) = fs::remove_file(&staging_path) {
            errors.push(format!(
                "could not remove staging reservation {}: {error}",
                staging_path.display()
            ));
        }
        if let Err(error) = fs::remove_file(&path) {
            errors.push(format!(
                "could not remove reserved state {}: {error}",
                path.display()
            ));
        }
        if let Err(error) = sync_parent(&path) {
            errors.push(error);
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    fn retain_ambiguous_begin_marker(self) -> Result<(), String> {
        let Self {
            file,
            path,
            staging_file,
            staging_path,
        } = self;
        drop(file);
        drop(staging_file);
        fs::remove_file(&staging_path).map_err(|error| {
            format!(
                "could not remove non-secret staging reservation {}: {error}",
                staging_path.display()
            )
        })?;
        sync_parent(&path)
    }

    fn commit_pending(
        self,
        device_id: DeviceId,
        credential_id: CredentialId,
        generation: CredentialGeneration,
        psk: &PairingPsk,
    ) -> Result<PendingStateFile, String> {
        let Self {
            file: reservation,
            path,
            mut staging_file,
            staging_path,
        } = self;
        let bytes = encode_state(
            STATE_PENDING,
            Some((device_id, credential_id, generation, psk)),
        );
        write_sync_and_verify(&mut staging_file, &staging_path, &bytes).map_err(|error| {
            format!(
                "{error}; owner-only staging file {} may contain partial secret material",
                staging_path.display()
            )
        })?;
        ensure_path_matches_file(&reservation, &path, "reserved state")?;
        drop(staging_file);
        fs::rename(&staging_path, &path).map_err(|error| {
            format!(
                "could not atomically replace reserved state {} with complete staging file {}: {error}",
                path.display(),
                staging_path.display()
            )
        })?;
        drop(reservation);
        sync_parent(&path).map_err(|error| {
            format!(
                "a complete Pending file may already exist at {} after atomic rename, but its directory entry could not be synchronized: {error}; keep it and assess it before resume or abort",
                path.display()
            )
        })?;
        let file = open_existing_state(&path).map_err(|error| {
            format!(
                "a complete Pending file was atomically installed at {}, but reopening it failed: {error}; keep it and assess it before resume or abort",
                path.display()
            )
        })?;
        Ok(PendingStateFile { file, path })
    }
}

impl PendingStateFile {
    fn open_pending(path: &Path) -> Result<PersistedPendingState, String> {
        let mut file = open_existing_state(path)?;
        let mut bytes = Zeroizing::new([0_u8; STATE_FILE_LENGTH]);
        file.read_exact(&mut bytes[..])
            .map_err(|error| format!("could not read state file {}: {error}", path.display()))?;
        if file
            .metadata()
            .map_err(|error| format!("could not inspect state file {}: {error}", path.display()))?
            .len()
            != STATE_FILE_LENGTH as u64
        {
            return Err(format!(
                "state file {} is not exactly {STATE_FILE_LENGTH} bytes",
                path.display()
            ));
        }
        if bytes[..8] != STATE_MAGIC
            || bytes[8..10] != STATE_FORMAT_VERSION.to_le_bytes()
            || bytes[11..16].iter().any(|byte| *byte != 0)
            || bytes[88..].iter().any(|byte| *byte != 0)
        {
            return Err(format!(
                "state file {} is not canonical version {STATE_FORMAT_VERSION}",
                path.display()
            ));
        }
        match bytes[STATE_STATUS_OFFSET as usize] {
            STATE_PENDING => {}
            STATE_RESERVED => {
                if bytes[16..88].iter().any(|byte| *byte != 0) {
                    return Err(format!(
                        "state file {} is a noncanonical reservation containing credential material",
                        path.display()
                    ));
                }
                return Err(format!(
                    "state file {} is only a reservation and contains no recoverable PSK; assess a possibly lost Begin offer and use physically confirmed abort-current",
                    path.display()
                ));
            }
            STATE_ACTIVE => {
                return Err(format!(
                    "state file {} is already Active and must not be paired again",
                    path.display()
                ));
            }
            state => {
                return Err(format!(
                    "state file {} has unknown state {state}",
                    path.display()
                ));
            }
        }
        let device_bytes: [u8; 16] = bytes[16..32]
            .try_into()
            .expect("fixed state field has exact length");
        let device_id = DeviceId::new(device_bytes)
            .map_err(|_| format!("state file {} has an invalid device ID", path.display()))?;
        let credential_id = CredentialId::new(
            bytes[32..48]
                .try_into()
                .expect("fixed state field has exact length"),
        );
        if credential_id.as_bytes().iter().all(|byte| *byte == 0) {
            return Err(format!(
                "state file {} has a zero credential ID",
                path.display()
            ));
        }
        let generation = CredentialGeneration::new(u64::from_le_bytes(
            bytes[48..56]
                .try_into()
                .expect("fixed state field has exact length"),
        ));
        if generation.get() == 0 {
            return Err(format!(
                "state file {} has a zero credential generation",
                path.display()
            ));
        }
        let mut psk_bytes = Zeroizing::new([0_u8; 32]);
        psk_bytes.copy_from_slice(&bytes[56..88]);
        let psk = PairingPsk::from_zeroizing(psk_bytes)
            .map_err(|_| format!("state file {} has an invalid zero PSK", path.display()))?;
        Ok(PersistedPendingState {
            file: PendingStateFile {
                file,
                path: path.to_path_buf(),
            },
            device_id,
            credential_id,
            generation,
            psk,
        })
    }

    fn mark_active(
        self,
        device_id: DeviceId,
        credential_id: CredentialId,
        activated_generation: CredentialGeneration,
        psk: &PairingPsk,
    ) -> Result<(), String> {
        let Self { file, path } = self;
        ensure_path_matches_file(&file, &path, "Pending state before Active replacement").map_err(
            |error| {
                format!(
                    "credential activated and confirmation verified, but {error}; authenticated-session reconciliation is required"
                )
            },
        )?;
        let (mut staging_file, staging_path) = create_staging_file(&path).map_err(|error| {
            format!(
                "credential activated and confirmation verified, but an owner-only Active staging file could not be created for {}: {error}; authenticated-session reconciliation is required",
                path.display()
            )
        })?;
        let expected = encode_state(
            STATE_ACTIVE,
            Some((device_id, credential_id, activated_generation, psk)),
        );
        if let Err(error) = write_sync_and_verify(&mut staging_file, &staging_path, &expected) {
            drop(staging_file);
            let cleanup = fs::remove_file(&staging_path)
                .and_then(|()| sync_parent(&path).map_err(io::Error::other));
            return Err(format!(
                "credential activated and confirmation verified, but {error}; Active staging cleanup for {} {} and authenticated-session reconciliation is required",
                staging_path.display(),
                if cleanup.is_ok() {
                    "completed"
                } else {
                    "failed"
                }
            ));
        }
        if let Err(error) =
            ensure_path_matches_file(&file, &path, "Pending state before Active replacement")
        {
            drop(staging_file);
            let cleanup = fs::remove_file(&staging_path)
                .and_then(|()| sync_parent(&path).map_err(io::Error::other));
            return Err(format!(
                "credential activated and confirmation verified, but {error}; Active staging cleanup for {} {} and authenticated-session reconciliation is required",
                staging_path.display(),
                if cleanup.is_ok() {
                    "completed"
                } else {
                    "failed"
                }
            ));
        }
        drop(staging_file);
        fs::rename(&staging_path, &path).map_err(|error| {
            format!(
                "credential activated and confirmation verified, but complete Active staging file {} could not atomically replace {}: {error}; preserve both files for authenticated-session reconciliation",
                staging_path.display(),
                path.display()
            )
        })?;
        drop(file);
        sync_parent(&path).map_err(|error| {
            format!(
                "credential activated and confirmation verified and a complete Active file may be installed at {}, but its directory entry could not be synchronized: {error}; authenticated-session reconciliation is required",
                path.display()
            )
        })?;

        let mut active = open_existing_state(&path).map_err(|error| {
            format!(
                "credential activated and confirmation verified, but the replaced Active state could not be reopened: {error}; authenticated-session reconciliation is required"
            )
        })?;
        if active
            .metadata()
            .map_err(|error| {
                format!(
                    "credential activated and confirmation verified, but Active state {} could not be inspected: {error}; authenticated-session reconciliation is required",
                    path.display()
                )
            })?
            .len()
            != STATE_FILE_LENGTH as u64
        {
            return Err(format!(
                "credential activated and confirmation verified, but state file {} is not exactly {STATE_FILE_LENGTH} bytes; authenticated-session reconciliation is required",
                path.display()
            ));
        }
        let mut observed = Zeroizing::new([0_u8; STATE_FILE_LENGTH]);
        active.read_exact(&mut observed[..]).map_err(|error| {
            format!(
                "credential activated and confirmation verified, but Active state {} could not be read back: {error}; authenticated-session reconciliation is required",
                path.display()
            )
        })?;
        if observed.as_ref() != expected.as_ref() {
            return Err(format!(
                "credential activated and confirmation verified, but state file {} did not read back as the complete expected Active credential; authenticated-session reconciliation is required",
                path.display()
            ));
        }
        ensure_path_matches_file(&active, &path, "Active state after replacement").map_err(
            |error| {
                format!(
                    "credential activated and confirmation verified, but {error}; authenticated-session reconciliation is required"
                )
            },
        )
    }
}

pub(crate) fn load_activated_credential(path: &Path) -> Result<ActivatedCredential, String> {
    let mut file = open_existing_state(path)?;
    let mut bytes = Zeroizing::new([0_u8; STATE_FILE_LENGTH]);
    file.read_exact(&mut bytes[..])
        .map_err(|error| format!("could not read state file {}: {error}", path.display()))?;
    if file
        .metadata()
        .map_err(|error| format!("could not inspect state file {}: {error}", path.display()))?
        .len()
        != STATE_FILE_LENGTH as u64
    {
        return Err(format!(
            "state file {} is not exactly {STATE_FILE_LENGTH} bytes",
            path.display()
        ));
    }
    ensure_path_matches_file(&file, path, "Active state")?;
    if bytes[..8] != STATE_MAGIC
        || bytes[8..10] != STATE_FORMAT_VERSION.to_le_bytes()
        || bytes[11..16].iter().any(|byte| *byte != 0)
        || bytes[88..].iter().any(|byte| *byte != 0)
    {
        return Err(format!(
            "state file {} is not canonical version {STATE_FORMAT_VERSION}",
            path.display()
        ));
    }
    if bytes[STATE_STATUS_OFFSET as usize] != STATE_ACTIVE {
        return Err(format!(
            "state file {} is not Active and cannot authenticate a session",
            path.display()
        ));
    }

    let device_id = DeviceId::new(
        bytes[16..32]
            .try_into()
            .expect("fixed state field has exact length"),
    )
    .map_err(|_| format!("state file {} has an invalid device ID", path.display()))?;
    let credential_id = CredentialId::new(
        bytes[32..48]
            .try_into()
            .expect("fixed state field has exact length"),
    );
    if credential_id.as_bytes().iter().all(|byte| *byte == 0) {
        return Err(format!(
            "state file {} has a zero credential ID",
            path.display()
        ));
    }
    let generation = CredentialGeneration::new(u64::from_le_bytes(
        bytes[48..56]
            .try_into()
            .expect("fixed state field has exact length"),
    ));
    if generation.get() == 0 {
        return Err(format!(
            "state file {} has a zero credential generation",
            path.display()
        ));
    }
    let mut psk = Zeroizing::new([0_u8; 32]);
    psk.copy_from_slice(&bytes[56..88]);
    if psk.iter().all(|byte| *byte == 0) {
        return Err(format!("state file {} has a zero PSK", path.display()));
    }
    Ok(ActivatedCredential {
        device_id,
        credential_id,
        generation,
        psk,
    })
}

fn encode_state(
    state: u8,
    pending: Option<(DeviceId, CredentialId, CredentialGeneration, &PairingPsk)>,
) -> Zeroizing<[u8; STATE_FILE_LENGTH]> {
    let mut bytes = Zeroizing::new([0_u8; STATE_FILE_LENGTH]);
    bytes[..8].copy_from_slice(&STATE_MAGIC);
    bytes[8..10].copy_from_slice(&STATE_FORMAT_VERSION.to_le_bytes());
    bytes[10] = state;
    if let Some((device_id, credential_id, generation, psk)) = pending {
        bytes[16..32].copy_from_slice(device_id.as_bytes());
        bytes[32..48].copy_from_slice(credential_id.as_bytes());
        bytes[48..56].copy_from_slice(&generation.get().to_le_bytes());
        bytes[56..88].copy_from_slice(psk.as_bytes());
    }
    bytes
}

fn write_sync_and_verify(
    file: &mut File,
    path: &Path,
    expected: &[u8; STATE_FILE_LENGTH],
) -> Result<(), String> {
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.write_all(expected))
        .and_then(|()| file.set_len(STATE_FILE_LENGTH as u64))
        .and_then(|()| file.sync_all())
        .map_err(|error| {
            format!(
                "could not write and sync state file {}: {error}",
                path.display()
            )
        })?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("could not rewind state file {}: {error}", path.display()))?;
    let mut observed = Zeroizing::new([0_u8; STATE_FILE_LENGTH]);
    file.read_exact(&mut observed[..])
        .map_err(|error| format!("could not read back state file {}: {error}", path.display()))?;
    if observed.as_ref() != expected {
        return Err(format!(
            "state file {} did not read back byte-for-byte",
            path.display()
        ));
    }
    Ok(())
}

fn secure_create_new(path: &Path, purpose: &str) -> Result<File, SecureCreateError> {
    ensure_secure_persistence_host().map_err(|message| SecureCreateError {
        message,
        already_exists: false,
    })?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(path).map_err(|error| SecureCreateError {
        already_exists: error.kind() == io::ErrorKind::AlreadyExists,
        message: format!(
            "could not create {purpose} {} without overwriting an existing path: {error}",
            path.display()
        ),
    })?;
    if let Err(error) =
        ensure_path_matches_file(&file, path, purpose).and_then(|()| ensure_owner_only(&file, path))
    {
        return Err(SecureCreateError {
            message: clean_just_created_nonsecret(file, path, error),
            already_exists: false,
        });
    }
    Ok(file)
}

fn clean_just_created_nonsecret(file: File, path: &Path, original: String) -> String {
    let identity = ensure_path_matches_file(&file, path, "new non-secret pairing-state");
    drop(file);
    if let Err(identity) = identity {
        return format!(
            "{original}; the newly created file handle was closed, but its named path was not removed because {identity}"
        );
    }
    match fs::remove_file(path).and_then(|()| sync_parent(path).map_err(io::Error::other)) {
        Ok(()) => original,
        Err(cleanup) => format!(
            "{original}; cleanup of newly created non-secret file {} failed: {cleanup}",
            path.display()
        ),
    }
}

fn open_existing_state(path: &Path) -> Result<File, String> {
    ensure_secure_persistence_host()?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("could not inspect state path {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(format!(
            "state path {} must be a regular non-symlink file",
            path.display()
        ));
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("could not open state file {}: {error}", path.display()))?;
    ensure_path_matches_file(&file, path, "state")?;
    ensure_owner_only(&file, path)?;
    Ok(file)
}

fn clean_pre_begin_file(path: &Path, original: String) -> String {
    match fs::remove_file(path).and_then(|()| sync_parent(path).map_err(io::Error::other)) {
        Ok(()) => original,
        Err(cleanup) => format!(
            "{original}; no Begin was sent, but cleanup of reserved state {} failed: {cleanup}",
            path.display()
        ),
    }
}

#[cfg(unix)]
fn ensure_path_matches_file(file: &File, path: &Path, purpose: &str) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt as _;

    let opened = file
        .metadata()
        .map_err(|error| format!("could not inspect open {purpose} file: {error}"))?;
    let named = fs::symlink_metadata(path).map_err(|error| {
        format!(
            "could not re-inspect {purpose} path {}: {error}",
            path.display()
        )
    })?;
    if !named.file_type().is_file() || opened.dev() != named.dev() || opened.ino() != named.ino() {
        return Err(format!(
            "{purpose} path {} no longer names the securely opened file",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_path_matches_file(_file: &File, path: &Path, purpose: &str) -> Result<(), String> {
    Err(format!(
        "secure {purpose} path validation is not implemented on this host for {}",
        path.display()
    ))
}

#[cfg(unix)]
fn ensure_owner_only(file: &File, path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = file
        .metadata()
        .map_err(|error| format!("could not inspect state file {}: {error}", path.display()))?
        .permissions()
        .mode();
    if mode & 0o077 != 0 || mode & 0o600 != 0o600 {
        return Err(format!(
            "state file {} must be owner-readable/writable with no group or other permissions; observed mode {:04o}",
            path.display(),
            mode & 0o7777
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_owner_only(_file: &File, path: &Path) -> Result<(), String> {
    Err(format!(
        "secure pairing-state persistence is not implemented on this host for {}",
        path.display()
    ))
}

fn create_staging_file(final_path: &Path) -> Result<(File, PathBuf), String> {
    let parent = state_parent(final_path);
    let file_name = final_path
        .file_name()
        .ok_or_else(|| format!("state path {} has no file name", final_path.display()))?;
    for _ in 0..STAGING_ATTEMPTS {
        let serial = NEXT_STAGING_FILE.fetch_add(1, Ordering::Relaxed);
        let mut stage_name = OsString::from(".");
        stage_name.push(file_name);
        stage_name.push(format!(".pairing-stage-{}-{serial}", std::process::id()));
        let stage_path = parent.join(stage_name);
        match secure_create_new(&stage_path, "pairing-state staging file") {
            Ok(file) => return Ok((file, stage_path)),
            Err(error) if error.already_exists => {}
            Err(error) => return Err(error.message),
        }
    }
    Err(format!(
        "could not reserve an owner-only staging file beside {} after {STAGING_ATTEMPTS} attempts",
        final_path.display()
    ))
}

fn ensure_secure_persistence_host() -> Result<(), String> {
    #[cfg(unix)]
    {
        Ok(())
    }
    #[cfg(not(unix))]
    {
        Err("pair/resume currently require Unix owner-only file semantics and directory fsync; abort-current remains available on this host".to_owned())
    }
}

fn state_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<(), String> {
    let parent = state_parent(path);
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            format!(
                "could not sync state-file directory {}: {error}",
                parent.display()
            )
        })
}

#[cfg(not(unix))]
fn sync_parent(path: &Path) -> Result<(), String> {
    Err(format!(
        "secure state-file directory synchronization is not implemented on this host for {}",
        state_parent(path).display()
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

fn ensure_pair_headroom(begin_sequence: u64) -> Result<(), String> {
    match begin_sequence.checked_add(2) {
        Some(activate_sequence) if activate_sequence < u64::MAX => Ok(()),
        _ => Err(format!(
            "pair requires usable sequences for Begin, ProofStart, and Activate, but Begin sequence {begin_sequence} leaves insufficient exact-next headroom; no Begin was sent at this sequence; {}",
            bus_reset_guidance()
        )),
    }
}

fn ensure_resume_headroom(proof_sequence: u64) -> Result<(), String> {
    match proof_sequence.checked_add(1) {
        Some(activate_sequence) if activate_sequence < u64::MAX => Ok(()),
        _ => Err(format!(
            "resume requires usable sequences for ProofStart and Activate, but ProofStart sequence {proof_sequence} leaves insufficient exact-next headroom; no ProofStart was sent at this sequence; {}",
            bus_reset_guidance()
        )),
    }
}

fn wait_before_next(deadline: Instant, last_sequence: u64, workflow: &str) -> Result<(), String> {
    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
        return Err(deadline_message(last_sequence, true, workflow));
    };
    if remaining <= Duration::from_millis(PRESENCE_POLL_MS) {
        return Err(deadline_message(last_sequence, true, workflow));
    }
    thread::sleep(Duration::from_millis(PRESENCE_POLL_MS));
    Ok(())
}

fn deadline_message(last_sequence: u64, response_received: bool, workflow: &str) -> String {
    if response_received {
        format!(
            "{workflow} timed out after last_sent_sequence={last_sequence}; that sequence was \
             consumed because its response was received; {}",
            bus_reset_guidance()
        )
    } else {
        format!(
            "{workflow} timed out before sending sequence {last_sequence}; {}",
            bus_reset_guidance()
        )
    }
}

fn next_sequence_guidance(last_sequence: u64) -> String {
    let next = next_usable_sequence(last_sequence)
        .map_or_else(|| "exhausted".to_owned(), |value| value.to_string());
    format!("next_sequence={next}; {}", bus_reset_guidance())
}

fn sequence_ambiguity_guidance(last_sequence: u64) -> String {
    format!(
        "last_sent_sequence={last_sequence} is consumed-or-ambiguous because the device may have \
         accepted it before its response was lost; {}",
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
        timeout: Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS)),
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

const fn pairing_failure_name(failure: PairingFailure) -> &'static str {
    match failure {
        PairingFailure::PhysicalPresenceRequired => "physical-presence-required",
        PairingFailure::Refused => "refused",
        PairingFailure::Blocked => "blocked",
        PairingFailure::Unavailable => "unavailable",
    }
}

const fn activate_failure_name(
    failure: reticulum_device_api_pairing::ActivateFailure,
) -> &'static str {
    match failure {
        reticulum_device_api_pairing::ActivateFailure::ProofRejected => "proof-rejected",
        reticulum_device_api_pairing::ActivateFailure::Refused => "refused",
        reticulum_device_api_pairing::ActivateFailure::Blocked => "blocked",
        reticulum_device_api_pairing::ActivateFailure::Unavailable => "unavailable",
    }
}

const fn abort_result_name(result: AbortResult) -> &'static str {
    match result {
        AbortResult::Aborted => "aborted",
        AbortResult::PhysicalPresenceRequired => "physical-presence-required",
        AbortResult::Refused => "refused",
        AbortResult::Blocked => "blocked",
        AbortResult::Unavailable => "unavailable",
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    #[cfg(unix)]
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    use super::*;

    static NEXT_PATH: AtomicU64 = AtomicU64::new(0);

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn temporary_path() -> PathBuf {
        let serial = NEXT_PATH.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "reticulum-e290-pairing-state-{}-{serial}.bin",
            std::process::id()
        ))
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
        let error = parse(&strings(&["--port", "/dev/test", "pair"]))
            .err()
            .unwrap();
        assert_eq!(error, "pair requires --state-file");
        let error = parse(&strings(&["--port", "/dev/test", "resume"]))
            .err()
            .unwrap();
        assert_eq!(error, "resume requires --state-file");
    }

    #[test]
    fn parser_keeps_abort_identifier_free() {
        let parsed = parse(&strings(&["--port", "/dev/test", "abort-current"])).unwrap();
        assert_eq!(parsed.command, Command::AbortCurrent);
        assert!(parsed.state_file.is_none());
        assert_eq!(parsed.sequence, 0);
        assert_eq!(parsed.timeout, Duration::from_millis(DEFAULT_TIMEOUT_MS));
    }

    #[test]
    fn parser_rejects_state_path_for_abort() {
        let error = parse(&strings(&[
            "--port",
            "/dev/test",
            "--state-file",
            "/tmp/device.key",
            "abort-current",
        ]))
        .err()
        .unwrap();
        assert_eq!(error, "abort-current does not accept --state-file");
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

    #[test]
    fn response_family_matcher_is_exact() {
        let begin = PairingResponse::Begin(BeginResponse::failure(1, PairingFailure::Unavailable));
        assert!(response_matches(ExpectedResponse::Begin, &begin));
        assert!(!response_matches(ExpectedResponse::Activate, &begin));
    }

    #[test]
    fn sequence_space_refuses_the_firmware_exhaustion_sentinel() {
        assert_eq!(next_usable_sequence(u64::MAX - 1), None);
        assert_eq!(next_usable_sequence(u64::MAX - 2), Some(u64::MAX - 1));
        assert!(require_next_sequence(u64::MAX - 1).is_err());
    }

    #[test]
    fn pair_and_resume_reserve_every_required_sequence_before_sending() {
        assert!(ensure_pair_headroom(u64::MAX - 3).is_ok());
        assert!(ensure_pair_headroom(u64::MAX - 2).is_err());
        assert!(ensure_pair_headroom(u64::MAX - 1).is_err());

        assert!(ensure_resume_headroom(u64::MAX - 2).is_ok());
        assert!(ensure_resume_headroom(u64::MAX - 1).is_err());
    }

    #[test]
    fn proof_challenge_device_must_match_the_persisted_offer() {
        let expected = DeviceId::new([0x11; 16]).unwrap();
        let substituted = DeviceId::new([0x22; 16]).unwrap();
        assert!(validate_challenge_device(expected, expected).is_ok());
        assert!(validate_challenge_device(expected, substituted).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn reserved_marker_is_owner_only_rejected_by_resume_and_discardable() {
        ensure_secure_persistence_host().unwrap();
        let path = temporary_path();
        let reservation = ReservedStateFile::reserve(&path).unwrap();
        let staging_path = reservation.staging_path.clone();
        let marker = fs::read(&path).unwrap();
        assert_eq!(marker.len(), STATE_FILE_LENGTH);
        assert_eq!(&marker[..8], &STATE_MAGIC);
        assert_eq!(marker[10], STATE_RESERVED);
        assert!(marker[11..].iter().all(|byte| *byte == 0));
        assert_eq!(
            reservation.file.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            reservation
                .staging_file
                .metadata()
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let error = PendingStateFile::open_pending(&path).err().unwrap();
        assert!(error.contains("only a reservation"));

        reservation.discard().unwrap();
        assert!(!path.exists());
        assert!(!staging_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn pending_state_round_trips_atomically_and_active_is_not_resumable() {
        let path = temporary_path();
        let pending_link = path.with_extension("pending-hard-link");
        let device_id = DeviceId::new([0x11; 16]).unwrap();
        let credential_id = CredentialId::new([0x22; 16]);
        let generation = CredentialGeneration::new(7);
        let activated_generation = CredentialGeneration::new(11);
        let psk = PairingPsk::new([0x33; 32]).unwrap();
        let reservation = ReservedStateFile::reserve(&path).unwrap();
        let staging_path = reservation.staging_path.clone();
        let state = reservation
            .commit_pending(device_id, credential_id, generation, &psk)
            .unwrap();
        assert!(!staging_path.exists());
        let pending = fs::read(&path).unwrap();
        assert_eq!(pending.len(), STATE_FILE_LENGTH);
        assert_eq!(&pending[..8], &STATE_MAGIC);
        assert_eq!(&pending[8..10], &STATE_FORMAT_VERSION.to_le_bytes());
        assert_eq!(pending[10], STATE_PENDING);
        assert_eq!(&pending[16..32], device_id.as_bytes());
        assert_eq!(&pending[32..48], credential_id.as_bytes());
        assert_eq!(&pending[48..56], &generation.get().to_le_bytes());
        assert_eq!(&pending[56..88], psk.as_bytes());
        assert_eq!(
            state.file.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        let error = load_activated_credential(&path).err().unwrap();
        assert!(error.contains("is not Active"));

        let persisted = PendingStateFile::open_pending(&path).unwrap();
        assert_eq!(persisted.device_id, device_id);
        assert_eq!(persisted.credential_id, credential_id);
        assert_eq!(persisted.generation, generation);
        assert_eq!(persisted.psk.as_bytes(), psk.as_bytes());
        drop(persisted);

        fs::hard_link(&path, &pending_link).unwrap();
        let pending_inode = fs::metadata(&pending_link).unwrap().ino();
        assert_eq!(state.file.metadata().unwrap().ino(), pending_inode);
        state
            .mark_active(device_id, credential_id, activated_generation, &psk)
            .unwrap();
        let active = fs::read(&path).unwrap();
        assert_eq!(active.len(), STATE_FILE_LENGTH);
        assert_eq!(active[10], STATE_ACTIVE);
        assert_eq!(&active[48..56], &activated_generation.get().to_le_bytes());
        assert_eq!(&active[56..88], psk.as_bytes());
        assert_ne!(fs::metadata(&path).unwrap().ino(), pending_inode);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let retained_pending = fs::read(&pending_link).unwrap();
        assert_eq!(retained_pending[10], STATE_PENDING);
        assert_eq!(&retained_pending[48..56], &generation.get().to_le_bytes());
        let activated = load_activated_credential(&path).unwrap();
        let (loaded_device, loaded_credential, loaded_generation, loaded_psk) =
            activated.into_parts();
        assert_eq!(loaded_device, device_id);
        assert_eq!(loaded_credential, credential_id);
        assert_eq!(loaded_generation, activated_generation);
        assert_eq!(loaded_psk.as_ref(), psk.as_bytes());
        let error = PendingStateFile::open_pending(&path).err().unwrap();
        assert!(error.contains("already Active"));
        fs::remove_file(path).unwrap();
        fs::remove_file(pending_link).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn resume_rejects_a_pending_file_with_broad_permissions() {
        let path = temporary_path();
        let psk = PairingPsk::new([3; 32]).unwrap();
        let state = ReservedStateFile::reserve(&path)
            .unwrap()
            .commit_pending(
                DeviceId::new([1; 16]).unwrap(),
                CredentialId::new([2; 16]),
                CredentialGeneration::new(1),
                &psk,
            )
            .unwrap();
        drop(state);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let error = PendingStateFile::open_pending(&path).err().unwrap();
        assert!(error.contains("owner-readable/writable"));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn reservation_never_overwrites_existing_material() {
        let path = temporary_path();
        fs::write(&path, b"existing").unwrap();
        let result = ReservedStateFile::reserve(&path);
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"existing");
        fs::remove_file(path).unwrap();
    }

    #[cfg(not(unix))]
    #[test]
    fn secret_persistence_is_rejected_before_touching_a_path() {
        let path = temporary_path();
        assert!(ReservedStateFile::reserve(&path).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn formatting_helpers_never_include_secret_material() {
        assert_eq!(hex(&[0x00, 0xab, 0xff]), "00abff");
        assert_eq!(
            pairing_failure_name(PairingFailure::PhysicalPresenceRequired),
            "physical-presence-required"
        );
        assert_eq!(abort_result_name(AbortResult::Aborted), "aborted");
    }
}
