//! Bounded product policy for durable inbound `lxmf.delivery` ownership.
//!
//! This module is transport-neutral. It classifies portable durable-ingress
//! outcomes and owns only opaque application-event retry capabilities. Radio,
//! interface registration, flash access, and ordinary TX scheduling remain in
//! their existing owners.

use reticulum_lxmf_durable_ingress::DurableIngressRetentionReason;
use reticulum_lxmf_ingress::{DeferredIngress, StampPolicy, WireLimits};
use reticulum_lxmf_model::MessageId;
use reticulum_lxmf_store::LxmfCommitError;
use reticulum_node_core::{
    ApplicationEventQuarantineReason, ApplicationEventRetryToken, ApplicationEventSequence,
    DelayedProofTransactionError, DestinationHash, InboundProofPolicy,
    LocalDestinationAnnounceAppDataError, LocalDestinationLinkPolicyError,
    LocalDestinationProofPolicyError, LocalDestinationRegistrationError, NodeActions, NodeCore,
};
use reticulum_tx_supervisor::OrdinaryRouterAdmission;

use crate::config;

/// Why an exact application event must be retried after bounded backoff.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LxmfRetryClass {
    /// A source identity or bounded validation dependency may become available.
    AdmissionDeferred,
    /// Another physical store currently owns mutation access.
    CrossStoreBusy,
    /// Fixed delayed-proof capacity is temporarily occupied.
    DelayedProofBusy,
    /// Backend completion is ambiguous; this exact candidate owns reconciliation.
    StoreReconcile,
    /// Another event owns store reconciliation and must run first.
    StoreBusy,
    /// A post-pending contradiction is held without spinning until reset/remount.
    StoreFaultHold,
}

impl LxmfRetryClass {
    const fn priority(self) -> u8 {
        match self {
            Self::StoreReconcile => 0,
            Self::StoreBusy => 1,
            Self::CrossStoreBusy => 2,
            Self::DelayedProofBusy => 3,
            Self::AdmissionDeferred => 4,
            Self::StoreFaultHold => u8::MAX,
        }
    }
}

/// Result of mount-gated local LXMF destination construction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LxmfDeliveryActivation {
    /// No mounted store existed, so no local destination was registered.
    Disabled,
    /// The stable inbound Single destination was registered with retained proofs.
    Active(DestinationHash),
}

/// Construction failure after the product elected to enable LXMF delivery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LxmfDeliveryActivationError {
    /// RNS rejected or lacked capacity for the additional destination.
    Registration(LocalDestinationRegistrationError),
    /// RNS could not retain canonical default LXMF announce application data.
    AnnounceAppData(LocalDestinationAnnounceAppDataError),
    /// RNS rejected retained-proof policy for the newly registered destination.
    ProofPolicy(LocalDestinationProofPolicyError),
    /// RNS could not enable inbound Links on the destination.
    LinkPolicy(LocalDestinationLinkPolicyError),
}

/// Why an action envelope cannot enter the dedicated released-proof holder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LxmfProofHolderError {
    /// The holder already owns one released proof envelope.
    Occupied,
    /// A delayed proof must release exactly one packet and no application events.
    NotOnePacketOnly,
}

/// Failed holder insertion preserving the exact action and admission owners.
#[must_use = "a rejected proof envelope remains caller-owned"]
pub struct LxmfProofHolderFailure {
    reason: LxmfProofHolderError,
    actions: NodeActions,
    admission: OrdinaryRouterAdmission,
}

impl LxmfProofHolderFailure {
    /// Stable insertion failure classification.
    pub const fn reason(&self) -> LxmfProofHolderError {
        self.reason
    }

    /// Recover the exact packet-only action and admission owners.
    pub fn into_parts(self) -> (NodeActions, OrdinaryRouterAdmission) {
        (self.actions, self.admission)
    }
}

/// One released delayed-proof envelope retained across supervisor pressure.
///
/// The holder is deliberately neither `Clone` nor `Copy`. While occupied it
/// rejects a second envelope unchanged, preventing a second packet buffer or
/// proof queue from being created outside the ordinary supervisor.
#[must_use = "a proof holder owns any staged packet until accepted or recovered"]
pub struct LxmfProofActionsHolder {
    pending: Option<(NodeActions, OrdinaryRouterAdmission)>,
}

