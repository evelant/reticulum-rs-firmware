//! Resident credential initialization ownership for the permanent E290 node.
//!
//! Boot remains read-only for erased or canonically interrupted credential
//! media. This coordinator consumes that boot result, owns the physical-
//! presence policy and any admitted initialization capability, and accepts only
//! forward progress when it later receives a fresh operation-scoped flash view.
//! Live pairing retains every semantic, policy, proof, and physical-store owner
//! until a definite terminal result; bearer framing remains outside this module.

use core::mem;

use rand_core::{CryptoRng, RngCore};
use reticulum_device_api::IdentitySummary;
use reticulum_device_api_adapter::{
    InboundMailboxPort, LxmfComposePort, LxmfInboxPort, ManualServiceAnnouncePort,
    NetworkConfigPort, NodeDiagnosticsPort, NomadFetchPort, PeerDiscoveryPort, ReticulumProbePort,
    SubmissionPort,
};
use reticulum_device_api_credential_store::{
    BoundCredentialStoreAccess, CommitPairingLifecycleSuccessorError, CredentialStoreBinding,
    CredentialStoreBindingError, CredentialStoreFault, CredentialStoreMountError,
    CredentialStoreRecovery, EmptyProvisionMediaClassification, MountedCredentialStore,
    PendingPairingLifecycleSuccessor, classify_empty_provision_media,
    commit_pairing_lifecycle_successor, mount, reconcile_pairing_lifecycle_successor,
    recover_empty_provision, recover_once,
};
use reticulum_device_api_credentials::{
    AuthorizationPolicyVersion, CredentialAuthority, CredentialId, CredentialLifecycleFaultKind,
    E290_CREDENTIAL_RECORD_CAPACITY, NewPendingCredential, PairingLifecycleStoreCandidate,
    PairingOrigin, PendingCredentialRef, Permissions, PrincipalId, SelectedCredential,
};
use reticulum_device_api_handoff::{LocalApiReply, LocalApiRequest};
use reticulum_device_api_pairing::{
    ActivateFailure, ActivateRequest, BearerBinding, DeviceChallenge, DeviceId, PairingFailure,
    PairingPsk, PairingTranscript, ProofChallenge, ProofStartRequest,
};
use reticulum_device_api_pairing_policy::{
    AbortOutcome, AcquirePairingExclusive, ActivationOutcome, ActiveLowButton, AttemptDecision,
    AttemptRefusal, BeginFacts, BeginOutcome, ButtonEffect, ConnectionId, ConnectionRefusal,
    ExclusiveAcquireOutcome, InitializableMedia, InitializationFacts, InitializationPermit,
    MonotonicMillis, PairingPolicy, PendingRef, PendingState, PermitError, PolicyEvent,
    RequestRefusal, RequestRefused, WindowClosed,
};
use reticulum_device_api_session::AuthenticatedGrant;
use zeroize::Zeroizing;

use crate::authenticated_api_node::{
    AuthenticatedApiDispatchFailure,
    dispatch_authenticated_request_with_inbox_lxmf_nomad_and_network_config,
};
use crate::credential_boot::{
    CredentialBootOutcome, CredentialBootState, MAXIMUM_CREDENTIAL_BOOT_OUTCOME_BYTES,
};
use crate::credential_pairing::{
    AbortAdmission, ActivateAdmission, BeginAdmission, CredentialPairingDriveOutcome,
    CredentialPairingStatus, LivePairingOwnership, MAX_PAIRING_ENTROPY_ATTEMPTS,
    MAXIMUM_LIVE_PAIRING_OWNERSHIP_BYTES, MutationCompletion, PairingDriveRetry, PairingMutation,
    ProofOwnership, ProofStartAdmission,
};

/// Conservative fixed-RAM ceiling for retained credential and pairing state.
pub const MAXIMUM_CREDENTIAL_RUNTIME_BYTES: usize = MAXIMUM_CREDENTIAL_BOOT_OUTCOME_BYTES
    + reticulum_device_api_pairing_policy::PAIRING_POLICY_RAM_CEILING
    + MAXIMUM_LIVE_PAIRING_OWNERSHIP_BYTES
    + 512;

/// Public snapshot of explicit initialization ownership.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialInitializationStatus {
    /// The mounted or failed boot state is not eligible for initialization.
    Unavailable,
    /// One exact boot-classified trajectory may be admitted under physical
    /// presence.
    Eligible {
        /// Exact media classification retained from boot.
        media: InitializableMedia,
    },
    /// An admitted operation remains owned until a definite physical result.
    InFlight {
        /// Exact media trajectory admitted by the policy.
        media: InitializableMedia,
        /// Whether this operation may already have attempted physical writes.
        physical_io_attempted: bool,
    },
    /// Initialization is permanently blocked for this boot while the admitted
    /// capability remains retained.
    Blocked {
        /// Stable non-secret reason for refusing further physical progress.
        reason: InitializationBlockReason,
    },
    /// Canonical empty revision 1 is mounted and policy ownership was released.
    Completed,
    /// Canonical empty revision 1 is retained, but policy completion violated
    /// an internal ownership invariant, so later mutation remains disabled.
    PolicyFault {
        /// Exact policy completion failure.
        error: PermitError,
    },
}

/// Stable fail-closed reason for stopping one admitted initialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitializationBlockReason {
    /// A later operation-scoped view did not name the retained product range.
    AccessBindingMismatch {
        /// Binding captured from the boot flash owner.
        expected: CredentialStoreBinding,
        /// Binding supplied by the later operation-scoped view.
        actual: CredentialStoreBinding,
    },
    /// The portable store rejected the operation-scoped binding before I/O.
    StoreBinding(CredentialStoreBindingError),
    /// Media moved outside the exact forward trajectory admitted by policy.
    MediaTrajectory {
        /// Trajectory admitted under physical presence.
        admitted: InitializableMedia,
        /// Latest read-only physical classification.
        observed: EmptyProvisionMediaClassification,
        /// Whether this owner may already have attempted physical writes.
        physical_io_attempted: bool,
    },
    /// Stable media inspection or recovery fault.
    MediaFault(CredentialStoreFault),
    /// A supposedly completed store was not canonical empty revision 1.
    NonCanonicalMountedAuthority,
}

/// Why an initialization request was not accepted by the resident owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitializationRequestRefusal {
    /// Credential boot state could not construct a safe pairing policy owner.
    PairingUnavailable,
    /// The physical-presence policy refused the request.
    Policy(RequestRefused),
}

/// Non-secret acceptance report; the actual permit remains inside the runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InitializationAccepted {
    media: InitializableMedia,
}

impl InitializationAccepted {
    /// Exact media trajectory bound into the retained permit.
    pub const fn media(self) -> InitializableMedia {
        self.media
    }
}

/// Retry condition that retains the exact admitted operation.
pub enum InitializationRetry<E> {
    /// The caller's fresh identity preflight was not ready; no store I/O ran.
    IdentityNotReady,
    /// A read or physical operation returned an ambiguous backend result.
    Backend(E),
    /// Exact write readback was not established; reclassification must decide
    /// whether the same operation can continue.
    Readback(CredentialStoreFault),
}

/// Result of driving at most one admitted initialization to a synchronous
/// physical outcome.
pub enum InitializationDriveOutcome<E> {
    /// Canonical empty revision 1 is mounted and mutation can proceed later.
    Completed,
    /// The exact permit remains retained for a later same-boot retry.
    Retry(InitializationRetry<E>),
    /// This boot retains the permit but will perform no further initialization
    /// I/O.
    Blocked(InitializationBlockReason),
    /// No initialization operation currently owns physical progress.
    NotInFlight(CredentialInitializationStatus),
}

/// Uniform refusal for ordinary authenticated-session credential selection.
///
/// Connection, pairing-exclusion, authority-publication, and credential
/// lookup failures deliberately collapse to this opaque result so callers
/// cannot use the selection edge as a credential-existence oracle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrdinarySessionSelectionRefusal {
    _private: (),
}

impl OrdinarySessionSelectionRefusal {
    const REFUSED: Self = Self { _private: () };
}

enum InitializationOwnership {
    Unavailable,
    Eligible(InitializableMedia),
    InFlight {
        permit: InitializationPermit,
        physical_io_attempted: bool,
    },
    Blocked {
        permit: InitializationPermit,
        reason: InitializationBlockReason,
    },
    Completed,
    PolicyFault(PermitError),
}

/// Backend-independent resident credential owner for the permanent E290 node.
///
/// The raw flash backend is never retained here. Each physical drive borrows a
/// fresh bound view from the sole product flash owner and verifies that it is
/// exactly the range captured at boot.
#[must_use = "credential and pairing ownership must remain resident"]
pub struct CredentialRuntime {
    binding: CredentialStoreBinding,
    device_id: DeviceId,
    boot_state: CredentialBootState,
    mounted: Option<MountedCredentialStore>,
    pairing: Option<PairingPolicy>,
    live_pairing: LivePairingOwnership,
    initialization: InitializationOwnership,
}

const _: () = assert!(mem::size_of::<CredentialRuntime>() <= MAXIMUM_CREDENTIAL_RUNTIME_BYTES);

impl CredentialRuntime {
    /// Consume boot ownership into the expected product-flash binding.
    pub fn from_boot(
        outcome: CredentialBootOutcome,
        expected: CredentialStoreBinding,
        device_id: DeviceId,
    ) -> Self {
        let (boot_state, mounted) = outcome.into_parts_for_binding(expected);
        let initialization = match boot_state {
            CredentialBootState::Ready | CredentialBootState::AuthenticationOnly { .. } => {
                InitializationOwnership::Completed
            }
            CredentialBootState::UninitializedErased => {
                InitializationOwnership::Eligible(InitializableMedia::ExactlyErased)
            }
            CredentialBootState::InitializationInterrupted => {
                InitializationOwnership::Eligible(InitializableMedia::RecoverableInterrupted)
            }
            _ => InitializationOwnership::Unavailable,
        };
        let pairing = pairing_policy_for_boot(boot_state, mounted.as_ref());
        Self {
            binding: expected,
            device_id,
            boot_state,
            mounted,
            pairing,
            live_pairing: LivePairingOwnership::Idle,
            initialization,
        }
    }

    /// Credential boot admission retained by the runtime.
    pub const fn credential_boot_state(&self) -> CredentialBootState {
        self.boot_state
    }

    /// Exact physical binding required for every later operation-scoped view.
    pub const fn binding(&self) -> CredentialStoreBinding {
        self.binding
    }

    /// Stable device-API identifier bound for the complete resident lifetime.
    pub const fn device_id(&self) -> DeviceId {
        self.device_id
    }

    /// Non-secret mounted authority revision, when one is retained.
    pub fn revision(&self) -> Option<u64> {
        self.mounted
            .as_ref()
            .map(|mounted| mounted.revision().get())
    }

    /// Number of active application credentials in the currently publishable
    /// authority.
    ///
    /// `None` distinguishes an unavailable authority from a valid empty
    /// authority. Bluetooth bond state is intentionally absent from this
    /// application-level setup fact.
    pub fn active_credential_count(&self) -> Option<usize> {
        if !self.boot_state.authority_publishable() {
            return None;
        }
        self.mounted
            .as_ref()
            .and_then(MountedCredentialStore::publishable_authority)
            .map(reticulum_device_api_credentials::CredentialAuthority::active_count)
    }

    /// Whether a retained authority can authenticate existing credentials.
    pub fn authority_publishable(&self) -> bool {
        self.boot_state.authority_publishable()
            && self
                .mounted
                .as_ref()
                .and_then(MountedCredentialStore::publishable_authority)
                .is_some()
    }

    /// Admit one ordinary session attempt and select its zeroizing credential.
    ///
    /// The pairing arbiter is checked before authority lookup and revalidated
    /// after selection. Every refusal is intentionally indistinguishable at
    /// this boundary, including a missing, pending, or revoked credential.
    pub fn select_ordinary_session(
        &mut self,
        at: MonotonicMillis,
        connection: ConnectionId,
        credential_id: CredentialId,
    ) -> Result<SelectedCredential, OrdinarySessionSelectionRefusal> {
        let permit = self
            .pairing
            .as_mut()
            .ok_or(OrdinarySessionSelectionRefusal::REFUSED)?
            .ordinary_session(at, connection)
            .map_err(|_| OrdinarySessionSelectionRefusal::REFUSED)?;
        let selected = self
            .mounted
            .as_ref()
            .and_then(MountedCredentialStore::publishable_authority)
            .filter(|_| self.boot_state.authority_publishable())
            .and_then(|authority| authority.select_for_handshake(credential_id).ok())
            .ok_or(OrdinarySessionSelectionRefusal::REFUSED)?;
        if !self
            .pairing
            .as_ref()
            .is_some_and(|policy| policy.ordinary_session_is_current(&permit))
        {
            return Err(OrdinarySessionSelectionRefusal::REFUSED);
        }
        Ok(selected)
    }

