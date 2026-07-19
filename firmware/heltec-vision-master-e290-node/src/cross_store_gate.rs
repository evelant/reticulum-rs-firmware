//! Pure cross-store physical-mutation exclusion for the permanent E290 flash owner.
//!
//! The credential store, durable submission journal, and bounded inbound
//! mailbox share one physical flash backend. The product coordinator serializes
//! every physical operation, while
//! these decisions make the stronger semantic rule explicit: retained journal
//! actor or projector work blocks admission of a credential physical mutation,
//! and any outstanding credential physical mutation blocks journal mutation
//! until credential ownership reaches a stable non-mutating state.
//!
//! Challenge-only credential work such as ProofStart neither owns nor initiates
//! physical mutation and intentionally does not consult the credential gate.

/// Admission decision for a credential operation that can mutate physical flash.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialPhysicalMutationGate {
    /// No journal mutation is retained, so credential mutation admission may run.
    Ready,
    /// Journal-owned physical progress must settle before credential admission.
    DeferredForJournalMutation,
}

/// Decide whether credential physical-mutation admission may run.
///
/// Both journal owners matter: actor state can retain an append or compaction,
/// while projector state can retain persistence required by an accepted frame.
/// Callers use this gate only for credential work that may lead to flash I/O;
/// challenge-only ProofStart work remains independent of journal ownership.
pub const fn credential_physical_mutation_gate(
    journal_actor_pending: bool,
    journal_projector_pending: bool,
) -> CredentialPhysicalMutationGate {
    if journal_actor_pending || journal_projector_pending {
        CredentialPhysicalMutationGate::DeferredForJournalMutation
    } else {
        CredentialPhysicalMutationGate::Ready
    }
}

/// Product decision before a path can initiate journal physical mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JournalMutationGate {
    /// The journal runtime exists and no credential operation excludes it.
    Ready,
    /// Credential-owned physical progress must settle before journal mutation.
    DeferredForCredentialMutation,
    /// Durable submission service has no usable resident runtime.
    RuntimeUnavailable,
}

/// Decide whether one journal mutation path may run.
///
/// Credential deferral is intentionally a different state from runtime absence.
/// Callers must retain journal service and retry after a deferral. The supplied
/// ownership fact covers initialization, live pairing, and credential-store
/// recovery; this gate deliberately does not reconstruct it from one sub-state.
pub const fn journal_mutation_gate(
    runtime_available: bool,
    credential_physical_mutation_outstanding: bool,
) -> JournalMutationGate {
    if credential_physical_mutation_outstanding {
        JournalMutationGate::DeferredForCredentialMutation
    } else if !runtime_available {
        JournalMutationGate::RuntimeUnavailable
    } else {
        JournalMutationGate::Ready
    }
}

/// Product decision before the bounded inbound mailbox can mutate flash.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InboundMailboxMutationGate {
    /// The mailbox is mounted and neither other durable owner retains mutation work.
    Ready,
    /// Credential-owned physical progress must settle before mailbox admission.
    DeferredForCredentialMutation,
    /// Submission-journal physical progress must settle before mailbox admission.
    DeferredForJournalMutation,
    /// The inbound mailbox has no usable resident runtime.
    RuntimeUnavailable,
}

/// Decide whether one inbound-mailbox commit may run synchronously.
///
/// The first qualification mailbox retains no ambiguous physical retry owner:
/// any non-reconciled backend failure disables that service for the boot. A
/// candidate deferred before I/O therefore owns plaintext but no flash
/// mutation. Credential and journal mutations still outrank it because their
/// retained operations must keep exclusive access to the shared backend.
pub const fn inbound_mailbox_mutation_gate(
    runtime_available: bool,
    credential_physical_mutation_outstanding: bool,
    journal_actor_pending: bool,
    journal_projector_pending: bool,
) -> InboundMailboxMutationGate {
    if credential_physical_mutation_outstanding {
        InboundMailboxMutationGate::DeferredForCredentialMutation
    } else if journal_actor_pending || journal_projector_pending {
        InboundMailboxMutationGate::DeferredForJournalMutation
    } else if !runtime_available {
        InboundMailboxMutationGate::RuntimeUnavailable
    } else {
        InboundMailboxMutationGate::Ready
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CredentialPhysicalMutationGate, InboundMailboxMutationGate, JournalMutationGate,
        credential_physical_mutation_gate, inbound_mailbox_mutation_gate, journal_mutation_gate,
    };

    #[test]
    fn either_retained_journal_owner_blocks_credential_physical_mutation() {
        let cases = [
            (false, false, CredentialPhysicalMutationGate::Ready),
            (
                true,
                false,
                CredentialPhysicalMutationGate::DeferredForJournalMutation,
            ),
            (
                false,
                true,
                CredentialPhysicalMutationGate::DeferredForJournalMutation,
            ),
            (
                true,
                true,
                CredentialPhysicalMutationGate::DeferredForJournalMutation,
            ),
        ];

        for (actor_pending, projector_pending, expected) in cases {
            assert_eq!(
                credential_physical_mutation_gate(actor_pending, projector_pending),
                expected,
                "actor_pending={actor_pending}, projector_pending={projector_pending}",
            );
        }
    }

    #[test]
    fn any_outstanding_credential_physical_mutation_blocks_journal_mutation() {
        for runtime_available in [false, true] {
            assert_eq!(
                journal_mutation_gate(runtime_available, true),
                JournalMutationGate::DeferredForCredentialMutation,
                "runtime_available={runtime_available}",
            );
        }
    }

    #[test]
    fn journal_runtime_state_controls_admission_without_credential_ownership() {
        assert_eq!(
            journal_mutation_gate(true, false),
            JournalMutationGate::Ready
        );
        assert_eq!(
            journal_mutation_gate(false, false),
            JournalMutationGate::RuntimeUnavailable
        );
    }

    #[test]
    fn credential_deferral_is_never_reported_as_runtime_unavailable() {
        let deferred = journal_mutation_gate(false, true);

        assert_eq!(deferred, JournalMutationGate::DeferredForCredentialMutation);
        assert_ne!(deferred, JournalMutationGate::RuntimeUnavailable);
    }

    #[test]
    fn inbox_commit_waits_for_every_retained_mutation_owner() {
        assert_eq!(
            inbound_mailbox_mutation_gate(true, false, false, false),
            InboundMailboxMutationGate::Ready
        );
        assert_eq!(
            inbound_mailbox_mutation_gate(true, true, false, false),
            InboundMailboxMutationGate::DeferredForCredentialMutation
        );
        for (actor, projector) in [(true, false), (false, true), (true, true)] {
            assert_eq!(
                inbound_mailbox_mutation_gate(true, false, actor, projector),
                InboundMailboxMutationGate::DeferredForJournalMutation
            );
        }
        assert_eq!(
            inbound_mailbox_mutation_gate(false, false, false, false),
            InboundMailboxMutationGate::RuntimeUnavailable
        );
    }

    #[test]
    fn credential_ownership_outranks_other_inbox_unavailability() {
        assert_eq!(
            inbound_mailbox_mutation_gate(false, true, true, true),
            InboundMailboxMutationGate::DeferredForCredentialMutation
        );
    }
}
