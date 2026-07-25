//! Rust-owned, fileless BLE appliance onboarding.
//!
//! The platform supplies only an opaque subscribed GATT byte stream. This
//! owner keeps live-pairing framing, transcript state, credential material,
//! durable recovery classification, and profile publication inside Rust.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use reticulum_device_api_pairing::BearerBinding;
use reticulum_device_pairing_client::{
    CredentialArtifactClassification, PairingProgress, PairingSession, PresenceOperation,
    ProgressObserver, classify_credential_artifact, cleanup_after_confirmed_abort,
};

use crate::NativeProfileSummary;
use crate::ble::{BleHub, NativeBleError, NativeBlePlatformCommand, PLATFORM_COMMAND_WAIT};
use crate::profile::NativeProfileStore;

/// User-visible operation currently owned by the native onboarding state
/// machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum NativeBleOnboardingOperation {
    /// Create and activate a new appliance credential.
    Pair,
    /// Resume a canonical durable Pending credential.
    Resume,
    /// Abort the device-selected Pending credential.
    AbortCurrent,
}

/// Coarse, secret-free phase suitable for UI polling.
#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum NativeBleOnboardingPhase {
    /// No selected subscribed GATT link is registered.
    Idle,
    /// A selected subscribed link is ready for one operation.
    LinkReady,
    /// Rust is validating local recovery state and claiming the selected link.
    Preparing,
    /// Rust is reading the device credential-store status.
    CheckingInitialization,
    /// Device credential-store initialization is in progress.
    Initializing,
    /// The device credential store is initialized.
    Initialized,
    /// GPIO physical presence is required for credential-store initialization.
    WaitingForInitializationPresence,
    /// GPIO physical presence is required to allocate a Pending credential.
    WaitingForBeginPresence,
    /// The Pending credential is durably resume-safe in app-private storage.
    PendingPersisted,
    /// GPIO physical presence is required to authorize the proof challenge.
    WaitingForProofPresence,
    /// The device proof challenge was accepted.
    ProofChallengeAccepted,
    /// Activation ambiguity was durably recorded before sending Activate.
    ActivationPrepared,
    /// An Active credential is being published into its device-keyed profile.
    PublishingProfile,
    /// Pairing completed and the returned public profile is active.
    Complete,
    /// GPIO physical presence is required to abort the device Pending owner.
    WaitingForAbortPresence,
    /// A confirmed abort is being reconciled with app-private recovery state.
    FinalizingAbort,
    /// Device and app-private Pending state were definitely cleared.
    Aborted,
    /// The last operation failed; inspect the coarse failure category.
    Failed,
}

/// Coarse failure category that never contains a credential, passkey, path, or
/// protocol payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum NativeBleOnboardingFailure {
    /// Another onboarding operation already owns this object.
    Busy,
    /// No selected, subscribed GATT generation was available.
    NoSubscribedLink,
    /// The device did not reach a definitely initialized state.
    InitializationIncomplete,
    /// A resume-safe Pending artifact exists and must be resumed or aborted.
    ResumeRequired,
    /// An ambiguous Begin reservation requires a confirmed device abort.
    AbortRequired,
    /// Activation may have been sent and requires authenticated reconciliation.
    ReconciliationRequired,
    /// App-private recovery state is malformed or otherwise unsafe.
    InvalidRecoveryState,
    /// The authenticated live-pairing exchange did not complete.
    ProtocolFailure,
    /// The activated credential could not be durably published or selected.
    ProfilePublicationFailure,
    /// The native blocking task terminated unexpectedly.
    Internal,
}

/// Secret-free polling snapshot for one native BLE onboarding owner.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct NativeBleOnboardingSnapshot {
    /// Strictly increasing local transition counter.
    pub revision: u64,
    /// Current coarse phase.
    pub phase: NativeBleOnboardingPhase,
    /// Current or most recent operation.
    pub operation: Option<NativeBleOnboardingOperation>,
    /// Registered platform link generation, when present.
    pub link_generation: Option<u64>,
    /// Public device-keyed profile after successful publication.
    pub completed_profile: Option<NativeProfileSummary>,
    /// Coarse failure category for the most recent failed operation.
    pub failure: Option<NativeBleOnboardingFailure>,
}