/// Exact returned-owner envelope from one synchronous proof sink failure.
#[must_use = "a failed proof sink still owns the exact packet and admission"]
pub struct LxmfProofSinkFailure<E> {
    reason: E,
    actions: NodeActions,
    admission: OrdinaryRouterAdmission,
}

impl<E> LxmfProofSinkFailure<E> {
    /// Return an offer classification with the exact unchanged inputs.
    pub fn returned(reason: E, actions: NodeActions, admission: OrdinaryRouterAdmission) -> Self {
        Self {
            reason,
            actions,
            admission,
        }
    }

    fn into_parts(self) -> (E, NodeActions, OrdinaryRouterAdmission) {
        (self.reason, self.actions, self.admission)
    }
}

/// Exclusive one-envelope capability for a synchronous supervisor offer.
///
/// The capability itself owns the actions and admission. It exposes them only
/// to one synchronous callback, and its mutable holder borrow prevents staging
/// another envelope until [`Self::try_submit`] transfers or restores the exact
/// owner. Dropping the capability without submitting restores the envelope.
#[must_use = "an in-flight proof offer owns its only packet envelope"]
pub struct LxmfProofOffer<'holder> {
    holder: &'holder mut LxmfProofActionsHolder,
    actions: Option<NodeActions>,
    admission: Option<OrdinaryRouterAdmission>,
}

impl LxmfProofOffer<'_> {
    /// Submit synchronously through one exact-return sink boundary.
    ///
    /// A rejecting callback returns [`LxmfProofSinkFailure`] with the same
    /// inputs. This method restores those owners before returning only the
    /// caller's scalar classification.
    pub fn try_submit<E, F>(mut self, sink: F) -> Result<(), E>
    where
        F: FnOnce(NodeActions, OrdinaryRouterAdmission) -> Result<(), LxmfProofSinkFailure<E>>,
    {
        let actions = self.actions.take().expect("proof offer owns actions");
        let admission = self.admission.take().expect("proof offer owns admission");
        match sink(actions, admission) {
            Ok(()) => Ok(()),
            Err(failure) => {
                let (reason, actions, admission) = failure.into_parts();
                debug_assert!(self.holder.pending.is_none());
                self.holder.pending = Some((actions, admission));
                Err(reason)
            }
        }
    }
}

impl Drop for LxmfProofOffer<'_> {
    fn drop(&mut self) {
        match (self.actions.take(), self.admission.take()) {
            (Some(actions), Some(admission)) => {
                debug_assert!(self.holder.pending.is_none());
                self.holder.pending = Some((actions, admission));
            }
            (None, None) => {}
            _ => unreachable!("proof offer always owns actions and admission together"),
        }
    }
}

impl LxmfProofActionsHolder {
    /// Construct an empty one-envelope holder.
    pub const fn new() -> Self {
        Self { pending: None }
    }

    /// Whether one packet-only proof envelope remains held.
    pub const fn is_occupied(&self) -> bool {
        self.pending.is_some()
    }

    /// Stage exactly one packet-only envelope.
    #[allow(clippy::result_large_err)]
    pub fn try_stage(
        &mut self,
        actions: NodeActions,
        admission: OrdinaryRouterAdmission,
    ) -> Result<(), LxmfProofHolderFailure> {
        if !actions.events.is_empty()
            || actions.retained_proof_count() != 0
            || actions.packets.len() != 1
        {
            return Err(LxmfProofHolderFailure {
                reason: LxmfProofHolderError::NotOnePacketOnly,
                actions,
                admission,
            });
        }
        if self.pending.is_some() {
            return Err(LxmfProofHolderFailure {
                reason: LxmfProofHolderError::Occupied,
                actions,
                admission,
            });
        }
        self.pending = Some((actions, admission));
        Ok(())
    }

    /// Begin one synchronous offer without exposing its packet owner.
    pub fn begin_offer(&mut self) -> Option<LxmfProofOffer<'_>> {
        let (actions, admission) = self.pending.take()?;
        Some(LxmfProofOffer {
            holder: self,
            actions: Some(actions),
            admission: Some(admission),
        })
    }
}

impl Default for LxmfProofActionsHolder {
    fn default() -> Self {
        Self::new()
    }
}

