//! Blocking host client for the resident Reticulum device-pairing lifecycle.
//!
//! The default `serial` feature provides the turnkey USB serial
//! `PairingClient`. Transport integrations can disable that feature and
//! construct a [`PairingSession`] over any blocking [`PairingStream`]. Either
//! path keeps one stream, decoder, bearer binding, and exact-next sequence
//! owner alive across status, optional initialization, Begin, ProofStart, and
//! Activate.
//!
//! The credential path is always one exact [`CREDENTIAL_ARTIFACT_BYTES`]-byte
//! image. It progresses from a secret-free Begin reservation to resume-safe
//! Pending, is durably changed to ActivationAmbiguous before Activate can be
//! sent, and becomes Active only after authenticated activation confirmation.
//! Use [`classify_credential_artifact`] to choose a recovery UI and
//! [`cleanup_after_confirmed_abort`] only after a definitely successful,
//! physically confirmed AbortCurrent response.
//!
//! ```no_run
//! # #[cfg(feature = "serial")]
//! # {
//! use std::{path::Path, time::Duration};
//!
//! use reticulum_device_pairing_client::PairingClient;
//!
//! # fn pair() -> Result<(), Box<dyn std::error::Error>> {
//! let client = PairingClient::new("/dev/cu.usbmodem-node")
//!     .with_timeout(Duration::from_secs(120));
//! let mut progress = |event| eprintln!("pairing progress: {event:?}");
//! let activated =
//!     client.initialize_and_pair(Path::new("device-credential.bin"), &mut progress)?;
//! println!("activated credential generation {}", activated.generation());
//! # Ok(())
//! # }
//! # }
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::{
    ffi::OsString,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use reticulum_device_api_framing::{DecodeEvent, FramedRecord, StreamDecoder};
use reticulum_device_api_pairing::{
    AbortCurrentRequest, AbortResult, ActivateRequest, ActivateResponse, BearerBinding, BeginOffer,
    BeginRequest, BeginResponse, ClientProof, CredentialGeneration, CredentialId, DeviceId,
    PairingFailure, PairingPsk, PairingRequest, PairingResponse, PairingTranscript, ProofChallenge,
    ProofStartRequest, ProofStartResponse,
};
use reticulum_device_api_pairing_control::{
    ControlRequest, ControlResponse, InitializationStatus, InitializeResult,
};
#[cfg(feature = "serial")]
use serialport::ClearBuffer;
use zeroize::Zeroizing;

#[cfg(feature = "serial")]
const BAUD_RATE: u32 = 115_200;
#[cfg(feature = "serial")]
const OPEN_SETTLE_MS: u64 = 250;
#[cfg(feature = "serial")]
const READ_SLICE_MS: u64 = 100;
const PRESENCE_POLL_MS: u64 = 200;

const STATE_MAGIC: [u8; 8] = *b"RDPKEY1\0";
const STATE_FORMAT_VERSION: u16 = 1;
const STATE_RESERVED: u8 = 0;
const STATE_PENDING: u8 = 1;
const STATE_ACTIVE: u8 = 2;
const STATE_ACTIVATION_AMBIGUOUS: u8 = 3;
const STATE_STATUS_OFFSET: u64 = 10;
/// Exact byte length of every durable host credential artifact.
pub const CREDENTIAL_ARTIFACT_BYTES: usize = 96;
const STATE_FILE_LENGTH: usize = CREDENTIAL_ARTIFACT_BYTES;
const STAGING_ATTEMPTS: u64 = 16;

static NEXT_STAGING_FILE: AtomicU64 = AtomicU64::new(0);

/// Default deadline for one blocking control or pairing workflow.
pub const DEFAULT_PAIRING_TIMEOUT: Duration = Duration::from_secs(120);

/// Blocking byte stream used by one transport-bound pairing session.
///
/// Reads should report [`io::ErrorKind::TimedOut`] periodically rather than
/// block past the session deadline. The trait is object-safe so native
/// transports can erase their concrete stream implementation at this boundary.
pub trait PairingStream: Read + Write + Send {}

impl<T> PairingStream for T where T: Read + Write + Send {}

/// Serial endpoint and initial exact-next sequence for a pre-auth connection.
#[cfg(feature = "serial")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PairingClient {
    port: String,
    sequence: u64,
    timeout: Duration,
}

#[cfg(feature = "serial")]
impl PairingClient {
    /// Construct a connector starting at sequence zero with the default timeout.
    pub fn new(port: impl Into<String>) -> Self {
        Self {
            port: port.into(),
            sequence: 0,
            timeout: DEFAULT_PAIRING_TIMEOUT,
        }
    }

    /// Set the exact next sequence for the confirmed USB connection epoch.
    #[must_use]
    pub const fn with_sequence(mut self, sequence: u64) -> Self {
        self.sequence = sequence;
        self
    }

    /// Set the deadline applied independently to each blocking workflow.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Configured serial endpoint path.
    pub fn port(&self) -> &str {
        &self.port
    }

    /// Configured initial exact-next sequence.
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Per-workflow blocking deadline.
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Open and configure one pre-auth serial connection.
    ///
    /// The returned session owns the TTY, streaming decoder, and exact-next
    /// sequence. Keep it alive across initialization and pairing so a single
    /// physical-presence window and one USB epoch cover the complete first-run
    /// workflow.
    pub fn connect(
        &self,
        observer: &mut dyn ProgressObserver,
    ) -> Result<PairingSession, PairingClientError> {
        ensure_usable_sequence(self.sequence).map_err(PairingClientError::new)?;
        if self.timeout.is_zero() {
            return Err(PairingClientError::new(
                "pairing timeout must be nonzero".to_owned(),
            ));
        }
        observer.observe(PairingProgress::OpeningPort);
        let mut port = serialport::new(&self.port, BAUD_RATE)
            .timeout(Duration::from_millis(READ_SLICE_MS))
            .open()
            .map_err(|error| {
                PairingClientError::new(format!("could not open {}: {error}", self.port))
            })?;
        port.write_data_terminal_ready(true).map_err(|error| {
            PairingClientError::new(format!("could not assert DTR on {}: {error}", self.port))
        })?;
        port.write_request_to_send(false).map_err(|error| {
            PairingClientError::new(format!("could not clear RTS on {}: {error}", self.port))
        })?;
        thread::sleep(Duration::from_millis(OPEN_SETTLE_MS));
        port.clear(ClearBuffer::Input).map_err(|error| {
            PairingClientError::new(format!(
                "could not clear stale input on {}: {error}",
                self.port
            ))
        })?;
        let session = PairingSession::from_stream(
            port,
            self.port.clone(),
            BearerBinding::UsbSerialJtag,
            self.sequence,
            self.timeout,
        )?;
        observer.observe(PairingProgress::PortReady);
        Ok(session)
    }

    /// Initialize an eligible device and pair it without releasing the TTY.
    ///
    /// A non-completed initialization outcome is returned as an error because
    /// Begin cannot safely proceed. The result contains public identifiers only.
    pub fn initialize_and_pair(
        &self,
        state_path: &Path,
        observer: &mut dyn ProgressObserver,
    ) -> Result<ActivationSummary, PairingClientError> {
        let mut session = self.connect(observer)?;
        let initialized = session.ensure_initialized(observer)?;
        if !initialized.is_completed() {
            return Err(PairingClientError::new(format!(
                "credential-store initialization did not complete: {}",
                initialized.outcome_name()
            )));
        }
        session.pair(state_path, observer)
    }
}

/// One open pre-auth connection with sole ownership of its stream and protocol state.
pub struct PairingSession {
    stream: Box<dyn PairingStream>,
    decoder: StreamDecoder,
    endpoint_name: String,
    bearer: BearerBinding,
    next_sequence: Option<u64>,
    timeout: Duration,
}

impl PairingSession {
    /// Construct a pairing session over one already-open blocking byte stream.
    ///
    /// `endpoint_name` is public diagnostic context only. `bearer` is the
    /// cryptographic protocol binding used for every bearer-profiled request
    /// and response; callers must select the binding for the actual stream.
    pub fn from_stream(
        stream: impl PairingStream + 'static,
        endpoint_name: impl Into<String>,
        bearer: BearerBinding,
        sequence: u64,
        timeout: Duration,
    ) -> Result<Self, PairingClientError> {
        ensure_usable_sequence(sequence).map_err(PairingClientError::new)?;
        if timeout.is_zero() {
            return Err(PairingClientError::new(
                "pairing timeout must be nonzero".to_owned(),
            ));
        }
        Ok(Self {
            stream: Box::new(stream),
            decoder: StreamDecoder::new(),
            endpoint_name: endpoint_name.into(),
            bearer,
            next_sequence: Some(sequence),
            timeout,
        })
    }

    /// Pairing bearer bound to this stream and its protocol transcript.
    pub const fn bearer(&self) -> BearerBinding {
        self.bearer
    }

    /// Diagnostic name for the selected transport endpoint.
    pub fn endpoint_name(&self) -> &str {
        &self.endpoint_name
    }

    /// Exact next request sequence, or `None` after ambiguity/exhaustion.
    pub const fn next_sequence(&self) -> Option<u64> {
        self.next_sequence
    }

    /// Read one public credential-store initialization status.
    pub fn initialization_status(
        &mut self,
        observer: &mut dyn ProgressObserver,
    ) -> Result<InitializationSummary, PairingClientError> {
        observer.observe(PairingProgress::CheckingInitialization);
        let deadline = self.deadline().map_err(PairingClientError::new)?;
        initialization_status_workflow(self, deadline).map_err(PairingClientError::new)
    }

    /// Run the complete status/initialize/poll workflow on this connection.
    pub fn ensure_initialized(
        &mut self,
        observer: &mut dyn ProgressObserver,
    ) -> Result<InitializationSummary, PairingClientError> {
        observer.observe(PairingProgress::CheckingInitialization);
        let deadline = self.deadline().map_err(PairingClientError::new)?;
        initialize_workflow(self, deadline, observer).map_err(PairingClientError::new)
    }

