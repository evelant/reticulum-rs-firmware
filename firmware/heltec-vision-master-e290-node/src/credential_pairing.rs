//! Transport-neutral resident ownership for the E290 live-pairing lifecycle.
//!
//! This module contains only the public, backend-independent request/drive
//! vocabulary and the private owners retained by [`crate::credential_runtime`].
//! It has no USB, executor, GPIO, radio, or platform-flash dependency.

use reticulum_device_api_credential_store::{
    CredentialStoreBindingError, CredentialStoreFault, PendingPairingLifecycleSuccessor,
};
use reticulum_device_api_credentials::{
    E290_CREDENTIAL_RECORD_CAPACITY, PairingLifecycleStoreCandidate,
};
use reticulum_device_api_pairing::{
    ActivateFailure, ActivationConfirmation, BeginOffer, PairingFailure, PairingPsk,
    PairingTranscript, ProofChallenge, VerifiedClientProof,
};
use reticulum_device_api_pairing_policy::{
    AbortPendingPermit, ActivationPermit, BeginAttemptPermit, PendingRef, ProofAttemptPermit,
};

/// Maximum complete entropy proposals made by one Begin or ProofStart request.
pub const MAX_PAIRING_ENTROPY_ATTEMPTS: u8 = 8;

/// Conservative fixed-RAM ceiling for retained live-pairing ownership.
pub const MAXIMUM_LIVE_PAIRING_OWNERSHIP_BYTES: usize = 6_144;

/// Durable credential mutation selected by one live-pairing operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingMutation {
    /// Add one fresh Pending enrollment.
    AddPending,
    /// Activate the exact proved Pending enrollment.
    ActivatePending,
    /// Replace the current Pending enrollment with an aborted tombstone.
    AbortPending,
}

/// Non-secret resident live-pairing state for scheduling and exclusion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialPairingStatus {
    /// No challenge or lifecycle mutation is retained.
    Idle,
    /// One challenge-only ProofStart continuation is outstanding.
    ProofOutstanding,
    /// A lifecycle mutation is admitted and waiting for a clean mounted store.
    AwaitingCleanStore(PairingMutation),
    /// A complete lifecycle candidate is ready for its first physical drive.
    MutationPrepared(PairingMutation),
    /// An ambiguous physical result retains the complete current/candidate owner.
    ReconcileRequired(PairingMutation),
    /// The inactive credential sector must be erased before another mutation.
    CleanupRequired,
    /// An internal ownership invariant failed closed for this boot.
    Blocked,
}

/// Result of admitting an empty Begin request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "Begin admission must be handled before accepting another request"]
pub enum BeginAdmission {
    /// A complete AddPending candidate is retained for physical drive.
    Accepted,
    /// Coarse protocol-level refusal.
    Refused(PairingFailure),
}

/// Result of handling one ProofStart request.
#[must_use = "a proof challenge must be delivered or explicitly dropped"]
pub enum ProofStartAdmission {
    /// Fresh challenge with its continuation state retained by the runtime.
    Challenge(ProofChallenge),
    /// Coarse protocol-level refusal.
    Refused(PairingFailure),
}

/// Result of consuming one Activate continuation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "Activate admission must be handled before driving credential mutation"]
pub enum ActivateAdmission {
    /// Possession was verified and activation ownership is retained.
    Accepted,
    /// Coarse protocol-level refusal.
    Refused(ActivateFailure),
}

/// Result of admitting an identifier-free AbortCurrent request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "Abort admission must be handled before driving credential mutation"]
pub enum AbortAdmission {
    /// Exact device-selected abort ownership is retained.
    Accepted,
    /// Coarse protocol-level refusal.
    Refused(PairingFailure),
}

/// Stable retry classification that never exposes a backend error value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingDriveRetry {
    /// Physical backend operation returned an ambiguous failure.
    Backend,
    /// Operation-scoped access did not match the mounted physical binding.
    Binding(CredentialStoreBindingError),
    /// Media inspection or exact readback failed.
    Media(CredentialStoreFault),
    /// A supposedly checked lifecycle candidate failed store preflight.
    Semantic,
}

/// One bounded synchronous drive result for resident live pairing.
#[must_use = "durable pairing outcomes and secret replies must be retained or explicitly handled"]
pub enum CredentialPairingDriveOutcome {
    /// No cleanup or lifecycle mutation needs physical progress.
    Idle,
    /// One inactive-sector cleanup completed.
    CleanupCompleted,
    /// Cleanup or candidate construction completed; the named mutation is ready.
    MutationPrepared(PairingMutation),
    /// Physical ambiguity now requires exact reconciliation.
    ReconcileRequired(PairingMutation),
    /// Exact ownership was retained for a later retry.
    Retry {
        /// Operation requiring the retry, if one has been admitted.
        mutation: Option<PairingMutation>,
        /// Stable retry category.
        reason: PairingDriveRetry,
    },
    /// AddPending is durable and the secret offer may be sent once.
    BeginOffered(BeginOffer),
    /// ActivatePending is durable and the confirmation may be sent once.
    Activated(ActivationConfirmation),
    /// AbortPending is durable and publishable.
    Aborted,
    /// An internal policy/store ownership invariant failed closed.
    Blocked(Option<PairingMutation>),
}

pub(crate) struct ProofOwnership {
    pub(crate) permit: ProofAttemptPermit,
    pub(crate) psk: PairingPsk,
    pub(crate) transcript: PairingTranscript,
}

pub(crate) enum MutationCompletion {
    Begin {
        permit: BeginAttemptPermit,
        device_id: reticulum_device_api_pairing::DeviceId,
        pending: PendingRef,
    },
    Activate {
        permit: ActivationPermit,
        verified: VerifiedClientProof,
        psk: PairingPsk,
    },
    Abort {
        permit: AbortPendingPermit,
    },
}

impl MutationCompletion {
    pub(crate) const fn mutation(&self) -> PairingMutation {
        match self {
            Self::Begin { .. } => PairingMutation::AddPending,
            Self::Activate { .. } => PairingMutation::ActivatePending,
            Self::Abort { .. } => PairingMutation::AbortPending,
        }
    }

    pub(crate) const fn pending(&self) -> PendingRef {
        match self {
            Self::Begin { pending, .. } => *pending,
            Self::Activate { permit, .. } => permit.pending(),
            Self::Abort { permit } => permit.pending(),
        }
    }
}

#[allow(
    clippy::large_enum_variant,
    reason = "allocation-free reconciliation must retain the undivided physical owner in RAM"
)]
pub(crate) enum LivePairingOwnership {
    Idle,
    Proof(ProofOwnership),
    AwaitingCleanStore(MutationCompletion),
    Prepared {
        completion: MutationCompletion,
        candidate: PairingLifecycleStoreCandidate<E290_CREDENTIAL_RECORD_CAPACITY>,
    },
    Reconciling {
        completion: MutationCompletion,
        pending: PendingPairingLifecycleSuccessor,
    },
    Blocked,
}

impl LivePairingOwnership {
    pub(crate) const fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }

    pub(crate) const fn mutation(&self) -> Option<PairingMutation> {
        match self {
            Self::AwaitingCleanStore(completion)
            | Self::Prepared { completion, .. }
            | Self::Reconciling { completion, .. } => Some(completion.mutation()),
            Self::Idle | Self::Proof(_) | Self::Blocked => None,
        }
    }
}

const _: () =
    assert!(core::mem::size_of::<LivePairingOwnership>() <= MAXIMUM_LIVE_PAIRING_OWNERSHIP_BYTES);
