//! First-run device initialization and pairing owned outside the browser.

use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock, mpsc};
use std::thread;
use std::time::Duration;

use reticulum_device_pairing_client::{
    CredentialArtifactClassification, PairingClient, PairingProgress, PresenceOperation,
    classify_credential_artifact, cleanup_after_confirmed_abort,
};
use serde::Serialize;
use tokio::sync::{oneshot, watch};
use ts_rs::TS;

use crate::bindings::{JsonSafeInteger, serialize_json_safe_u64};
use crate::profile::DeviceProfile;
use crate::serial::{SerialConnectionGate, discover_usb_serials, resolve_usb_port};

const COMMAND_CAPACITY: usize = 4;
const COMMAND_WAIT: Duration = Duration::from_millis(100);

/// Browser-safe phase of one exact device's first-run lifecycle.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum OnboardingState {
    /// No host credential artifact exists and a new pairing may start.
    NeedsPairing,
    /// A lost Begin may have created device Pending state; only an explicit,
    /// physically confirmed abort is safe.
    AbortRequired,
    /// A canonical host Pending artifact can be resumed or explicitly aborted.
    ResumeAvailable,
    /// Activate may have committed; automatic recovery would be unsafe.
    ActivationAmbiguous,
    /// An Active credential exists and ordinary authentication may proceed.
    CredentialReady,
    /// A blocking pre-authentication workflow owns the serial connection.
    Working {
        /// Coarse, secret-free operation stage.
        stage: OnboardingStage,
    },
    /// Activation completed; a real USB disappearance and reappearance is
    /// required before the authenticated client starts a fresh epoch.
    AwaitingUsbReset {
        /// Whether disappearance has already been observed.
        observed_disconnect: bool,
    },
    /// Local state or the selected device needs operator attention.
    Faulted {
        /// Browser-safe failure category; detailed diagnostics remain local.
        reason: OnboardingFault,
    },
    /// The onboarding owner has stopped.
    Stopped,
}

/// Secret-free progress stage shown during explicit onboarding.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum OnboardingStage {
    /// Resolving and opening the selected USB interface.
    OpeningDevice,
    /// Reading the public initialization state.
    CheckingInitialization,
    /// Waiting for the user to release, then hold GPIO21 for two seconds.
    WaitingForInitializationPresence,
    /// Credential storage is being initialized or polled.
    Initializing,
    /// Waiting for physical presence for Begin or ProofStart.
    WaitingForPairingPresence,
    /// A Pending credential is durably retained and proof is in progress.
    Proving,
    /// Activate is being attempted after durable ambiguity marking.
    Activating,
    /// Waiting for physical presence before an explicit abort.
    WaitingForAbortPresence,
    /// A Pending credential is being explicitly resumed.
    Resuming,
}

/// Browser-safe onboarding failure category.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum OnboardingFault {
    /// The configured credential artifact is malformed or unsafe to open.
    InvalidCredentialArtifact,
    /// The exact selected USB device is not currently available.
    DeviceUnavailable,
    /// Initialization or pairing did not complete; local recovery state was
    /// reclassified before this category was published.
    ProtocolOrPersistenceFailure,
    /// The command is not valid for the current durable recovery state.
    InvalidRecoveryAction,
}

/// Immutable browser-safe onboarding projection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct OnboardingSnapshot {
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    revision: u64,
    usb_serial: String,
    lifecycle: OnboardingState,
}

impl OnboardingSnapshot {
    /// Monotonic in-process onboarding revision.
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Canonical stable USB descriptor serial selected outside the browser.
    pub fn usb_serial(&self) -> &str {
        &self.usb_serial
    }

    /// Current public lifecycle state.
    pub const fn lifecycle(&self) -> &OnboardingState {
        &self.lifecycle
    }
}

/// Fixed first-run policy for one selected profile and USB descriptor serial.
#[derive(Clone, Debug)]
pub struct OnboardingConfig {
    profile: DeviceProfile,
    explicit_port: Option<String>,
    connection_gate: SerialConnectionGate,
}