/// Register `lxmf.delivery` only when durable storage mounted successfully.
///
/// This runs before the node enters the permanent supervisor. A failure is a
/// construction-time fail-stop because the narrow node API intentionally has
/// no destination-unregister operation.
pub fn activate_lxmf_delivery<
    const PATHS: usize,
    const ANNOUNCES: usize,
    const DEDUPLICATION: usize,
    const LINKS: usize,
    const PACKET_BUFFERS: usize,
>(
    node: &mut NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, PACKET_BUFFERS>,
    store_available: bool,
    announce_app_data: &[u8],
) -> Result<LxmfDeliveryActivation, LxmfDeliveryActivationError> {
    if !store_available {
        return Ok(LxmfDeliveryActivation::Disabled);
    }
    let destination = node
        .register_inbound_single_destination(
            config::RNS_LXMF_APPLICATION_NAME,
            &config::RNS_LXMF_DELIVERY_ASPECTS,
        )
        .map_err(LxmfDeliveryActivationError::Registration)?;
    node.set_destination_announce_app_data(&destination, Some(announce_app_data))
        .map_err(LxmfDeliveryActivationError::AnnounceAppData)?;
    node.set_destination_accepts_links(&destination, true)
        .map_err(LxmfDeliveryActivationError::LinkPolicy)?;
    node.set_destination_inbound_proof_policy(&destination, InboundProofPolicy::Retain)
        .map_err(LxmfDeliveryActivationError::ProofPolicy)?;
    Ok(LxmfDeliveryActivation::Active(destination))
}

/// Explicit terminal policy for an event that cannot become a durable message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LxmfTerminalReject {
    /// The event did not belong to the configured LXMF destination.
    Unrelated,
    /// Wire, signature, destination, or stamp policy rejected the message.
    InvalidMessage,
    /// Validated evidence contradicted the exact retained event or durable model.
    CandidateContradiction,
    /// Required retained-proof state was absent or structurally invalid.
    ProofInvariant,
    /// The append-only store has no remaining physical capacity.
    StoreFull,
    /// The caller-owned semantic index has no remaining entry.
    IndexFull,
    /// Stable logical handles are exhausted under the selected format.
    HandleExhausted,
    /// Binding, committed media, or readback state failed closed.
    StoreFault,
    /// One message ID names conflicting authenticated material; other messages remain usable.
    HashCollision,
}

/// Product action for one portable durable-ingress retention reason.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LxmfRetentionAction {
    /// Retain the exact lease behind an opaque retry capability.
    Retry(LxmfRetryClass),
    /// Retain the exact event/token without retry spinning after a pending fault.
    HoldPendingFault,
    /// End ownership under one explicit terminal rejection policy.
    Terminal(LxmfTerminalReject),
}

/// Select the permanent E290's bounded LXMF wire-validation profile.
pub const fn wire_limits() -> WireLimits {
    WireLimits::new(
        config::LXMF_MAX_WIRE_BYTES,
        config::LXMF_MAX_VALUE_BYTES,
        config::LXMF_MAX_CONTAINER_ITEMS,
        config::LXMF_MAX_TOTAL_VALUES,
        config::LXMF_MAX_SCAN_STEPS,
        config::LXMF_MAX_NESTING_DEPTH,
    )
}

/// Select the receiver stamp policy.
///
/// The appliance does not require stamps or tickets. The wire validator still
/// preserves and validates the structure of any supplied stamp.
pub const fn stamp_policy() -> StampPolicy<'static> {
    StampPolicy::NotRequired
}

/// Whether one non-reconciliation retry must be evicted for proof liveness.
///
/// The pressure lane opens only when a retained ordinary non-packet batch has
/// actually filled the application owner and a durable proof is waiting. A
/// generally busy TX lane alone never authorizes message loss.
pub const fn should_relieve_lxmf_retry(
    application_owner_capacity_backpressured: bool,
    fail_closed_draining: bool,
    proof_holder_occupied: bool,
    ready_proofs: usize,
) -> bool {
    application_owner_capacity_backpressured
        && !fail_closed_draining
        && (proof_holder_occupied || ready_proofs != 0)
}