    /// Revalidate and dispatch one session-authenticated request against the
    /// currently publishable authority without exposing that authority.
    ///
    /// The caller supplies a copy-only public identity summary and the disjoint
    /// logical submission port. Missing, replaced, revoked, or otherwise
    /// unpublished authority state therefore takes the helper's zero-port-I/O
    /// authentication-failure path.
    #[allow(
        clippy::result_large_err,
        reason = "terminal failure must retain the exact allocation-free request owner"
    )]
    pub fn dispatch_authenticated_request<P, N>(
        &self,
        request: LocalApiRequest<AuthenticatedGrant>,
        identity: IdentitySummary,
        port: &mut P,
        nomad_port: &mut N,
    ) -> Result<LocalApiReply, AuthenticatedApiDispatchFailure>
    where
        P: SubmissionPort
            + InboundMailboxPort
            + LxmfInboxPort
            + LxmfComposePort
            + PeerDiscoveryPort
            + NetworkConfigPort
            + ManualServiceAnnouncePort
            + NodeDiagnosticsPort,
        N: NomadFetchPort,
    {
        let authority = self
            .mounted
            .as_ref()
            .and_then(MountedCredentialStore::publishable_authority)
            .filter(|_| self.boot_state.authority_publishable());
        dispatch_authenticated_request_with_inbox_lxmf_nomad_and_network_config(
            request, authority, identity, port, nomad_port,
        )
    }

    /// Revalidate and dispatch through the complete appliance surface plus one
    /// independent volatile Reticulum proof-probe owner.
    #[allow(
        clippy::result_large_err,
        reason = "terminal failure must retain the exact allocation-free request owner"
    )]
    pub fn dispatch_authenticated_request_with_probe<P, N, Q>(
        &self,
        request: LocalApiRequest<AuthenticatedGrant>,
        identity: IdentitySummary,
        port: &mut P,
        nomad_port: &mut N,
        probe_port: &mut Q,
    ) -> Result<LocalApiReply, AuthenticatedApiDispatchFailure>
    where
        P: SubmissionPort
            + InboundMailboxPort
            + LxmfInboxPort
            + LxmfComposePort
            + PeerDiscoveryPort
            + NetworkConfigPort
            + ManualServiceAnnouncePort
            + NodeDiagnosticsPort,
        N: NomadFetchPort,
        Q: ReticulumProbePort,
    {
        let authority = self
            .mounted
            .as_ref()
            .and_then(MountedCredentialStore::publishable_authority)
            .filter(|_| self.boot_state.authority_publishable());
        crate::authenticated_api_node::dispatch_authenticated_request_with_inbox_lxmf_nomad_network_config_and_probe(
            request,
            authority,
            identity,
            port,
            nomad_port,
            probe_port,
        )
    }

    /// Whether the retained authority is physically and locally eligible for a
    /// future credential mutation.
    pub fn mutation_eligible(&self) -> bool {
        self.boot_state.mutation_eligible()
            && self.pairing.is_some()
            && self.live_pairing.is_idle()
            && self
                .mounted
                .as_ref()
                .is_some_and(|mounted| mounted.recovery() == CredentialStoreRecovery::Clean)
            && !matches!(
                self.initialization,
                InitializationOwnership::Blocked { .. } | InitializationOwnership::PolicyFault(_)
            )
    }

    /// Whether boot supplied enough validated state to own pairing policy.
    pub const fn pairing_policy_available(&self) -> bool {
        self.pairing.is_some()
    }

    /// Current non-secret live-pairing ownership and cleanup state.
    pub fn live_pairing_status(&self) -> CredentialPairingStatus {
        match &self.live_pairing {
            LivePairingOwnership::Idle => {
                if !matches!(
                    self.boot_state,
                    CredentialBootState::AuthenticationOnly { .. }
                ) && self
                    .mounted
                    .as_ref()
                    .is_some_and(|store| store.recovery() != CredentialStoreRecovery::Clean)
                {
                    CredentialPairingStatus::CleanupRequired
                } else {
                    CredentialPairingStatus::Idle
                }
            }
            LivePairingOwnership::Proof(_) => CredentialPairingStatus::ProofOutstanding,
            LivePairingOwnership::AwaitingCleanStore(completion) => {
                CredentialPairingStatus::AwaitingCleanStore(completion.mutation())
            }
            LivePairingOwnership::Prepared { completion, .. } => {
                CredentialPairingStatus::MutationPrepared(completion.mutation())
            }
            LivePairingOwnership::Reconciling { completion, .. } => {
                CredentialPairingStatus::ReconcileRequired(completion.mutation())
            }
            LivePairingOwnership::Blocked => CredentialPairingStatus::Blocked,
        }
    }

    /// Whether journal physical mutation must remain excluded by credential work.
    pub fn credential_physical_mutation_outstanding(&self) -> bool {
        matches!(
            self.initialization,
            InitializationOwnership::InFlight { .. }
        ) || self.live_pairing.mutation().is_some()
            || (!matches!(
                self.boot_state,
                CredentialBootState::AuthenticationOnly { .. }
            ) && self
                .mounted
                .as_ref()
                .is_some_and(|store| store.recovery() != CredentialStoreRecovery::Clean))
    }

    /// Current non-secret explicit-initialization state.
    pub const fn initialization_status(&self) -> CredentialInitializationStatus {
        match &self.initialization {
            InitializationOwnership::Unavailable => CredentialInitializationStatus::Unavailable,
            InitializationOwnership::Eligible(media) => {
                CredentialInitializationStatus::Eligible { media: *media }
            }
            InitializationOwnership::InFlight {
                permit,
                physical_io_attempted,
            } => CredentialInitializationStatus::InFlight {
                media: permit.media(),
                physical_io_attempted: *physical_io_attempted,
            },
            InitializationOwnership::Blocked { reason, .. } => {
                CredentialInitializationStatus::Blocked { reason: *reason }
            }
            InitializationOwnership::Completed => CredentialInitializationStatus::Completed,
            InitializationOwnership::PolicyFault(error) => {
                CredentialInitializationStatus::PolicyFault { error: *error }
            }
        }
    }

    /// Forward one strictly increasing connection epoch to the retained policy.
    pub fn pairing_connected(
        &mut self,
        now: MonotonicMillis,
        connection: ConnectionId,
    ) -> Option<Result<Option<WindowClosed>, ConnectionRefusal>> {
        let outcome = self.pairing.as_mut()?.connected(now, connection);
        if outcome.is_ok() || matches!(outcome, Err(ConnectionRefusal::ClockRegression)) {
            self.cancel_challenge_only();
        }
        Some(outcome)
    }

    /// Forward a disconnect while retaining any admitted physical operation.
    pub fn pairing_disconnected(
        &mut self,
        now: MonotonicMillis,
        connection: ConnectionId,
    ) -> Option<PolicyEvent> {
        let event = self.pairing.as_mut()?.disconnected(now, connection);
        if matches!(event, PolicyEvent::Fault(_)) {
            self.cancel_challenge_only();
        } else {
            self.cancel_challenge_for_connection(connection);
        }
        Some(event)
    }

    /// Forward one already-debounced physical-presence sample.
    pub fn pairing_observe_button(
        &mut self,
        now: MonotonicMillis,
        level: ActiveLowButton,
    ) -> Option<ButtonEffect> {
        if matches!(
            self.boot_state,
            CredentialBootState::AuthenticationOnly { .. }
        ) {
            let event = self.pairing.as_mut()?.poll_timeout(now);
            return Some(match event {
                PolicyEvent::None => ButtonEffect::None,
                PolicyEvent::Closed(closed) => ButtonEffect::Closed(closed),
                PolicyEvent::Fault(fault) => ButtonEffect::Fault(fault),
            });
        }
        let effect = self.pairing.as_mut()?.observe_button(now, level);
        if matches!(effect, ButtonEffect::Closed(_) | ButtonEffect::Fault(_)) {
            self.cancel_challenge_only();
        }
        Some(effect)
    }

    /// Confirm that the bearer granted the requested exclusive connection.
    pub fn pairing_exclusive_acquired(
        &mut self,
        now: MonotonicMillis,
        effect: AcquirePairingExclusive,
    ) -> Option<ExclusiveAcquireOutcome> {
        if matches!(
            self.boot_state,
            CredentialBootState::AuthenticationOnly { .. }
        ) {
            let event = self.pairing.as_mut()?.poll_timeout(now);
            return Some(match event {
                PolicyEvent::None => ExclusiveAcquireOutcome::Stale,
                PolicyEvent::Closed(closed) => ExclusiveAcquireOutcome::Closed(closed),
                PolicyEvent::Fault(fault) => ExclusiveAcquireOutcome::Fault(fault),
            });
        }
        let outcome = self.pairing.as_mut()?.exclusive_acquired(now, effect);
        if matches!(
            &outcome,
            ExclusiveAcquireOutcome::Closed(_) | ExclusiveAcquireOutcome::Fault(_)
        ) {
            self.cancel_challenge_only();
        }
        Some(outcome)
    }

    /// Poll the retained physical-presence window deadline.
    pub fn pairing_poll_timeout(&mut self, now: MonotonicMillis) -> Option<PolicyEvent> {
        let challenge_expired = matches!(
            &self.live_pairing,
            LivePairingOwnership::Proof(proof) if now >= proof.permit.deadline()
        );
        let event = self.pairing.as_mut()?.poll_timeout(now);
        if challenge_expired || !matches!(event, PolicyEvent::None) {
            self.cancel_challenge_only();
        }
        Some(event)
    }

    /// Admit one empty Begin request and retain its complete AddPending candidate.
    ///
    /// This performs no physical I/O. The caller must exclude journal mutation
    /// before admission and then drive the accepted owner with
    /// [`Self::drive_live_pairing`].
    pub fn request_pairing_begin<R>(
        &mut self,
        bearer: BearerBinding,
        now: MonotonicMillis,
        connection: ConnectionId,
        rng: &mut R,
    ) -> BeginAdmission
    where
        R: RngCore + CryptoRng,
    {
        if matches!(
            self.boot_state,
            CredentialBootState::AuthenticationOnly { .. }
        ) {
            return BeginAdmission::Refused(PairingFailure::Unavailable);
        }
        let mutation_ready = self.mutation_eligible();
        let (retained_capacity_available, next_revision_available) = self
            .mounted
            .as_ref()
            .and_then(MountedCredentialStore::publishable_authority)
            .map(|authority| {
                (
                    authority.record_count() < E290_CREDENTIAL_RECORD_CAPACITY,
                    authority.revision().next().is_ok(),
                )
            })
            .unwrap_or((false, false));
        let Some(policy) = self.pairing.as_mut() else {
            return BeginAdmission::Refused(PairingFailure::Unavailable);
        };
        let decision = policy.begin(
            now,
            connection,
            BeginFacts::new(
                mutation_ready,
                retained_capacity_available,
                next_revision_available,
            ),
        );
        let permit = match decision {
            AttemptDecision::Admitted { permit, .. } => permit,
            AttemptDecision::Refused { reason, .. } => {
                return BeginAdmission::Refused(public_attempt_refusal(reason));
            }
        };
        if !self.live_pairing.is_idle() {
            let result = policy.finish_begin(permit, BeginOutcome::NotCommitted);
            if result.is_err() {
                self.live_pairing = LivePairingOwnership::Blocked;
            }
            return BeginAdmission::Refused(PairingFailure::Blocked);
        }
        let Some(authority) = self
            .mounted
            .as_ref()
            .and_then(MountedCredentialStore::publishable_authority)
        else {
            let result = policy.finish_begin(permit, BeginOutcome::NotCommitted);
            if result.is_err() {
                self.live_pairing = LivePairingOwnership::Blocked;
            }
            return BeginAdmission::Refused(PairingFailure::Blocked);
        };
        let Some((candidate, pending)) = prepare_begin_candidate(authority, rng) else {
            let result = policy.finish_begin(permit, BeginOutcome::NotCommitted);
            if result.is_err() {
                self.live_pairing = LivePairingOwnership::Blocked;
            }
            return BeginAdmission::Refused(PairingFailure::Blocked);
        };
        self.live_pairing = LivePairingOwnership::Prepared {
            completion: MutationCompletion::Begin {
                permit,
                bearer,
                device_id: self.device_id,
                pending,
            },
            candidate,
        };
        BeginAdmission::Accepted
    }

    /// Admit ProofStart, select only the exact publishable Pending secret, and
    /// retain its permit, secret, and transcript for one Activate continuation.
    pub fn request_pairing_proof_start<R>(
        &mut self,
        now: MonotonicMillis,
        connection: ConnectionId,
        request: &ProofStartRequest,
        rng: &mut R,
    ) -> ProofStartAdmission
    where
        R: RngCore + CryptoRng,
    {
        if matches!(
            self.boot_state,
            CredentialBootState::AuthenticationOnly { .. }
        ) {
            return ProofStartAdmission::Refused(PairingFailure::Unavailable);
        }
        let pending = match PendingRef::new(request.credential_id(), request.generation()) {
            Ok(pending) => pending,
            Err(_) => return ProofStartAdmission::Refused(PairingFailure::Refused),
        };
        let Some(policy) = self.pairing.as_mut() else {
            return ProofStartAdmission::Refused(PairingFailure::Unavailable);
        };
        let permit = match policy.proof(now, connection, pending) {
            AttemptDecision::Admitted { permit, .. } => permit,
            AttemptDecision::Refused { reason, .. } => {
                return ProofStartAdmission::Refused(public_attempt_refusal(reason));
            }
        };
        if !self.live_pairing.is_idle() {
            let _ = policy.proof_rejected(permit);
            return ProofStartAdmission::Refused(PairingFailure::Blocked);
        }
        let selected = self.mounted.as_ref().and_then(|store| {
            store
                .select_pending_for_proof(PendingCredentialRef::new(
                    pending.id(),
                    pending.generation(),
                ))
                .ok()
        });
        let Some(selected) = selected else {
            let _ = policy.proof_rejected(permit);
            return ProofStartAdmission::Refused(PairingFailure::Refused);
        };
        let (_, selected_psk) = selected.into_parts();
        let psk = match PairingPsk::from_zeroizing(selected_psk) {
            Ok(psk) => psk,
            Err(_) => {
                let _ = policy.proof_rejected(permit);
                return ProofStartAdmission::Refused(PairingFailure::Blocked);
            }
        };
        let Some(challenge) = generate_challenge(rng) else {
            let _ = policy.proof_rejected(permit);
            return ProofStartAdmission::Refused(PairingFailure::Blocked);
        };
        let Some(protocol_connection) =
            reticulum_device_api_pairing::ConnectionId::new(permit.connection().get())
        else {
            let _ = policy.proof_rejected(permit);
            return ProofStartAdmission::Refused(PairingFailure::Blocked);
        };
        let Some(protocol_window) =
            reticulum_device_api_pairing::WindowId::new(permit.window().get())
        else {
            let _ = policy.proof_rejected(permit);
            return ProofStartAdmission::Refused(PairingFailure::Blocked);
        };
        let challenge = match ProofChallenge::new(
            request.bearer(),
            self.device_id,
            protocol_connection,
            protocol_window,
            pending.id(),
            pending.generation(),
            challenge,
        ) {
            Ok(challenge) => challenge,
            Err(_) => {
                let _ = policy.proof_rejected(permit);
                return ProofStartAdmission::Refused(PairingFailure::Blocked);
            }
        };
        let transcript = match PairingTranscript::new(request, &challenge) {
            Ok(transcript) => transcript,
            Err(_) => {
                let _ = policy.proof_rejected(permit);
                return ProofStartAdmission::Refused(PairingFailure::Blocked);
            }
        };
        self.live_pairing = LivePairingOwnership::Proof(ProofOwnership {
            permit,
            psk,
            transcript,
        });
        ProofStartAdmission::Challenge(challenge)
    }

    /// Consume the only outstanding ProofStart continuation.
    ///
    /// A wrong reference or HMAC is terminal for the challenge-only operation:
    /// policy ownership is released and every retained secret is dropped.
    pub fn request_pairing_activate(
        &mut self,
        bearer: BearerBinding,
        now: MonotonicMillis,
        connection: ConnectionId,
        request: ActivateRequest,
    ) -> ActivateAdmission {
        if matches!(
            self.boot_state,
            CredentialBootState::AuthenticationOnly { .. }
        ) {
            drop(request);
            return ActivateAdmission::Refused(ActivateFailure::Unavailable);
        }
        let ownership = mem::replace(&mut self.live_pairing, LivePairingOwnership::Blocked);
        let LivePairingOwnership::Proof(proof) = ownership else {
            self.live_pairing = ownership;
            drop(request);
            return ActivateAdmission::Refused(ActivateFailure::ProofRejected);
        };
        if proof.transcript.bearer() != bearer {
            drop(request);
            self.live_pairing = match self
                .pairing
                .as_mut()
                .and_then(|policy| policy.proof_rejected(proof.permit).ok())
            {
                Some(()) => LivePairingOwnership::Idle,
                None => LivePairingOwnership::Blocked,
            };
            return ActivateAdmission::Refused(ActivateFailure::ProofRejected);
        }
        let continuation_is_current = self.pairing.as_mut().is_some_and(|policy| {
            policy.proof_continuation_is_current(now, connection, &proof.permit)
        });
        if !continuation_is_current {
            drop(request);
            self.live_pairing = match self
                .pairing
                .as_mut()
                .and_then(|policy| policy.proof_rejected(proof.permit).ok())
            {
                Some(()) => LivePairingOwnership::Idle,
                None => LivePairingOwnership::Blocked,
            };
            return ActivateAdmission::Refused(ActivateFailure::ProofRejected);
        }
        let verified = match request.verify_continuation(&proof.psk, &proof.transcript) {
            Ok(verified) => verified,
            Err(_) => {
                self.live_pairing = match self
                    .pairing
                    .as_mut()
                    .and_then(|policy| policy.proof_rejected(proof.permit).ok())
                {
                    Some(()) => LivePairingOwnership::Idle,
                    None => LivePairingOwnership::Blocked,
                };
                return ActivateAdmission::Refused(ActivateFailure::ProofRejected);
            }
        };
        let Some(policy) = self.pairing.as_mut() else {
            self.live_pairing = LivePairingOwnership::Blocked;
            return ActivateAdmission::Refused(ActivateFailure::Unavailable);
        };
        let permit = match policy.proof_verified(proof.permit) {
            Ok(permit) => permit,
            Err(_) => {
                self.live_pairing = LivePairingOwnership::Blocked;
                return ActivateAdmission::Refused(ActivateFailure::Blocked);
            }
        };
        self.live_pairing =
            LivePairingOwnership::AwaitingCleanStore(MutationCompletion::Activate {
                permit,
                verified,
                psk: proof.psk,
            });
        ActivateAdmission::Accepted
    }

    /// Admit identifier-free AbortCurrent for the device-selected sole Pending.
    pub fn request_pairing_abort_current(
        &mut self,
        now: MonotonicMillis,
        connection: ConnectionId,
    ) -> AbortAdmission {
        if matches!(
            self.boot_state,
            CredentialBootState::AuthenticationOnly { .. }
        ) {
            return AbortAdmission::Refused(PairingFailure::Unavailable);
        }
        let Some(policy) = self.pairing.as_mut() else {
            return AbortAdmission::Refused(PairingFailure::Unavailable);
        };
        let permit = match policy.abort_current(now, connection) {
            Ok(permit) => permit,
            Err(refused) => {
                return AbortAdmission::Refused(public_request_refusal(refused.reason()));
            }
        };
        if !self.live_pairing.is_idle() {
            let _ = policy.finish_abort(permit, AbortOutcome::NotCommitted);
            return AbortAdmission::Refused(PairingFailure::Blocked);
        }
        self.live_pairing =
            LivePairingOwnership::AwaitingCleanStore(MutationCompletion::Abort { permit });
        AbortAdmission::Accepted
    }

    /// Advance at most one cleanup, candidate-construction, commit, or
    /// reconciliation stage through the supplied operation-scoped store view.
    pub fn drive_live_pairing<A>(&mut self, access: &mut A) -> CredentialPairingDriveOutcome
    where
        A: BoundCredentialStoreAccess,
    {
        if matches!(
            self.boot_state,
            CredentialBootState::AuthenticationOnly { .. }
        ) {
            return CredentialPairingDriveOutcome::Blocked(self.live_pairing.mutation());
        }
        let ownership = mem::replace(&mut self.live_pairing, LivePairingOwnership::Blocked);
        match ownership {
            LivePairingOwnership::Idle => {
                self.live_pairing = LivePairingOwnership::Idle;
                self.drive_credential_cleanup(access, None)
            }
            ownership @ LivePairingOwnership::Proof(_) => {
                self.live_pairing = ownership;
                CredentialPairingDriveOutcome::Idle
            }
            LivePairingOwnership::AwaitingCleanStore(completion) => {
                self.drive_awaiting_clean_store(access, completion)
            }
            LivePairingOwnership::Prepared {
                completion,
                candidate,
            } => self.drive_prepared_mutation(access, completion, candidate),
            LivePairingOwnership::Reconciling {
                completion,
                pending,
            } => self.drive_reconciliation(access, completion, pending),
            LivePairingOwnership::Blocked => {
                self.live_pairing = LivePairingOwnership::Blocked;
                CredentialPairingDriveOutcome::Blocked(None)
            }
        }
    }

    fn drive_credential_cleanup<A>(
        &mut self,
        access: &mut A,
        mutation: Option<PairingMutation>,
    ) -> CredentialPairingDriveOutcome
    where
        A: BoundCredentialStoreAccess,
    {
        match self.cleanup_once(access) {
            Ok(true) => CredentialPairingDriveOutcome::CleanupCompleted,
            Ok(false) => CredentialPairingDriveOutcome::Idle,
            Err(reason) => CredentialPairingDriveOutcome::Retry { mutation, reason },
        }
    }

    fn cleanup_once<A>(&mut self, access: &mut A) -> Result<bool, PairingDriveRetry>
    where
        A: BoundCredentialStoreAccess,
    {
        let Some(store) = self.mounted.take() else {
            return Err(PairingDriveRetry::Semantic);
        };
        if store.recovery() == CredentialStoreRecovery::Clean {
            self.mounted = Some(store);
            return Ok(false);
        }
        match recover_once(store, access) {
            Ok(store) => {
                self.mounted = Some(store);
                Ok(true)
            }
            Err(error) => {
                let reason = public_store_retry(error.error());
                self.mounted = Some(error.into_store());
                Err(reason)
            }
        }
    }

    fn drive_awaiting_clean_store<A>(
        &mut self,
        access: &mut A,
        completion: MutationCompletion,
    ) -> CredentialPairingDriveOutcome
    where
        A: BoundCredentialStoreAccess,
    {
        let mutation = completion.mutation();
        if self
            .mounted
            .as_ref()
            .is_some_and(|store| store.recovery() != CredentialStoreRecovery::Clean)
        {
            let outcome = self.drive_credential_cleanup(access, Some(mutation));
            self.live_pairing = LivePairingOwnership::AwaitingCleanStore(completion);
            return outcome;
        }
        let candidate = self
            .mounted
            .as_ref()
            .and_then(MountedCredentialStore::publishable_authority)
            .and_then(|authority| match &completion {
                MutationCompletion::Activate { .. } => authority
                    .plan_activate_pending(PendingCredentialRef::new(
                        completion.pending().id(),
                        completion.pending().generation(),
                    ))
                    .ok(),
                MutationCompletion::Abort { .. } => authority
                    .plan_abort_pending(PendingCredentialRef::new(
                        completion.pending().id(),
                        completion.pending().generation(),
                    ))
                    .ok(),
                MutationCompletion::Begin { .. } => None,
            })
            .map(|plan| plan.into_store_candidate());
        let Some(candidate) = candidate else {
            self.finish_uncommitted(completion);
            self.live_pairing = LivePairingOwnership::Blocked;
            return CredentialPairingDriveOutcome::Blocked(Some(mutation));
        };
        self.live_pairing = LivePairingOwnership::Prepared {
            completion,
            candidate,
        };
        CredentialPairingDriveOutcome::MutationPrepared(mutation)
    }

    fn drive_prepared_mutation<A>(
        &mut self,
        access: &mut A,
        completion: MutationCompletion,
        candidate: PairingLifecycleStoreCandidate<E290_CREDENTIAL_RECORD_CAPACITY>,
    ) -> CredentialPairingDriveOutcome
    where
        A: BoundCredentialStoreAccess,
    {
        let mutation = completion.mutation();
        let Some(current) = self.mounted.take() else {
            self.live_pairing = LivePairingOwnership::Blocked;
            return CredentialPairingDriveOutcome::Blocked(Some(mutation));
        };
        match commit_pairing_lifecycle_successor(current, access, candidate) {
            Ok(committed) => self.finish_committed(completion, committed.into_store()),
            Err(CommitPairingLifecycleSuccessorError::Semantic(error)) => {
                let (current, candidate) = error.into_parts();
                self.mounted = Some(current);
                drop(candidate);
                self.finish_uncommitted(completion);
                self.live_pairing = LivePairingOwnership::Blocked;
                CredentialPairingDriveOutcome::Blocked(Some(mutation))
            }
            Err(CommitPairingLifecycleSuccessorError::Physical(error)) => {
                let pending = error.into_pending();
                self.live_pairing = LivePairingOwnership::Reconciling {
                    completion,
                    pending,
                };
                CredentialPairingDriveOutcome::ReconcileRequired(mutation)
            }
        }
    }

    fn drive_reconciliation<A>(
        &mut self,
        access: &mut A,
        completion: MutationCompletion,
        pending: PendingPairingLifecycleSuccessor,
    ) -> CredentialPairingDriveOutcome
    where
        A: BoundCredentialStoreAccess,
    {
        let mutation = completion.mutation();
        match reconcile_pairing_lifecycle_successor(pending, access) {
            Ok(committed) => self.finish_committed(completion, committed.into_store()),
            Err(error) => {
                let reason = public_store_retry(error.error());
                let pending = error.into_pending();
                self.live_pairing = LivePairingOwnership::Reconciling {
                    completion,
                    pending,
                };
                CredentialPairingDriveOutcome::Retry {
                    mutation: Some(mutation),
                    reason,
                }
            }
        }
    }

    fn finish_committed(
        &mut self,
        completion: MutationCompletion,
        store: MountedCredentialStore,
    ) -> CredentialPairingDriveOutcome {
        let mutation = completion.mutation();
        match completion {
            MutationCompletion::Begin {
                permit,
                bearer,
                device_id,
                pending,
            } => {
                let selected = store.select_pending_for_proof(PendingCredentialRef::new(
                    pending.id(),
                    pending.generation(),
                ));
                self.mounted = Some(store);
                let policy_result = self
                    .pairing
                    .as_mut()
                    .map(|policy| {
                        policy.finish_begin(permit, BeginOutcome::PendingCommitted(pending))
                    })
                    .unwrap_or(Err(PermitError::NoOperation));
                if policy_result.is_err() {
                    self.live_pairing = LivePairingOwnership::Blocked;
                    return CredentialPairingDriveOutcome::Blocked(Some(mutation));
                }
                self.live_pairing = LivePairingOwnership::Idle;
                let Ok(selected) = selected else {
                    self.live_pairing = LivePairingOwnership::Blocked;
                    return CredentialPairingDriveOutcome::Blocked(Some(mutation));
                };
                let (selected_pending, psk) = selected.into_parts();
                if selected_pending.id() != pending.id()
                    || selected_pending.generation() != pending.generation()
                {
                    self.live_pairing = LivePairingOwnership::Blocked;
                    return CredentialPairingDriveOutcome::Blocked(Some(mutation));
                }
                let Ok(psk) = PairingPsk::from_zeroizing(psk) else {
                    self.live_pairing = LivePairingOwnership::Blocked;
                    return CredentialPairingDriveOutcome::Blocked(Some(mutation));
                };
                match reticulum_device_api_pairing::BeginOffer::after_pending_commit(
                    bearer,
                    device_id,
                    pending.id(),
                    pending.generation(),
                    psk,
                ) {
                    Ok(offer) => CredentialPairingDriveOutcome::BeginOffered(offer),
                    Err(_) => {
                        self.live_pairing = LivePairingOwnership::Blocked;
                        CredentialPairingDriveOutcome::Blocked(Some(mutation))
                    }
                }
            }
            MutationCompletion::Activate {
                permit,
                verified,
                psk,
            } => {
                let pending = permit.pending();
                let active = store
                    .publishable_authority()
                    .and_then(|authority| authority.select_for_handshake(pending.id()).ok());
                self.mounted = Some(store);
                let result = self
                    .pairing
                    .as_mut()
                    .map(|policy| {
                        policy.finish_activation(permit, ActivationOutcome::ActiveCommitted)
                    })
                    .unwrap_or(Err(PermitError::NoOperation));
                if result.is_err() {
                    self.live_pairing = LivePairingOwnership::Blocked;
                    return CredentialPairingDriveOutcome::Blocked(Some(mutation));
                }
                self.live_pairing = LivePairingOwnership::Idle;
                let Some(active) = active else {
                    self.live_pairing = LivePairingOwnership::Blocked;
                    return CredentialPairingDriveOutcome::Blocked(Some(mutation));
                };
                let (active_id, active_generation, active_psk) = active.into_parts();
                if active_id != pending.id() {
                    self.live_pairing = LivePairingOwnership::Blocked;
                    return CredentialPairingDriveOutcome::Blocked(Some(mutation));
                }
                let Ok(active_psk) = PairingPsk::from_zeroizing(active_psk) else {
                    self.live_pairing = LivePairingOwnership::Blocked;
                    return CredentialPairingDriveOutcome::Blocked(Some(mutation));
                };
                drop(psk);
                match verified.into_activation_confirmation(active_generation, &active_psk) {
                    Ok(confirmation) => CredentialPairingDriveOutcome::Activated(confirmation),
                    Err(_) => {
                        self.live_pairing = LivePairingOwnership::Blocked;
                        CredentialPairingDriveOutcome::Blocked(Some(mutation))
                    }
                }
            }
            MutationCompletion::Abort { permit } => {
                self.mounted = Some(store);
                let result = self
                    .pairing
                    .as_mut()
                    .map(|policy| policy.finish_abort(permit, AbortOutcome::TombstoneCommitted))
                    .unwrap_or(Err(PermitError::NoOperation));
                if result.is_err() {
                    self.live_pairing = LivePairingOwnership::Blocked;
                    return CredentialPairingDriveOutcome::Blocked(Some(mutation));
                }
                self.live_pairing = LivePairingOwnership::Idle;
                CredentialPairingDriveOutcome::Aborted
            }
        }
    }

    fn finish_uncommitted(&mut self, completion: MutationCompletion) {
        let result = match completion {
            MutationCompletion::Begin { permit, .. } => self
                .pairing
                .as_mut()
                .map(|policy| policy.finish_begin(permit, BeginOutcome::NotCommitted)),
            MutationCompletion::Activate { permit, .. } => self.pairing.as_mut().map(|policy| {
                policy
                    .finish_activation(permit, ActivationOutcome::NotCommitted)
                    .map(|_| ())
            }),
            MutationCompletion::Abort { permit } => self
                .pairing
                .as_mut()
                .map(|policy| policy.finish_abort(permit, AbortOutcome::NotCommitted)),
        };
        self.live_pairing = if matches!(result, Some(Ok(()))) {
            LivePairingOwnership::Idle
        } else {
            LivePairingOwnership::Blocked
        };
    }

    fn cancel_challenge_for_connection(&mut self, connection: ConnectionId) {
        let should_cancel = matches!(
            &self.live_pairing,
            LivePairingOwnership::Proof(proof) if proof.permit.connection() == connection
        );
        if should_cancel {
            self.cancel_challenge_only();
        }
    }

    fn cancel_challenge_only(&mut self) {
        let ownership = mem::replace(&mut self.live_pairing, LivePairingOwnership::Blocked);
        let LivePairingOwnership::Proof(proof) = ownership else {
            self.live_pairing = ownership;
            return;
        };
        self.live_pairing = match self
            .pairing
            .as_mut()
            .and_then(|policy| policy.proof_rejected(proof.permit).ok())
        {
            Some(()) => LivePairingOwnership::Idle,
            None => LivePairingOwnership::Blocked,
        };
    }

    /// Admit explicit initialization while retaining the single-use permit.
    ///
    /// `identity_ready` is a trusted admission-time assertion. The caller must
    /// supply a fresh identity preflight again to [`Self::drive_initialization`].
    pub fn request_initialization(
        &mut self,
        now: MonotonicMillis,
        connection: ConnectionId,
        identity_ready: bool,
    ) -> Result<InitializationAccepted, InitializationRequestRefusal> {
        if matches!(
            self.boot_state,
            CredentialBootState::AuthenticationOnly { .. }
        ) {
            return Err(InitializationRequestRefusal::PairingUnavailable);
        }
        let media = match &self.initialization {
            InitializationOwnership::Eligible(media) => Some(*media),
            _ => None,
        };
        let policy = self
            .pairing
            .as_mut()
            .ok_or(InitializationRequestRefusal::PairingUnavailable)?;
        let permit = policy
            .initialize(
                now,
                connection,
                InitializationFacts::new(identity_ready, media),
            )
            .map_err(InitializationRequestRefusal::Policy)?;
        let admitted = permit.media();
        self.initialization = InitializationOwnership::InFlight {
            permit,
            physical_io_attempted: false,
        };
        Ok(InitializationAccepted { media: admitted })
    }

    /// Reclassify and synchronously advance one admitted initialization.
    ///
    /// The admitted capability is retained across identity-not-ready, backend,
    /// and exact-readback ambiguity. A binding mismatch, backward trajectory,
    /// noncanonical media, or noncanonical completed authority blocks this boot
    /// without releasing the capability. The mounted revision-1 owner is
    /// installed before policy completion is consumed.
    pub fn drive_initialization<A>(
        &mut self,
        access: &mut A,
        identity_ready: bool,
    ) -> InitializationDriveOutcome<A::Error>
    where
        A: BoundCredentialStoreAccess,
    {
        let current = mem::replace(
            &mut self.initialization,
            InitializationOwnership::Unavailable,
        );
        let (permit, physical_io_attempted) = match current {
            InitializationOwnership::InFlight {
                permit,
                physical_io_attempted,
            } => (permit, physical_io_attempted),
            InitializationOwnership::Blocked { permit, reason } => {
                self.initialization = InitializationOwnership::Blocked { permit, reason };
                return InitializationDriveOutcome::Blocked(reason);
            }
            other => {
                self.initialization = other;
                return InitializationDriveOutcome::NotInFlight(self.initialization_status());
            }
        };

        if !identity_ready {
            self.initialization = InitializationOwnership::InFlight {
                permit,
                physical_io_attempted,
            };
            return InitializationDriveOutcome::Retry(InitializationRetry::IdentityNotReady);
        }

        let actual = access.credential_store_binding();
        if actual != self.binding {
            return self.block(
                permit,
                InitializationBlockReason::AccessBindingMismatch {
                    expected: self.binding,
                    actual,
                },
            );
        }

        let observed = match classify_empty_provision_media(access) {
            Ok(observed) => observed,
            Err(CredentialStoreMountError::Backend(error)) => {
                self.initialization = InitializationOwnership::InFlight {
                    permit,
                    physical_io_attempted,
                };
                return InitializationDriveOutcome::Retry(InitializationRetry::Backend(error));
            }
            Err(CredentialStoreMountError::Binding(error)) => {
                return self.block(permit, InitializationBlockReason::StoreBinding(error));
            }
            Err(CredentialStoreMountError::Fault(fault)) => {
                return self.block(permit, InitializationBlockReason::MediaFault(fault));
            }
        };
        let admitted = permit.media();
        if !trajectory_is_forward(admitted, physical_io_attempted, observed) {
            return self.block(
                permit,
                InitializationBlockReason::MediaTrajectory {
                    admitted,
                    observed,
                    physical_io_attempted,
                },
            );
        }

        let result = if observed == EmptyProvisionMediaClassification::CommittedEmptyRevision1 {
            mount(access)
        } else {
            recover_empty_provision(access)
        };
        let mounted = match result {
            Ok(mounted) => mounted,
            Err(CredentialStoreMountError::Backend(error)) => {
                self.initialization = InitializationOwnership::InFlight {
                    permit,
                    physical_io_attempted: true,
                };
                return InitializationDriveOutcome::Retry(InitializationRetry::Backend(error));
            }
            Err(CredentialStoreMountError::Binding(error)) => {
                return self.block(permit, InitializationBlockReason::StoreBinding(error));
            }
            Err(CredentialStoreMountError::Fault(
                fault @ CredentialStoreFault::ReadbackMismatch { .. },
            )) => {
                self.initialization = InitializationOwnership::InFlight {
                    permit,
                    physical_io_attempted: true,
                };
                return InitializationDriveOutcome::Retry(InitializationRetry::Readback(fault));
            }
            Err(CredentialStoreMountError::Fault(fault)) => {
                return self.block(permit, InitializationBlockReason::MediaFault(fault));
            }
        };

        if !is_canonical_empty_revision_one(&mounted, self.binding) {
            self.mounted = Some(mounted);
            return self.block(
                permit,
                InitializationBlockReason::NonCanonicalMountedAuthority,
            );
        }

        self.mounted = Some(mounted);
        self.boot_state = CredentialBootState::Ready;
        let finish = self
            .pairing
            .as_mut()
            .expect("an admitted permit has one resident policy owner")
            .finish_initialization(permit);
        match finish {
            Ok(()) => {
                self.initialization = InitializationOwnership::Completed;
                InitializationDriveOutcome::Completed
            }
            Err(error) => {
                self.initialization = InitializationOwnership::PolicyFault(error);
                InitializationDriveOutcome::NotInFlight(self.initialization_status())
            }
        }
    }

    fn block<E>(
        &mut self,
        permit: InitializationPermit,
        reason: InitializationBlockReason,
    ) -> InitializationDriveOutcome<E> {
        self.initialization = InitializationOwnership::Blocked { permit, reason };
        InitializationDriveOutcome::Blocked(reason)
    }
}