    /// Pair a new credential and atomically persist it at `state_path`.
    pub fn pair(
        &mut self,
        state_path: &Path,
        observer: &mut dyn ProgressObserver,
    ) -> Result<ActivationSummary, PairingClientError> {
        ensure_secure_persistence_host().map_err(PairingClientError::new)?;
        let sequence = self.require_next().map_err(PairingClientError::new)?;
        ensure_pair_headroom(sequence).map_err(PairingClientError::new)?;
        let deadline = self.deadline().map_err(PairingClientError::new)?;
        pair_workflow(self, deadline, state_path, observer).map_err(PairingClientError::new)
    }

    /// Resume only a canonical resume-safe Pending credential artifact.
    pub fn resume(
        &mut self,
        state_path: &Path,
        observer: &mut dyn ProgressObserver,
    ) -> Result<ActivationSummary, PairingClientError> {
        ensure_secure_persistence_host().map_err(PairingClientError::new)?;
        let sequence = self.require_next().map_err(PairingClientError::new)?;
        ensure_resume_headroom(sequence).map_err(PairingClientError::new)?;
        let deadline = self.deadline().map_err(PairingClientError::new)?;
        resume_workflow(self, deadline, state_path, observer).map_err(PairingClientError::new)
    }

    /// Ask the device to abort its sole Pending credential.
    ///
    /// This identifier-free operation does not inspect or mutate a host
    /// credential artifact.
    pub fn abort_current(
        &mut self,
        observer: &mut dyn ProgressObserver,
    ) -> Result<AbortSummary, PairingClientError> {
        self.require_next().map_err(PairingClientError::new)?;
        let deadline = self.deadline().map_err(PairingClientError::new)?;
        abort_workflow(self, deadline, observer).map_err(PairingClientError::new)
    }

    fn deadline(&self) -> Result<Instant, String> {
        Instant::now()
            .checked_add(self.timeout)
            .ok_or_else(|| "pairing timeout is too large for the host monotonic clock".to_owned())
    }

    fn require_next(&self) -> Result<u64, String> {
        self.next_sequence.ok_or_else(|| {
            format!(
                "pre-auth request sequence is ambiguous or exhausted; {}",
                connection_epoch_reset_guidance()
            )
        })
    }
}

/// Coarse operation that may require the user-presence button.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PresenceOperation {
    /// Initialize an eligible credential store.
    Initialize,
    /// Allocate a new durable Pending credential.
    Begin,
    /// Challenge an existing exact Pending credential.
    ProofStart,
    /// Abort the device-selected Pending credential.
    AbortCurrent,
}

/// Secret-free progress suitable for projection into a service snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingProgress {
    /// The client is opening and configuring the selected serial endpoint.
    OpeningPort,
    /// The endpoint is configured and stale input has been cleared.
    PortReady,
    /// Public credential-store status is being checked.
    CheckingInitialization,
    /// Initialization has been admitted or is still in flight.
    Initializing,
    /// The credential store is initialized and ready for pairing.
    Initialized,
    /// The device requires the release-then-hold physical-presence gesture.
    WaitingForPhysicalPresence(PresenceOperation),
    /// The offered Pending credential is durably resume-safe on the host.
    PendingPersisted,
    /// A matching proof challenge was received and validated.
    ProofChallengeAccepted,
    /// The artifact was durably marked activation-ambiguous before Activate.
    ActivationPrepared,
    /// Activation was confirmed and the host artifact is durably Active.
    Activated,
    /// The device definitely committed an aborted Pending tombstone.
    Aborted,
}

/// Observer for secret-free blocking-workflow progress.
pub trait ProgressObserver {
    /// Observe one coarse transition. Implementations should return promptly.
    fn observe(&mut self, progress: PairingProgress);
}

impl<F> ProgressObserver for F
where
    F: FnMut(PairingProgress),
{
    fn observe(&mut self, progress: PairingProgress) {
        self(progress);
    }
}

/// Public credential-store initialization status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitializationState {
    /// Initialization control is unavailable.
    Unavailable,
    /// The eligible store requires explicit initialization.
    Required,
    /// A previously accepted initialization is still in flight.
    InFlight,
    /// Initialization is complete.
    Completed,
    /// Initialization is blocked by a fail-closed condition.
    Blocked,
}

impl InitializationState {
    /// Stable one-byte protocol code.
    pub const fn code(self) -> u8 {
        match self {
            Self::Unavailable => 0,
            Self::Required => 1,
            Self::InFlight => 2,
            Self::Completed => 3,
            Self::Blocked => 4,
        }
    }

    /// Stable CLI/service name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Required => "initialization-required",
            Self::InFlight => "in-flight",
            Self::Completed => "completed",
            Self::Blocked => "blocked",
        }
    }
}

impl From<InitializationStatus> for InitializationState {
    fn from(status: InitializationStatus) -> Self {
        match status {
            InitializationStatus::Unavailable => Self::Unavailable,
            InitializationStatus::InitializationRequired => Self::Required,
            InitializationStatus::InFlight => Self::InFlight,
            InitializationStatus::Completed => Self::Completed,
            InitializationStatus::Blocked => Self::Blocked,
        }
    }
}

/// Public terminal or retry result of an explicit Initialize request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitializeOutcome {
    /// Initialization completed durably.
    Completed,
    /// The device retained the operation and the host should poll status.
    Retrying,
    /// A release-then-hold gesture is required.
    PhysicalPresenceRequired,
    /// Policy refused initialization.
    Refused,
    /// Initialization is blocked by a fail-closed condition.
    Blocked,
    /// Initialization control is unavailable.
    Unavailable,
}

impl InitializeOutcome {
    /// Stable one-byte protocol code.
    pub const fn code(self) -> u8 {
        match self {
            Self::Completed => 0,
            Self::Retrying => 1,
            Self::PhysicalPresenceRequired => 2,
            Self::Refused => 3,
            Self::Blocked => 4,
            Self::Unavailable => 5,
        }
    }

    /// Stable CLI/service name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Retrying => "retrying",
            Self::PhysicalPresenceRequired => "physical-presence-required",
            Self::Refused => "refused",
            Self::Blocked => "blocked",
            Self::Unavailable => "unavailable",
        }
    }
}

impl From<InitializeResult> for InitializeOutcome {
    fn from(result: InitializeResult) -> Self {
        match result {
            InitializeResult::Completed => Self::Completed,
            InitializeResult::Retrying => Self::Retrying,
            InitializeResult::PhysicalPresenceRequired => Self::PhysicalPresenceRequired,
            InitializeResult::Refused => Self::Refused,
            InitializeResult::Blocked => Self::Blocked,
            InitializeResult::Unavailable => Self::Unavailable,
        }
    }
}

/// Terminal response family returned by initialization workflows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitializationResponse {
    /// A status response terminated the workflow.
    Status(InitializationState),
    /// An explicit Initialize response terminated the workflow.
    Initialize(InitializeOutcome),
}

/// Sequence and public result of a status or initialize workflow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InitializationSummary {
    sequence: u64,
    response: InitializationResponse,
}

impl InitializationSummary {
    /// Construct a summary from an already validated public response.
    pub const fn from_response(sequence: u64, response: InitializationResponse) -> Self {
        Self { sequence, response }
    }

    /// Sequence consumed by the terminal response.
    pub const fn sequence(self) -> u64 {
        self.sequence
    }

    /// Terminal response family and public outcome.
    pub const fn response(self) -> InitializationResponse {
        self.response
    }

    /// Exact next sequence if this connection epoch still has usable sequence space.
    pub const fn next_sequence(self) -> Option<u64> {
        next_usable_sequence(self.sequence)
    }

    /// Whether the credential store is definitely initialized.
    pub const fn is_completed(self) -> bool {
        matches!(
            self.response,
            InitializationResponse::Status(InitializationState::Completed)
                | InitializationResponse::Initialize(InitializeOutcome::Completed)
        )
    }

    /// Stable public name for the terminal outcome.
    pub const fn outcome_name(self) -> &'static str {
        match self.response {
            InitializationResponse::Status(state) => state.name(),
            InitializationResponse::Initialize(outcome) => outcome.name(),
        }
    }
}

/// Secret-free summary of a durably activated credential.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActivationSummary {
    sequence: u64,
    device_id: [u8; 16],
    credential_id: [u8; 16],
    generation: u64,
}

impl ActivationSummary {
    /// Sequence consumed by the successful Activate request.
    pub const fn sequence(self) -> u64 {
        self.sequence
    }

    /// Stable public device-API identifier learned from the offer.
    pub const fn device_id(self) -> [u8; 16] {
        self.device_id
    }

    /// Activated opaque credential identifier.
    pub const fn credential_id(self) -> [u8; 16] {
        self.credential_id
    }

    /// Actual device-selected Active generation.
    pub const fn generation(self) -> u64 {
        self.generation
    }

    /// Exact next sequence if this connection epoch still has usable sequence space.
    pub const fn next_sequence(self) -> Option<u64> {
        next_usable_sequence(self.sequence)
    }
}

/// Secret-free coarse result of identifier-free AbortCurrent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AbortOutcome {
    /// The device durably committed its Pending credential as aborted.
    Aborted,
    /// A fresh release-then-hold gesture is still required.
    PhysicalPresenceRequired,
    /// Policy refused the request.
    Refused,
    /// Device state blocked the request.
    Blocked,
    /// Pairing is unavailable.
    Unavailable,
}

impl AbortOutcome {
    /// Stable one-byte protocol code.
    pub const fn code(self) -> u8 {
        match self {
            Self::Aborted => 0,
            Self::PhysicalPresenceRequired => 1,
            Self::Refused => 2,
            Self::Blocked => 3,
            Self::Unavailable => 4,
        }
    }