/// Failure returned by a native BLE onboarding operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Error)]
pub enum NativeBleOnboardingError {
    /// Another onboarding operation already owns this object.
    Busy,
    /// No selected, subscribed GATT generation was available.
    NoSubscribedLink,
    /// The device did not reach a definitely initialized state.
    InitializationIncomplete,
    /// A resume-safe Pending artifact exists and must be resumed or aborted.
    ResumeRequired,
    /// An ambiguous Begin reservation requires a confirmed device abort.
    AbortRequired,
    /// Activation may have been sent and requires authenticated reconciliation.
    ReconciliationRequired,
    /// App-private recovery state is malformed or otherwise unsafe.
    InvalidRecoveryState,
    /// The authenticated live-pairing exchange did not complete.
    ProtocolFailure,
    /// The activated credential could not be durably published or selected.
    ProfilePublicationFailure,
    /// The native blocking task terminated unexpectedly.
    Internal,
}

impl fmt::Display for NativeBleOnboardingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Busy => "another BLE onboarding operation is already active",
            Self::NoSubscribedLink => "no subscribed BLE onboarding link is ready",
            Self::InitializationIncomplete => {
                "the device credential store did not finish initialization"
            }
            Self::ResumeRequired => "a pending credential must be resumed or aborted",
            Self::AbortRequired => "an ambiguous pairing begin requires a confirmed abort",
            Self::ReconciliationRequired => {
                "credential activation requires authenticated reconciliation"
            }
            Self::InvalidRecoveryState => "app-private pairing recovery state is invalid",
            Self::ProtocolFailure => "the BLE live-pairing protocol did not complete",
            Self::ProfilePublicationFailure => {
                "the activated device profile was not durably published"
            }
            Self::Internal => "the native BLE onboarding task failed internally",
        })
    }
}

impl std::error::Error for NativeBleOnboardingError {}

impl From<NativeBleOnboardingError> for NativeBleOnboardingFailure {
    fn from(error: NativeBleOnboardingError) -> Self {
        match error {
            NativeBleOnboardingError::Busy => Self::Busy,
            NativeBleOnboardingError::NoSubscribedLink => Self::NoSubscribedLink,
            NativeBleOnboardingError::InitializationIncomplete => Self::InitializationIncomplete,
            NativeBleOnboardingError::ResumeRequired => Self::ResumeRequired,
            NativeBleOnboardingError::AbortRequired => Self::AbortRequired,
            NativeBleOnboardingError::ReconciliationRequired => Self::ReconciliationRequired,
            NativeBleOnboardingError::InvalidRecoveryState => Self::InvalidRecoveryState,
            NativeBleOnboardingError::ProtocolFailure => Self::ProtocolFailure,
            NativeBleOnboardingError::ProfilePublicationFailure => Self::ProfilePublicationFailure,
            NativeBleOnboardingError::Internal => Self::Internal,
        }
    }
}

#[derive(Clone)]
struct OnboardingState {
    revision: u64,
    phase: NativeBleOnboardingPhase,
    operation: Option<NativeBleOnboardingOperation>,
    link_generation: Option<u64>,
    completed_profile: Option<NativeProfileSummary>,
    failure: Option<NativeBleOnboardingFailure>,
    operation_active: bool,
}

impl Default for OnboardingState {
    fn default() -> Self {
        Self {
            revision: 0,
            phase: NativeBleOnboardingPhase::Idle,
            operation: None,
            link_generation: None,
            completed_profile: None,
            failure: None,
            operation_active: false,
        }
    }
}

impl OnboardingState {
    fn snapshot(&self) -> NativeBleOnboardingSnapshot {
        NativeBleOnboardingSnapshot {
            revision: self.revision,
            phase: self.phase,
            operation: self.operation,
            link_generation: self.link_generation,
            completed_profile: self.completed_profile.clone(),
            failure: self.failure,
        }
    }

    fn transition(&mut self, phase: NativeBleOnboardingPhase) {
        self.revision = self.revision.saturating_add(1);
        self.phase = phase;
    }
}