/// Preserve exact pending-store authority across a pre-I/O deferral.
///
/// Only an entry already classified as `StoreReconcile` and already carrying
/// its authenticated pending message may retain that class. A generic
/// `StoreBusy` or other retry can never acquire reconciliation authority merely
/// because the store currently has a pending mutation.
pub const fn retry_after_pre_io_deferral(
    attempted_class: Option<LxmfRetryClass>,
    attempted_message_id: Option<MessageId>,
    fallback: LxmfRetryClass,
) -> (LxmfRetryClass, Option<MessageId>) {
    if matches!(attempted_class, Some(LxmfRetryClass::StoreReconcile))
        && attempted_message_id.is_some()
    {
        (LxmfRetryClass::StoreReconcile, attempted_message_id)
    } else {
        (fallback, None)
    }
}

/// Classify one portable durable-ingress retention without losing its lease.
///
/// A pending message observed after a non-`AmbiguousMutationPending` store
/// error means this exact attempted candidate still owns reconciliation. Even
/// a normally terminal media fault must retain its structural event authority;
/// discarding it would strand the sole flash coordinator behind a pending bit.
pub const fn retention_action<E>(
    reason: &DurableIngressRetentionReason<E>,
    pending_after_call: Option<MessageId>,
) -> LxmfRetentionAction {
    if pending_after_call.is_some() {
        match reason {
            DurableIngressRetentionReason::Store(LxmfCommitError::Backend { .. }) => {
                return LxmfRetentionAction::Retry(LxmfRetryClass::StoreReconcile);
            }
            DurableIngressRetentionReason::Store(
                LxmfCommitError::Binding(_)
                | LxmfCommitError::Fault(_)
                | LxmfCommitError::Full { .. }
                | LxmfCommitError::IndexFull { .. }
                | LxmfCommitError::HashCollision { .. }
                | LxmfCommitError::HandleExhausted,
            ) => return LxmfRetentionAction::HoldPendingFault,
            _ => {}
        }
    }
    match reason {
        DurableIngressRetentionReason::Unrelated(_) => {
            LxmfRetentionAction::Terminal(LxmfTerminalReject::Unrelated)
        }
        DurableIngressRetentionReason::Deferred(_) => {
            LxmfRetentionAction::Retry(LxmfRetryClass::AdmissionDeferred)
        }
        DurableIngressRetentionReason::Rejected(_) => {
            LxmfRetentionAction::Terminal(LxmfTerminalReject::InvalidMessage)
        }
        DurableIngressRetentionReason::Candidate(_) => {
            LxmfRetentionAction::Terminal(LxmfTerminalReject::CandidateContradiction)
        }
        DurableIngressRetentionReason::DelayedProof(DelayedProofTransactionError::Reservation(
            _,
        )) => LxmfRetentionAction::Retry(LxmfRetryClass::DelayedProofBusy),
        DurableIngressRetentionReason::DelayedProof(
            DelayedProofTransactionError::ProofNotPresent,
        ) => LxmfRetentionAction::Terminal(LxmfTerminalReject::ProofInvariant),
        DurableIngressRetentionReason::Store(LxmfCommitError::Backend { .. }) => {
            LxmfRetentionAction::Retry(LxmfRetryClass::StoreReconcile)
        }
        DurableIngressRetentionReason::Store(LxmfCommitError::AmbiguousMutationPending {
            ..
        }) => LxmfRetentionAction::Retry(LxmfRetryClass::StoreBusy),
        DurableIngressRetentionReason::Store(LxmfCommitError::Full { .. }) => {
            LxmfRetentionAction::Terminal(LxmfTerminalReject::StoreFull)
        }
        DurableIngressRetentionReason::Store(LxmfCommitError::IndexFull { .. }) => {
            LxmfRetentionAction::Terminal(LxmfTerminalReject::IndexFull)
        }
        DurableIngressRetentionReason::Store(LxmfCommitError::HandleExhausted) => {
            LxmfRetentionAction::Terminal(LxmfTerminalReject::HandleExhausted)
        }
        DurableIngressRetentionReason::Store(
            LxmfCommitError::Binding(_) | LxmfCommitError::Fault(_),
        ) => LxmfRetentionAction::Terminal(LxmfTerminalReject::StoreFault),
        DurableIngressRetentionReason::Store(LxmfCommitError::HashCollision { .. }) => {
            LxmfRetentionAction::Terminal(LxmfTerminalReject::HashCollision)
        }
    }
}

