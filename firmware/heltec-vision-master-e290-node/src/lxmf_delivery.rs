//! Bounded product policy for durable inbound `lxmf.delivery` ownership.
//!
//! This module is transport-neutral. It classifies portable durable-ingress
//! outcomes and owns only opaque application-event retry capabilities. Radio,
//! interface registration, flash access, and ordinary TX scheduling remain in
//! their existing owners.

use reticulum_lxmf_durable_ingress::DurableIngressRetentionReason;
use reticulum_lxmf_ingress::{StampPolicy, WireLimits};
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
    node.set_destination_announce_app_data(
        &destination,
        Some(&config::LXMF_DELIVERY_ANNOUNCE_APP_DATA),
    )
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

/// Select the first receiver stamp policy.
///
/// Stamp enforcement and tickets remain a later product-policy tranche. The
/// wire validator still preserves and validates the structure of any supplied
/// stamp under this explicit policy.
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

/// One opaque exact-event retry owner and its product scheduling evidence.
#[must_use = "a retained LXMF event retry entry must remain in its bounded set"]
pub struct LxmfRetryEntry<'slots> {
    token: ApplicationEventRetryToken<'slots>,
    retry_not_before_ms: u64,
    class: LxmfRetryClass,
    message_id: Option<MessageId>,
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
        }
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
mod tests {
    use super::*;
    use rand_core::{CryptoRng, RngCore};
    use reticulum_lxmf_durable_ingress::{DurableCandidateError, EventCarrierMismatch};
    use reticulum_lxmf_ingress::{DeferredIngress, RejectedIngress, UnrelatedEvent};
    use reticulum_lxmf_store::{LxmfStoreBindingError, LxmfStoreDeviceId, LxmfStoreFault};
    use reticulum_lxmf_wire::WireError;
    use reticulum_node_core::{
        AnnounceEmissionTime, ApplicationEvent, ApplicationEventDiscardReason,
        ApplicationEventOwner, ApplicationEventSlot, MonotonicSeconds, NodeActions, NodeConfig,
        NodeIdentity, NodeInstanceId,
    };
    use std::{vec, vec::Vec};