fn prepare_begin_candidate<R>(
    authority: &CredentialAuthority<E290_CREDENTIAL_RECORD_CAPACITY>,
    rng: &mut R,
) -> Option<(
    PairingLifecycleStoreCandidate<E290_CREDENTIAL_RECORD_CAPACITY>,
    PendingRef,
)>
where
    R: RngCore + CryptoRng,
{
    for _ in 0..MAX_PAIRING_ENTROPY_ATTEMPTS {
        let mut credential_bytes = [0_u8; 16];
        let mut principal_bytes = [0_u8; 16];
        let mut psk = Zeroizing::new([0_u8; 32]);
        rng.fill_bytes(&mut credential_bytes);
        rng.fill_bytes(&mut principal_bytes);
        rng.fill_bytes(&mut *psk);
        let credential_id = CredentialId::new(credential_bytes);
        let permissions = Permissions::from_bits(
            Permissions::READ_SUBMISSION_STATUS.bits()
                | Permissions::EXPERIMENTAL_SUBMIT_RNS_DATA.bits()
                | Permissions::MANAGE_NETWORK_CONFIG.bits(),
        )
        .expect("the fixed developer policy contains only stable permission bits");
        let enrollment = NewPendingCredential::new(
            credential_id,
            PrincipalId(principal_bytes),
            permissions,
            PairingOrigin::UsbPhysicalPresence,
            AuthorizationPolicyVersion::new(1),
            psk,
        );
        match authority.plan_add_pending(enrollment) {
            Ok(plan) => {
                let pending = PendingRef::new(
                    credential_id,
                    reticulum_device_api_credentials::CredentialGeneration::new(
                        plan.candidate_revision().get(),
                    ),
                )
                .ok()?;
                return Some((plan.into_store_candidate(), pending));
            }
            Err(fault) => {
                let retryable = matches!(
                    fault.kind(),
                    CredentialLifecycleFaultKind::ZeroCredentialId
                        | CredentialLifecycleFaultKind::CredentialIdAlreadyRetained
                        | CredentialLifecycleFaultKind::ZeroPrincipal
                        | CredentialLifecycleFaultKind::ZeroPsk
                        | CredentialLifecycleFaultKind::DuplicatePsk
                );
                drop(fault.into_enrollment());
                if !retryable {
                    return None;
                }
            }
        }
    }
    None
}