/// Sole native owner for one selected BLE onboarding ceremony.
///
/// This object deliberately owns a different byte hub from
/// [`crate::NativeAppliance`], preventing an ordinary authenticated connector
/// from racing to claim the pre-authentication stream.
#[derive(uniffi::Object)]
pub struct NativeBleOnboarding {
    profile_store: Arc<NativeProfileStore>,
    hub: Arc<BleHub>,
    state: Arc<Mutex<OnboardingState>>,
}

#[uniffi::export(async_runtime = "tokio")]
impl NativeBleOnboarding {
    /// Create a native onboarding owner sharing the app's device-keyed profile
    /// store.
    #[uniffi::constructor]
    pub fn open(profile_store: Arc<NativeProfileStore>) -> Arc<Self> {
        Arc::new(Self {
            profile_store,
            hub: BleHub::new(),
            state: Arc::new(Mutex::new(OnboardingState::default())),
        })
    }

    /// Return the latest coarse, secret-free onboarding projection.
    pub fn snapshot(&self) -> Result<NativeBleOnboardingSnapshot, NativeBleOnboardingError> {
        Ok(self.lock_state()?.snapshot())
    }

    /// Register the selected, connected, and subscribed GATT generation.
    ///
    /// On iOS, CoreBluetooth does not expose whether an existing bond was
    /// silently reused. The platform owner therefore retains this link without
    /// sending protocol bytes until the user explicitly confirms that the
    /// peripheral completed its Bluetooth-security step.
    pub fn ble_link_connected(
        &self,
        peripheral_id: String,
        max_write_bytes: u32,
    ) -> Result<u64, NativeBleError> {
        let generation = self.hub.link_connected(peripheral_id, max_write_bytes)?;
        let mut state = self.state.lock().map_err(|_| NativeBleError::Internal {
            reason: "BLE onboarding state lock is poisoned".to_owned(),
        })?;
        state.link_generation = Some(generation);
        state.operation = None;
        state.completed_profile = None;
        state.failure = None;
        state.transition(NativeBleOnboardingPhase::LinkReady);
        Ok(generation)
    }

    /// Append opaque bytes from one confirmed TX characteristic indication.
    ///
    /// During this alpha, the platform adapter relays these bounded bytes
    /// through TypeScript without parsing, logging, or persistence. A later
    /// Swift/Kotlin-to-Rust pump will remove that transient JavaScript hop.
    pub fn ble_ingest_indication(
        &self,
        generation: u64,
        bytes: Vec<u8>,
    ) -> Result<(), NativeBleError> {
        self.hub.ingest_indication(generation, bytes)
    }

    /// Await the next opaque write-with-response or disconnect command.
    ///
    /// The returned bytes may contain pairing protocol material. The platform
    /// adapter must forward them directly and must not inspect, log, or persist
    /// them.
    pub async fn ble_next_platform_command(
        &self,
        generation: u64,
    ) -> Result<Option<NativeBlePlatformCommand>, NativeBleError> {
        let hub = Arc::clone(&self.hub);
        tokio::task::spawn_blocking(move || {
            hub.next_platform_command(generation, PLATFORM_COMMAND_WAIT)
        })
        .await
        .map_err(|_| NativeBleError::Internal {
            reason: "BLE onboarding command waiter terminated".to_owned(),
        })?
    }

    /// Confirm that one opaque GATT write completed successfully.
    pub fn ble_write_succeeded(&self, generation: u64, token: u64) -> Result<(), NativeBleError> {
        self.hub.write_succeeded(generation, token)
    }

    /// Report an opaque GATT write failure and close the generation.
    pub fn ble_write_failed(
        &self,
        generation: u64,
        token: u64,
        reason: String,
    ) -> Result<(), NativeBleError> {
        self.hub.write_failed(generation, token, reason)
    }

    /// Report that the platform GATT link and subscription are gone.
    pub fn ble_disconnected(&self, generation: u64, reason: String) -> Result<(), NativeBleError> {
        self.hub.disconnected(generation, reason)?;
        let mut state = self.state.lock().map_err(|_| NativeBleError::Internal {
            reason: "BLE onboarding state lock is poisoned".to_owned(),
        })?;
        if state.link_generation == Some(generation) {
            state.link_generation = None;
            if !state.operation_active
                && !matches!(
                    state.phase,
                    NativeBleOnboardingPhase::Complete
                        | NativeBleOnboardingPhase::Aborted
                        | NativeBleOnboardingPhase::Failed
                )
            {
                state.transition(NativeBleOnboardingPhase::Idle);
            } else {
                state.revision = state.revision.saturating_add(1);
            }
        }
        Ok(())
    }