    /// Stable CLI/service name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Aborted => "aborted",
            Self::PhysicalPresenceRequired => "physical-presence-required",
            Self::Refused => "refused",
            Self::Blocked => "blocked",
            Self::Unavailable => "unavailable",
        }
    }

    /// Whether a Pending tombstone was definitely committed.
    pub const fn is_aborted(self) -> bool {
        matches!(self, Self::Aborted)
    }
}

impl From<AbortResult> for AbortOutcome {
    fn from(result: AbortResult) -> Self {
        match result {
            AbortResult::Aborted => Self::Aborted,
            AbortResult::PhysicalPresenceRequired => Self::PhysicalPresenceRequired,
            AbortResult::Refused => Self::Refused,
            AbortResult::Blocked => Self::Blocked,
            AbortResult::Unavailable => Self::Unavailable,
        }
    }
}

/// Sequence and coarse result returned by AbortCurrent.
#[derive(Debug, Eq, PartialEq)]
pub struct AbortSummary {
    sequence: u64,
    outcome: AbortOutcome,
}

impl AbortSummary {
    /// Sequence consumed by the terminal AbortCurrent request.
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Coarse device result.
    pub const fn outcome(&self) -> AbortOutcome {
        self.outcome
    }

    /// Exact next sequence if this connection epoch still has usable sequence space.
    pub const fn next_sequence(&self) -> Option<u64> {
        next_usable_sequence(self.sequence)
    }

    /// Consume a definitely successful response into the cleanup capability.
    ///
    /// Refused, blocked, unavailable, and still-presence-gated responses never
    /// produce a capability, so they cannot authorize host artifact removal.
    pub fn into_confirmed_abort(self) -> Option<ConfirmedAbort> {
        self.outcome.is_aborted().then_some(ConfirmedAbort {
            sequence: self.sequence,
        })
    }
}

/// Single-use authority to clean one host artifact after definite AbortCurrent.
///
/// This value has no public constructor and implements neither `Clone` nor
/// `Copy`. It is returned only by [`AbortSummary::into_confirmed_abort`] for an
/// authenticated protocol outcome of [`AbortOutcome::Aborted`].
#[derive(Debug, Eq, PartialEq)]
pub struct ConfirmedAbort {
    sequence: u64,
}

impl ConfirmedAbort {
    /// Sequence consumed by the definitely successful AbortCurrent request.
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }
}

/// Non-secret host pairing or initialization failure.
#[derive(Debug)]
pub struct PairingClientError {
    message: String,
}

impl PairingClientError {
    fn new(message: String) -> Self {
        Self { message }
    }
}

impl fmt::Display for PairingClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PairingClientError {}

#[derive(Clone, Copy)]
enum ExpectedResponse {
    Begin,
    ProofStart,
    Activate,
    AbortCurrent,
}

#[derive(Clone, Copy)]
enum ExpectedControlResponse {
    Status,
    Initialize,
}

struct PendingStateFile {
    file: File,
    path: PathBuf,
}

struct ActivationAmbiguousStateFile {
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

struct DecodedCredential {
    classification: CredentialArtifactClassification,
    device_id: DeviceId,
    credential_id: CredentialId,
    generation: CredentialGeneration,
    psk: PairingPsk,
}

enum DecodedArtifact {
    Reserved,
    Credential(DecodedCredential),
}

/// Non-secret recovery classification of one configured credential path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialArtifactClassification {
    /// No filesystem entry exists at the path.
    Missing,
    /// A canonical secret-free reservation marks a possibly accepted lost Begin.
    ReservedOrAmbiguousBegin,
    /// A canonical Pending credential is safe to resume.
    PendingResumeSafe,
    /// Activate may have been sent; neither resume nor abort is safe.
    ActivationAmbiguous,
    /// A canonical Active credential can be used for authentication.
    Active,
    /// The path, permissions, file identity, format, or fields are invalid.
    Invalid,
}

/// Classify a credential artifact without returning any credential material.
///
/// The classifier rejects symlinks, non-regular or multiply linked files,
/// broad Unix permissions, path/file identity changes, noncanonical bytes, and
/// invalid secret fields. All read scratch and any decoded PSK owner are
/// zeroized before return.
pub fn classify_credential_artifact(path: &Path) -> CredentialArtifactClassification {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            CredentialArtifactClassification::Missing
        }
        Err(_) => CredentialArtifactClassification::Invalid,
        Ok(_) => {
            classify_existing_artifact(path).unwrap_or(CredentialArtifactClassification::Invalid)
        }
    }
}

/// Canonical artifact states that may be removed after definite device abort.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CleanedCredentialArtifact {
    /// A secret-free ambiguous-Begin reservation was removed.
    ReservedOrAmbiguousBegin,
    /// A resume-safe Pending credential was removed.
    PendingResumeSafe,
}

/// Remove a Reserved or Pending host artifact after definite AbortCurrent.
///
/// This operation reopens the owner-only non-symlink file, validates its exact
/// canonical state, rechecks that the named path still identifies the opened
/// inode, removes it, and synchronizes the parent directory. Active,
/// activation-ambiguous, invalid, missing, or multiply linked files are never
/// removed. The single-use confirmation argument can only be obtained from a
/// definitely successful [`PairingSession::abort_current`] result.
pub fn cleanup_after_confirmed_abort(
    _confirmation: ConfirmedAbort,
    path: &Path,
) -> Result<CleanedCredentialArtifact, PairingClientError> {
    cleanup_after_confirmed_abort_inner(path).map_err(PairingClientError::new)
}

/// Canonically loaded Active artifact for an authenticated host client.
///
/// This secret-bearing owner deliberately implements neither `Clone` nor
/// `Debug`; consuming it is the only way to take its zeroizing PSK.
///
/// ```compile_fail
/// use reticulum_device_pairing_client::ActivatedCredential;
///
/// fn require_debug<T: core::fmt::Debug>() {}
/// require_debug::<ActivatedCredential>();
/// ```
pub struct ActivatedCredential {
    device_id: DeviceId,
    credential_id: CredentialId,
    generation: CredentialGeneration,
    psk: Zeroizing<[u8; 32]>,
}

