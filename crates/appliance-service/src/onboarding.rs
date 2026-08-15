//! Shared app-facing types for secure appliance onboarding.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::bindings::{JsonSafeInteger, serialize_json_safe_u64};

/// Browser-safe phase of one appliance's first-run lifecycle.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum OnboardingState {
    /// No client credential exists and a new pairing may start.
    NeedsPairing,
    /// A lost Begin may have created device Pending state; only an explicit,
    /// physically confirmed abort is safe.
    AbortRequired,
    /// A canonical client Pending artifact can be resumed or explicitly aborted.
    ResumeAvailable,
    /// Activate may have committed; automatic recovery would be unsafe.
    ActivationAmbiguous,
    /// An Active credential exists and ordinary authentication may proceed.
    CredentialReady,
    /// A pre-authentication workflow owns the selected local bearer.
    Working {
        /// Coarse, secret-free operation stage.
        stage: OnboardingStage,
    },
    /// Local state or the selected device needs operator attention.
    Faulted {
        /// App-safe failure category; detailed diagnostics remain local.
        reason: OnboardingFault,
    },
    /// The onboarding owner has stopped.
    Stopped,
}

/// Secret-free progress stage shown during explicit onboarding.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum OnboardingStage {
    /// Opening the selected local bearer.
    OpeningDevice,
    /// The BLE link is subscribed but intentionally idle while the user
    /// completes GPIO21-authorized operating-system pairing.
    WaitingForBleSecurity,
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

/// App-safe onboarding failure category.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum OnboardingFault {
    /// The configured credential artifact is malformed or unsafe to open.
    InvalidCredentialArtifact,
    /// The credential is canonical but cannot target the selected device.
    UnsupportedDevice,
    /// The exact selected device is not currently available.
    DeviceUnavailable,
    /// Initialization or pairing did not complete; local recovery state was
    /// reclassified before this category was published.
    ProtocolOrPersistenceFailure,
    /// The command is not valid for the current durable recovery state.
    InvalidRecoveryAction,
}

/// Immutable app-safe onboarding projection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct OnboardingSnapshot {
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    revision: u64,
    device_label: String,
    lifecycle: OnboardingState,
}

impl OnboardingSnapshot {
    /// Monotonic in-process onboarding revision.
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Bearer-neutral public label for the selected appliance.
    pub fn device_label(&self) -> &str {
        &self.device_label
    }

    /// Current public lifecycle state.
    pub const fn lifecycle(&self) -> &OnboardingState {
        &self.lifecycle
    }
}

/// Frontend action owner for first-run credential setup.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum OnboardingMethod {
    /// Backend-owned secure device pairing.
    ManagedPairing,
    /// Native-app import of an already activated credential.
    CredentialImport,
}

/// Current onboarding availability and optional lifecycle snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct OnboardingView {
    available: bool,
    method: Option<OnboardingMethod>,
    snapshot: Option<OnboardingSnapshot>,
}

impl OnboardingView {
    pub(crate) const fn unavailable() -> Self {
        Self {
            available: false,
            method: None,
            snapshot: None,
        }
    }
}

/// Explicit recovery action for a retained pairing artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAction {
    /// Resume a canonical Pending artifact.
    ResumeKnownPending,
    /// Abort a device-side Pending credential that has no matching client secret.
    AbortOrphan,
}

/// Request one explicit pairing recovery transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, TS)]
pub struct RecoveryRequest {
    /// Operator-selected recovery transition.
    pub action: RecoveryAction,
}
