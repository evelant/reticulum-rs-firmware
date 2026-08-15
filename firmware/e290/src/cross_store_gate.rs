//! Pure cross-store physical-mutation exclusion for the permanent E290 flash owner.
//!
//! The credential store, durable submission journal, and append-only LXMF
//! store share one physical flash backend. The product
//! coordinator serializes every physical operation, while these decisions make
//! the stronger semantic exclusion rules explicit for retained mutations.
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
    /// LXMF-owned physical progress must settle before credential admission.
    DeferredForLxmfMutation,
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
    lxmf_mutation_pending: bool,
) -> CredentialPhysicalMutationGate {
    if journal_actor_pending || journal_projector_pending {
        CredentialPhysicalMutationGate::DeferredForJournalMutation
    } else if lxmf_mutation_pending {
        CredentialPhysicalMutationGate::DeferredForLxmfMutation
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
    /// LXMF-owned physical progress must settle before journal mutation.
    DeferredForLxmfMutation,
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
    lxmf_mutation_pending: bool,
) -> JournalMutationGate {
    if credential_physical_mutation_outstanding {
        JournalMutationGate::DeferredForCredentialMutation
    } else if lxmf_mutation_pending {
        JournalMutationGate::DeferredForLxmfMutation
    } else if !runtime_available {
        JournalMutationGate::RuntimeUnavailable
    } else {
        JournalMutationGate::Ready
    }
}

/// Admission decision before backend-free journal projection can retain work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JournalProjectionGate {
    /// Projection may run and can create journal persistence work.
    Ready,
    /// Preserve the exact frame outside the runtime until LXMF reconciliation.
    RetainForLxmfMutation,
}

/// Prevent journal projection from creating a cross-store retry deadlock.
///
/// Projection itself does not touch flash, but it can retain journal
/// persistence. Creating that owner while LXMF already retains an ambiguous
/// mutation would block each store behind the other.
pub const fn journal_projection_gate(lxmf_mutation_pending: bool) -> JournalProjectionGate {
    if lxmf_mutation_pending {
        JournalProjectionGate::RetainForLxmfMutation
    } else {
        JournalProjectionGate::Ready
    }
}

/// Product decision before a future LXMF candidate can reach the mounted store.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LxmfMutationGate {
    /// Other stores are stable; the LXMF store must now validate the candidate.
    Ready,
    /// Credential-owned physical progress must settle before LXMF admission.
    DeferredForCredentialMutation,
    /// Submission-journal physical progress must settle before LXMF admission.
    DeferredForJournalMutation,
    /// The LXMF store has no usable resident runtime.
    RuntimeUnavailable,
}

/// Decide whether a future exact LXMF candidate may reach store validation.
///
/// `lxmf_mutation_pending` is deliberately not a blocking condition. Only the
/// mounted store owns enough information to prove that a candidate is the exact
/// retry of its retained mutation; it rejects a different candidate before
/// physical I/O. Blocking here on the store's own pending bit would deadlock the
/// only structurally valid reconciliation path.
pub const fn lxmf_mutation_gate(
    runtime_available: bool,
    credential_physical_mutation_outstanding: bool,
    journal_actor_pending: bool,
    journal_projector_pending: bool,
    lxmf_mutation_pending: bool,
) -> LxmfMutationGate {
    let _ = lxmf_mutation_pending;
    if credential_physical_mutation_outstanding {
        LxmfMutationGate::DeferredForCredentialMutation
    } else if journal_actor_pending || journal_projector_pending {
        LxmfMutationGate::DeferredForJournalMutation
    } else if !runtime_available {
        LxmfMutationGate::RuntimeUnavailable
    } else {
        LxmfMutationGate::Ready
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CredentialPhysicalMutationGate, JournalMutationGate, JournalProjectionGate,
        LxmfMutationGate, credential_physical_mutation_gate, journal_mutation_gate,
        journal_projection_gate, lxmf_mutation_gate,
    };

    #[test]
    fn credential_gate_truth_table_covers_both_journal_owners_and_lxmf() {
        for actor_pending in [false, true] {
            for projector_pending in [false, true] {
                for lxmf_pending in [false, true] {
                    let expected = if actor_pending || projector_pending {
                        CredentialPhysicalMutationGate::DeferredForJournalMutation
                    } else if lxmf_pending {
                        CredentialPhysicalMutationGate::DeferredForLxmfMutation
                    } else {
                        CredentialPhysicalMutationGate::Ready
                    };
                    assert_eq!(
                        credential_physical_mutation_gate(
                            actor_pending,
                            projector_pending,
                            lxmf_pending,
                        ),
                        expected,
                        "actor={actor_pending} projector={projector_pending} lxmf={lxmf_pending}",
                    );
                }
            }
        }
    }

    #[test]
    fn any_outstanding_credential_physical_mutation_blocks_journal_mutation() {
        for runtime_available in [false, true] {
            for lxmf_pending in [false, true] {
                assert_eq!(
                    journal_mutation_gate(runtime_available, true, lxmf_pending),
                    JournalMutationGate::DeferredForCredentialMutation,
                    "runtime_available={runtime_available} lxmf={lxmf_pending}",
                );
            }
        }
    }

    #[test]
    fn journal_gate_truth_table_covers_runtime_credentials_and_lxmf() {
        for runtime_available in [false, true] {
            for credential_pending in [false, true] {
                for lxmf_pending in [false, true] {
                    let expected = if credential_pending {
                        JournalMutationGate::DeferredForCredentialMutation
                    } else if lxmf_pending {
                        JournalMutationGate::DeferredForLxmfMutation
                    } else if !runtime_available {
                        JournalMutationGate::RuntimeUnavailable
                    } else {
                        JournalMutationGate::Ready
                    };
                    assert_eq!(
                        journal_mutation_gate(runtime_available, credential_pending, lxmf_pending,),
                        expected,
                        "runtime={runtime_available} credential={credential_pending} lxmf={lxmf_pending}",
                    );
                }
            }
        }
    }

    #[test]
    fn journal_projection_retains_the_frame_while_lxmf_owns_reconciliation() {
        assert_eq!(journal_projection_gate(false), JournalProjectionGate::Ready);
        assert_eq!(
            journal_projection_gate(true),
            JournalProjectionGate::RetainForLxmfMutation
        );
    }

    #[test]
    fn lxmf_gate_truth_table_never_deadlocks_its_own_exact_retry() {
        for runtime_available in [false, true] {
            for credential_pending in [false, true] {
                for actor_pending in [false, true] {
                    for projector_pending in [false, true] {
                        let expected = if credential_pending {
                            LxmfMutationGate::DeferredForCredentialMutation
                        } else if actor_pending || projector_pending {
                            LxmfMutationGate::DeferredForJournalMutation
                        } else if !runtime_available {
                            LxmfMutationGate::RuntimeUnavailable
                        } else {
                            LxmfMutationGate::Ready
                        };
                        let without_pending = lxmf_mutation_gate(
                            runtime_available,
                            credential_pending,
                            actor_pending,
                            projector_pending,
                            false,
                        );
                        let with_pending = lxmf_mutation_gate(
                            runtime_available,
                            credential_pending,
                            actor_pending,
                            projector_pending,
                            true,
                        );
                        assert_eq!(without_pending, expected);
                        assert_eq!(with_pending, expected);
                    }
                }
            }
        }
    }
}