    /// Initialize the device when needed, create a new credential, and publish
    /// its device-keyed profile.
    pub async fn pair(&self) -> Result<NativeProfileSummary, NativeBleOnboardingError> {
        self.execute_profile_operation(NativeBleOnboardingOperation::Pair)
            .await
    }

    /// Initialize the device when needed, resume one canonical Pending
    /// credential, and publish its device-keyed profile.
    pub async fn resume(&self) -> Result<NativeProfileSummary, NativeBleOnboardingError> {
        self.execute_profile_operation(NativeBleOnboardingOperation::Resume)
            .await
    }

    /// Ask the device to abort its sole Pending credential and, only after a
    /// definitely successful response, remove safe local recovery state.
    pub async fn abort_current(&self) -> Result<(), NativeBleOnboardingError> {
        self.begin_operation(NativeBleOnboardingOperation::AbortCurrent)?;
        let result = self
            .spawn_operation(NativeBleOnboardingOperation::AbortCurrent)
            .await?;
        debug_assert!(result.is_none());
        Ok(())
    }
}

impl NativeBleOnboarding {
    async fn execute_profile_operation(
        &self,
        operation: NativeBleOnboardingOperation,
    ) -> Result<NativeProfileSummary, NativeBleOnboardingError> {
        self.begin_operation(operation)?;
        self.spawn_operation(operation)
            .await?
            .ok_or(NativeBleOnboardingError::Internal)
    }

    async fn spawn_operation(
        &self,
        operation: NativeBleOnboardingOperation,
    ) -> Result<Option<NativeProfileSummary>, NativeBleOnboardingError> {
        let hub = Arc::clone(&self.hub);
        let profile_store = Arc::clone(&self.profile_store);
        let state = Arc::clone(&self.state);
        let state_for_join_failure = Arc::clone(&state);
        match tokio::task::spawn_blocking(move || {
            let result = run_operation(operation, &profile_store, &hub, &state);
            finish_operation(&state, operation, &result);
            result
        })
        .await
        {
            Ok(result) => result,
            Err(_) => {
                let result = Err(NativeBleOnboardingError::Internal);
                finish_operation(&state_for_join_failure, operation, &result);
                result
            }
        }
    }