/// Return the exact LXMF source whose retained identity can unblock admission.
///
/// Other deferred classes still use periodic bounded retry, but cannot be
/// woken by an announce because they do not name an identity dependency.
pub fn admission_deferred_source<E>(
    reason: &DurableIngressRetentionReason<E>,
) -> Option<DestinationHash> {
    match reason {
        DurableIngressRetentionReason::Deferred(DeferredIngress::SourceIdentityUnavailable {
            source,
        }) => Some(DestinationHash::new(*source)),
        _ => None,
    }
}

/// Deterministic bounded exponential delay for an admission-deferred event.
///
/// The original FIFO sequence spreads unrelated retained events without an
/// RNG or new authority. Attempt zero is normalized to the first attempt.
pub fn admission_retry_delay_ms(attempt: u8, sequence: ApplicationEventSequence) -> u64 {
    let attempt = attempt.max(1);
    let exponent = attempt.saturating_sub(1).min(15);
    let nominal_cap =
        config::LXMF_ADMISSION_RETRY_MAX_MS.saturating_sub(config::LXMF_ADMISSION_RETRY_MAX_MS / 5);
    let nominal = config::LXMF_ADMISSION_RETRY_INITIAL_MS
        .saturating_mul(1_u64 << exponent)
        .min(nominal_cap);
    let jitter_cap = (nominal / 5).min(config::LXMF_ADMISSION_RETRY_MAX_MS - nominal);
    let mixed = sequence
        .get()
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(u64::from(attempt).wrapping_mul(0xbf58_476d_1ce4_e5b9));
    nominal + mixed % jitter_cap.saturating_add(1)
}

/// One opaque exact-event retry owner and its product scheduling evidence.
#[must_use = "a retained LXMF event retry entry must remain in its bounded set"]
pub struct LxmfRetryEntry<'slots> {
    token: ApplicationEventRetryToken<'slots>,
    retry_not_before_ms: u64,
    class: LxmfRetryClass,
    message_id: Option<MessageId>,
    admission_attempt: u8,
    admission_source: Option<DestinationHash>,
}

impl<'slots> LxmfRetryEntry<'slots> {
    /// Quarantine one exact lease-derived token for bounded retry.
    pub const fn new(
        token: ApplicationEventRetryToken<'slots>,
        retry_not_before_ms: u64,
        class: LxmfRetryClass,
        message_id: Option<MessageId>,
    ) -> Self {
        Self {
            token,
            retry_not_before_ms,
            class,
            message_id,
            admission_attempt: 0,
            admission_source: None,
        }
    }

    /// Attach the bounded retry history for an admission-deferred event.
    ///
    /// Non-admission classes cannot acquire an identity wake dependency.
    pub const fn with_admission_state(
        mut self,
        attempt: u8,
        source: Option<DestinationHash>,
    ) -> Self {
        if matches!(self.class, LxmfRetryClass::AdmissionDeferred) {
            self.admission_attempt = attempt;
            self.admission_source = source;
        }
        self
    }

    /// Exact diagnostic event-slot index. This is not retry authority.
    pub const fn slot(&self) -> usize {
        self.token.event_id().slot().get()
    }

    /// Original FIFO sequence used for deterministic due selection.
    pub const fn sequence(&self) -> ApplicationEventSequence {
        self.token.sequence()
    }

    /// Product retry class.
    pub const fn class(&self) -> LxmfRetryClass {
        self.class
    }

    /// Authenticated message known to own store reconciliation, when available.
    pub const fn message_id(&self) -> Option<MessageId> {
        self.message_id
    }

    /// Number of consecutive retries for the same admission dependency.
    pub const fn admission_attempt(&self) -> u8 {
        self.admission_attempt
    }

    /// Exact announced destination whose identity can wake this retry.
    pub const fn admission_source(&self) -> Option<DestinationHash> {
        self.admission_source
    }

    /// Current absolute retry deadline, exposed for bounded diagnostics/tests.
    pub const fn retry_not_before_ms(&self) -> u64 {
        self.retry_not_before_ms
    }

    /// Consume the scheduling wrapper and recover the only retry authority.
    pub fn into_token(self) -> ApplicationEventRetryToken<'slots> {
        self.token
    }
}

/// Rejected insertion preserving the exact opaque retry owner.
#[must_use = "a failed retry insertion still owns its exact token"]
pub struct LxmfRetryInsertFailure<'slots> {
    entry: LxmfRetryEntry<'slots>,
}

