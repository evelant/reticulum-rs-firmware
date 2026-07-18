//! Pure cross-store mutation exclusion for the permanent E290 flash owner.
//!
//! Credential initialization and the durable submission journal share one
//! physical flash backend. The product coordinator serializes every physical
//! operation, while these decisions make the stronger semantic rule explicit:
//! an already-retained journal mutation blocks initialization admission, and
//! an admitted credential initialization blocks new journal mutations until it
//! reaches a stable non-in-flight state.

use crate::credential_runtime::CredentialInitializationStatus;

/// Admission decision for a new credential-store initialization request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialInitializationGate {
    /// No journal mutation is retained, so policy admission may run.
    Ready,
    /// Journal-owned physical progress must settle before policy admission.
    DeferredForJournalMutation,
}

/// Decide whether credential initialization policy admission may run.
pub const fn credential_initialization_gate(
    journal_actor_pending: bool,
    journal_projector_pending: bool,
) -> CredentialInitializationGate {
    if journal_actor_pending || journal_projector_pending {
        CredentialInitializationGate::DeferredForJournalMutation
    } else {
        CredentialInitializationGate::Ready
    }
}

/// Product decision before a path can initiate journal physical mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JournalMutationGate {
    /// The journal runtime exists and no credential operation excludes it.
    Ready,
    /// An admitted credential initialization temporarily owns mutation access.
    DeferredForCredentialInitialization,
    /// Durable submission service has no usable resident runtime.
    RuntimeUnavailable,
}

/// Decide whether one journal mutation path may run.
///
/// Initialization deferral is intentionally a different state from runtime
/// absence. Callers must retain journal service and retry after a deferral.
pub const fn journal_mutation_gate(
    runtime_available: bool,
    initialization: CredentialInitializationStatus,
) -> JournalMutationGate {
    if matches!(
        initialization,
        CredentialInitializationStatus::InFlight { .. }
    ) {
        JournalMutationGate::DeferredForCredentialInitialization
    } else if !runtime_available {
        JournalMutationGate::RuntimeUnavailable
    } else {
        JournalMutationGate::Ready
    }
}

#[cfg(test)]
mod tests {
    use reticulum_device_api_pairing_policy::{InitializableMedia, PermitError};

    use super::{
        CredentialInitializationGate, JournalMutationGate, credential_initialization_gate,
        journal_mutation_gate,
    };
    use crate::credential_runtime::{CredentialInitializationStatus, InitializationBlockReason};

    #[test]
    fn either_retained_journal_mutation_blocks_initialization_admission() {
        assert_eq!(
            credential_initialization_gate(true, false),
            CredentialInitializationGate::DeferredForJournalMutation
        );
        assert_eq!(
            credential_initialization_gate(false, true),
            CredentialInitializationGate::DeferredForJournalMutation
        );
        assert_eq!(
            credential_initialization_gate(true, true),
            CredentialInitializationGate::DeferredForJournalMutation
        );
        assert_eq!(
            credential_initialization_gate(false, false),
            CredentialInitializationGate::Ready
        );
    }

    #[test]
    fn initialization_in_flight_blocks_journal_mutation_before_and_after_io() {
        for physical_io_attempted in [false, true] {
            assert_eq!(
                journal_mutation_gate(
                    true,
                    CredentialInitializationStatus::InFlight {
                        media: InitializableMedia::ExactlyErased,
                        physical_io_attempted,
                    },
                ),
                JournalMutationGate::DeferredForCredentialInitialization
            );
        }
    }

    #[test]
    fn stable_credential_states_leave_available_journal_runtime_ready() {
        let stable_states = [
            CredentialInitializationStatus::Unavailable,
            CredentialInitializationStatus::Eligible {
                media: InitializableMedia::ExactlyErased,
            },
            CredentialInitializationStatus::Eligible {
                media: InitializableMedia::RecoverableInterrupted,
            },
            CredentialInitializationStatus::Blocked {
                reason: InitializationBlockReason::NonCanonicalMountedAuthority,
            },
            CredentialInitializationStatus::Completed,
            CredentialInitializationStatus::PolicyFault {
                error: PermitError::NoOperation,
            },
        ];

        for state in stable_states {
            assert_eq!(
                journal_mutation_gate(true, state),
                JournalMutationGate::Ready,
                "state={state:?}"
            );
        }
    }

    #[test]
    fn initialization_deferral_is_never_reported_as_runtime_unavailable() {
        let deferred = journal_mutation_gate(
            true,
            CredentialInitializationStatus::InFlight {
                media: InitializableMedia::RecoverableInterrupted,
                physical_io_attempted: true,
            },
        );
        assert_eq!(
            deferred,
            JournalMutationGate::DeferredForCredentialInitialization
        );
        assert_ne!(deferred, JournalMutationGate::RuntimeUnavailable);
        assert_eq!(
            journal_mutation_gate(
                false,
                CredentialInitializationStatus::InFlight {
                    media: InitializableMedia::ExactlyErased,
                    physical_io_attempted: false,
                },
            ),
            JournalMutationGate::DeferredForCredentialInitialization,
            "in-flight ownership remains an explicit deferral even when no journal runtime exists"
        );

        assert_eq!(
            journal_mutation_gate(false, CredentialInitializationStatus::Completed),
            JournalMutationGate::RuntimeUnavailable
        );
    }
}