fn generate_challenge<R>(rng: &mut R) -> Option<DeviceChallenge>
where
    R: RngCore + CryptoRng,
{
    for _ in 0..MAX_PAIRING_ENTROPY_ATTEMPTS {
        let mut bytes = Zeroizing::new([0_u8; 32]);
        rng.fill_bytes(&mut *bytes);
        if let Ok(challenge) = DeviceChallenge::from_zeroizing(bytes) {
            return Some(challenge);
        }
    }
    None
}

const fn public_attempt_refusal(refusal: AttemptRefusal) -> PairingFailure {
    match refusal {
        AttemptRefusal::NotConnected
        | AttemptRefusal::WrongConnection
        | AttemptRefusal::WindowNotOpen
        | AttemptRefusal::TimedOut => PairingFailure::PhysicalPresenceRequired,
        AttemptRefusal::PendingExists
        | AttemptRefusal::PendingMissing
        | AttemptRefusal::PendingMismatch
        | AttemptRefusal::CapacityExhausted => PairingFailure::Refused,
        AttemptRefusal::OperationInFlight
        | AttemptRefusal::MutationBlocked
        | AttemptRefusal::RevisionExhausted
        | AttemptRefusal::ClockRegression
        | AttemptRefusal::OperationIdExhausted => PairingFailure::Blocked,
    }
}

const fn public_request_refusal(refusal: RequestRefusal) -> PairingFailure {
    match refusal {
        RequestRefusal::NotConnected
        | RequestRefusal::WrongConnection
        | RequestRefusal::WindowNotOpen
        | RequestRefusal::TimedOut => PairingFailure::PhysicalPresenceRequired,
        RequestRefusal::PendingExists
        | RequestRefusal::PendingMissing
        | RequestRefusal::PendingMismatch
        | RequestRefusal::InitializationNotEligible => PairingFailure::Refused,
        RequestRefusal::OperationInFlight
        | RequestRefusal::ClockRegression
        | RequestRefusal::OperationIdExhausted => PairingFailure::Blocked,
    }
}

fn public_store_retry<E>(error: &CredentialStoreMountError<E>) -> PairingDriveRetry {
    match error {
        CredentialStoreMountError::Backend(_) => PairingDriveRetry::Backend,
        CredentialStoreMountError::Binding(error) => PairingDriveRetry::Binding(*error),
        CredentialStoreMountError::Fault(fault) => PairingDriveRetry::Media(*fault),
    }
}

fn pairing_policy_for_boot(
    state: CredentialBootState,
    mounted: Option<&MountedCredentialStore>,
) -> Option<PairingPolicy> {
    let pending = match state {
        CredentialBootState::UninitializedErased
        | CredentialBootState::InitializationInterrupted => PendingState::None,
        CredentialBootState::Ready | CredentialBootState::AuthenticationOnly { .. } => {
            let authority = mounted?.publishable_authority()?;
            match authority.pending_credential().ok()? {
                None => PendingState::None,
                Some(pending) => {
                    PendingState::One(PendingRef::new(pending.id(), pending.generation()).ok()?)
                }
            }
        }
        CredentialBootState::Blocked { .. }
        | CredentialBootState::Corrupt { .. }
        | CredentialBootState::Backend { .. } => return None,
    };
    Some(PairingPolicy::new(pending))
}

const fn trajectory_is_forward(
    admitted: InitializableMedia,
    physical_io_attempted: bool,
    observed: EmptyProvisionMediaClassification,
) -> bool {
    matches!(
        (admitted, physical_io_attempted, observed),
        (
            InitializableMedia::ExactlyErased,
            false,
            EmptyProvisionMediaClassification::ExactlyErased,
        ) | (
            InitializableMedia::ExactlyErased,
            true,
            EmptyProvisionMediaClassification::ExactlyErased
                | EmptyProvisionMediaClassification::RecoverableInterrupted
                | EmptyProvisionMediaClassification::CommittedEmptyRevision1,
        ) | (
            InitializableMedia::RecoverableInterrupted,
            false,
            EmptyProvisionMediaClassification::RecoverableInterrupted,
        ) | (
            InitializableMedia::RecoverableInterrupted,
            true,
            EmptyProvisionMediaClassification::RecoverableInterrupted
                | EmptyProvisionMediaClassification::CommittedEmptyRevision1,
        )
    )
}