impl<'slots> LxmfRetryInsertFailure<'slots> {
    /// Recover the unchanged entry.
    pub fn into_entry(self) -> LxmfRetryEntry<'slots> {
        self.entry
    }
}

/// Last-resort bounded owners for retry capabilities whose structure failed.
///
/// Any occupied value stops LXMF event processing but does not stop the shared
/// application-event consumer. Keeping this separate from [`LxmfRetrySet`]
/// prevents a stale, foreign, or otherwise unreacquirable token from becoming
/// an invisible non-due retry that the scheduler will never inspect again.
#[must_use = "an authority fault owns an exact application-event retry capability"]
pub struct LxmfAuthorityFault<'slots, const N: usize> {
    entries: [Option<LxmfRetryEntry<'slots>>; N],
}

impl<'slots, const N: usize> LxmfAuthorityFault<'slots, N> {
    /// Construct an empty authority-fault owner.
    pub const fn new() -> Self {
        Self {
            entries: [const { None }; N],
        }
    }

    /// Whether LXMF admission must remain stopped.
    pub const fn is_some(&self) -> bool {
        let mut index = 0;
        while index < N {
            if self.entries[index].is_some() {
                return true;
            }
            index += 1;
        }
        false
    }

    /// Whether no authority fault has been captured.
    pub const fn is_none(&self) -> bool {
        !self.is_some()
    }

    /// Message ID attached to the exact retained authority, if known.
    pub fn contains_message(&self, message_id: MessageId) -> bool {
        self.entries
            .iter()
            .flatten()
            .any(|entry| entry.message_id() == Some(message_id))
    }

    /// Retry class preserved from the failed structural operation.
    pub fn has_fault_hold(&self, message_id: MessageId) -> bool {
        self.entries.iter().flatten().any(|entry| {
            entry.class() == LxmfRetryClass::StoreFaultHold
                && entry.message_id() == Some(message_id)
        })
    }

    /// Capture one exact owner, returning it unchanged when the set is full.
    pub fn try_capture(
        &mut self,
        entry: LxmfRetryEntry<'slots>,
    ) -> Result<(), LxmfRetryInsertFailure<'slots>> {
        let Some(vacant) = self.entries.iter_mut().find(|slot| slot.is_none()) else {
            return Err(LxmfRetryInsertFailure { entry });
        };
        *vacant = Some(entry);
        Ok(())
    }
}

impl<const N: usize> Default for LxmfAuthorityFault<'_, N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Fixed retry owners aligned one-for-one with application-event slots.
#[must_use = "dropping the retry set drops structural authority for quarantined events"]
pub struct LxmfRetrySet<'slots, const N: usize> {
    entries: [Option<LxmfRetryEntry<'slots>>; N],
}

impl<'slots, const N: usize> LxmfRetrySet<'slots, N> {
    /// Construct an empty allocation-free retry set.
    pub const fn new() -> Self {
        Self {
            entries: [const { None }; N],
        }
    }

    /// Number of structurally retained retry owners.
    pub fn len(&self) -> usize {
        self.entries.iter().filter(|entry| entry.is_some()).count()
    }