impl ActivatedCredential {
    /// Consume the artifact into identity fields and a zeroizing PSK owner.
    pub fn into_parts(
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

fn initialization_status_workflow(
    session: &mut PairingSession,
    deadline: Instant,
) -> Result<InitializationSummary, String> {
    let sequence = session.require_next()?;
    let response = exchange_control(
        session,
        ControlRequest::status(sequence),
        ExpectedControlResponse::Status,
        deadline,
    )
    .map_err(|error| error.message)?;
    Ok(initialization_summary(response))
}

fn initialize_workflow(
    session: &mut PairingSession,
    deadline: Instant,
    observer: &mut dyn ProgressObserver,
) -> Result<InitializationSummary, String> {
    let sequence = session.require_next()?;
    let response = initialize_workflow_with(
        sequence,
        |request| {
            let expected = match request {
                ControlRequest::Status { .. } => ExpectedControlResponse::Status,
                ControlRequest::Initialize { .. } => ExpectedControlResponse::Initialize,
            };
            exchange_control(session, request, expected, deadline).map_err(|error| error.message)
        },
        |last_sequence| wait_before_next(deadline, last_sequence, "initialization workflow"),
        observer,
    )?;
    Ok(initialization_summary(response))
}

fn initialize_workflow_with(
    mut sequence: u64,
    mut exchange_request: impl FnMut(ControlRequest) -> Result<ControlResponse, String>,
    mut wait_before_next_request: impl FnMut(u64) -> Result<(), String>,
    observer: &mut dyn ProgressObserver,
) -> Result<ControlResponse, String> {
    let mut status = exchange_request(ControlRequest::status(sequence))?;
    let mut announced_presence = false;
    loop {
        match status {
            ControlResponse::Status {
                status:
                    InitializationStatus::Completed
                    | InitializationStatus::Unavailable
                    | InitializationStatus::Blocked,
                ..
            } => {
                if matches!(
                    status,
                    ControlResponse::Status {
                        status: InitializationStatus::Completed,
                        ..
                    }
                ) {
                    observer.observe(PairingProgress::Initialized);
                }
                return Ok(status);
            }
            ControlResponse::Status {
                status: InitializationStatus::InFlight,
                ..
            } => {
                observer.observe(PairingProgress::Initializing);
                wait_before_next_request(sequence)?;
                sequence = next_usable_sequence(sequence).ok_or_else(|| {
                    format!(
                        "host request sequence exhausted after sequence {sequence}; {}",
                        connection_epoch_reset_guidance()
                    )
                })?;
                status = exchange_request(ControlRequest::status(sequence))?;
            }
            ControlResponse::Status {
                status: InitializationStatus::InitializationRequired,
                ..
            } => {
                sequence = next_usable_sequence(sequence).ok_or_else(|| {
                    format!(
                        "host request sequence exhausted after sequence {sequence}; {}",
                        connection_epoch_reset_guidance()
                    )
                })?;
                let response = exchange_request(ControlRequest::initialize(sequence))?;
                let ControlResponse::Initialize { result, .. } = response else {
                    return Err("device returned status response to initialize request".to_owned());
                };
                match result {
                    InitializeResult::Completed => {
                        observer.observe(PairingProgress::Initialized);
                        return Ok(response);
                    }
                    InitializeResult::Refused
                    | InitializeResult::Blocked
                    | InitializeResult::Unavailable => return Ok(response),
                    InitializeResult::PhysicalPresenceRequired => {
                        if !announced_presence {
                            observer.observe(PairingProgress::WaitingForPhysicalPresence(
                                PresenceOperation::Initialize,
                            ));
                            announced_presence = true;
                        }
                    }
                    InitializeResult::Retrying => {
                        observer.observe(PairingProgress::Initializing);
                    }
                }
                wait_before_next_request(sequence)?;
                sequence = next_usable_sequence(sequence).ok_or_else(|| {
                    format!(
                        "host request sequence exhausted after sequence {sequence}; {}",
                        connection_epoch_reset_guidance()
                    )
                })?;
                status = exchange_request(ControlRequest::status(sequence))?;
            }
            ControlResponse::Initialize { .. } => {
                return Err("device returned initialize response to status request".to_owned());
            }
        }
    }
}

fn initialization_summary(response: ControlResponse) -> InitializationSummary {
    match response {
        ControlResponse::Status { sequence, status } => InitializationSummary {
            sequence,
            response: InitializationResponse::Status(status.into()),
        },
        ControlResponse::Initialize { sequence, result } => InitializationSummary {
            sequence,
            response: InitializationResponse::Initialize(result.into()),
        },
    }
}

fn pair_workflow(
    session: &mut PairingSession,
    deadline: Instant,
    state_path: &Path,
    observer: &mut dyn ProgressObserver,
) -> Result<ActivationSummary, String> {
    let client_nonce = random_nonzero_nonce()?;
    let reservation = ReservedStateFile::reserve(state_path)?;
    let (begin_sequence, offer) = match begin_until_offered(session, deadline, observer) {
        Ok(offered) => offered,
        Err(error) if error.pending_may_exist => {
            let cleanup = match reservation.retain_ambiguous_begin_marker() {
                Ok(()) => String::new(),
                Err(cleanup) => format!("; staging cleanup failed: {cleanup}"),
            };
            return Err(format!(
                "{}; an owner-only reserved state marker remains at {} because a lost Begin offer may have committed Pending state{cleanup}; use a fresh confirmed connection epoch and physically confirmed abort-current before removing it",
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
    let (bearer, device_id, credential_id, generation, psk) = offer.into_parts();
    debug_assert_eq!(bearer, session.bearer);
    let state = reservation
        .commit_pending(device_id, credential_id, generation, &psk)
        .map_err(|error| {
            format!(
                "Pending credential was durably offered at Begin sequence {begin_sequence}, but host persistence did not complete cleanly: {error}; {}; keep every state artifact and assess it: resume only from a canonical complete Pending file, otherwise run physically confirmed abort-current before removing artifacts",
                next_sequence_guidance(begin_sequence)
            )
        })?;
    observer.observe(PairingProgress::PendingPersisted);
    complete_pairing(
        session,
        deadline,
        state,
        device_id,
        credential_id,
        generation,
        psk,
        client_nonce,
        false,
        observer,
    )
}

fn resume_workflow(
    session: &mut PairingSession,
    deadline: Instant,
    state_path: &Path,
    observer: &mut dyn ProgressObserver,
) -> Result<ActivationSummary, String> {
    let sequence = session.require_next()?;
    ensure_resume_headroom(sequence)?;
    let persisted = PendingStateFile::open_pending(state_path)?;
    let client_nonce = random_nonzero_nonce()?;
    complete_pairing(
        session,
        deadline,
        persisted.file,
        persisted.device_id,
        persisted.credential_id,
        persisted.generation,
        persisted.psk,
        client_nonce,
        true,
        observer,
    )
}

#[allow(clippy::too_many_arguments)]
fn complete_pairing(
    session: &mut PairingSession,
    deadline: Instant,
    state: PendingStateFile,
    device_id: DeviceId,
    credential_id: CredentialId,
    generation: CredentialGeneration,
    psk: PairingPsk,
    client_nonce: [u8; 32],
    resumed: bool,
    observer: &mut dyn ProgressObserver,
) -> Result<ActivationSummary, String> {
    let (proof_sequence, proof_request, challenge) = proof_until_challenged(
        session,
        deadline,
        credential_id,
        generation,
        client_nonce,
        &state.path,
        resumed,
        observer,
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
    observer.observe(PairingProgress::ProofChallengeAccepted);

    let activate_sequence = session.require_next()?;
    let client_proof = ClientProof::calculate(&psk, &transcript);
    let activate_request =
        ActivateRequest::new(activate_sequence, credential_id, generation, client_proof).map_err(
            |error| format!("could not construct canonical Activate request: {error:?}"),
        )?;
    let activation_state_path = state.path.clone();
    let state = state
        .mark_activation_ambiguous(device_id, credential_id, generation, &psk)
        .map_err(|error| {
            format!(
                "could not durably mark activation ambiguity before Activate: {error}; do not send or resume until the artifact at {} is classified",
                activation_state_path.display()
            )
        })?;
    observer.observe(PairingProgress::ActivationPrepared);

    let response = match exchange_pairing(
        session,
        PairingRequest::Activate(activate_request),
        ExpectedResponse::Activate,
        deadline,
    ) {
        Ok(response) => response,
        Err(error) if error.request_was_or_may_have_been_accepted => {
            return Err(format!(
                "{}; activation-ambiguous host state remains at {}; activation may have committed, so do not resume, guess Active, or abort until authenticated reconciliation",
                error.message,
                state.path.display()
            ));
        }
        Err(error) => {
            let path = state.path.clone();
            return match state.restore_pending(device_id, credential_id, generation, &psk) {
                Ok(_) => Err(format!(
                    "{}; Activate was not sent, and resume-safe Pending state remains at {}; retry from a confirmed sequence epoch",
                    error.message,
                    path.display()
                )),
                Err(restore) => Err(format!(
                    "{}; Activate was not sent, but restoring resume-safe Pending state at {} failed: {restore}; classify the artifact before recovery",
                    error.message,
                    path.display()
                )),
            };
        }
    };
    let activate = match response {
        PairingResponse::Activate(response) => response,
        _ => unreachable!("pairing exchange enforces response family"),
    };
    let activated_generation = match &activate {
        ActivateResponse::Activated { .. } => {
            if !activate.verify_confirmation(&psk, &transcript) {
                return Err(format!(
                    "device activation confirmation was invalid; activation-ambiguous state remains at {}; {}",
                    state.path.display(),
                    next_sequence_guidance(activate_sequence)
                ));
            }
            activate
                .generation()
                .expect("the Activated response always carries its durable generation")
        }
        ActivateResponse::Failure { failure, .. } => {
            let path = state.path.clone();
            return match state.restore_pending(device_id, credential_id, generation, &psk) {
                Ok(_) => Err(format!(
                    "Activate failed outcome={} code={}; resume-safe Pending state remains at {}; {}",
                    activate_failure_name(*failure),
                    failure.code(),
                    path.display(),
                    next_sequence_guidance(activate_sequence)
                )),
                Err(restore) => Err(format!(
                    "Activate failed outcome={} code={}, but restoring resume-safe Pending state at {} failed: {restore}; classify the artifact before recovery",
                    activate_failure_name(*failure),
                    failure.code(),
                    path.display()
                )),
            };
        }
    };

    state.mark_active(device_id, credential_id, activated_generation, &psk)?;
    observer.observe(PairingProgress::Activated);
    Ok(ActivationSummary {
        sequence: activate_sequence,
        device_id: *device_id.as_bytes(),
        credential_id: *credential_id.as_bytes(),
        generation: activated_generation.get(),
    })
}

#[allow(clippy::too_many_arguments)]
fn proof_until_challenged(
    session: &mut PairingSession,
    deadline: Instant,
    credential_id: CredentialId,
    generation: CredentialGeneration,
    mut client_nonce: [u8; 32],
    state_path: &Path,
    resumed: bool,
    observer: &mut dyn ProgressObserver,
) -> Result<(u64, ProofStartRequest, ProofChallenge), String> {
    let mut announced_presence = false;
    loop {
        let sequence = session.require_next()?;
        ensure_resume_headroom(sequence).map_err(|error| {
            format!("{error}; Pending state remains at {}", state_path.display())
        })?;
        let proof_request = ProofStartRequest::new(
            session.bearer,
            sequence,
            credential_id,
            generation,
            client_nonce,
        )
        .map_err(|error| format!("could not construct canonical ProofStart request: {error:?}"))?;
        let response = exchange_pairing(
            session,
            PairingRequest::ProofStart(proof_request),
            ExpectedResponse::ProofStart,
            deadline,
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
                    observer.observe(PairingProgress::WaitingForPhysicalPresence(
                        PresenceOperation::ProofStart,
                    ));
                    announced_presence = true;
                }
                wait_before_next(deadline, sequence, "pairing ProofStart").map_err(|error| {
                    format!("{error}; Pending state remains at {}", state_path.display())
                })?;
                let next = session.require_next().map_err(|error| {
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
            }
            PairingResponse::ProofStart(ProofStartResponse::Failure { failure, .. }) => {
                let reconciliation = if resumed {
                    " this may be a legacy Pending artifact left by an older client after ambiguous Activate; do not guess or abort;"
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
            _ => unreachable!("pairing exchange enforces response family"),
        }
    }
}

fn begin_until_offered(
    session: &mut PairingSession,
    deadline: Instant,
    observer: &mut dyn ProgressObserver,
) -> Result<(u64, BeginOffer), BeginWorkflowError> {
    let mut announced_presence = false;
    loop {
        let sequence = session
            .require_next()
            .map_err(|message| BeginWorkflowError {
                message,
                pending_may_exist: false,
            })?;
        ensure_pair_headroom(sequence).map_err(|message| BeginWorkflowError {
            message,
            pending_may_exist: false,
        })?;
        let response = exchange_pairing(
            session,
            PairingRequest::Begin(BeginRequest::new(sequence)),
            ExpectedResponse::Begin,
            deadline,
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
                    observer.observe(PairingProgress::WaitingForPhysicalPresence(
                        PresenceOperation::Begin,
                    ));
                    announced_presence = true;
                }
                wait_before_next(deadline, sequence, "pairing Begin").map_err(|message| {
                    BeginWorkflowError {
                        message,
                        pending_may_exist: false,
                    }
                })?;
            }
            PairingResponse::Begin(BeginResponse::Failure { failure, .. }) => {
                let next = session
                    .next_sequence
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
            _ => unreachable!("pairing exchange enforces response family"),
        }
    }
}

fn abort_workflow(
    session: &mut PairingSession,
    deadline: Instant,
    observer: &mut dyn ProgressObserver,
) -> Result<AbortSummary, String> {
    let mut announced_presence = false;
    loop {
        let sequence = session.require_next()?;
        let response = exchange_pairing(
            session,
            PairingRequest::AbortCurrent(AbortCurrentRequest::new(sequence)),
            ExpectedResponse::AbortCurrent,
            deadline,
        )
        .map_err(|error| error.message)?;
        let PairingResponse::AbortCurrent(response) = response else {
            unreachable!("pairing exchange enforces response family");
        };
        let result = response.result();
        if result == AbortResult::PhysicalPresenceRequired {
            if !announced_presence {
                observer.observe(PairingProgress::WaitingForPhysicalPresence(
                    PresenceOperation::AbortCurrent,
                ));
                announced_presence = true;
            }
            wait_before_next(deadline, sequence, "AbortCurrent")?;
            continue;
        }
        let outcome = AbortOutcome::from(result);
        if outcome.is_aborted() {
            observer.observe(PairingProgress::Aborted);
        }
        return Ok(AbortSummary { sequence, outcome });
    }
}

fn exchange_pairing(
    session: &mut PairingSession,
    request: PairingRequest,
    expected: ExpectedResponse,
    deadline: Instant,
) -> Result<PairingResponse, ExchangeError> {
    let sequence = request.sequence();
    if session.next_sequence != Some(sequence) {
        return Err(ExchangeError::before_send(format!(
            "request sequence {sequence} is not the session's exact-next sequence"
        )));
    }
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
    session.next_sequence = None;
    session.stream.write_all(frame.encoded()).map_err(|error| {
        ExchangeError::after_send(post_send_failure(
            "request write",
            &session.endpoint_name,
            &error,
            sequence,
        ))
    })?;
    session.stream.flush().map_err(|error| {
        ExchangeError::after_send(post_send_failure(
            "request flush",
            &session.endpoint_name,
            &error,
            sequence,
        ))
    })?;

    let mut bytes = Zeroizing::new([0_u8; 256]);
    while Instant::now() < deadline {
        match session.stream.read(&mut bytes[..]) {
            Ok(0) => {}
            Ok(length) => {
                for byte in &bytes[..length] {
                    let DecodeEvent::Record(record) = session.decoder.push(*byte) else {
                        continue;
                    };
                    let Ok(response) = PairingResponse::from_record(session.bearer, record) else {
                        continue;
                    };
                    if response.sequence() != sequence {
                        continue;
                    }
                    if response_matches(expected, &response) {
                        session.next_sequence = next_usable_sequence(sequence);
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
                    &session.endpoint_name,
                    &error,
                    sequence,
                )));
            }
        }
    }
    Err(ExchangeError::after_send(format!(
        "timed out waiting for sequence {sequence} on {}; {}",
        session.endpoint_name,
        sequence_ambiguity_guidance(sequence)
    )))
}

fn exchange_control(
    session: &mut PairingSession,
    request: ControlRequest,
    expected: ExpectedControlResponse,
    deadline: Instant,
) -> Result<ControlResponse, ExchangeError> {
    let sequence = request.sequence();
    if session.next_sequence != Some(sequence) {
        return Err(ExchangeError::before_send(format!(
            "request sequence {sequence} is not the session's exact-next sequence"
        )));
    }
    if Instant::now() >= deadline {
        return Err(ExchangeError::before_send(deadline_message(
            sequence,
            false,
            "initialization workflow",
        )));
    }
    let frame = FramedRecord::encode(&request.into_record()).map_err(|_| {
        ExchangeError::before_send("canonical request did not fit its fixed frame owner".to_owned())
    })?;
    session.next_sequence = None;
    session.stream.write_all(frame.encoded()).map_err(|error| {
        ExchangeError::after_send(post_send_failure(
            "request write",
            &session.endpoint_name,
            &error,
            sequence,
        ))
    })?;
    session.stream.flush().map_err(|error| {
        ExchangeError::after_send(post_send_failure(
            "request flush",
            &session.endpoint_name,
            &error,
            sequence,
        ))
    })?;

    let mut bytes = Zeroizing::new([0_u8; 256]);
    while Instant::now() < deadline {
        match session.stream.read(&mut bytes[..]) {
            Ok(0) => {}
            Ok(length) => {
                for byte in &bytes[..length] {
                    let DecodeEvent::Record(record) = session.decoder.push(*byte) else {
                        continue;
                    };
                    let Ok(response) = ControlResponse::from_record(record) else {
                        continue;
                    };
                    if response.sequence() != sequence {
                        continue;
                    }
                    if control_response_matches(expected, response) {
                        session.next_sequence = next_usable_sequence(sequence);
                        return Ok(response);
                    }
                    return Err(ExchangeError::after_send(format!(
                        "device returned the wrong initialization response family for sequence {sequence}"
                    )));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
            Err(error) => {
                return Err(ExchangeError::after_send(post_send_failure(
                    "response read",
                    &session.endpoint_name,
                    &error,
                    sequence,
                )));
            }
        }
    }
    Err(ExchangeError::after_send(format!(
        "timed out waiting for sequence {sequence} on {}; {}",
        session.endpoint_name,
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

const fn control_response_matches(
    expected: ExpectedControlResponse,
    response: ControlResponse,
) -> bool {
    matches!(
        (expected, response),
        (
            ExpectedControlResponse::Status,
            ControlResponse::Status { .. }
        ) | (
            ExpectedControlResponse::Initialize,
            ControlResponse::Initialize { .. }
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
        ensure_owner_only(&reservation, &path)?;
        ensure_single_link(&reservation, &path)?;
        verify_named_state_bytes(
            &mut staging_file,
            &staging_path,
            "Pending staging state",
            &bytes,
        )?;
        fs::rename(&staging_path, &path).map_err(|error| {
            format!(
                "could not atomically replace reserved state {} with complete staging file {}: {error}",
                path.display(),
                staging_path.display()
            )
        })?;
        ensure_replaced_destination_was_unlinked(&reservation, &path, "reserved state")?;
        verify_named_state_bytes(&mut staging_file, &path, "installed Pending state", &bytes)?;
        drop(reservation);
        sync_parent(&path).map_err(|error| {
            format!(
                "a complete Pending file may already exist at {} after atomic rename, but its directory entry could not be synchronized: {error}; keep it and assess it before resume or abort",
                path.display()
            )
        })?;
        verify_named_state_bytes(&mut staging_file, &path, "synced Pending state", &bytes)?;
        Ok(PendingStateFile {
            file: staging_file,
            path,
        })
    }
}

impl PendingStateFile {
    fn open_pending(path: &Path) -> Result<PersistedPendingState, String> {
        let mut file = open_existing_state(path)?;
        let decoded = read_decoded_artifact(&mut file, path)?;
        ensure_path_matches_file(&file, path, "Pending state")?;
        match decoded {
            DecodedArtifact::Reserved => Err(format!(
                "state file {} is only a reservation and contains no recoverable PSK; assess a possibly lost Begin offer and use physically confirmed abort-current",
                path.display()
            )),
            DecodedArtifact::Credential(credential) => match credential.classification {
                CredentialArtifactClassification::PendingResumeSafe => Ok(PersistedPendingState {
                    file: Self {
                        file,
                        path: path.to_path_buf(),
                    },
                    device_id: credential.device_id,
                    credential_id: credential.credential_id,
                    generation: credential.generation,
                    psk: credential.psk,
                }),
                CredentialArtifactClassification::ActivationAmbiguous => Err(format!(
                    "state file {} is activation-ambiguous and must not be resumed or aborted; authenticated reconciliation is required",
                    path.display()
                )),
                CredentialArtifactClassification::Active => Err(format!(
                    "state file {} is already Active and must not be paired again",
                    path.display()
                )),
                CredentialArtifactClassification::Missing
                | CredentialArtifactClassification::ReservedOrAmbiguousBegin
                | CredentialArtifactClassification::Invalid => {
                    unreachable!("decoded credential has one canonical secret-bearing state")
                }
            },
        }
    }

    fn mark_activation_ambiguous(
        self,
        device_id: DeviceId,
        credential_id: CredentialId,
        generation: CredentialGeneration,
        psk: &PairingPsk,
    ) -> Result<ActivationAmbiguousStateFile, String> {
        let Self { file, path } = self;
        let file = replace_secret_state(
            file,
            &path,
            "Pending state before activation marker",
            "activation-ambiguous",
            STATE_ACTIVATION_AMBIGUOUS,
            device_id,
            credential_id,
            generation,
            psk,
        )?;
        Ok(ActivationAmbiguousStateFile { file, path })
    }
}

impl ActivationAmbiguousStateFile {
    fn restore_pending(
        self,
        device_id: DeviceId,
        credential_id: CredentialId,
        generation: CredentialGeneration,
        psk: &PairingPsk,
    ) -> Result<PendingStateFile, String> {
        let Self { file, path } = self;
        let file = replace_secret_state(
            file,
            &path,
            "activation-ambiguous state before Pending restoration",
            "resume-safe Pending",
            STATE_PENDING,
            device_id,
            credential_id,
            generation,
            psk,
        )?;
        Ok(PendingStateFile { file, path })
    }

    fn mark_active(
        self,
        device_id: DeviceId,
        credential_id: CredentialId,
        activated_generation: CredentialGeneration,
        psk: &PairingPsk,
    ) -> Result<(), String> {
        let Self { file, path } = self;
        replace_secret_state(
            file,
            &path,
            "activation-ambiguous state before Active replacement",
            "Active",
            STATE_ACTIVE,
            device_id,
            credential_id,
            activated_generation,
            psk,
        )?;
        Ok(())
    }
}

/// Load one canonical owner-only Active credential artifact.
///
/// The returned secret owner implements neither `Clone` nor `Debug`.
pub fn load_activated_credential(path: &Path) -> Result<ActivatedCredential, PairingClientError> {
    load_activated_credential_inner(path).map_err(PairingClientError::new)
}

fn load_activated_credential_inner(path: &Path) -> Result<ActivatedCredential, String> {
    let mut file = open_existing_state(path)?;
    let decoded = read_decoded_artifact(&mut file, path)?;
    ensure_path_matches_file(&file, path, "Active state")?;
    match decoded {
        DecodedArtifact::Reserved => Err(format!(
            "state file {} is not Active and cannot authenticate a session",
            path.display()
        )),
        DecodedArtifact::Credential(credential)
            if credential.classification == CredentialArtifactClassification::Active =>
        {
            Ok(ActivatedCredential {
                device_id: credential.device_id,
                credential_id: credential.credential_id,
                generation: credential.generation,
                psk: credential.psk.into_bytes(),
            })
        }
        DecodedArtifact::Credential(_) => Err(format!(
            "state file {} is not Active and cannot authenticate a session",
            path.display()
        )),
    }
}

fn classify_existing_artifact(path: &Path) -> Result<CredentialArtifactClassification, String> {
    let mut file = open_existing_state(path)?;
    let decoded = read_decoded_artifact(&mut file, path)?;
    ensure_path_matches_file(&file, path, "credential artifact")?;
    Ok(match decoded {
        DecodedArtifact::Reserved => CredentialArtifactClassification::ReservedOrAmbiguousBegin,
        DecodedArtifact::Credential(credential) => credential.classification,
    })
}

fn cleanup_after_confirmed_abort_inner(path: &Path) -> Result<CleanedCredentialArtifact, String> {
    let mut file = open_existing_state(path)?;
    let decoded = read_decoded_artifact(&mut file, path)?;
    let cleaned = match &decoded {
        DecodedArtifact::Reserved => CleanedCredentialArtifact::ReservedOrAmbiguousBegin,
        DecodedArtifact::Credential(credential)
            if credential.classification == CredentialArtifactClassification::PendingResumeSafe =>
        {
            CleanedCredentialArtifact::PendingResumeSafe
        }
        DecodedArtifact::Credential(credential) => {
            return Err(format!(
                "credential artifact {} is {:?} and must not be removed after AbortCurrent",
                path.display(),
                credential.classification
            ));
        }
    };
    ensure_path_matches_file(
        &file,
        path,
        "credential artifact before confirmed-abort cleanup",
    )?;
    ensure_single_link(&file, path)?;
    drop(decoded);
    drop(file);
    fs::remove_file(path).map_err(|error| {
        format!(
            "could not remove credential artifact {} after confirmed abort: {error}",
            path.display()
        )
    })?;
    sync_parent(path)?;
    Ok(cleaned)
}

fn read_decoded_artifact(file: &mut File, path: &Path) -> Result<DecodedArtifact, String> {
    let length = file
        .metadata()
        .map_err(|error| format!("could not inspect state file {}: {error}", path.display()))?
        .len();
    if length != STATE_FILE_LENGTH as u64 {
        return Err(format!(
            "state file {} is not exactly {STATE_FILE_LENGTH} bytes",
            path.display()
        ));
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("could not rewind state file {}: {error}", path.display()))?;
    let mut bytes = Zeroizing::new([0_u8; STATE_FILE_LENGTH]);
    file.read_exact(&mut bytes[..])
        .map_err(|error| format!("could not read state file {}: {error}", path.display()))?;
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
    let state = bytes[STATE_STATUS_OFFSET as usize];
    if state == STATE_RESERVED {
        if bytes[16..88].iter().any(|byte| *byte != 0) {
            return Err(format!(
                "state file {} is a noncanonical reservation containing credential material",
                path.display()
            ));
        }
        return Ok(DecodedArtifact::Reserved);
    }
    let classification = match state {
        STATE_PENDING => CredentialArtifactClassification::PendingResumeSafe,
        STATE_ACTIVE => CredentialArtifactClassification::Active,
        STATE_ACTIVATION_AMBIGUOUS => CredentialArtifactClassification::ActivationAmbiguous,
        unknown => {
            return Err(format!(
                "state file {} has unknown state {unknown}",
                path.display()
            ));
        }
    };
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
    let mut psk_bytes = Zeroizing::new([0_u8; 32]);
    psk_bytes.copy_from_slice(&bytes[56..88]);
    let psk = PairingPsk::from_zeroizing(psk_bytes)
        .map_err(|_| format!("state file {} has an invalid zero PSK", path.display()))?;
    Ok(DecodedArtifact::Credential(DecodedCredential {
        classification,
        device_id,
        credential_id,
        generation,
        psk,
    }))
}

#[allow(clippy::too_many_arguments)]
fn replace_secret_state(
    file: File,
    path: &Path,
    current_purpose: &str,
    target_purpose: &str,
    target_state: u8,
    device_id: DeviceId,
    credential_id: CredentialId,
    generation: CredentialGeneration,
    psk: &PairingPsk,
) -> Result<File, String> {
    ensure_path_matches_file(&file, path, current_purpose)?;
    let (mut staging_file, staging_path) = create_staging_file(path).map_err(|error| {
        format!(
            "could not create owner-only {target_purpose} staging file for {}: {error}",
            path.display()
        )
    })?;
    let expected = encode_state(
        target_state,
        Some((device_id, credential_id, generation, psk)),
    );
    if let Err(error) = write_sync_and_verify(&mut staging_file, &staging_path, &expected) {
        drop(staging_file);
        let cleanup = fs::remove_file(&staging_path)
            .and_then(|()| sync_parent(path).map_err(io::Error::other));
        return Err(format!(
            "{error}; {target_purpose} staging cleanup for {} {}",
            staging_path.display(),
            if cleanup.is_ok() {
                "completed"
            } else {
                "failed"
            }
        ));
    }
    if let Err(error) = ensure_path_matches_file(&file, path, current_purpose)
        .and_then(|()| ensure_owner_only(&file, path))
        .and_then(|()| ensure_single_link(&file, path))
        .and_then(|()| {
            verify_named_state_bytes(
                &mut staging_file,
                &staging_path,
                &format!("{target_purpose} staging state"),
                &expected,
            )
        })
    {
        drop(staging_file);
        let cleanup = fs::remove_file(&staging_path)
            .and_then(|()| sync_parent(path).map_err(io::Error::other));
        return Err(format!(
            "{error}; {target_purpose} staging cleanup for {} {}",
            staging_path.display(),
            if cleanup.is_ok() {
                "completed"
            } else {
                "failed"
            }
        ));
    }
    fs::rename(&staging_path, path).map_err(|error| {
        format!(
            "could not atomically replace {} with complete {target_purpose} staging file {}: {error}; preserve both files and classify them",
            path.display(),
            staging_path.display()
        )
    })?;
    ensure_replaced_destination_was_unlinked(&file, path, current_purpose)?;
    verify_named_state_bytes(
        &mut staging_file,
        path,
        &format!("installed {target_purpose} state"),
        &expected,
    )?;
    drop(file);
    sync_parent(path).map_err(|error| {
        format!(
            "a complete {target_purpose} file may be installed at {}, but its directory entry could not be synchronized: {error}; classify it before recovery",
            path.display()
        )
    })?;
    verify_named_state_bytes(
        &mut staging_file,
        path,
        &format!("synced {target_purpose} state"),
        &expected,
    )?;
    let observed = read_decoded_artifact(&mut staging_file, path).map_err(|error| {
        format!(
            "{target_purpose} replacement at {} did not decode after installation: {error}",
            path.display()
        )
    })?;
    let observed_classification = match observed {
        DecodedArtifact::Reserved => CredentialArtifactClassification::ReservedOrAmbiguousBegin,
        DecodedArtifact::Credential(credential) => credential.classification,
    };
    let expected_classification = match target_state {
        STATE_PENDING => CredentialArtifactClassification::PendingResumeSafe,
        STATE_ACTIVE => CredentialArtifactClassification::Active,
        STATE_ACTIVATION_AMBIGUOUS => CredentialArtifactClassification::ActivationAmbiguous,
        _ => unreachable!("secret replacement target is a known state"),
    };
    if observed_classification != expected_classification {
        return Err(format!(
            "{target_purpose} replacement at {} read back with the wrong state",
            path.display()
        ));
    }
    ensure_path_matches_file(&staging_file, path, target_purpose)?;
    ensure_single_link(&staging_file, path)?;
    Ok(staging_file)
}

#[cfg(unix)]
fn ensure_single_link(file: &File, path: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt as _;

    let links = file
        .metadata()
        .map_err(|error| format!("could not inspect state file {}: {error}", path.display()))?
        .nlink();
    if links != 1 {
        return Err(format!(
            "credential artifact {} has {links} hard links and cannot be safely used",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn ensure_replaced_destination_was_unlinked(
    replaced: &File,
    path: &Path,
    purpose: &str,
) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt as _;

    let links = replaced
        .metadata()
        .map_err(|error| {
            format!(
                "could not inspect replaced {purpose} descriptor for {}: {error}",
                path.display()
            )
        })?
        .nlink();
    if links != 0 {
        return Err(format!(
            "replaced {purpose} descriptor for {} still has {links} links; the destination changed during atomic replacement",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_single_link(_file: &File, _path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(not(unix))]
fn ensure_replaced_destination_was_unlinked(
    _replaced: &File,
    _path: &Path,
    _purpose: &str,
) -> Result<(), String> {
    Ok(())
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

fn verify_named_state_bytes(
    file: &mut File,
    path: &Path,
    purpose: &str,
    expected: &[u8; STATE_FILE_LENGTH],
) -> Result<(), String> {
    ensure_path_matches_file(file, path, purpose)?;
    ensure_owner_only(file, path)?;
    ensure_single_link(file, path)?;
    let length = file
        .metadata()
        .map_err(|error| format!("could not inspect {purpose} {}: {error}", path.display()))?
        .len();
    if length != STATE_FILE_LENGTH as u64 {
        return Err(format!(
            "{purpose} {} is not exactly {STATE_FILE_LENGTH} bytes",
            path.display()
        ));
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("could not rewind {purpose} {}: {error}", path.display()))?;
    let mut observed = Zeroizing::new([0_u8; STATE_FILE_LENGTH]);
    file.read_exact(&mut observed[..])
        .map_err(|error| format!("could not read {purpose} {}: {error}", path.display()))?;
    if observed.as_ref() != expected {
        return Err(format!(
            "{purpose} {} did not read back byte-for-byte",
            path.display()
        ));
    }
    ensure_path_matches_file(file, path, purpose)?;
    ensure_single_link(file, path)
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
    if let Err(error) = ensure_path_matches_file(&file, path, purpose)
        .and_then(|()| ensure_owner_only(&file, path))
        .and_then(|()| ensure_single_link(&file, path))
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
    ensure_single_link(&file, path)?;
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
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let metadata = file
        .metadata()
        .map_err(|error| format!("could not inspect state file {}: {error}", path.display()))?;
    let mode = metadata.permissions().mode();
    if mode & 0o077 != 0 || mode & 0o600 != 0o600 {
        return Err(format!(
            "state file {} must be owner-readable/writable with no group or other permissions; observed mode {:04o}",
            path.display(),
            mode & 0o7777
        ));
    }
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(format!(
            "state file {} must be owned by the effective user",
            path.display()
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

fn ensure_usable_sequence(sequence: u64) -> Result<(), String> {
    if sequence == u64::MAX {
        Err(format!(
            "request sequence must be less than {}; the firmware refuses the maximum and exhausts the epoch",
            u64::MAX
        ))
    } else {
        Ok(())
    }
}

fn ensure_pair_headroom(begin_sequence: u64) -> Result<(), String> {
    match begin_sequence.checked_add(2) {
        Some(activate_sequence) if activate_sequence < u64::MAX => Ok(()),
        _ => Err(format!(
            "pair requires usable sequences for Begin, ProofStart, and Activate, but Begin sequence {begin_sequence} leaves insufficient exact-next headroom; no Begin was sent at this sequence; {}",
            connection_epoch_reset_guidance()
        )),
    }
}

fn ensure_resume_headroom(proof_sequence: u64) -> Result<(), String> {
    match proof_sequence.checked_add(1) {
        Some(activate_sequence) if activate_sequence < u64::MAX => Ok(()),
        _ => Err(format!(
            "resume requires usable sequences for ProofStart and Activate, but ProofStart sequence {proof_sequence} leaves insufficient exact-next headroom; no ProofStart was sent at this sequence; {}",
            connection_epoch_reset_guidance()
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
            connection_epoch_reset_guidance()
        )
    } else {
        format!(
            "{workflow} timed out before sending sequence {last_sequence}; {}",
            connection_epoch_reset_guidance()
        )
    }
}

fn next_sequence_guidance(last_sequence: u64) -> String {
    let next = next_usable_sequence(last_sequence)
        .map_or_else(|| "exhausted".to_owned(), |value| value.to_string());
    format!(
        "next_sequence={next}; {}",
        connection_epoch_reset_guidance()
    )
}

fn sequence_ambiguity_guidance(last_sequence: u64) -> String {
    format!(
        "last_sent_sequence={last_sequence} is consumed-or-ambiguous because the device may have \
         accepted it before its response was lost; {}",
        connection_epoch_reset_guidance()
    )
}

fn post_send_failure(
    operation: &str,
    endpoint_name: &str,
    error: &io::Error,
    sequence: u64,
) -> String {
    format!(
        "{operation} failed on {endpoint_name}: {error}; {}",
        sequence_ambiguity_guidance(sequence)
    )
}

const fn connection_epoch_reset_guidance() -> &'static str {
    "opening or closing a transport stream does not start a new sequence epoch; confirm the device created a fresh accepted-connection epoch before restarting at sequence 0"
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

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        fs,
        sync::{
            Arc, Mutex,
            atomic::{AtomicU64, Ordering},
        },
    };

    #[cfg(unix)]
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    use super::*;

    static NEXT_PATH: AtomicU64 = AtomicU64::new(0);
    const USB_PROOF_REQUEST_WIRE: &str = "0007524441310126010101010101010101010101010101010102010101010101010240020101010202020139101112131415161718191a1b1c1d1e1f0807060504030201404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f0101010101010101010101010101010100";

    struct InMemoryDuplex {
        inbound: VecDeque<u8>,
        written: Arc<Mutex<Vec<u8>>>,
    }

    impl Read for InMemoryDuplex {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            if self.inbound.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "scripted inbound stream is empty",
                ));
            }
            let length = output.len().min(self.inbound.len());
            for slot in &mut output[..length] {
                *slot = self
                    .inbound
                    .pop_front()
                    .expect("length is bounded by the inbound queue");
            }
            Ok(length)
        }
    }

    impl Write for InMemoryDuplex {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            self.written
                .lock()
                .expect("duplex write capture mutex must not be poisoned")
                .extend_from_slice(input);
            Ok(input.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn ascending<const N: usize>(start: u8) -> [u8; N] {
        let mut bytes = [0_u8; N];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = start.wrapping_add(index as u8);
        }
        bytes
    }

    fn proof_exchange_wire(bearer: BearerBinding) -> Vec<u8> {
        let sequence = 1;
        let credential_id = CredentialId::new(ascending::<16>(0x10));
        let generation = CredentialGeneration::new(0x0102_0304_0506_0708);
        let challenge = reticulum_device_api_pairing::ProofChallenge::new(
            bearer,
            DeviceId::new([0x44; 16]).unwrap(),
            reticulum_device_api_pairing::ConnectionId::new(7).unwrap(),
            reticulum_device_api_pairing::WindowId::new(9).unwrap(),
            credential_id,
            generation,
            reticulum_device_api_pairing::DeviceChallenge::new([0x55; 32]).unwrap(),
        )
        .unwrap();
        let response =
            PairingResponse::ProofStart(ProofStartResponse::challenge(sequence, challenge));
        let response_frame = FramedRecord::encode(&response.into_record()).unwrap();
        let written = Arc::new(Mutex::new(Vec::new()));
        let stream = InMemoryDuplex {
            inbound: response_frame.encoded().iter().copied().collect(),
            written: Arc::clone(&written),
        };
        let mut session = PairingSession::from_stream(
            stream,
            "in-memory-duplex",
            bearer,
            sequence,
            Duration::from_secs(1),
        )
        .unwrap();
        let request = ProofStartRequest::new(
            bearer,
            sequence,
            credential_id,
            generation,
            ascending::<32>(0x40),
        )
        .unwrap();
        let response = exchange_pairing(
            &mut session,
            PairingRequest::ProofStart(request),
            ExpectedResponse::ProofStart,
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap_or_else(|error| panic!("{}", error.message));
        assert!(matches!(
            response,
            PairingResponse::ProofStart(ProofStartResponse::Challenge {
                challenge,
                ..
            }) if challenge.bearer() == bearer
        ));
        assert_eq!(session.bearer(), bearer);
        assert_eq!(session.endpoint_name(), "in-memory-duplex");
        assert_eq!(session.next_sequence(), Some(sequence + 1));
        written
            .lock()
            .expect("duplex write capture mutex must not be poisoned")
            .clone()
    }

    #[test]
    fn usb_stream_session_preserves_the_independent_proof_request_vector() {
        assert_eq!(
            proof_exchange_wire(BearerBinding::UsbSerialJtag),
            hex::decode(USB_PROOF_REQUEST_WIRE).unwrap()
        );
    }

    #[test]
    fn ble_stream_session_emits_and_accepts_bearer_code_two() {
        let wire = proof_exchange_wire(BearerBinding::BleGatt);
        let mut decoder = StreamDecoder::new();
        let record = wire
            .into_iter()
            .find_map(|byte| match decoder.push(byte) {
                DecodeEvent::Record(record) => Some(record),
                DecodeEvent::Pending => None,
                DecodeEvent::MalformedCobs | DecodeEvent::MalformedRecord(_) => {
                    panic!("pairing session emitted a malformed BLE record")
                }
                DecodeEvent::Overflow => panic!("pairing session emitted an overlong BLE record"),
            })
            .expect("one complete BLE pairing request");
        assert_eq!(record.payload()[6], BearerBinding::BleGatt.code());
        assert!(matches!(
            PairingRequest::from_record(BearerBinding::BleGatt, record),
            Ok(PairingRequest::ProofStart(request))
                if request.bearer() == BearerBinding::BleGatt
        ));
    }

    fn temporary_path() -> PathBuf {
        let serial = NEXT_PATH.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "reticulum-device-pairing-state-{}-{serial}.bin",
            std::process::id()
        ))
    }

    fn confirmed_abort() -> ConfirmedAbort {
        AbortSummary {
            sequence: 21,
            outcome: AbortOutcome::Aborted,
        }
        .into_confirmed_abort()
        .unwrap()
    }

    #[test]
    fn response_family_matcher_is_exact() {
        let begin = PairingResponse::Begin(BeginResponse::failure(1, PairingFailure::Unavailable));
        assert!(response_matches(ExpectedResponse::Begin, &begin));
        assert!(!response_matches(ExpectedResponse::Activate, &begin));
    }

    #[test]
    fn initialize_workflow_advances_exact_next_across_presence_and_retry_polling() {
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
        let mut progress = Vec::new();
        let response = initialize_workflow_with(
            10,
            |request| {
                let (expected, response) = scripted.pop_front().expect("unexpected request");
                assert_eq!(request, expected);
                Ok(response)
            },
            |last_sequence| {
                waits.push(last_sequence);
                Ok(())
            },
            &mut |event| progress.push(event),
        )
        .unwrap();

        assert_eq!(
            response,
            ControlResponse::status(15, InitializationStatus::Completed)
        );
        assert_eq!(waits, [11, 13, 14]);
        assert_eq!(
            progress
                .iter()
                .filter(|event| {
                    **event
                        == PairingProgress::WaitingForPhysicalPresence(
                            PresenceOperation::Initialize,
                        )
                })
                .count(),
            1
        );
        assert!(progress.contains(&PairingProgress::Initialized));
        assert!(scripted.is_empty());
    }

    #[test]
    fn sequence_space_refuses_the_firmware_exhaustion_sentinel() {
        assert_eq!(next_usable_sequence(u64::MAX - 1), None);
        assert_eq!(next_usable_sequence(u64::MAX - 2), Some(u64::MAX - 1));
        assert!(ensure_usable_sequence(u64::MAX).is_err());
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
        assert!(error.to_string().contains("is not Active"));

        let persisted = PendingStateFile::open_pending(&path).unwrap();
        assert_eq!(persisted.device_id, device_id);
        assert_eq!(persisted.credential_id, credential_id);
        assert_eq!(persisted.generation, generation);
        assert_eq!(persisted.psk.as_bytes(), psk.as_bytes());
        drop(persisted);

        let pending_inode = fs::metadata(&path).unwrap().ino();
        assert_eq!(state.file.metadata().unwrap().ino(), pending_inode);
        let ambiguous = state
            .mark_activation_ambiguous(device_id, credential_id, generation, &psk)
            .unwrap();
        assert_eq!(
            classify_credential_artifact(&path),
            CredentialArtifactClassification::ActivationAmbiguous
        );
        assert_eq!(fs::read(&path).unwrap()[10], STATE_ACTIVATION_AMBIGUOUS);
        let ambiguous_inode = fs::metadata(&path).unwrap().ino();
        assert_ne!(ambiguous_inode, pending_inode);
        let error = PendingStateFile::open_pending(&path).err().unwrap();
        assert!(error.contains("activation-ambiguous"));
        ambiguous
            .mark_active(device_id, credential_id, activated_generation, &psk)
            .unwrap();
        let active = fs::read(&path).unwrap();
        assert_eq!(active.len(), STATE_FILE_LENGTH);
        assert_eq!(active[10], STATE_ACTIVE);
        assert_eq!(&active[48..56], &activated_generation.get().to_le_bytes());
        assert_eq!(&active[56..88], psk.as_bytes());
        assert_ne!(fs::metadata(&path).unwrap().ino(), ambiguous_inode);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
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
    }

    #[cfg(unix)]
    #[test]
    fn hard_linked_pending_is_invalid_and_cannot_cross_the_activation_boundary() {
        let path = temporary_path();
        let alias = path.with_extension("pending-hard-link");
        let device_id = DeviceId::new([0x31; 16]).unwrap();
        let credential_id = CredentialId::new([0x32; 16]);
        let generation = CredentialGeneration::new(4);
        let psk = PairingPsk::new([0x33; 32]).unwrap();
        let pending = ReservedStateFile::reserve(&path)
            .unwrap()
            .commit_pending(device_id, credential_id, generation, &psk)
            .unwrap();
        fs::hard_link(&path, &alias).unwrap();

        assert_eq!(
            classify_credential_artifact(&path),
            CredentialArtifactClassification::Invalid
        );
        assert_eq!(
            classify_credential_artifact(&alias),
            CredentialArtifactClassification::Invalid
        );
        assert!(PendingStateFile::open_pending(&path).is_err());
        assert!(
            pending
                .mark_activation_ambiguous(device_id, credential_id, generation, &psk)
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap()[10], STATE_PENDING);
        assert_eq!(fs::read(&alias).unwrap()[10], STATE_PENDING);

        fs::remove_file(path).unwrap();
        fs::remove_file(alias).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn activation_marker_is_conservative_and_definite_no_send_can_restore_pending() {
        let path = temporary_path();
        let device_id = DeviceId::new([0x41; 16]).unwrap();
        let credential_id = CredentialId::new([0x42; 16]);
        let generation = CredentialGeneration::new(9);
        let psk = PairingPsk::new([0x43; 32]).unwrap();
        let pending = ReservedStateFile::reserve(&path)
            .unwrap()
            .commit_pending(device_id, credential_id, generation, &psk)
            .unwrap();
        assert_eq!(
            classify_credential_artifact(&path),
            CredentialArtifactClassification::PendingResumeSafe
        );
        let ambiguous = pending
            .mark_activation_ambiguous(device_id, credential_id, generation, &psk)
            .unwrap();
        assert_eq!(
            classify_credential_artifact(&path),
            CredentialArtifactClassification::ActivationAmbiguous
        );
        assert!(PendingStateFile::open_pending(&path).is_err());

        let pending = ambiguous
            .restore_pending(device_id, credential_id, generation, &psk)
            .unwrap();
        drop(pending);
        assert_eq!(
            classify_credential_artifact(&path),
            CredentialArtifactClassification::PendingResumeSafe
        );
        assert!(PendingStateFile::open_pending(&path).is_ok());
        fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn confirmed_abort_cleanup_removes_only_reserved_or_resume_safe_pending() {
        for outcome in [
            AbortOutcome::PhysicalPresenceRequired,
            AbortOutcome::Refused,
            AbortOutcome::Blocked,
            AbortOutcome::Unavailable,
        ] {
            assert!(
                AbortSummary {
                    sequence: 20,
                    outcome,
                }
                .into_confirmed_abort()
                .is_none()
            );
        }

        let reserved_path = temporary_path();
        let reservation = ReservedStateFile::reserve(&reserved_path).unwrap();
        reservation.retain_ambiguous_begin_marker().unwrap();
        assert_eq!(
            classify_credential_artifact(&reserved_path),
            CredentialArtifactClassification::ReservedOrAmbiguousBegin
        );
        assert_eq!(
            cleanup_after_confirmed_abort(confirmed_abort(), &reserved_path).unwrap(),
            CleanedCredentialArtifact::ReservedOrAmbiguousBegin
        );
        assert_eq!(
            classify_credential_artifact(&reserved_path),
            CredentialArtifactClassification::Missing
        );

        let pending_path = temporary_path();
        let psk = PairingPsk::new([0x53; 32]).unwrap();
        let pending = ReservedStateFile::reserve(&pending_path)
            .unwrap()
            .commit_pending(
                DeviceId::new([0x51; 16]).unwrap(),
                CredentialId::new([0x52; 16]),
                CredentialGeneration::new(3),
                &psk,
            )
            .unwrap();
        drop(pending);
        assert_eq!(
            cleanup_after_confirmed_abort(confirmed_abort(), &pending_path).unwrap(),
            CleanedCredentialArtifact::PendingResumeSafe
        );
        assert_eq!(
            classify_credential_artifact(&pending_path),
            CredentialArtifactClassification::Missing
        );
    }

    #[cfg(unix)]
    #[test]
    fn confirmed_abort_cleanup_refuses_ambiguous_active_and_multiply_linked_files() {
        let ambiguous_path = temporary_path();
        let device_id = DeviceId::new([0x61; 16]).unwrap();
        let credential_id = CredentialId::new([0x62; 16]);
        let generation = CredentialGeneration::new(5);
        let psk = PairingPsk::new([0x63; 32]).unwrap();
        let ambiguous = ReservedStateFile::reserve(&ambiguous_path)
            .unwrap()
            .commit_pending(device_id, credential_id, generation, &psk)
            .unwrap()
            .mark_activation_ambiguous(device_id, credential_id, generation, &psk)
            .unwrap();
        drop(ambiguous);
        assert!(cleanup_after_confirmed_abort(confirmed_abort(), &ambiguous_path).is_err());
        assert!(ambiguous_path.exists());
        fs::remove_file(&ambiguous_path).unwrap();

        let active_path = temporary_path();
        ReservedStateFile::reserve(&active_path)
            .unwrap()
            .commit_pending(device_id, credential_id, generation, &psk)
            .unwrap()
            .mark_activation_ambiguous(device_id, credential_id, generation, &psk)
            .unwrap()
            .mark_active(device_id, credential_id, CredentialGeneration::new(7), &psk)
            .unwrap();
        assert_eq!(
            classify_credential_artifact(&active_path),
            CredentialArtifactClassification::Active
        );
        assert!(cleanup_after_confirmed_abort(confirmed_abort(), &active_path).is_err());
        assert!(active_path.exists());
        fs::remove_file(&active_path).unwrap();

        let linked_path = temporary_path();
        let linked_alias = linked_path.with_extension("alias");
        let pending = ReservedStateFile::reserve(&linked_path)
            .unwrap()
            .commit_pending(device_id, credential_id, generation, &psk)
            .unwrap();
        drop(pending);
        fs::hard_link(&linked_path, &linked_alias).unwrap();
        assert!(cleanup_after_confirmed_abort(confirmed_abort(), &linked_path).is_err());
        assert!(linked_path.exists());
        fs::remove_file(linked_path).unwrap();
        fs::remove_file(linked_alias).unwrap();
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
        assert_eq!(
            classify_credential_artifact(&path),
            CredentialArtifactClassification::Invalid
        );
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
    fn public_names_never_include_secret_material() {
        assert_eq!(
            pairing_failure_name(PairingFailure::PhysicalPresenceRequired),
            "physical-presence-required"
        );
        assert_eq!(AbortOutcome::Aborted.name(), "aborted");
    }
}