    fn retry_token(
        owner: &mut ApplicationEventOwner<'static>,
        destination: u8,
    ) -> ApplicationEventRetryToken<'static> {
        owner
            .try_offer_actions(NodeActions::without_retained_proofs(
                vec![ApplicationEvent::DataReceived {
                    destination: [destination; 16],
                    payload: Vec::from([destination]),
                }],
                vec![],
                0,
            ))
            .expect("test event fits");
        owner
            .lease_next()
            .expect("test event is ready")
            .quarantine_for_retry(LXMF_RETRY_QUARANTINE)
    }

    fn static_owner<const N: usize>() -> ApplicationEventOwner<'static> {
        let slots = std::boxed::Box::leak(std::boxed::Box::new(
            [const { ApplicationEventSlot::new() }; N],
        ));
        ApplicationEventOwner::new(slots)
    }

    #[derive(Default)]
    struct CounterRng(u8);

    impl RngCore for CounterRng {
        fn next_u32(&mut self) -> u32 {
            let mut bytes = [0; 4];
            self.fill_bytes(&mut bytes);
            u32::from_le_bytes(bytes)
        }

        fn next_u64(&mut self) -> u64 {
            let mut bytes = [0; 8];
            self.fill_bytes(&mut bytes);
            u64::from_le_bytes(bytes)
        }

        fn fill_bytes(&mut self, destination: &mut [u8]) {
            for byte in destination {
                self.0 = self.0.wrapping_add(1);
                *byte = self.0;
            }
        }

        fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
            self.fill_bytes(destination);
            Ok(())
        }
    }

    impl CryptoRng for CounterRng {}

    fn packet_only_announce(tag: u8) -> NodeActions {
        let mut node = NodeCore::<4, 1, 4, 2, 0>::new(
            NodeIdentity::from_private_key(&[tag; 64]).expect("test identity"),
            "proof-holder",
            &["source"],
            NodeInstanceId::new([tag; 16]),
            NodeConfig::endpoint(),
        )
        .expect("test node");
        let mut rng = CounterRng::default();
        node.queue_announce(
            None,
            AnnounceEmissionTime::new(1).expect("announce time"),
            &mut rng,
        )
        .expect("announce fits");
        let actions = node.flush_announces(MonotonicSeconds::new(1), &mut rng);
        assert_eq!(actions.packets.len(), 1);
        assert!(actions.events.is_empty());
        actions
    }

    #[test]
    fn selected_wire_and_stamp_profile_is_explicit() {
        assert_eq!(
            wire_limits(),
            WireLimits::new(4_096, 2_048, 256, 2_048, 65_536, 16)
        );
        assert!(matches!(stamp_policy(), StampPolicy::NotRequired));
    }

    #[test]
    fn mount_gated_activation_registers_only_the_durable_service() {
        let mut disabled = NodeCore::<4, 1, 4, 2, 0>::new(
            NodeIdentity::from_private_key(&[0x51; 64]).expect("test identity"),
            config::RNS_APPLICATION_NAME,
            &config::RNS_PRIMARY_ASPECTS,
            NodeInstanceId::new([1; 16]),
            NodeConfig::transport(),
        )
        .expect("test node");
        let primary = disabled.destination_hash();
        disabled.set_inbound_proof_policy(InboundProofPolicy::Always);
        assert_eq!(
            activate_lxmf_delivery(&mut disabled, false),
            Ok(LxmfDeliveryActivation::Disabled)
        );
        assert_eq!(disabled.destination_hash(), primary);
        let expected = disabled
            .register_inbound_single_destination(
                config::RNS_LXMF_APPLICATION_NAME,
                &config::RNS_LXMF_DELIVERY_ASPECTS,
            )
            .expect("disabled activation consumed no destination slot");

        let mut active = NodeCore::<4, 1, 4, 2, 0>::new(
            NodeIdentity::from_private_key(&[0x51; 64]).expect("test identity"),
            config::RNS_APPLICATION_NAME,
            &config::RNS_PRIMARY_ASPECTS,
            NodeInstanceId::new([2; 16]),
            NodeConfig::transport(),
        )
        .expect("test node");
        active.set_inbound_proof_policy(InboundProofPolicy::Always);
        assert_eq!(
            activate_lxmf_delivery(&mut active, true),
            Ok(LxmfDeliveryActivation::Active(expected))
        );
        assert_eq!(active.destination_hash(), primary);
    }

    #[test]
    fn malformed_is_terminal_while_source_missing_is_retryable() {
        let malformed = DurableIngressRetentionReason::<()>::Rejected(RejectedIngress::Wire(
            WireError::TooShort {
                minimum: 113,
                actual: 1,
            },
        ));
        assert_eq!(
            retention_action(&malformed, None),
            LxmfRetentionAction::Terminal(LxmfTerminalReject::InvalidMessage)
        );

        let missing = DurableIngressRetentionReason::<()>::Deferred(
            DeferredIngress::SourceIdentityUnavailable { source: [0x5a; 16] },
        );
        assert_eq!(
            retention_action(&missing, None),
            LxmfRetentionAction::Retry(LxmfRetryClass::AdmissionDeferred)
        );
    }

    #[test]
    fn busy_and_ambiguous_store_results_preserve_exact_retry_policy() {
        let backend = DurableIngressRetentionReason::Store(LxmfCommitError::Backend {
            stage: reticulum_lxmf_store::LxmfProgramStage::Commit,
            error: (),
        });
        assert_eq!(
            retention_action(&backend, Some(MessageId::new([6; 32]))),
            LxmfRetentionAction::Retry(LxmfRetryClass::StoreReconcile)
        );
        let message_id = MessageId::new([7; 32]);
        let busy =
            DurableIngressRetentionReason::<()>::Store(LxmfCommitError::AmbiguousMutationPending {
                message_id,
            });
        assert_eq!(
            retention_action(&busy, Some(message_id)),
            LxmfRetentionAction::Retry(LxmfRetryClass::StoreBusy)
        );
    }

    #[test]
    fn pre_io_deferral_preserves_only_existing_reconciliation_authority() {
        let pending = MessageId::new([0x7a; 32]);
        for fallback in [
            LxmfRetryClass::AdmissionDeferred,
            LxmfRetryClass::CrossStoreBusy,
            LxmfRetryClass::DelayedProofBusy,
        ] {
            assert_eq!(
                retry_after_pre_io_deferral(
                    Some(LxmfRetryClass::StoreReconcile),
                    Some(pending),
                    fallback,
                ),
                (LxmfRetryClass::StoreReconcile, Some(pending)),
            );
            assert_eq!(
                retry_after_pre_io_deferral(
                    Some(LxmfRetryClass::StoreBusy),
                    Some(pending),
                    fallback,
                ),
                (fallback, None),
                "StoreBusy must never impersonate exact reconciliation authority",
            );
        }

        let mut owner = static_owner::<1>();
        let (class, message_id) = retry_after_pre_io_deferral(
            Some(LxmfRetryClass::StoreReconcile),
            Some(pending),
            LxmfRetryClass::AdmissionDeferred,
        );
        let mut retries = LxmfRetrySet::<1>::new();
        retries
            .try_insert(LxmfRetryEntry::new(
                retry_token(&mut owner, 0x7a),
                10,
                class,
                message_id,
            ))
            .unwrap_or_else(|_| panic!("exact pending owner fits"));
        assert!(retries.contains_pending_owner(pending));
        assert!(retries.take_pressure_relief().is_none());
        assert!(retries.take_due(9, Some(pending)).is_none());
        assert_eq!(
            retries
                .take_due(10, Some(pending))
                .expect("identity-restored retry remains selectable")
                .message_id(),
            Some(pending),
        );
    }

    #[test]
    fn index_store_and_handle_capacity_have_explicit_terminal_policy() {
        for (reason, expected) in [
            (
                DurableIngressRetentionReason::<()>::Store(LxmfCommitError::Full {
                    required_extents: 2,
                    remaining_extents: 1,
                }),
                LxmfTerminalReject::StoreFull,
            ),
            (
                DurableIngressRetentionReason::<()>::Store(LxmfCommitError::IndexFull {
                    capacity: 512,
                }),
                LxmfTerminalReject::IndexFull,
            ),
            (
                DurableIngressRetentionReason::<()>::Store(LxmfCommitError::HandleExhausted),
                LxmfTerminalReject::HandleExhausted,
            ),
        ] {
            assert_eq!(
                retention_action(&reason, None),
                LxmfRetentionAction::Terminal(expected)
            );
        }
    }

    #[test]
    fn contradictions_and_store_faults_fail_closed() {
        let candidate = DurableIngressRetentionReason::<()>::Candidate(
            DurableCandidateError::EventCarrierMismatch(EventCarrierMismatch::Destination),
        );
        assert_eq!(
            retention_action(&candidate, None),
            LxmfRetentionAction::Terminal(LxmfTerminalReject::CandidateContradiction)
        );
        let binding = DurableIngressRetentionReason::<()>::Store(LxmfCommitError::Binding(
            LxmfStoreBindingError::DeviceMismatch {
                expected: LxmfStoreDeviceId::new([1; 16]),
                actual: LxmfStoreDeviceId::new([2; 16]),
            },
        ));
        assert_eq!(
            retention_action(&binding, None),
            LxmfRetentionAction::Terminal(LxmfTerminalReject::StoreFault)
        );
        let fault = DurableIngressRetentionReason::<()>::Store(LxmfCommitError::Fault(
            LxmfStoreFault::UnknownProgrammedExtent { extent: 3 },
        ));
        assert_eq!(
            retention_action(&fault, None),
            LxmfRetentionAction::Terminal(LxmfTerminalReject::StoreFault)
        );
        assert_eq!(
            retention_action(&fault, Some(MessageId::new([0xf0; 32]))),
            LxmfRetentionAction::HoldPendingFault,
            "a post-pending fault must retain exact reconciliation authority"
        );
        let collision =
            DurableIngressRetentionReason::<()>::Store(LxmfCommitError::HashCollision {
                message_id: MessageId::new([0xc0; 32]),
            });
        assert_eq!(
            retention_action(&collision, None),
            LxmfRetentionAction::Terminal(LxmfTerminalReject::HashCollision),
            "a collision rejects only this candidate and does not disable the service"
        );
    }

    #[test]
    fn one_packet_proof_holder_preserves_supervisor_return_and_rejects_a_second() {
        let mut holder = LxmfProofActionsHolder::new();
        let first = packet_only_announce(0x61);
        let first_pointer = first.packets.as_ptr();
        holder
            .try_stage(first, config::ordinary_admission(10))
            .unwrap_or_else(|_| panic!("one packet fits"));
        let busy = holder
            .begin_offer()
            .expect("offer takes sole owner")
            .try_submit(|actions, admission| {
                assert_eq!(actions.packets.as_ptr(), first_pointer);
                Err(LxmfProofSinkFailure::returned((), actions, admission))
            });
        assert_eq!(busy, Err(()));
        assert!(holder.is_occupied());

        let second = packet_only_announce(0x62);
        let second_pointer = second.packets.as_ptr();
        let failure = holder
            .try_stage(second, config::ordinary_admission(20))
            .expect_err("occupied holder rejects a second release");
        assert_eq!(failure.reason(), LxmfProofHolderError::Occupied);
        let (second, _) = failure.into_parts();
        assert_eq!(second.packets.as_ptr(), second_pointer);

        drop(
            holder
                .begin_offer()
                .expect("cancelled offer temporarily owns first proof"),
        );
        assert!(holder.is_occupied(), "cancelled offer restores first proof");

        holder
            .begin_offer()
            .expect("first returned proof remains exact")
            .try_submit::<(), _>(|actions, _admission| {
                assert_eq!(actions.packets.as_ptr(), first_pointer);
                assert_eq!(actions.packets.len(), 1);
                Ok(())
            })
            .expect("ordinary sink accepts first proof");
        holder
            .try_stage(second, config::ordinary_admission(20))
            .unwrap_or_else(|_| panic!("second fits only after first leaves"));
    }

    #[test]
    fn circular_proof_pressure_evicts_exactly_one_non_pending_retry() {
        let mut owner = static_owner::<{ config::APPLICATION_EVENT_SLOTS }>();
        let mut retries = LxmfRetrySet::<{ config::APPLICATION_EVENT_SLOTS }>::new();
        let pending = MessageId::new([0x91; 32]);
        retries
            .try_insert(LxmfRetryEntry::new(
                retry_token(&mut owner, 0),
                0,
                LxmfRetryClass::StoreReconcile,
                Some(pending),
            ))
            .unwrap_or_else(|_| panic!("pending owner fits"));
        for marker in 1..15 {
            retries
                .try_insert(LxmfRetryEntry::new(
                    retry_token(&mut owner, marker),
                    0,
                    LxmfRetryClass::AdmissionDeferred,
                    None,
                ))
                .unwrap_or_else(|_| panic!("deferred owner fits"));
        }
        assert_eq!(owner.capacities().vacant, 1);
        assert_eq!(retries.len(), 15);
        for capacity_backpressured in [false, true] {
            for fail_closed in [false, true] {
                for holder_occupied in [false, true] {
                    for ready_proofs in [0, 1] {
                        assert_eq!(
                            should_relieve_lxmf_retry(
                                capacity_backpressured,
                                fail_closed,
                                holder_occupied,
                                ready_proofs,
                            ),
                            capacity_backpressured
                                && !fail_closed
                                && (holder_occupied || ready_proofs != 0),
                        );
                    }
                }
            }
        }

        let evicted = retries
            .take_pressure_relief()
            .expect("one non-pending retry relieves circular pressure");
        assert_eq!(evicted.class(), LxmfRetryClass::AdmissionDeferred);
        owner
            .try_reacquire_quarantined(evicted.into_token())
            .expect("exact evictee reacquires")
            .discard(ApplicationEventDiscardReason::DownstreamCapacity);
        assert_eq!(owner.capacities().vacant, 2);
        assert_eq!(retries.len(), 14, "one pass evicts exactly one owner");
        assert!(retries.contains_reconciliation_owner(pending));

        owner
            .try_offer_actions(NodeActions::without_retained_proofs(
                vec![
                    ApplicationEvent::Tick {
                        expired_paths: 1,
                        closed_links: 0,
                    },
                    ApplicationEvent::Tick {
                        expired_paths: 0,
                        closed_links: 1,
                    },
                ],
                vec![],
                0,
            ))
            .expect("two-event retained ordinary batch now fits atomically");
        assert_eq!(owner.capacities().ready, 2);
    }

    #[test]
    fn pressure_relief_never_selects_reconciliation_or_fault_hold() {
        let mut owner = static_owner::<2>();
        let pending = MessageId::new([0xa5; 32]);
        let mut retries = LxmfRetrySet::<2>::new();
        for (marker, class) in [
            (1, LxmfRetryClass::StoreReconcile),
            (2, LxmfRetryClass::StoreFaultHold),
        ] {
            retries
                .try_insert(LxmfRetryEntry::new(
                    retry_token(&mut owner, marker),
                    0,
                    class,
                    Some(pending),
                ))
                .unwrap_or_else(|_| panic!("protected owner fits"));
        }
        assert!(retries.take_pressure_relief().is_none());
        assert!(retries.take_nonheld().is_none());
        assert!(retries.contains_reconciliation_owner(pending));
        assert!(retries.has_fault_hold(pending));
    }

    #[test]
    fn foreign_reacquire_fault_is_owned_outside_retry_scheduler() {
        let mut original = static_owner::<1>();
        let token = retry_token(&mut original, 0x31);
        let mut foreign = static_owner::<1>();
        let failure = foreign
            .try_reacquire_quarantined(token)
            .expect_err("foreign owner rejects exact capability");
        let first_message = MessageId::new([0x31; 32]);
        let mut fault = LxmfAuthorityFault::<2>::new();
        fault
            .try_capture(LxmfRetryEntry::new(
                failure.into_token(),
                u64::MAX,
                LxmfRetryClass::AdmissionDeferred,
                Some(first_message),
            ))
            .unwrap_or_else(|_| panic!("first authority fault is retained"));
        assert!(fault.is_some());
        assert!(fault.contains_message(first_message));

        let mut second_owner = static_owner::<1>();
        let second_message = MessageId::new([0x32; 32]);
        fault
            .try_capture(LxmfRetryEntry::new(
                retry_token(&mut second_owner, 0x32),
                0,
                LxmfRetryClass::CrossStoreBusy,
                Some(second_message),
            ))
            .unwrap_or_else(|_| panic!("second distinct authority is also retained"));
        assert!(fault.contains_message(second_message));

        let mut overflow_owner = static_owner::<1>();
        let overflow = LxmfRetryEntry::new(
            retry_token(&mut overflow_owner, 0x33),
            0,
            LxmfRetryClass::CrossStoreBusy,
            None,
        );
        let overflow = fault
            .try_capture(overflow)
            .expect_err("bounded fault owner returns overflow unchanged")
            .into_entry();
        overflow_owner
            .try_reacquire_quarantined(overflow.into_token())
            .expect("rejected overflow authority remains exact")
            .discard(ApplicationEventDiscardReason::Superseded);
    }

    #[test]
    fn sixteen_tokens_progress_independently_and_pending_store_owner_wins() {
        let mut owner = static_owner::<{ config::APPLICATION_EVENT_SLOTS }>();
        let mut retries = LxmfRetrySet::<{ config::APPLICATION_EVENT_SLOTS }>::new();
        let pending = MessageId::new([0x44; 32]);
        for marker in 0..config::APPLICATION_EVENT_SLOTS {
            let token = retry_token(&mut owner, marker as u8);
            let message_id = (marker == 9).then_some(pending);
            let class = if message_id.is_some() {
                LxmfRetryClass::StoreReconcile
            } else {
                LxmfRetryClass::AdmissionDeferred
            };
            retries
                .try_insert(LxmfRetryEntry::new(token, 10, class, message_id))
                .unwrap_or_else(|_| panic!("each physical slot has one retry owner"));
        }
        assert_eq!(retries.len(), config::APPLICATION_EVENT_SLOTS);
        assert!(retries.take_due(9, Some(pending)).is_none());
        let exact = retries
            .take_due(10, Some(pending))
            .expect("pending store owner is selected");
        assert_eq!(exact.message_id(), Some(pending));
        let exact_slot = exact.slot();
        let lease = owner
            .try_reacquire_quarantined(exact.into_token())
            .expect("opaque token reacquires exact event");
        lease.discard(reticulum_node_core::ApplicationEventDiscardReason::Superseded);
        assert_eq!(retries.len(), config::APPLICATION_EVENT_SLOTS - 1);

        let unrelated = retries
            .take_due(10, None)
            .expect("another slot can progress independently");
        assert_ne!(unrelated.slot(), exact_slot);
        owner
            .try_reacquire_quarantined(unrelated.into_token())
            .expect("independent token remains valid")
            .discard(reticulum_node_core::ApplicationEventDiscardReason::Superseded);
    }

    #[test]
    fn store_busy_token_cannot_impersonate_reconciliation_owner() {
        let mut owner = static_owner::<2>();
        let pending = MessageId::new([0xa1; 32]);
        let exact = retry_token(&mut owner, 1);
        let blocked = retry_token(&mut owner, 2);
        let mut retries = LxmfRetrySet::<2>::new();
        retries
            .try_insert(LxmfRetryEntry::new(
                exact,
                0,
                LxmfRetryClass::StoreReconcile,
                Some(pending),
            ))
            .unwrap_or_else(|_| panic!("exact owner fits"));
        retries
            .try_insert(LxmfRetryEntry::new(
                blocked,
                0,
                LxmfRetryClass::StoreBusy,
                None,
            ))
            .unwrap_or_else(|_| panic!("blocked owner fits"));

        let selected = retries
            .take_due(0, Some(pending))
            .expect("only exact reconciliation owner is eligible");
        assert_eq!(selected.class(), LxmfRetryClass::StoreReconcile);
        assert_eq!(selected.message_id(), Some(pending));
        assert!(retries.take_due(0, Some(pending)).is_none());
        owner
            .try_reacquire_quarantined(selected.into_token())
            .expect("selected exact owner remains structurally valid")
            .discard(reticulum_node_core::ApplicationEventDiscardReason::Superseded);
    }

    #[test]
    fn stale_and_foreign_tokens_fail_closed_without_scalar_substitution() {
        let mut first = static_owner::<1>();
        let token = retry_token(&mut first, 1);
        let mut foreign = static_owner::<1>();
        let failure = foreign
            .try_reacquire_quarantined(token)
            .expect_err("equal scalar slots from another owner are foreign");
        assert_eq!(
            failure.reason(),
            reticulum_node_core::ApplicationEventRetryError::ForeignOwner
        );
        let token = failure.into_token();
        let lease = first
            .try_reacquire_quarantined(token)
            .expect("unchanged token still authorizes its original owner");
        lease.discard(reticulum_node_core::ApplicationEventDiscardReason::Superseded);
    }

    #[test]
    fn unrelated_classification_is_never_mistaken_for_durable_lxmf() {
        let reason =
            DurableIngressRetentionReason::<()>::Unrelated(UnrelatedEvent::OtherDestination {
                expected: reticulum_lxmf_ingress::LocalDeliveryDestination::new([1; 16]),
                actual: [2; 16],
            });
        assert_eq!(
            retention_action(&reason, None),
            LxmfRetentionAction::Terminal(LxmfTerminalReject::Unrelated)
        );
    }
}