    /// Whether no retry owner is retained.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Insert one token into its corresponding application-event slot.
    ///
    /// Duplicate-slot insertion is rejected without replacing either owner.
    pub fn try_insert(
        &mut self,
        entry: LxmfRetryEntry<'slots>,
    ) -> Result<(), LxmfRetryInsertFailure<'slots>> {
        let slot = entry.slot();
        let Some(destination) = self.entries.get_mut(slot) else {
            return Err(LxmfRetryInsertFailure { entry });
        };
        if destination.is_some() {
            return Err(LxmfRetryInsertFailure { entry });
        }
        *destination = Some(entry);
        Ok(())
    }

    /// Take one due retry, prioritizing the exact store-reconciliation owner.
    ///
    /// While the store reports a pending message, unrelated retries are not
    /// selected. This prevents a second candidate from starving the one owner
    /// able to reconcile ambiguous physical completion.
    pub fn take_due(
        &mut self,
        now_ms: u64,
        pending_message_id: Option<MessageId>,
    ) -> Option<LxmfRetryEntry<'slots>> {
        let selected = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| entry.as_ref().map(|entry| (index, entry)))
            .filter(|(_, entry)| entry.retry_not_before_ms <= now_ms)
            .filter(|(_, entry)| entry.class != LxmfRetryClass::StoreFaultHold)
            .filter(|(_, entry)| {
                pending_message_id.is_none()
                    || (entry.class == LxmfRetryClass::StoreReconcile
                        && entry.message_id == pending_message_id)
            })
            .min_by_key(|(_, entry)| (entry.class.priority(), entry.sequence().get()))
            .map(|(index, _)| index)?;
        self.entries[selected].take()
    }

    /// Wake admission retries whose exact source identity was just learned.
    ///
    /// The retained tokens and FIFO sequence remain unchanged. Entries that
    /// are already due are not counted as newly woken.
    pub fn wake_admission_for_source(&mut self, source: DestinationHash, now_ms: u64) -> usize {
        let mut woken = 0;
        for entry in self.entries.iter_mut().flatten() {
            if entry.class == LxmfRetryClass::AdmissionDeferred
                && entry.admission_source == Some(source)
                && entry.retry_not_before_ms > now_ms
            {
                entry.retry_not_before_ms = now_ms;
                woken += 1;
            }
        }
        woken
    }

    /// Whether the exact pending-store reconciliation owner is retained.
    pub fn contains_reconciliation_owner(&self, message_id: MessageId) -> bool {
        self.entries.iter().flatten().any(|entry| {
            entry.class == LxmfRetryClass::StoreReconcile && entry.message_id == Some(message_id)
        })
    }

    /// Whether a retryable or non-spinning held token owns this pending message.
    pub fn contains_pending_owner(&self, message_id: MessageId) -> bool {
        self.entries.iter().flatten().any(|entry| {
            matches!(
                entry.class,
                LxmfRetryClass::StoreReconcile | LxmfRetryClass::StoreFaultHold
            ) && entry.message_id == Some(message_id)
        })
    }

    /// Whether a non-spinning held token owns this pending store message.
    pub fn has_fault_hold(&self, message_id: MessageId) -> bool {
        self.entries.iter().flatten().any(|entry| {
            entry.class == LxmfRetryClass::StoreFaultHold && entry.message_id == Some(message_id)
        })
    }

    /// Take the oldest retry that cannot own pending-store recovery.
    ///
    /// `StoreReconcile` and `StoreFaultHold` tokens are never returned. Callers
    /// may discard later LXMF events while retaining every possible pending
    /// physical-mutation authority.
    pub fn take_nonheld(&mut self) -> Option<LxmfRetryEntry<'slots>> {
        let selected = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| entry.as_ref().map(|entry| (index, entry)))
            .filter(|(_, entry)| {
                !matches!(
                    entry.class,
                    LxmfRetryClass::StoreReconcile | LxmfRetryClass::StoreFaultHold
                )
            })
            .min_by_key(|(_, entry)| entry.sequence().get())
            .map(|(index, _)| index)?;
        self.entries[selected].take()
    }

    /// Evict one retry that cannot own physical-store reconciliation.
    ///
    /// This is the bounded pressure-relief lane used when the ordinary router
    /// is retaining a non-packet batch and therefore cannot yet accept a
    /// released durable proof. A reconciliation or fault-held token is never
    /// eligible, even if the caller's current store snapshot is inconsistent;
    /// those are the only structural authorities capable of clearing a
    /// pending physical mutation after recovery.
    pub fn take_pressure_relief(&mut self) -> Option<LxmfRetryEntry<'slots>> {
        let selected = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| entry.as_ref().map(|entry| (index, entry)))
            .filter(|(_, entry)| {
                !matches!(
                    entry.class,
                    LxmfRetryClass::StoreReconcile | LxmfRetryClass::StoreFaultHold
                )
            })
            .min_by_key(|(_, entry)| entry.sequence().get())
            .map(|(index, _)| index)?;
        self.entries[selected].take()
    }
}

impl<const N: usize> Default for LxmfRetrySet<'_, N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Quarantine reason shared by every bounded product retry.
pub const LXMF_RETRY_QUARANTINE: ApplicationEventQuarantineReason =
    ApplicationEventQuarantineReason::ConsumerDeferred;

#[cfg(test)]
#[path = "lxmf_delivery_tests.rs"]
mod tests;