    fn begin_operation(
        &self,
        operation: NativeBleOnboardingOperation,
    ) -> Result<(), NativeBleOnboardingError> {
        let mut state = self.lock_state()?;
        if state.operation_active {
            return Err(NativeBleOnboardingError::Busy);
        }
        if state.link_generation.is_none() {
            state.operation = Some(operation);
            state.failure = Some(NativeBleOnboardingFailure::NoSubscribedLink);
            state.transition(NativeBleOnboardingPhase::Failed);
            return Err(NativeBleOnboardingError::NoSubscribedLink);
        }
        state.operation_active = true;
        state.operation = Some(operation);
        state.completed_profile = None;
        state.failure = None;
        state.transition(NativeBleOnboardingPhase::Preparing);
        Ok(())
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, OnboardingState>, NativeBleOnboardingError> {
        self.state
            .lock()
            .map_err(|_| NativeBleOnboardingError::Internal)
    }
}

impl Drop for NativeBleOnboarding {
    fn drop(&mut self) {
        self.hub
            .request_owner_disconnect("native BLE onboarding owner was released");
    }
}

fn run_operation(
    operation: NativeBleOnboardingOperation,
    profile_store: &Arc<NativeProfileStore>,
    hub: &Arc<BleHub>,
    state: &Arc<Mutex<OnboardingState>>,
) -> Result<Option<NativeProfileSummary>, NativeBleOnboardingError> {
    let credential_path = profile_store.onboarding_credential_path();
    let classification = classify_credential_artifact(&credential_path);
    preflight_operation(operation, classification)?;

    let (stream, _lease, _peripheral_id) = hub
        .claim_link()
        .map_err(|_| NativeBleOnboardingError::NoSubscribedLink)?;
    let mut session = PairingSession::from_stream(
        stream,
        "BLE onboarding",
        BearerBinding::BleGatt,
        0,
        reticulum_device_pairing_client::DEFAULT_PAIRING_TIMEOUT,
    )
    .map_err(|_| NativeBleOnboardingError::ProtocolFailure)?;
    let mut observer = NativeProgressObserver {
        state: Arc::clone(state),
    };

    match operation {
        NativeBleOnboardingOperation::Pair | NativeBleOnboardingOperation::Resume => {
            let initialization = session
                .ensure_initialized(&mut observer)
                .map_err(|_| NativeBleOnboardingError::ProtocolFailure)?;
            if !initialization.is_completed() {
                return Err(NativeBleOnboardingError::InitializationIncomplete);
            }
            let activation = match operation {
                NativeBleOnboardingOperation::Pair => session
                    .pair(&credential_path, &mut observer)
                    .map_err(|_| NativeBleOnboardingError::ProtocolFailure)?,
                NativeBleOnboardingOperation::Resume => session
                    .resume(&credential_path, &mut observer)
                    .map_err(|_| NativeBleOnboardingError::ProtocolFailure)?,
                NativeBleOnboardingOperation::AbortCurrent => unreachable!(),
            };
            transition_state(state, NativeBleOnboardingPhase::PublishingProfile);
            let profile = profile_store
                .promote_onboarding_credential(
                    activation.device_id(),
                    activation.credential_id(),
                    activation.generation(),
                )
                .map_err(|_| NativeBleOnboardingError::ProfilePublicationFailure)?;
            Ok(Some(profile))
        }
        NativeBleOnboardingOperation::AbortCurrent => {
            let abort = session
                .abort_current(&mut observer)
                .map_err(|_| NativeBleOnboardingError::ProtocolFailure)?;
            let Some(confirmed) = abort.into_confirmed_abort() else {
                return Err(NativeBleOnboardingError::ProtocolFailure);
            };
            transition_state(state, NativeBleOnboardingPhase::FinalizingAbort);
            match classify_credential_artifact(&credential_path) {
                CredentialArtifactClassification::Missing => {}
                CredentialArtifactClassification::ReservedOrAmbiguousBegin
                | CredentialArtifactClassification::PendingResumeSafe => {
                    cleanup_after_confirmed_abort(confirmed, &credential_path)
                        .map_err(|_| NativeBleOnboardingError::InvalidRecoveryState)?;
                }
                CredentialArtifactClassification::ActivationAmbiguous
                | CredentialArtifactClassification::Active
                | CredentialArtifactClassification::Invalid => {
                    return Err(NativeBleOnboardingError::InvalidRecoveryState);
                }
            }
            Ok(None)
        }
    }
}

fn preflight_operation(
    operation: NativeBleOnboardingOperation,
    classification: CredentialArtifactClassification,
) -> Result<(), NativeBleOnboardingError> {
    match (operation, classification) {
        (NativeBleOnboardingOperation::Pair, CredentialArtifactClassification::Missing)
        | (
            NativeBleOnboardingOperation::Resume,
            CredentialArtifactClassification::PendingResumeSafe,
        )
        | (
            NativeBleOnboardingOperation::AbortCurrent,
            CredentialArtifactClassification::Missing
            | CredentialArtifactClassification::ReservedOrAmbiguousBegin
            | CredentialArtifactClassification::PendingResumeSafe,
        ) => Ok(()),
        (
            NativeBleOnboardingOperation::Pair,
            CredentialArtifactClassification::PendingResumeSafe,
        ) => Err(NativeBleOnboardingError::ResumeRequired),
        (
            NativeBleOnboardingOperation::Pair | NativeBleOnboardingOperation::Resume,
            CredentialArtifactClassification::ReservedOrAmbiguousBegin,
        ) => Err(NativeBleOnboardingError::AbortRequired),
        (
            _,
            CredentialArtifactClassification::ActivationAmbiguous
            | CredentialArtifactClassification::Active,
        ) => Err(NativeBleOnboardingError::ReconciliationRequired),
        (_, CredentialArtifactClassification::Invalid) => {
            Err(NativeBleOnboardingError::InvalidRecoveryState)
        }
        (NativeBleOnboardingOperation::Resume, CredentialArtifactClassification::Missing) => {
            Err(NativeBleOnboardingError::InvalidRecoveryState)
        }
    }
}

fn finish_operation(
    state: &Arc<Mutex<OnboardingState>>,
    operation: NativeBleOnboardingOperation,
    result: &Result<Option<NativeProfileSummary>, NativeBleOnboardingError>,
) {
    let Ok(mut state) = state.lock() else {
        return;
    };
    state.operation_active = false;
    state.operation = Some(operation);
    match result {
        Ok(Some(profile)) => {
            state.completed_profile = Some(profile.clone());
            state.failure = None;
            state.transition(NativeBleOnboardingPhase::Complete);
        }
        Ok(None) => {
            state.completed_profile = None;
            state.failure = None;
            state.transition(NativeBleOnboardingPhase::Aborted);
        }
        Err(error) => {
            state.completed_profile = None;
            state.failure = Some((*error).into());
            state.transition(NativeBleOnboardingPhase::Failed);
        }
    }
}

fn transition_state(state: &Arc<Mutex<OnboardingState>>, phase: NativeBleOnboardingPhase) {
    if let Ok(mut state) = state.lock() {
        state.transition(phase);
    }
}

struct NativeProgressObserver {
    state: Arc<Mutex<OnboardingState>>,
}

impl ProgressObserver for NativeProgressObserver {
    fn observe(&mut self, progress: PairingProgress) {
        let phase = match progress {
            PairingProgress::OpeningPort => NativeBleOnboardingPhase::Preparing,
            PairingProgress::PortReady => NativeBleOnboardingPhase::LinkReady,
            PairingProgress::CheckingInitialization => {
                NativeBleOnboardingPhase::CheckingInitialization
            }
            PairingProgress::Initializing => NativeBleOnboardingPhase::Initializing,
            PairingProgress::Initialized => NativeBleOnboardingPhase::Initialized,
            PairingProgress::WaitingForPhysicalPresence(operation) => match operation {
                PresenceOperation::Initialize => {
                    NativeBleOnboardingPhase::WaitingForInitializationPresence
                }
                PresenceOperation::Begin => NativeBleOnboardingPhase::WaitingForBeginPresence,
                PresenceOperation::ProofStart => NativeBleOnboardingPhase::WaitingForProofPresence,
                PresenceOperation::AbortCurrent => {
                    NativeBleOnboardingPhase::WaitingForAbortPresence
                }
            },
            PairingProgress::PendingPersisted => NativeBleOnboardingPhase::PendingPersisted,
            PairingProgress::ProofChallengeAccepted => {
                NativeBleOnboardingPhase::ProofChallengeAccepted
            }
            PairingProgress::ActivationPrepared => NativeBleOnboardingPhase::ActivationPrepared,
            PairingProgress::Activated => NativeBleOnboardingPhase::PublishingProfile,
            PairingProgress::Aborted => NativeBleOnboardingPhase::FinalizingAbort,
        };
        transition_state(&self.state, phase);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(std::path::PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("test time follows Unix epoch")
                .as_nanos();
            let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "reticulum-native-onboarding-{label}-{}-{nonce}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("test root creates");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;

                fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                    .expect("test root is owner-only");
            }
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn store(root: &TestDirectory) -> Arc<NativeProfileStore> {
        NativeProfileStore::open(root.0.to_string_lossy().into_owned(), None, None)
            .expect("test profile store opens")
    }

    fn write_active_pairing_artifact(
        store: &NativeProfileStore,
        device_id: [u8; 16],
        credential_id: [u8; 16],
        generation: u64,
    ) {
        let path = store.onboarding_credential_path();
        let mut bytes = [0_u8; 96];
        bytes[..8].copy_from_slice(b"RDPKEY1\0");
        bytes[8..10].copy_from_slice(&1_u16.to_le_bytes());
        bytes[10] = 2;
        bytes[16..32].copy_from_slice(&device_id);
        bytes[32..48].copy_from_slice(&credential_id);
        bytes[48..56].copy_from_slice(&generation.to_le_bytes());
        bytes[56..88].fill(0x5a);
        fs::write(&path, bytes).expect("active pairing artifact writes");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .expect("pairing artifact is owner-only");
        }
    }