impl OnboardingConfig {
    /// Bind onboarding to one already-created private device profile.
    pub fn new(profile: DeviceProfile, connection_gate: SerialConnectionGate) -> Self {
        Self {
            profile,
            explicit_port: None,
            connection_gate,
        }
    }

    /// Retain an identity-checked explicit port for diagnostics.
    #[must_use]
    pub fn with_explicit_port(mut self, explicit_port: Option<String>) -> Self {
        self.explicit_port = explicit_port;
        self
    }
}

enum Command {
    Start(oneshot::Sender<Result<(), String>>),
    Resume(oneshot::Sender<Result<(), String>>),
    Abort(oneshot::Sender<Result<(), String>>),
    Refresh(oneshot::Sender<Result<(), String>>),
    Shutdown,
}

/// Bounded onboarding-handle failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OnboardingError {
    /// Command queue is currently full.
    Busy,
    /// Owner thread stopped before accepting or answering the command.
    Stopped,
    /// Current durable state does not permit the requested action.
    Operation(String),
}

impl fmt::Display for OnboardingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => formatter.write_str("onboarding command queue is busy"),
            Self::Stopped => formatter.write_str("onboarding service has stopped"),
            Self::Operation(error) => formatter.write_str(error),
        }
    }
}

impl std::error::Error for OnboardingError {}

/// Cloneable command and immutable-snapshot handle for first-run pairing.
#[derive(Clone)]
pub struct OnboardingHandle {
    commands: mpsc::SyncSender<Command>,
    snapshot: Arc<RwLock<Arc<OnboardingSnapshot>>>,
    terminated: watch::Receiver<bool>,
}

impl OnboardingHandle {
    /// Read the latest browser-safe state without contacting the owner.
    pub fn snapshot(&self) -> Arc<OnboardingSnapshot> {
        self.snapshot
            .read()
            .expect("onboarding snapshot lock must not be poisoned")
            .clone()
    }

    /// Begin initialization and a new pairing. Accepted work continues on the
    /// owner thread after this method returns.
    pub async fn start(&self) -> Result<(), OnboardingError> {
        self.request(Command::Start).await
    }

    /// Resume a canonical resume-safe Pending artifact.
    pub async fn resume(&self) -> Result<(), OnboardingError> {
        self.request(Command::Resume).await
    }

    /// Physically confirm AbortCurrent and remove only a safely removable host
    /// artifact after the device reports a definite durable abort.
    pub async fn abort(&self) -> Result<(), OnboardingError> {
        self.request(Command::Abort).await
    }

    /// Reclassify the configured credential path.
    pub async fn refresh(&self) -> Result<(), OnboardingError> {
        self.request(Command::Refresh).await
    }

    /// Ask the owner to stop without waiting for thread termination.
    pub fn shutdown(&self) -> Result<(), OnboardingError> {
        self.commands
            .try_send(Command::Shutdown)
            .map_err(map_try_send)
    }

    /// Stop the owner and wait for its final snapshot.
    pub async fn shutdown_and_wait(&self) -> Result<(), OnboardingError> {
        let mut terminated = self.terminated.clone();
        if *terminated.borrow() {
            return Ok(());
        }
        loop {
            match self.shutdown() {
                Ok(()) => break,
                Err(OnboardingError::Busy) => tokio::time::sleep(COMMAND_WAIT).await,
                Err(error) => return Err(error),
            }
        }
        while !*terminated.borrow_and_update() {
            terminated
                .changed()
                .await
                .map_err(|_| OnboardingError::Stopped)?;
        }
        Ok(())
    }

    async fn request(
        &self,
        command: impl FnOnce(oneshot::Sender<Result<(), String>>) -> Command,
    ) -> Result<(), OnboardingError> {
        let (reply, receive) = oneshot::channel();
        self.commands
            .try_send(command(reply))
            .map_err(map_try_send)?;
        receive
            .await
            .map_err(|_| OnboardingError::Stopped)?
            .map_err(OnboardingError::Operation)
    }
}