fn is_canonical_empty_revision_one(
    mounted: &MountedCredentialStore,
    binding: CredentialStoreBinding,
) -> bool {
    mounted.binding() == binding
        && mounted.revision().get() == 1
        && mounted.recovery() == CredentialStoreRecovery::Clean
        && mounted
            .publishable_authority()
            .is_some_and(|authority| authority.record_count() == 0)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::mem::size_of;

    use embedded_storage::nor_flash::{
        ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
        check_erase, check_read, check_write,
    };
    use reticulum_device_api_credential_store::{
        BoundCredentialStore, CredentialStoreDeviceId, PARTITION_SIZE, PHYSICAL_FORMAT_VERSION,
        commit_successor, mount, recover_empty_provision,
    };
    use reticulum_device_api_credentials::{
        AuthorityRevision, CredentialAudit, CredentialAuthorityBuilder, CredentialGeneration,
        CredentialRecord, CredentialStatus,
    };
    use reticulum_device_api_pairing::{
        ActivateRequest, BeginOffer, ClientProof, PairingTranscript, ProofStartRequest,
    };
    use reticulum_device_api_pairing_policy::{
        BUTTON_HOLD_MILLIS, CloseReason, PAIRING_WINDOW_MILLIS, RequestRefusal,
    };
    use std::vec;
    use std::vec::Vec;

    use super::*;
    use crate::credential_boot::{CredentialBootState, boot_credentials};

    const ABSOLUTE_OFFSET: usize = 0x52_0000;
    const SECTOR_SIZE: usize = PARTITION_SIZE / 2;
    const CONNECTION_VALUE: u64 = 1;
    const BUTTON_PRESS_MILLIS: u64 = 10;
    const WINDOW_OPEN_MILLIS: u64 = BUTTON_PRESS_MILLIS + BUTTON_HOLD_MILLIS;
    const REQUEST_MILLIS: u64 = WINDOW_OPEN_MILLIS + 1;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum FakeError {
        Injected,
        Bounds,
        Alignment,
        IllegalProgramming,
    }

    impl NorFlashError for FakeError {
        fn kind(&self) -> NorFlashErrorKind {
            match self {
                Self::Bounds => NorFlashErrorKind::OutOfBounds,
                Self::Alignment => NorFlashErrorKind::NotAligned,
                Self::Injected | Self::IllegalProgramming => NorFlashErrorKind::Other,
            }
        }
    }

    #[derive(Clone, Copy)]
    enum WriteFault {
        Partial(usize),
        LostReply,
    }

    struct FakeNor {
        bytes: Vec<u8>,
        reads: usize,
        writes: usize,
        erases: usize,
        fail_next_read: bool,
        fail_next_write: Option<WriteFault>,
        fail_next_erase: bool,
    }

    impl FakeNor {
        fn erased() -> Self {
            Self {
                bytes: vec![0xff; PARTITION_SIZE],
                reads: 0,
                writes: 0,
                erases: 0,
                fail_next_read: false,
                fail_next_write: None,
                fail_next_erase: false,
            }
        }

        fn reset_io(&mut self) {
            self.reads = 0;
            self.writes = 0;
            self.erases = 0;
            self.fail_next_read = false;
            self.fail_next_write = None;
            self.fail_next_erase = false;
        }

        fn range(&self, offset: u32, len: usize) -> Result<core::ops::Range<usize>, FakeError> {
            let start = usize::try_from(offset).map_err(|_| FakeError::Bounds)?;
            let end = start.checked_add(len).ok_or(FakeError::Bounds)?;
            if end > self.bytes.len() {
                return Err(FakeError::Bounds);
            }
            Ok(start..end)
        }

        fn program(&mut self, offset: usize, bytes: &[u8]) -> Result<(), FakeError> {
            let target = &mut self.bytes[offset..offset + bytes.len()];
            if target
                .iter()
                .zip(bytes)
                .any(|(current, next)| current & next != *next)
            {
                return Err(FakeError::IllegalProgramming);
            }
            for (current, next) in target.iter_mut().zip(bytes) {
                *current &= *next;
            }
            Ok(())
        }
    }

    impl ErrorType for FakeNor {
        type Error = FakeError;
    }

    impl ReadNorFlash for FakeNor {
        const READ_SIZE: usize = 1;

        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
            check_read(self, offset, bytes.len()).map_err(map_check)?;
            let range = self.range(offset, bytes.len())?;
            self.reads += 1;
            if core::mem::take(&mut self.fail_next_read) {
                return Err(FakeError::Injected);
            }
            bytes.copy_from_slice(&self.bytes[range]);
            Ok(())
        }

        fn capacity(&self) -> usize {
            self.bytes.len()
        }
    }

    impl NorFlash for FakeNor {
        const WRITE_SIZE: usize = 1;
        const ERASE_SIZE: usize = SECTOR_SIZE;

        fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
            check_write(self, offset, bytes.len()).map_err(map_check)?;
            let range = self.range(offset, bytes.len())?;
            self.writes += 1;
            match self.fail_next_write.take() {
                Some(WriteFault::Partial(cut)) => {
                    let cut = cut.min(bytes.len());
                    self.program(range.start, &bytes[..cut])?;
                    Err(FakeError::Injected)
                }
                Some(WriteFault::LostReply) => {
                    self.program(range.start, bytes)?;
                    Err(FakeError::Injected)
                }
                None => self.program(range.start, bytes),
            }
        }

        fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            check_erase(self, from, to).map_err(map_check)?;
            let len = usize::try_from(to - from).map_err(|_| FakeError::Bounds)?;
            let range = self.range(from, len)?;
            self.erases += 1;
            if core::mem::take(&mut self.fail_next_erase) {
                return Err(FakeError::Injected);
            }
            self.bytes[range].fill(0xff);
            Ok(())
        }
    }

    impl MultiwriteNorFlash for FakeNor {}

    fn map_check(kind: NorFlashErrorKind) -> FakeError {
        match kind {
            NorFlashErrorKind::OutOfBounds => FakeError::Bounds,
            NorFlashErrorKind::NotAligned => FakeError::Alignment,
            _ => FakeError::Injected,
        }
    }

    const fn binding(device_byte: u8) -> CredentialStoreBinding {
        CredentialStoreBinding::new(
            CredentialStoreDeviceId::new([device_byte; 16]),
            ABSOLUTE_OFFSET,
            PARTITION_SIZE,
            PHYSICAL_FORMAT_VERSION,
        )
    }

    fn bound(
        flash: FakeNor,
        store_binding: CredentialStoreBinding,
    ) -> BoundCredentialStore<FakeNor> {
        BoundCredentialStore::new(flash, store_binding)
    }

    fn time(value: u64) -> MonotonicMillis {
        MonotonicMillis::new(value)
    }

    fn connection() -> ConnectionId {
        ConnectionId::new(CONNECTION_VALUE).expect("test connection is nonzero")
    }

    fn open_pairing_window(runtime: &mut CredentialRuntime) {
        assert_eq!(
            runtime.pairing_connected(time(0), connection()),
            Some(Ok(None))
        );
        assert!(matches!(
            runtime.pairing_observe_button(time(0), ActiveLowButton::High),
            Some(ButtonEffect::None)
        ));
        assert!(matches!(
            runtime.pairing_observe_button(time(BUTTON_PRESS_MILLIS), ActiveLowButton::Low),
            Some(ButtonEffect::None)
        ));
        let acquire =
            match runtime.pairing_observe_button(time(WINDOW_OPEN_MILLIS), ActiveLowButton::Low) {
                Some(ButtonEffect::AcquirePairingExclusive(acquire)) => acquire,
                _ => panic!("continuous hold did not request pairing exclusivity"),
            };
        assert!(matches!(
            runtime.pairing_exclusive_acquired(time(WINDOW_OPEN_MILLIS), acquire),
            Some(ExclusiveAcquireOutcome::Opened(opened))
                if opened.connection() == connection()
        ));
    }

    fn admit_initialization(runtime: &mut CredentialRuntime, expected: InitializableMedia) {
        open_pairing_window(runtime);
        let accepted = runtime
            .request_initialization(time(REQUEST_MILLIS), connection(), true)
            .unwrap_or_else(|_| panic!("eligible initialization was refused"));
        assert_eq!(accepted.media(), expected);
        assert_eq!(
            runtime.initialization_status(),
            CredentialInitializationStatus::InFlight {
                media: expected,
                physical_io_attempted: false,
            }
        );
    }

    fn erased_runtime() -> (CredentialRuntime, BoundCredentialStore<FakeNor>) {
        let store_binding = binding(0x51);
        let mut access = bound(FakeNor::erased(), store_binding);
        let boot = boot_credentials(&mut access);
        assert_eq!(boot.state(), CredentialBootState::UninitializedErased);
        access.backend_mut().reset_io();
        (
            CredentialRuntime::from_boot(boot, store_binding, device_id(0x51)),
            access,
        )
    }

    fn interrupted_runtime() -> (CredentialRuntime, BoundCredentialStore<FakeNor>) {
        let store_binding = binding(0x52);
        let mut seed = bound(FakeNor::erased(), store_binding);
        seed.backend_mut().fail_next_write = Some(WriteFault::Partial(37));
        assert!(matches!(
            recover_empty_provision(&mut seed),
            Err(CredentialStoreMountError::Backend(FakeError::Injected))
        ));
        let mut access = bound(seed.into_backend(), store_binding);
        access.backend_mut().reset_io();
        let boot = boot_credentials(&mut access);
        assert_eq!(boot.state(), CredentialBootState::InitializationInterrupted);
        access.backend_mut().reset_io();
        (
            CredentialRuntime::from_boot(boot, store_binding, device_id(0x52)),
            access,
        )
    }

    fn committed_bytes(store_binding: CredentialStoreBinding) -> Vec<u8> {
        let mut access = bound(FakeNor::erased(), store_binding);
        let mounted = recover_empty_provision(&mut access)
            .unwrap_or_else(|_| panic!("test revision-1 provisioning failed"));
        assert_eq!(mounted.revision().get(), 1);
        access.into_backend().bytes
    }

    struct TestRng {
        next: u8,
        zero: bool,
        fills: usize,
    }

    impl TestRng {
        const fn patterned(seed: u8) -> Self {
            Self {
                next: seed,
                zero: false,
                fills: 0,
            }
        }

        const fn zero() -> Self {
            Self {
                next: 0,
                zero: true,
                fills: 0,
            }
        }
    }

    impl RngCore for TestRng {
        fn next_u32(&mut self) -> u32 {
            let mut bytes = [0_u8; 4];
            self.fill_bytes(&mut bytes);
            u32::from_le_bytes(bytes)
        }

        fn next_u64(&mut self) -> u64 {
            let mut bytes = [0_u8; 8];
            self.fill_bytes(&mut bytes);
            u64::from_le_bytes(bytes)
        }

        fn fill_bytes(&mut self, dest: &mut [u8]) {
            self.fills += 1;
            if self.zero {
                dest.fill(0);
                return;
            }
            for byte in dest {
                if self.next == 0 {
                    self.next = 1;
                }
                *byte = self.next;
                self.next = self.next.wrapping_add(1);
            }
        }

        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
            self.fill_bytes(dest);
            Ok(())
        }
    }

    impl CryptoRng for TestRng {}

    fn device_id(seed: u8) -> DeviceId {
        DeviceId::new([seed; 16]).expect("test device ID is nonzero")
    }

    fn ready_pairing_runtime() -> (CredentialRuntime, BoundCredentialStore<FakeNor>) {
        let (mut runtime, mut access) = erased_runtime();
        admit_initialization(&mut runtime, InitializableMedia::ExactlyErased);
        assert!(runtime.credential_physical_mutation_outstanding());
        assert!(matches!(
            runtime.drive_initialization(&mut access, true),
            InitializationDriveOutcome::Completed
        ));
        assert!(!runtime.credential_physical_mutation_outstanding());
        (runtime, access)
    }

    fn authentication_only_runtime() -> (
        CredentialRuntime,
        BoundCredentialStore<FakeNor>,
        CredentialId,
        reticulum_device_api_credentials::CredentialGeneration,
        Zeroizing<[u8; 32]>,
    ) {
        let (mut runtime, mut access) = ready_pairing_runtime();
        let mut rng = TestRng::patterned(0x17);
        let offer = commit_begin(&mut runtime, &mut access, &mut rng);
        let credential_id = offer.credential_id();
        let _ = admit_valid_activation(&mut runtime, &mut rng, &offer);
        assert!(matches!(
            runtime.drive_live_pairing(&mut access),
            CredentialPairingDriveOutcome::CleanupCompleted
        ));
        assert!(matches!(
            runtime.drive_live_pairing(&mut access),
            CredentialPairingDriveOutcome::MutationPrepared(PairingMutation::ActivatePending)
        ));
        assert!(matches!(
            runtime.drive_live_pairing(&mut access),
            CredentialPairingDriveOutcome::Activated(_)
        ));
        let active = runtime
            .mounted
            .as_ref()
            .and_then(MountedCredentialStore::publishable_authority)
            .and_then(|authority| authority.select_for_handshake(credential_id).ok())
            .expect("activated test credential is publishable");
        let (selected_id, generation, expected_psk) = active.into_parts();
        assert_eq!(selected_id, credential_id);

        let store_binding = runtime.binding();
        let device_id = runtime.device_id();
        let mut flash = access.into_backend();
        flash.fail_next_erase = true;
        let mut access = bound(flash, store_binding);
        let boot = boot_credentials(&mut access);
        assert_eq!(
            boot.state(),
            CredentialBootState::AuthenticationOnly {
                cleanup_failure: crate::credential_boot::CredentialBootFailure::Backend,
            }
        );
        access.backend_mut().reset_io();
        (
            CredentialRuntime::from_boot(boot, store_binding, device_id),
            access,
            credential_id,
            generation,
            expected_psk,
        )
    }

    fn commit_begin(
        runtime: &mut CredentialRuntime,
        access: &mut BoundCredentialStore<FakeNor>,
        rng: &mut TestRng,
    ) -> BeginOffer {
        assert_eq!(
            runtime.request_pairing_begin(
                reticulum_device_api_pairing::BearerBinding::UsbSerialJtag,
                time(REQUEST_MILLIS + 1),
                connection(),
                rng,
            ),
            BeginAdmission::Accepted
        );
        assert_eq!(
            runtime.live_pairing_status(),
            CredentialPairingStatus::MutationPrepared(PairingMutation::AddPending)
        );
        match runtime.drive_live_pairing(access) {
            CredentialPairingDriveOutcome::BeginOffered(offer) => offer,
            _ => panic!("prepared Begin did not commit"),
        }
    }

    fn start_proof(
        runtime: &mut CredentialRuntime,
        rng: &mut TestRng,
        offer: &BeginOffer,
    ) -> (ProofStartRequest, ProofChallenge) {
        let request = ProofStartRequest::new(
            reticulum_device_api_pairing::BearerBinding::UsbSerialJtag,
            2,
            offer.credential_id(),
            offer.generation(),
            [0x5a; 32],
        )
        .expect("test ProofStart is valid");
        let challenge = match runtime.request_pairing_proof_start(
            time(REQUEST_MILLIS + 2),
            connection(),
            &request,
            rng,
        ) {
            ProofStartAdmission::Challenge(challenge) => challenge,
            ProofStartAdmission::Refused(_) => panic!("proof was refused"),
        };
        (request, challenge)
    }

    fn admit_valid_activation(
        runtime: &mut CredentialRuntime,
        rng: &mut TestRng,
        offer: &BeginOffer,
    ) -> (PairingTranscript, ClientProof) {
        let (request, challenge) = start_proof(runtime, rng, offer);
        assert_eq!(challenge.device_id(), runtime.device_id());
        assert_eq!(challenge.connection_id().get(), connection().get());
        let transcript = PairingTranscript::new(&request, &challenge)
            .unwrap_or_else(|_| panic!("matching test transcript was rejected"));
        let proof = ClientProof::calculate(offer.psk(), &transcript);
        let verification_proof = ClientProof::calculate(offer.psk(), &transcript);
        let activate = ActivateRequest::new(3, offer.credential_id(), offer.generation(), proof)
            .expect("test Activate is valid");
        assert_eq!(
            runtime.request_pairing_activate(
                reticulum_device_api_pairing::BearerBinding::UsbSerialJtag,
                time(REQUEST_MILLIS + 3),
                connection(),
                activate,
            ),
            ActivateAdmission::Accepted
        );
        (transcript, verification_proof)
    }

    fn prepare_activation_candidate(
        runtime: &mut CredentialRuntime,
        access: &mut BoundCredentialStore<FakeNor>,
        seed: u8,
    ) {
        let mut rng = TestRng::patterned(seed);
        let offer = commit_begin(runtime, access, &mut rng);
        let _ = admit_valid_activation(runtime, &mut rng, &offer);
        assert!(matches!(
            runtime.drive_live_pairing(access),
            CredentialPairingDriveOutcome::CleanupCompleted
        ));
        assert!(matches!(
            runtime.drive_live_pairing(access),
            CredentialPairingDriveOutcome::MutationPrepared(PairingMutation::ActivatePending)
        ));
    }

    fn start_third_attempt_proof(
        runtime: &mut CredentialRuntime,
        access: &mut BoundCredentialStore<FakeNor>,
        seed: u8,
    ) {
        let mut rng = TestRng::patterned(seed);
        let offer = commit_begin(runtime, access, &mut rng);
        assert!(matches!(
            runtime.drive_live_pairing(access),
            CredentialPairingDriveOutcome::CleanupCompleted
        ));
        assert_eq!(
            runtime.request_pairing_begin(
                reticulum_device_api_pairing::BearerBinding::UsbSerialJtag,
                time(REQUEST_MILLIS + 2),
                connection(),
                &mut rng,
            ),
            BeginAdmission::Refused(PairingFailure::Refused)
        );
        let _ = start_proof(runtime, &mut rng, &offer);
        assert_eq!(
            runtime.live_pairing_status(),
            CredentialPairingStatus::ProofOutstanding
        );
        assert!(!runtime.credential_physical_mutation_outstanding());
        assert!(
            runtime
                .pairing
                .as_ref()
                .is_some_and(PairingPolicy::operation_outstanding)
        );
    }

    #[test]
    fn erased_media_requires_presence_then_establishes_only_empty_revision_one() {
        let (mut runtime, mut access) = erased_runtime();
        assert_eq!(
            runtime.credential_boot_state(),
            CredentialBootState::UninitializedErased
        );
        assert_eq!(runtime.revision(), None);
        assert_eq!(runtime.active_credential_count(), None);
        assert!(!runtime.authority_publishable());
        assert!(!runtime.mutation_eligible());
        assert!(runtime.pairing_policy_available());
        assert_eq!(
            runtime.initialization_status(),
            CredentialInitializationStatus::Eligible {
                media: InitializableMedia::ExactlyErased,
            }
        );

        admit_initialization(&mut runtime, InitializableMedia::ExactlyErased);
        assert!(matches!(
            runtime.drive_initialization(&mut access, true),
            InitializationDriveOutcome::Completed
        ));
        assert!(access.backend().reads > 0);
        assert!(access.backend().writes > 0);
        assert_eq!(access.backend().erases, 0);
        assert_eq!(
            runtime.initialization_status(),
            CredentialInitializationStatus::Completed
        );
        assert_eq!(runtime.credential_boot_state(), CredentialBootState::Ready);
        assert_eq!(runtime.revision(), Some(1));
        assert_eq!(runtime.active_credential_count(), Some(0));
        assert!(runtime.authority_publishable());
        assert!(runtime.mutation_eligible());

        let mounted = mount(&mut access).unwrap_or_else(|_| panic!("revision 1 did not remount"));
        let authority = mounted
            .publishable_authority()
            .expect("completed empty authority must be publishable");
        assert_eq!(
            authority.record_count(),
            0,
            "initialization implicitly ran Begin"
        );
    }

    #[test]
    fn identity_not_ready_retries_without_store_io_or_losing_the_permit() {
        let (mut runtime, mut access) = erased_runtime();
        admit_initialization(&mut runtime, InitializableMedia::ExactlyErased);
        access.backend_mut().reset_io();

        assert!(matches!(
            runtime.drive_initialization(&mut access, false),
            InitializationDriveOutcome::Retry(InitializationRetry::IdentityNotReady)
        ));
        assert_eq!(access.backend().reads, 0);
        assert_eq!(access.backend().writes, 0);
        assert_eq!(access.backend().erases, 0);
        assert_eq!(
            runtime.initialization_status(),
            CredentialInitializationStatus::InFlight {
                media: InitializableMedia::ExactlyErased,
                physical_io_attempted: false,
            }
        );
        assert!(matches!(
            runtime.drive_initialization(&mut access, true),
            InitializationDriveOutcome::Completed
        ));
    }

    #[test]
    fn classifier_backend_ambiguity_retains_unattempted_ownership() {
        let (mut runtime, mut access) = erased_runtime();
        admit_initialization(&mut runtime, InitializableMedia::ExactlyErased);
        access.backend_mut().reset_io();
        access.backend_mut().fail_next_read = true;

        assert!(matches!(
            runtime.drive_initialization(&mut access, true),
            InitializationDriveOutcome::Retry(InitializationRetry::Backend(FakeError::Injected))
        ));
        assert_eq!(access.backend().reads, 1);
        assert_eq!(access.backend().writes, 0);
        assert_eq!(
            runtime.initialization_status(),
            CredentialInitializationStatus::InFlight {
                media: InitializableMedia::ExactlyErased,
                physical_io_attempted: false,
            }
        );
        assert!(matches!(
            runtime.drive_initialization(&mut access, true),
            InitializationDriveOutcome::Completed
        ));
    }

    #[test]
    fn partial_write_and_lost_reply_reconcile_under_the_same_permit() {
        for fault in [WriteFault::Partial(37), WriteFault::LostReply] {
            let (mut runtime, mut access) = erased_runtime();
            admit_initialization(&mut runtime, InitializableMedia::ExactlyErased);
            access.backend_mut().reset_io();
            access.backend_mut().fail_next_write = Some(fault);

            assert!(matches!(
                runtime.drive_initialization(&mut access, true),
                InitializationDriveOutcome::Retry(InitializationRetry::Backend(
                    FakeError::Injected
                ))
            ));
            assert_eq!(access.backend().writes, 1);
            assert_eq!(
                runtime.initialization_status(),
                CredentialInitializationStatus::InFlight {
                    media: InitializableMedia::ExactlyErased,
                    physical_io_attempted: true,
                }
            );
            assert!(matches!(
                runtime.drive_initialization(&mut access, true),
                InitializationDriveOutcome::Completed
            ));
            assert_eq!(runtime.revision(), Some(1));
        }
    }

    #[test]
    fn interrupted_boot_trajectory_can_finish_initialization() {
        let (mut runtime, mut access) = interrupted_runtime();
        assert_eq!(
            runtime.credential_boot_state(),
            CredentialBootState::InitializationInterrupted
        );
        assert_eq!(runtime.active_credential_count(), None);
        assert!(runtime.pairing_policy_available());
        assert_eq!(
            runtime.initialization_status(),
            CredentialInitializationStatus::Eligible {
                media: InitializableMedia::RecoverableInterrupted,
            }
        );
        admit_initialization(&mut runtime, InitializableMedia::RecoverableInterrupted);
        assert!(matches!(
            runtime.drive_initialization(&mut access, true),
            InitializationDriveOutcome::Completed
        ));
        assert_eq!(runtime.revision(), Some(1));
        assert_eq!(runtime.active_credential_count(), Some(0));
        assert!(runtime.authority_publishable());
        assert!(runtime.mutation_eligible());
    }

    #[test]
    fn interrupted_boot_cannot_move_backward_to_erased_media() {
        let (mut runtime, mut access) = interrupted_runtime();
        admit_initialization(&mut runtime, InitializableMedia::RecoverableInterrupted);
        access.backend_mut().bytes.fill(0xff);
        access.backend_mut().reset_io();

        let expected = InitializationBlockReason::MediaTrajectory {
            admitted: InitializableMedia::RecoverableInterrupted,
            observed: EmptyProvisionMediaClassification::ExactlyErased,
            physical_io_attempted: false,
        };
        assert!(matches!(
            runtime.drive_initialization(&mut access, true),
            InitializationDriveOutcome::Blocked(reason) if reason == expected
        ));
        assert_eq!(
            runtime.initialization_status(),
            CredentialInitializationStatus::Blocked { reason: expected }
        );
        assert_eq!(access.backend().writes, 0);
        assert_eq!(access.backend().erases, 0);
    }

    #[test]
    fn off_trajectory_and_preexisting_commit_are_first_drive_contradictions() {
        for (observed, bytes) in [
            (EmptyProvisionMediaClassification::NotRecoverable, {
                let mut bytes = vec![0xff; PARTITION_SIZE];
                bytes[SECTOR_SIZE] = 0xfe;
                bytes
            }),
            (
                EmptyProvisionMediaClassification::CommittedEmptyRevision1,
                committed_bytes(binding(0x51)),
            ),
        ] {
            let (mut runtime, mut access) = erased_runtime();
            admit_initialization(&mut runtime, InitializableMedia::ExactlyErased);
            access.backend_mut().bytes = bytes;
            access.backend_mut().reset_io();
            let expected = InitializationBlockReason::MediaTrajectory {
                admitted: InitializableMedia::ExactlyErased,
                observed,
                physical_io_attempted: false,
            };

            assert!(matches!(
                runtime.drive_initialization(&mut access, true),
                InitializationDriveOutcome::Blocked(reason) if reason == expected
            ));
            assert_eq!(access.backend().writes, 0);
            assert_eq!(access.backend().erases, 0);
        }
    }

    #[test]
    fn wrong_operation_binding_blocks_before_any_store_io() {
        let (mut runtime, _) = erased_runtime();
        admit_initialization(&mut runtime, InitializableMedia::ExactlyErased);
        let expected_binding = runtime.binding();
        let actual_binding = binding(0x99);
        let mut wrong_access = bound(FakeNor::erased(), actual_binding);
        let expected = InitializationBlockReason::AccessBindingMismatch {
            expected: expected_binding,
            actual: actual_binding,
        };

        assert!(matches!(
            runtime.drive_initialization(&mut wrong_access, true),
            InitializationDriveOutcome::Blocked(reason) if reason == expected
        ));
        assert_eq!(wrong_access.backend().reads, 0);
        assert_eq!(wrong_access.backend().writes, 0);
        assert_eq!(wrong_access.backend().erases, 0);
        assert!(matches!(
            runtime.drive_initialization(&mut wrong_access, true),
            InitializationDriveOutcome::Blocked(reason) if reason == expected
        ));
        assert_eq!(wrong_access.backend().reads, 0);
    }

    #[test]
    fn disconnect_and_timeout_retain_in_flight_ownership_until_finish() {
        for disconnect in [true, false] {
            let (mut runtime, mut access) = erased_runtime();
            admit_initialization(&mut runtime, InitializableMedia::ExactlyErased);
            let event = if disconnect {
                runtime.pairing_disconnected(time(REQUEST_MILLIS + 1), connection())
            } else {
                runtime.pairing_poll_timeout(time(WINDOW_OPEN_MILLIS + PAIRING_WINDOW_MILLIS))
            };
            assert!(matches!(
                event,
                Some(PolicyEvent::Closed(closed))
                    if closed.reason()
                        == if disconnect { CloseReason::Disconnect } else { CloseReason::Timeout }
            ));
            assert_eq!(
                runtime.initialization_status(),
                CredentialInitializationStatus::InFlight {
                    media: InitializableMedia::ExactlyErased,
                    physical_io_attempted: false,
                }
            );
            assert!(matches!(
                runtime.drive_initialization(&mut access, true),
                InitializationDriveOutcome::Completed
            ));
        }
    }

    #[test]
    fn repeated_admission_is_refused_without_replacing_owned_initialization() {
        let (mut runtime, access) = erased_runtime();
        admit_initialization(&mut runtime, InitializableMedia::ExactlyErased);
        let refusal = runtime
            .request_initialization(time(REQUEST_MILLIS + 1), connection(), true)
            .expect_err("second initialization was unexpectedly admitted");
        assert!(matches!(
            refusal,
            InitializationRequestRefusal::Policy(refused)
                if refused.reason() == RequestRefusal::OperationInFlight
        ));
        assert_eq!(
            runtime.initialization_status(),
            CredentialInitializationStatus::InFlight {
                media: InitializableMedia::ExactlyErased,
                physical_io_attempted: false,
            }
        );
        assert_eq!(access.backend().reads, 0);
        assert_eq!(access.backend().writes, 0);
    }

    #[test]
    fn begin_proof_activate_commits_end_to_end_with_bound_confirmation() {
        let (mut runtime, mut access) = ready_pairing_runtime();
        let mut rng = TestRng::patterned(0x11);
        let offer = commit_begin(&mut runtime, &mut access, &mut rng);
        assert_eq!(offer.device_id(), runtime.device_id());
        assert_eq!(
            runtime.live_pairing_status(),
            CredentialPairingStatus::CleanupRequired
        );
        assert!(runtime.credential_physical_mutation_outstanding());

        let (transcript, verification_proof) =
            admit_valid_activation(&mut runtime, &mut rng, &offer);
        assert_eq!(
            runtime.live_pairing_status(),
            CredentialPairingStatus::AwaitingCleanStore(PairingMutation::ActivatePending)
        );

        assert!(matches!(
            runtime.drive_live_pairing(&mut access),
            CredentialPairingDriveOutcome::CleanupCompleted
        ));
        assert_eq!(
            runtime.live_pairing_status(),
            CredentialPairingStatus::AwaitingCleanStore(PairingMutation::ActivatePending)
        );
        assert!(matches!(
            runtime.drive_live_pairing(&mut access),
            CredentialPairingDriveOutcome::MutationPrepared(PairingMutation::ActivatePending)
        ));
        let confirmation = match runtime.drive_live_pairing(&mut access) {
            CredentialPairingDriveOutcome::Activated(confirmation) => confirmation,
            _ => panic!("prepared activation did not commit"),
        };
        assert!(confirmation.verify(offer.psk(), &transcript, &verification_proof));
        assert_eq!(confirmation.credential_id(), offer.credential_id());

        let authority = runtime
            .mounted
            .as_ref()
            .and_then(MountedCredentialStore::publishable_authority)
            .expect("activated authority is publishable");
        let (active_id, active_generation, _active_psk) = authority
            .select_for_handshake(offer.credential_id())
            .expect("committed Active credential is selectable")
            .into_parts();
        assert_eq!(active_id, offer.credential_id());
        assert!(active_generation.get() > offer.generation().get());
        assert_eq!(confirmation.generation(), active_generation);
        assert_eq!(authority.active_count(), 1);
        assert_eq!(runtime.active_credential_count(), Some(1));
        assert!(
            authority
                .pending_credential()
                .unwrap_or_else(|_| panic!("activated authority has multiple pending records"))
                .is_none()
        );
        assert_eq!(
            runtime.live_pairing_status(),
            CredentialPairingStatus::CleanupRequired
        );
        assert!(runtime.credential_physical_mutation_outstanding());
        assert!(matches!(
            runtime.drive_live_pairing(&mut access),
            CredentialPairingDriveOutcome::CleanupCompleted
        ));
        assert_eq!(runtime.live_pairing_status(), CredentialPairingStatus::Idle);
        assert!(!runtime.credential_physical_mutation_outstanding());
    }

    #[test]
    fn activation_confirmation_uses_store_generation_when_pending_is_older() {
        const PENDING_PSK: [u8; 32] = [0x31; 32];
        const OTHER_PSK: [u8; 32] = [0x72; 32];

        let store_binding = binding(0x5a);
        let mut access = bound(FakeNor::erased(), store_binding);
        let mounted = recover_empty_provision(&mut access)
            .unwrap_or_else(|_| panic!("test revision-1 provisioning failed"));
        let pending_id = CredentialId::new([0x41; 16]);
        let pending_record = || {
            CredentialRecord::with_secret(
                pending_id,
                CredentialGeneration::new(2),
                PrincipalId([0x51; 16]),
                Permissions::EXPERIMENTAL_SUBMIT_RNS_DATA,
                CredentialStatus::Pending,
                CredentialAudit::new(
                    AuthorityRevision::new(2),
                    AuthorityRevision::new(2),
                    PairingOrigin::UsbPhysicalPresence,
                    AuthorizationPolicyVersion::new(1),
                ),
                PENDING_PSK,
            )
        };
        let revision_two = CredentialAuthorityBuilder::new(AuthorityRevision::new(2))
            .unwrap_or_else(|fault| panic!("revision-2 builder failed: {:?}", fault.kind()))
            .insert(pending_record())
            .unwrap_or_else(|fault| panic!("revision-2 pending failed: {:?}", fault.kind()))
            .finish();
        let mounted = commit_successor(mounted, &mut access, revision_two)
            .unwrap_or_else(|_| panic!("revision-2 commit failed"));
        let mounted = recover_once(mounted, &mut access)
            .unwrap_or_else(|_| panic!("revision-2 cleanup failed"));
        assert_eq!(mounted.recovery(), CredentialStoreRecovery::Clean);

        let other = CredentialRecord::with_secret(
            CredentialId::new([0x42; 16]),
            CredentialGeneration::new(3),
            PrincipalId([0x52; 16]),
            Permissions::READ_SUBMISSION_STATUS,
            CredentialStatus::Active,
            CredentialAudit::new(
                AuthorityRevision::new(3),
                AuthorityRevision::new(3),
                PairingOrigin::UsbPhysicalPresence,
                AuthorizationPolicyVersion::new(1),
            ),
            OTHER_PSK,
        );
        let revision_three = CredentialAuthorityBuilder::new(AuthorityRevision::new(3))
            .unwrap_or_else(|fault| panic!("revision-3 builder failed: {:?}", fault.kind()))
            .insert(pending_record())
            .unwrap_or_else(|fault| panic!("revision-3 pending failed: {:?}", fault.kind()))
            .insert(other)
            .unwrap_or_else(|fault| panic!("revision-3 active failed: {:?}", fault.kind()))
            .finish();
        let mounted = commit_successor(mounted, &mut access, revision_three)
            .unwrap_or_else(|_| panic!("revision-3 commit failed"));
        let mounted = recover_once(mounted, &mut access)
            .unwrap_or_else(|_| panic!("revision-3 cleanup failed"));
        assert_eq!(mounted.recovery(), CredentialStoreRecovery::Clean);
        drop(mounted);

        access.backend_mut().reset_io();
        let boot = boot_credentials(&mut access);
        assert_eq!(boot.state(), CredentialBootState::Ready);
        access.backend_mut().reset_io();
        let device_id = device_id(0x5a);
        let mut runtime = CredentialRuntime::from_boot(boot, store_binding, device_id);
        open_pairing_window(&mut runtime);
        let offer = BeginOffer::after_pending_commit(
            reticulum_device_api_pairing::BearerBinding::UsbSerialJtag,
            device_id,
            pending_id,
            CredentialGeneration::new(2),
            PairingPsk::new(PENDING_PSK).unwrap(),
        )
        .unwrap();
        let mut rng = TestRng::patterned(0x63);
        let (transcript, verification_proof) =
            admit_valid_activation(&mut runtime, &mut rng, &offer);
        assert!(matches!(
            runtime.drive_live_pairing(&mut access),
            CredentialPairingDriveOutcome::MutationPrepared(PairingMutation::ActivatePending)
        ));
        let confirmation = match runtime.drive_live_pairing(&mut access) {
            CredentialPairingDriveOutcome::Activated(confirmation) => confirmation,
            _ => panic!("non-adjacent activation did not commit"),
        };
        let authority = runtime
            .mounted
            .as_ref()
            .and_then(MountedCredentialStore::publishable_authority)
            .expect("activated authority is publishable");
        let (_, active_generation, _active_psk) = authority
            .select_for_handshake(pending_id)
            .expect("activated credential is selectable")
            .into_parts();
        assert_eq!(offer.generation().get(), 2);
        assert_eq!(active_generation.get(), 4);
        assert_ne!(active_generation.get(), offer.generation().get() + 1);
        assert_eq!(confirmation.generation(), active_generation);
        assert!(confirmation.verify(offer.psk(), &transcript, &verification_proof));
    }

    #[test]
    fn authentication_only_retains_ordinary_session_selection() {
        let (mut runtime, _access, credential_id, generation, expected_psk) =
            authentication_only_runtime();
        assert!(runtime.authority_publishable());
        assert!(!runtime.mutation_eligible());
        assert!(runtime.pairing_policy_available());
        assert_eq!(
            runtime.initialization_status(),
            CredentialInitializationStatus::Completed
        );
        assert_eq!(
            runtime.pairing_connected(time(0), connection()),
            Some(Ok(None))
        );

        let selected = runtime
            .select_ordinary_session(time(1), connection(), credential_id)
            .unwrap_or_else(|_| panic!("active credential was unavailable in AuthenticationOnly"));
        let (selected_id, selected_generation, selected_psk) = selected.into_parts();
        assert_eq!(selected_id, credential_id);
        assert_eq!(selected_generation, generation);
        assert_eq!(&*selected_psk, &*expected_psk);

        let unknown = CredentialId::new([0x99; 16]);
        assert!(matches!(
            runtime.select_ordinary_session(time(2), connection(), unknown),
            Err(error) if error == OrdinarySessionSelectionRefusal::REFUSED
        ));
        assert!(matches!(
            runtime.select_ordinary_session(
                time(3),
                ConnectionId::new(2).expect("test connection is nonzero"),
                credential_id,
            ),
            Err(error) if error == OrdinarySessionSelectionRefusal::REFUSED
        ));
    }

    #[test]
    fn rebooted_active_authority_reports_initialized_and_admits_another_credential() {
        let (mut runtime, mut access) = ready_pairing_runtime();
        let mut rng = TestRng::patterned(0x71);
        let offer = commit_begin(&mut runtime, &mut access, &mut rng);
        assert!(matches!(
            runtime.drive_live_pairing(&mut access),
            CredentialPairingDriveOutcome::CleanupCompleted
        ));
        let _ = admit_valid_activation(&mut runtime, &mut rng, &offer);
        assert!(matches!(
            runtime.drive_live_pairing(&mut access),
            CredentialPairingDriveOutcome::MutationPrepared(PairingMutation::ActivatePending)
        ));
        assert!(matches!(
            runtime.drive_live_pairing(&mut access),
            CredentialPairingDriveOutcome::Activated(_)
        ));

        let store_binding = runtime.binding();
        let device_id = runtime.device_id();
        let mut access = bound(access.into_backend(), store_binding);
        let boot = boot_credentials(&mut access);
        assert_eq!(boot.state(), CredentialBootState::Ready);
        access.backend_mut().reset_io();
        let mut rebooted = CredentialRuntime::from_boot(boot, store_binding, device_id);

        assert_eq!(rebooted.active_credential_count(), Some(1));
        assert_eq!(
            rebooted.initialization_status(),
            CredentialInitializationStatus::Completed
        );
        open_pairing_window(&mut rebooted);
        assert_eq!(
            rebooted.request_pairing_begin(
                reticulum_device_api_pairing::BearerBinding::BleGatt,
                time(REQUEST_MILLIS + 1),
                connection(),
                &mut rng,
            ),
            BeginAdmission::Accepted
        );
    }

    #[test]
    fn authentication_only_refuses_every_credential_mutation_path_without_io() {
        let (mut runtime, mut access, credential_id, generation, _psk) =
            authentication_only_runtime();
        assert_eq!(runtime.live_pairing_status(), CredentialPairingStatus::Idle);
        assert!(!runtime.credential_physical_mutation_outstanding());
        assert!(matches!(
            runtime.pairing_observe_button(time(1), ActiveLowButton::High),
            Some(ButtonEffect::None)
        ));
        assert!(matches!(
            runtime.pairing_observe_button(time(BUTTON_PRESS_MILLIS), ActiveLowButton::Low),
            Some(ButtonEffect::None)
        ));
        assert!(matches!(
            runtime.pairing_observe_button(time(WINDOW_OPEN_MILLIS), ActiveLowButton::Low),
            Some(ButtonEffect::None)
        ));

        let mut rng = TestRng::patterned(0x27);
        assert_eq!(
            runtime.request_pairing_begin(
                reticulum_device_api_pairing::BearerBinding::UsbSerialJtag,
                time(REQUEST_MILLIS + 1),
                connection(),
                &mut rng
            ),
            BeginAdmission::Refused(PairingFailure::Unavailable)
        );
        assert_eq!(rng.fills, 0);

        let proof_start = ProofStartRequest::new(
            reticulum_device_api_pairing::BearerBinding::UsbSerialJtag,
            2,
            credential_id,
            generation,
            [0x5a; 32],
        )
        .expect("test ProofStart is valid");
        assert!(matches!(
            runtime.request_pairing_proof_start(
                time(REQUEST_MILLIS + 2),
                connection(),
                &proof_start,
                &mut rng,
            ),
            ProofStartAdmission::Refused(PairingFailure::Unavailable)
        ));
        assert_eq!(rng.fills, 0);

        let activate = ActivateRequest::new(
            3,
            credential_id,
            generation,
            ClientProof::from_bytes([0x6b; 32]),
        )
        .expect("test Activate is valid");
        assert_eq!(
            runtime.request_pairing_activate(
                reticulum_device_api_pairing::BearerBinding::UsbSerialJtag,
                time(REQUEST_MILLIS + 3),
                connection(),
                activate,
            ),
            ActivateAdmission::Refused(ActivateFailure::Unavailable)
        );
        assert_eq!(
            runtime.request_pairing_abort_current(time(REQUEST_MILLIS + 4), connection()),
            AbortAdmission::Refused(PairingFailure::Unavailable)
        );
        assert!(matches!(
            runtime.request_initialization(time(REQUEST_MILLIS + 5), connection(), true),
            Err(InitializationRequestRefusal::PairingUnavailable)
        ));
        assert!(matches!(
            runtime.drive_initialization(&mut access, true),
            InitializationDriveOutcome::NotInFlight(CredentialInitializationStatus::Completed)
        ));
        assert!(matches!(
            runtime.drive_live_pairing(&mut access),
            CredentialPairingDriveOutcome::Blocked(None)
        ));
        assert_eq!(runtime.live_pairing_status(), CredentialPairingStatus::Idle);
        assert!(!runtime.credential_physical_mutation_outstanding());
        assert_eq!(access.backend().reads, 0);
        assert_eq!(access.backend().writes, 0);
        assert_eq!(access.backend().erases, 0);
    }

    #[test]
    fn abort_current_commits_only_after_cleanup_and_retains_a_tombstone() {
        let (mut runtime, mut access) = ready_pairing_runtime();
        let mut rng = TestRng::patterned(0x31);
        let offer = commit_begin(&mut runtime, &mut access, &mut rng);
        assert_eq!(
            runtime.request_pairing_abort_current(time(REQUEST_MILLIS + 2), connection(),),
            AbortAdmission::Accepted
        );
        assert_eq!(
            runtime.live_pairing_status(),
            CredentialPairingStatus::AwaitingCleanStore(PairingMutation::AbortPending)
        );
        assert!(matches!(
            runtime.drive_live_pairing(&mut access),
            CredentialPairingDriveOutcome::CleanupCompleted
        ));
        assert!(matches!(
            runtime.drive_live_pairing(&mut access),
            CredentialPairingDriveOutcome::MutationPrepared(PairingMutation::AbortPending)
        ));
        assert!(matches!(
            runtime.drive_live_pairing(&mut access),
            CredentialPairingDriveOutcome::Aborted
        ));

        let authority = runtime
            .mounted
            .as_ref()
            .and_then(MountedCredentialStore::publishable_authority)
            .expect("aborted authority is publishable");
        assert_eq!(authority.record_count(), 1);
        assert_eq!(authority.active_count(), 0);
        assert!(
            authority
                .pending_credential()
                .unwrap_or_else(|_| panic!("aborted authority has multiple pending records"))
                .is_none()
        );
        assert_eq!(
            runtime.live_pairing_status(),
            CredentialPairingStatus::CleanupRequired
        );
        drop(offer);
    }

    #[test]
    fn bad_wrong_connection_and_expired_activate_each_destroy_the_challenge() {
        enum Fault {
            BadProof,
            WrongConnection,
            Expired,
        }

        for (seed, fault) in [
            (0x41, Fault::BadProof),
            (0x51, Fault::WrongConnection),
            (0x61, Fault::Expired),
        ] {
            let (mut runtime, mut access) = ready_pairing_runtime();
            let mut rng = TestRng::patterned(seed);
            let offer = commit_begin(&mut runtime, &mut access, &mut rng);
            let (request, challenge) = start_proof(&mut runtime, &mut rng, &offer);
            let transcript = PairingTranscript::new(&request, &challenge)
                .unwrap_or_else(|_| panic!("matching test transcript was rejected"));
            let proof = match fault {
                Fault::BadProof => {
                    let calculated = ClientProof::calculate(offer.psk(), &transcript);
                    let mut bytes = *calculated.as_bytes();
                    bytes[0] ^= 1;
                    ClientProof::from_bytes(bytes)
                }
                Fault::WrongConnection | Fault::Expired => {
                    ClientProof::calculate(offer.psk(), &transcript)
                }
            };
            let activate =
                ActivateRequest::new(3, offer.credential_id(), offer.generation(), proof)
                    .expect("test Activate is valid");
            let request_connection = match fault {
                Fault::WrongConnection => ConnectionId::new(2).expect("nonzero connection"),
                Fault::BadProof | Fault::Expired => connection(),
            };
            let now = match fault {
                Fault::Expired => time(WINDOW_OPEN_MILLIS + PAIRING_WINDOW_MILLIS),
                Fault::BadProof | Fault::WrongConnection => time(REQUEST_MILLIS + 3),
            };
            assert_eq!(
                runtime.request_pairing_activate(
                    reticulum_device_api_pairing::BearerBinding::UsbSerialJtag,
                    now,
                    request_connection,
                    activate
                ),
                ActivateAdmission::Refused(ActivateFailure::ProofRejected)
            );
            assert_eq!(
                runtime.live_pairing_status(),
                CredentialPairingStatus::CleanupRequired
            );
            assert!(
                !runtime
                    .pairing
                    .as_ref()
                    .is_some_and(PairingPolicy::operation_outstanding)
            );
        }
    }

    #[test]
    fn clock_fault_in_an_intervening_request_invalidates_a_valid_continuation() {
        let (mut runtime, mut access) = ready_pairing_runtime();
        let mut rng = TestRng::patterned(0x68);
        let offer = commit_begin(&mut runtime, &mut access, &mut rng);
        let (request, challenge) = start_proof(&mut runtime, &mut rng, &offer);
        let transcript = PairingTranscript::new(&request, &challenge)
            .unwrap_or_else(|_| panic!("matching test transcript was rejected"));
        let proof = ClientProof::calculate(offer.psk(), &transcript);
        let activate = ActivateRequest::new(3, offer.credential_id(), offer.generation(), proof)
            .expect("test Activate is valid");

        assert_eq!(
            runtime.request_pairing_begin(
                reticulum_device_api_pairing::BearerBinding::UsbSerialJtag,
                time(1),
                connection(),
                &mut rng
            ),
            BeginAdmission::Refused(PairingFailure::Blocked)
        );
        assert_eq!(
            runtime.request_pairing_activate(
                reticulum_device_api_pairing::BearerBinding::UsbSerialJtag,
                time(REQUEST_MILLIS + 3),
                connection(),
                activate,
            ),
            ActivateAdmission::Refused(ActivateFailure::ProofRejected)
        );
        assert_eq!(
            runtime.live_pairing_status(),
            CredentialPairingStatus::CleanupRequired
        );
        assert!(
            !runtime
                .pairing
                .as_ref()
                .is_some_and(PairingPolicy::operation_outstanding)
        );
    }

    #[test]
    fn abort_current_does_not_reveal_pending_state_outside_physical_presence() {
        let (mut without_pending, _) = ready_pairing_runtime();
        assert!(matches!(
            without_pending.pairing_disconnected(time(REQUEST_MILLIS + 1), connection()),
            Some(PolicyEvent::Closed(_))
        ));
        let without =
            without_pending.request_pairing_abort_current(time(REQUEST_MILLIS + 2), connection());

        let (mut with_pending, mut access) = ready_pairing_runtime();
        let mut rng = TestRng::patterned(0x69);
        let _offer = commit_begin(&mut with_pending, &mut access, &mut rng);
        assert!(matches!(
            with_pending.pairing_disconnected(time(REQUEST_MILLIS + 2), connection()),
            Some(PolicyEvent::Closed(_))
        ));
        let with =
            with_pending.request_pairing_abort_current(time(REQUEST_MILLIS + 3), connection());

        assert_eq!(
            without,
            AbortAdmission::Refused(PairingFailure::PhysicalPresenceRequired)
        );
        assert_eq!(with, without);
    }

    #[test]
    fn begin_and_challenge_entropy_exhaustion_is_bounded_and_releases_policy() {
        let (mut runtime, _) = ready_pairing_runtime();
        let mut rng = TestRng::zero();
        assert_eq!(
            runtime.request_pairing_begin(
                reticulum_device_api_pairing::BearerBinding::UsbSerialJtag,
                time(REQUEST_MILLIS + 1),
                connection(),
                &mut rng,
            ),
            BeginAdmission::Refused(PairingFailure::Blocked)
        );
        assert_eq!(rng.fills, usize::from(MAX_PAIRING_ENTROPY_ATTEMPTS) * 3);
        assert_eq!(runtime.live_pairing_status(), CredentialPairingStatus::Idle);
        assert!(
            !runtime
                .pairing
                .as_ref()
                .is_some_and(PairingPolicy::operation_outstanding)
        );

        let (mut runtime, mut access) = ready_pairing_runtime();
        let mut begin_rng = TestRng::patterned(0x72);
        let offer = commit_begin(&mut runtime, &mut access, &mut begin_rng);
        let request = ProofStartRequest::new(
            reticulum_device_api_pairing::BearerBinding::UsbSerialJtag,
            2,
            offer.credential_id(),
            offer.generation(),
            [0x5a; 32],
        )
        .expect("test ProofStart is valid");
        let mut challenge_rng = TestRng::zero();
        assert!(matches!(
            runtime.request_pairing_proof_start(
                time(REQUEST_MILLIS + 2),
                connection(),
                &request,
                &mut challenge_rng,
            ),
            ProofStartAdmission::Refused(PairingFailure::Blocked)
        ));
        assert_eq!(
            challenge_rng.fills,
            usize::from(MAX_PAIRING_ENTROPY_ATTEMPTS)
        );
        assert_eq!(
            runtime.live_pairing_status(),
            CredentialPairingStatus::CleanupRequired
        );
        assert!(
            !runtime
                .pairing
                .as_ref()
                .is_some_and(PairingPolicy::operation_outstanding)
        );
    }

    #[test]
    fn partial_write_and_lost_reply_reconcile_begin_without_releasing_ownership() {
        for (seed, fault) in [
            (0x81, WriteFault::Partial(37)),
            (0x91, WriteFault::LostReply),
        ] {
            let (mut runtime, mut access) = ready_pairing_runtime();
            let mut rng = TestRng::patterned(seed);
            assert_eq!(
                runtime.request_pairing_begin(
                    reticulum_device_api_pairing::BearerBinding::UsbSerialJtag,
                    time(REQUEST_MILLIS + 1),
                    connection(),
                    &mut rng,
                ),
                BeginAdmission::Accepted
            );
            access.backend_mut().fail_next_write = Some(fault);
            assert!(matches!(
                runtime.drive_live_pairing(&mut access),
                CredentialPairingDriveOutcome::ReconcileRequired(PairingMutation::AddPending)
            ));
            assert_eq!(
                runtime.live_pairing_status(),
                CredentialPairingStatus::ReconcileRequired(PairingMutation::AddPending)
            );
            assert!(runtime.mounted.is_none());
            assert!(runtime.credential_physical_mutation_outstanding());
            assert!(matches!(
                runtime.drive_live_pairing(&mut access),
                CredentialPairingDriveOutcome::BeginOffered(_)
            ));
            assert!(runtime.mounted.is_some());
            assert_eq!(
                runtime.live_pairing_status(),
                CredentialPairingStatus::CleanupRequired
            );
        }
    }

    #[test]
    fn third_attempt_disconnect_timeout_and_replacement_cancel_only_the_challenge() {
        for action in 0_u8..3 {
            let (mut runtime, mut access) = ready_pairing_runtime();
            start_third_attempt_proof(&mut runtime, &mut access, 0xa1 + action);
            match action {
                0 => {
                    let other = ConnectionId::new(2).expect("nonzero connection");
                    assert!(matches!(
                        runtime.pairing_disconnected(time(REQUEST_MILLIS + 3), other),
                        Some(PolicyEvent::None)
                    ));
                    assert_eq!(
                        runtime.live_pairing_status(),
                        CredentialPairingStatus::ProofOutstanding
                    );
                    assert!(matches!(
                        runtime.pairing_disconnected(time(REQUEST_MILLIS + 4), connection()),
                        Some(PolicyEvent::None)
                    ));
                }
                1 => {
                    assert!(matches!(
                        runtime.pairing_poll_timeout(time(
                            WINDOW_OPEN_MILLIS + PAIRING_WINDOW_MILLIS,
                        )),
                        Some(PolicyEvent::None)
                    ));
                }
                2 => {
                    let replacement = ConnectionId::new(2).expect("nonzero connection");
                    assert!(matches!(
                        runtime.pairing_connected(time(REQUEST_MILLIS + 3), replacement),
                        Some(Ok(None))
                    ));
                }
                _ => unreachable!(),
            }
            assert_eq!(runtime.live_pairing_status(), CredentialPairingStatus::Idle);
            assert!(!runtime.credential_physical_mutation_outstanding());
            assert!(
                !runtime
                    .pairing
                    .as_ref()
                    .is_some_and(PairingPolicy::operation_outstanding)
            );
        }
    }

    #[test]
    fn mismatched_typed_candidate_blocks_before_store_io_and_does_not_retry() {
        let (mut runtime_a, mut access_a) = ready_pairing_runtime();
        let (mut runtime_b, mut access_b) = ready_pairing_runtime();
        prepare_activation_candidate(&mut runtime_a, &mut access_a, 0xb1);
        prepare_activation_candidate(&mut runtime_b, &mut access_b, 0xc1);

        let ownership_a =
            core::mem::replace(&mut runtime_a.live_pairing, LivePairingOwnership::Blocked);
        let ownership_b =
            core::mem::replace(&mut runtime_b.live_pairing, LivePairingOwnership::Blocked);
        let (completion_a, candidate_a) = match ownership_a {
            LivePairingOwnership::Prepared {
                completion,
                candidate,
            } => (completion, candidate),
            _ => panic!("runtime A did not retain a prepared activation"),
        };
        let (completion_b, candidate_b) = match ownership_b {
            LivePairingOwnership::Prepared {
                completion,
                candidate,
            } => (completion, candidate),
            _ => panic!("runtime B did not retain a prepared activation"),
        };
        runtime_a.live_pairing = LivePairingOwnership::Prepared {
            completion: completion_a,
            candidate: candidate_b,
        };
        runtime_b.live_pairing = LivePairingOwnership::Prepared {
            completion: completion_b,
            candidate: candidate_a,
        };
        access_a.backend_mut().reset_io();

        assert!(matches!(
            runtime_a.drive_live_pairing(&mut access_a),
            CredentialPairingDriveOutcome::Blocked(Some(PairingMutation::ActivatePending))
        ));
        assert_eq!(access_a.backend().reads, 0);
        assert_eq!(access_a.backend().writes, 0);
        assert_eq!(access_a.backend().erases, 0);
        assert_eq!(
            runtime_a.live_pairing_status(),
            CredentialPairingStatus::Blocked
        );
        assert!(matches!(
            runtime_a.drive_live_pairing(&mut access_a),
            CredentialPairingDriveOutcome::Blocked(None)
        ));
        assert_eq!(access_a.backend().reads, 0);
        assert!(
            !runtime_a
                .pairing
                .as_ref()
                .is_some_and(PairingPolicy::operation_outstanding)
        );
        assert!(
            runtime_a
                .mounted
                .as_ref()
                .and_then(MountedCredentialStore::publishable_authority)
                .and_then(|authority| authority.pending_credential().ok())
                .flatten()
                .is_some()
        );
    }

    #[test]
    fn resident_runtime_stays_within_its_fixed_ram_ceiling() {
        assert!(size_of::<LivePairingOwnership>() <= MAXIMUM_LIVE_PAIRING_OWNERSHIP_BYTES);
        assert!(size_of::<CredentialRuntime>() <= MAXIMUM_CREDENTIAL_RUNTIME_BYTES);
    }
}