    #[test]
    fn snapshots_expose_only_coarse_state_and_public_link_generation() {
        let root = TestDirectory::new("snapshot");
        let onboarding = NativeBleOnboarding::open(store(&root));
        assert_eq!(
            onboarding.snapshot().expect("initial snapshot"),
            NativeBleOnboardingSnapshot {
                revision: 0,
                phase: NativeBleOnboardingPhase::Idle,
                operation: None,
                link_generation: None,
                completed_profile: None,
                failure: None,
            }
        );

        let generation = onboarding
            .ble_link_connected("selected-platform-id".to_owned(), 20)
            .expect("selected link registers");
        let snapshot = onboarding.snapshot().expect("linked snapshot");
        assert_eq!(snapshot.phase, NativeBleOnboardingPhase::LinkReady);
        assert_eq!(snapshot.link_generation, Some(generation));
        assert_eq!(snapshot.operation, None);
        assert_eq!(snapshot.failure, None);
    }

    #[tokio::test]
    async fn starting_without_a_selected_link_fails_without_exposing_storage_context() {
        let root = TestDirectory::new("missing-link");
        let onboarding = NativeBleOnboarding::open(store(&root));
        assert_eq!(
            onboarding.pair().await,
            Err(NativeBleOnboardingError::NoSubscribedLink)
        );
        let snapshot = onboarding.snapshot().expect("failure snapshot");
        assert_eq!(snapshot.phase, NativeBleOnboardingPhase::Failed);
        assert_eq!(
            snapshot.failure,
            Some(NativeBleOnboardingFailure::NoSubscribedLink)
        );
        assert_eq!(
            NativeBleOnboardingError::NoSubscribedLink.to_string(),
            "no subscribed BLE onboarding link is ready"
        );
    }