fn map_try_send(error: mpsc::TrySendError<Command>) -> OnboardingError {
    match error {
        mpsc::TrySendError::Full(_) => OnboardingError::Busy,
        mpsc::TrySendError::Disconnected(_) => OnboardingError::Stopped,
    }
}

/// Start the sole pre-authentication lifecycle owner for one device profile.
pub fn start_onboarding(config: OnboardingConfig) -> Result<OnboardingHandle, OnboardingError> {
    let initial_state = state_for_artifact(&config.profile.credential_path());
    let initial = Arc::new(OnboardingSnapshot {
        revision: 1,
        usb_serial: config.profile.usb_serial().to_owned(),
        lifecycle: initial_state,
    });
    let (commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
    let snapshot = Arc::new(RwLock::new(initial.clone()));
    let (termination_tx, termination_rx) = watch::channel(false);
    let handle = OnboardingHandle {
        commands,
        snapshot: snapshot.clone(),
        terminated: termination_rx,
    };
    thread::Builder::new()
        .name("lxmf-onboarding".to_owned())
        .spawn(move || {
            Owner::new(config, receiver, snapshot).run();
            termination_tx.send_replace(true);
        })
        .map_err(|error| {
            OnboardingError::Operation(format!("could not start onboarding owner: {error}"))
        })?;
    Ok(handle)
}

struct Owner {
    config: OnboardingConfig,
    commands: mpsc::Receiver<Command>,
    snapshot: Arc<RwLock<Arc<OnboardingSnapshot>>>,
    next_revision: Arc<AtomicU64>,
    stop_requested: bool,
}

impl Owner {
    fn new(
        config: OnboardingConfig,
        commands: mpsc::Receiver<Command>,
        snapshot: Arc<RwLock<Arc<OnboardingSnapshot>>>,
    ) -> Self {
        Self {
            config,
            commands,
            snapshot,
            next_revision: Arc::new(AtomicU64::new(2)),
            stop_requested: false,
        }
    }

    fn run(mut self) {
        while !self.stop_requested {
            match self.commands.recv() {
                Ok(Command::Start(reply)) => {
                    if !matches!(self.current_state(), OnboardingState::NeedsPairing) {
                        let _ =
                            reply.send(Err("new pairing is not available in this state".into()));
                        continue;
                    }
                    let _ = reply.send(Ok(()));
                    self.run_pair(false);
                }
                Ok(Command::Resume(reply)) => {
                    if !matches!(self.current_state(), OnboardingState::ResumeAvailable) {
                        let _ = reply.send(Err("resume is not available in this state".into()));
                        continue;
                    }
                    let _ = reply.send(Ok(()));
                    self.run_pair(true);
                }
                Ok(Command::Abort(reply)) => {
                    if !matches!(
                        self.current_state(),
                        OnboardingState::AbortRequired | OnboardingState::ResumeAvailable
                    ) {
                        let _ = reply.send(Err("abort is not available in this state".into()));
                        continue;
                    }
                    let _ = reply.send(Ok(()));
                    self.run_abort();
                }
                Ok(Command::Refresh(reply)) => {
                    self.publish(state_for_artifact(&self.config.profile.credential_path()));
                    let _ = reply.send(Ok(()));
                }
                Ok(Command::Shutdown) | Err(_) => break,
            }
        }
        self.publish(OnboardingState::Stopped);
    }

    fn run_pair(&mut self, resume: bool) {
        let _exclusive = self.config.connection_gate.acquire_exclusive();
        let port = match resolve_usb_port(
            self.config.profile.usb_serial(),
            self.config.explicit_port.as_deref(),
        ) {
            Ok(port) => port,
            Err(error) => {
                eprintln!("onboarding device resolution failed: {error}");
                self.publish(OnboardingState::Faulted {
                    reason: OnboardingFault::DeviceUnavailable,
                });
                return;
            }
        };
        if resume {
            self.publish(OnboardingState::Working {
                stage: OnboardingStage::Resuming,
            });
        }
        let mut observer = self.observer();
        let client = PairingClient::new(port);
        let result = if resume {
            client.connect(&mut observer).and_then(|mut session| {
                session.resume(&self.config.profile.credential_path(), &mut observer)
            })
        } else {
            client.initialize_and_pair(&self.config.profile.credential_path(), &mut observer)
        };
        drop(observer);
        match result {
            Ok(_) => self.wait_for_reset(),
            Err(error) => {
                eprintln!("onboarding workflow failed: {error}");
                let recovered = state_for_artifact(&self.config.profile.credential_path());
                if recovered == OnboardingState::CredentialReady {
                    // Active is written only after a verified activation response. A
                    // later host durability error can therefore leave a canonical
                    // Active path even though the pairing call returned an error;
                    // that device epoch still must be reset before authentication.
                    self.wait_for_reset();
                } else {
                    self.publish(recovered);
                }
            }
        }
    }

    fn run_abort(&mut self) {
        let _exclusive = self.config.connection_gate.acquire_exclusive();
        let port = match resolve_usb_port(
            self.config.profile.usb_serial(),
            self.config.explicit_port.as_deref(),
        ) {
            Ok(port) => port,
            Err(error) => {
                eprintln!("onboarding abort device resolution failed: {error}");
                self.publish(OnboardingState::Faulted {
                    reason: OnboardingFault::DeviceUnavailable,
                });
                return;
            }
        };
        let mut observer = self.observer();
        let result = PairingClient::new(port)
            .connect(&mut observer)
            .and_then(|mut session| session.abort_current(&mut observer));
        drop(observer);
        match result {
            Ok(summary) => {
                let outcome = summary.outcome();
                let Some(confirmation) = summary.into_confirmed_abort() else {
                    eprintln!("onboarding abort did not complete: {}", outcome.name());
                    self.publish(state_for_artifact(&self.config.profile.credential_path()));
                    return;
                };
                match cleanup_after_confirmed_abort(
                    confirmation,
                    &self.config.profile.credential_path(),
                ) {
                    Ok(_) => self.publish(OnboardingState::NeedsPairing),
                    Err(error) => {
                        eprintln!("onboarding abort cleanup failed: {error}");
                        self.publish(OnboardingState::Faulted {
                            reason: OnboardingFault::ProtocolOrPersistenceFailure,
                        });
                    }
                }
            }
            Err(error) => {
                eprintln!("onboarding abort failed: {error}");
                self.publish(state_for_artifact(&self.config.profile.credential_path()));
            }
        }
    }

    fn wait_for_reset(&mut self) {
        let serial = self.config.profile.usb_serial().to_owned();
        let mut observed_disconnect = false;
        self.publish(OnboardingState::AwaitingUsbReset {
            observed_disconnect,
        });
        loop {
            match self.commands.recv_timeout(COMMAND_WAIT) {
                Ok(Command::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.stop_requested = true;
                    return;
                }
                Ok(command) => reject_while_reset_pending(command),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            let present = match discover_usb_serials() {
                Ok(serials) => serials.iter().any(|candidate| candidate == &serial),
                Err(error) => {
                    eprintln!("onboarding reset observation failed: {error}");
                    continue;
                }
            };
            if !observed_disconnect && !present {
                observed_disconnect = true;
                self.publish(OnboardingState::AwaitingUsbReset {
                    observed_disconnect,
                });
            } else if observed_disconnect && present {
                self.publish(OnboardingState::CredentialReady);
                return;
            }
        }
    }

    fn observer(&self) -> impl FnMut(PairingProgress) + use<> {
        let usb_serial = self.config.profile.usb_serial().to_owned();
        let snapshot = self.snapshot.clone();
        let next_revision = self.next_revision.clone();
        move |progress| {
            publish_shared(
                &snapshot,
                &next_revision,
                &usb_serial,
                OnboardingState::Working {
                    stage: stage_for_progress(progress),
                },
            );
        }
    }

    fn current_state(&self) -> OnboardingState {
        self.snapshot
            .read()
            .expect("onboarding snapshot lock must not be poisoned")
            .lifecycle
            .clone()
    }

    fn publish(&self, state: OnboardingState) {
        publish_shared(
            &self.snapshot,
            &self.next_revision,
            self.config.profile.usb_serial(),
            state,
        );
    }
}

fn reject_while_reset_pending(command: Command) {
    let error = || Err("USB reset observation is already in progress".to_owned());
    match command {
        Command::Start(reply)
        | Command::Resume(reply)
        | Command::Abort(reply)
        | Command::Refresh(reply) => {
            let _ = reply.send(error());
        }
        Command::Shutdown => {}
    }
}

fn publish_shared(
    published: &Arc<RwLock<Arc<OnboardingSnapshot>>>,
    next_revision: &AtomicU64,
    usb_serial: &str,
    lifecycle: OnboardingState,
) {
    let revision = next_revision.fetch_add(1, Ordering::Relaxed);
    let snapshot = Arc::new(OnboardingSnapshot {
        revision,
        usb_serial: usb_serial.to_owned(),
        lifecycle,
    });
    *published
        .write()
        .expect("onboarding snapshot lock must not be poisoned") = snapshot;
}

fn state_for_artifact(path: &Path) -> OnboardingState {
    match classify_credential_artifact(path) {
        CredentialArtifactClassification::Missing => OnboardingState::NeedsPairing,
        CredentialArtifactClassification::ReservedOrAmbiguousBegin => {
            OnboardingState::AbortRequired
        }
        CredentialArtifactClassification::PendingResumeSafe => OnboardingState::ResumeAvailable,
        CredentialArtifactClassification::ActivationAmbiguous => {
            OnboardingState::ActivationAmbiguous
        }
        CredentialArtifactClassification::Active => OnboardingState::CredentialReady,
        CredentialArtifactClassification::Invalid => OnboardingState::Faulted {
            reason: OnboardingFault::InvalidCredentialArtifact,
        },
    }
}

const fn stage_for_progress(progress: PairingProgress) -> OnboardingStage {
    match progress {
        PairingProgress::OpeningPort | PairingProgress::PortReady => OnboardingStage::OpeningDevice,
        PairingProgress::CheckingInitialization => OnboardingStage::CheckingInitialization,
        PairingProgress::WaitingForPhysicalPresence(PresenceOperation::Initialize) => {
            OnboardingStage::WaitingForInitializationPresence
        }
        PairingProgress::Initializing | PairingProgress::Initialized => {
            OnboardingStage::Initializing
        }
        PairingProgress::WaitingForPhysicalPresence(
            PresenceOperation::Begin | PresenceOperation::ProofStart,
        ) => OnboardingStage::WaitingForPairingPresence,
        PairingProgress::PendingPersisted | PairingProgress::ProofChallengeAccepted => {
            OnboardingStage::Proving
        }
        PairingProgress::ActivationPrepared | PairingProgress::Activated => {
            OnboardingStage::Activating
        }
        PairingProgress::WaitingForPhysicalPresence(PresenceOperation::AbortCurrent)
        | PairingProgress::Aborted => OnboardingStage::WaitingForAbortPresence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_projection_never_contains_protocol_or_secret_material() {
        assert_eq!(
            stage_for_progress(PairingProgress::WaitingForPhysicalPresence(
                PresenceOperation::ProofStart
            )),
            OnboardingStage::WaitingForPairingPresence
        );
        assert_eq!(
            stage_for_progress(PairingProgress::ActivationPrepared),
            OnboardingStage::Activating
        );
    }

    #[test]
    fn missing_artifact_is_the_only_automatic_new_pairing_state() {
        assert!(matches!(
            state_for_artifact(Path::new("/definitely/missing/reticulum-credential")),
            OnboardingState::NeedsPairing
        ));
    }
}