    #[test]
    fn operation_preflight_preserves_resume_and_abort_recovery_boundaries() {
        assert_eq!(
            preflight_operation(
                NativeBleOnboardingOperation::Pair,
                CredentialArtifactClassification::PendingResumeSafe,
            ),
            Err(NativeBleOnboardingError::ResumeRequired)
        );
        assert_eq!(
            preflight_operation(
                NativeBleOnboardingOperation::Resume,
                CredentialArtifactClassification::ReservedOrAmbiguousBegin,
            ),
            Err(NativeBleOnboardingError::AbortRequired)
        );
        assert_eq!(
            preflight_operation(
                NativeBleOnboardingOperation::AbortCurrent,
                CredentialArtifactClassification::ActivationAmbiguous,
            ),
            Err(NativeBleOnboardingError::ReconciliationRequired)
        );
    }

    #[test]
    fn activated_artifact_is_promoted_without_exposing_or_retaining_a_staging_file() {
        let root = TestDirectory::new("promotion");
        let store = store(&root);
        let device_id = [0x31; 16];
        let credential_id = [0x42; 16];
        write_active_pairing_artifact(&store, device_id, credential_id, 9);

        let profile = store
            .promote_onboarding_credential(device_id, credential_id, 9)
            .expect("matching authenticated result promotes");
        assert_eq!(profile.profile_key, hex::encode(device_id));
        assert_eq!(profile.credential.credential_id, hex::encode(credential_id));
        assert_eq!(profile.credential.generation, 9);
        assert!(
            !store.onboarding_credential_path().exists(),
            "the app-private live-pairing artifact is removed after publication"
        );
        let snapshot = store.snapshot().expect("promoted profile snapshot");
        assert_eq!(
            snapshot.active_profile_key.as_deref(),
            Some(profile.profile_key.as_str())
        );
        assert_eq!(snapshot.profiles, vec![profile]);
    }

    #[test]
    fn promotion_rejects_a_result_that_does_not_match_the_authenticated_response() {
        let root = TestDirectory::new("promotion-mismatch");
        let store = store(&root);
        let device_id = [0x51; 16];
        let credential_id = [0x62; 16];
        write_active_pairing_artifact(&store, device_id, credential_id, 11);

        assert!(
            store
                .promote_onboarding_credential([0x52; 16], credential_id, 11)
                .is_err()
        );
        assert!(store.onboarding_credential_path().exists());
        assert!(
            store
                .snapshot()
                .expect("failed promotion leaves store readable")
                .profiles
                .is_empty()
        );
    }
}
