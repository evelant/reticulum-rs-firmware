//! Atomic handoff from a Reticulum application event to durable LXMF ownership.
//!
//! The handoff borrows an exact application event for cryptographic admission,
//! streams its validated normalized bytes into the mounted LXMF store, and only
//! then acknowledges the local event lease. On replay, the first committed wire
//! observation remains authoritative; an alternate arriving wire is not stored.
//! Every outcome short of a completed or already-durable commit returns the
//! exact unresolved lease to the caller.
//!
//! This ordering does not delay an RNS delivery proof. The current Rete ingress
//! constructs that proof before it exposes the application event, so target
//! composition needs a separate delayed-proof owner before a remote receipt can
//! imply durable LXMF delivery.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::fmt;

use reticulum_lxmf_ingress::{
    CarrierKind, DeferredIngress, IngressOutcome, LocalDeliveryDestination, RejectedIngress,
    SourceIdentityResolver, StampAdmission, StampPolicy, UnrelatedEvent, ValidatedIngressEvidence,
    WireLimits, validate_application_event,
};
use reticulum_lxmf_model::{
    AuthenticatedMaterialFingerprint, CandidateError, CarrierLengthMismatch, CarrierProvenance,
    DestinationHash, DurableMessageReceipt, InboundMessageCandidate, InboundMessageLengths,
    InboundMessageMetadata, InvalidStampCost, MessageId, MessageLengthOverflow, NormalizedWire,
    RequiredStampCost, SourceHash, StampAdmissionProvenance,
};
use reticulum_lxmf_store::{
    BoundLxmfStoreAccess, LxmfCommitError, LxmfCommitOutcome, MountedLxmfStore,
};
use reticulum_node_core::{ApplicationEventId, ApplicationEventLease};

#[cfg(test)]
mod tests;

/// Whether this event created a durable message or replayed one already owned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DurableIngressCommitKind {
    /// Exact message bytes and metadata became durable during this handoff.
    New,
    /// The authenticated logical message was already durable.
    Replay,
}

/// Successful durable ownership transfer for one application event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DurableIngressSuccess {
    event_id: ApplicationEventId,
    receipt: DurableMessageReceipt,
    kind: DurableIngressCommitKind,
}

impl DurableIngressSuccess {
    /// Generation-safe identity of the acknowledged application event.
    pub const fn event_id(self) -> ApplicationEventId {
        self.event_id
    }

    /// Stable receipt for the durably owned logical message.
    pub const fn receipt(self) -> DurableMessageReceipt {
        self.receipt
    }

    /// Whether this handoff committed new durable state or observed a replay.
    pub const fn kind(self) -> DurableIngressCommitKind {
        self.kind
    }
}

/// Failure while translating validated ingress evidence into a durable candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DurableCandidateError {
    /// One exact host-size length did not fit the portable durable model.
    LengthOverflow(MessageLengthOverflow),
    /// Carrier provenance contradicted normalized and physical byte counts.
    CarrierLength(CarrierLengthMismatch),
    /// Wire admission supplied a proof-of-work cost outside durable protocol bounds.
    InvalidStampCost(InvalidStampCost),
    /// Borrowed exact bytes contradicted their immutable metadata.
    Candidate(CandidateError),
}

/// Why the exact application event was not transferred to durable ownership.
#[derive(Debug)]
pub enum DurableIngressRetentionReason<E> {
    /// The event belongs to another application consumer.
    Unrelated(UnrelatedEvent),
    /// Admission needs identity state, bounded work, or a future carrier binding.
    Deferred(DeferredIngress),
    /// Wire, signature, destination, or receiver stamp policy rejected the event.
    Rejected(RejectedIngress),
    /// Validated evidence could not form a self-consistent durable candidate.
    Candidate(DurableCandidateError),
    /// The mounted durable store blocked or failed without committing the event.
    Store(LxmfCommitError<E>),
}

/// Exact unresolved application-event lease paired with a typed retention reason.
///
/// Dropping this wrapper follows node-core's fail-closed unresolved-lease path.
/// Call [`Self::into_lease`] to retry, quarantine, discard, or route the exact
/// event to another consumer.
#[must_use = "a retained application-event lease still requires an explicit disposition"]
pub struct RetainedApplicationEvent<'owner, 'slots, E> {
    lease: ApplicationEventLease<'owner, 'slots>,
    reason: DurableIngressRetentionReason<E>,
}

impl<'owner, 'slots, E> RetainedApplicationEvent<'owner, 'slots, E> {
    fn new(
        lease: ApplicationEventLease<'owner, 'slots>,
        reason: DurableIngressRetentionReason<E>,
    ) -> Self {
        Self { lease, reason }
    }

    /// Generation-safe identity of the exact retained event.
    pub const fn event_id(&self) -> ApplicationEventId {
        self.lease.id()
    }

    /// Typed reason the event remains caller-owned.
    pub const fn reason(&self) -> &DurableIngressRetentionReason<E> {
        &self.reason
    }

    /// Recover the exact unresolved event lease, discarding only the reason.
    pub fn into_lease(self) -> ApplicationEventLease<'owner, 'slots> {
        self.lease
    }

    /// Recover the exact unresolved event lease and its typed reason.
    pub fn into_parts(
        self,
    ) -> (
        ApplicationEventLease<'owner, 'slots>,
        DurableIngressRetentionReason<E>,
    ) {
        (self.lease, self.reason)
    }
}

impl<E: fmt::Debug> fmt::Debug for RetainedApplicationEvent<'_, '_, E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RetainedApplicationEvent")
            .field("event_id", &self.event_id())
            .field("reason", &self.reason)
            .field("event", &"<redacted>")
            .finish()
    }
}

/// Complete atomic durable-ingress outcome.
#[must_use = "retained outcomes still own an unresolved application-event lease"]
pub enum DurableIngressOutcome<'owner, 'slots, E> {
    /// Store ownership completed and the application event was acknowledged.
    Durable(DurableIngressSuccess),
    /// No durable ownership completed; the exact unresolved lease is returned.
    Retained(RetainedApplicationEvent<'owner, 'slots, E>),
}

impl<E: fmt::Debug> fmt::Debug for DurableIngressOutcome<'_, '_, E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Durable(success) => formatter.debug_tuple("Durable").field(success).finish(),
            Self::Retained(retained) => formatter.debug_tuple("Retained").field(retained).finish(),
        }
    }
}

/// Validate and durably own one exact application event before acknowledging it.
///
/// The mounted store is semantic state recovered from the same exact bound NOR
/// range represented by `access`. Validation borrows `lease.event()` without a
/// payload copy. A successful new commit or replay result ends every event
/// borrow before [`ApplicationEventLease::acknowledge`] releases the slot. Any
/// admission, candidate-construction, capacity, binding, backend, recovery, or
/// collision failure returns the original lease unchanged. This governs only
/// local application-event acknowledgement; it does not delay the RNS proof
/// already constructed by the current Rete ingress path.
#[allow(clippy::too_many_arguments)]
pub fn commit_application_event<'owner, 'slots, R, A, const MESSAGES: usize>(
    lease: ApplicationEventLease<'owner, 'slots>,
    local_destination: LocalDeliveryDestination,
    limits: WireLimits,
    source_identities: &R,
    stamp_policy: StampPolicy<'_>,
    store: &mut MountedLxmfStore<MESSAGES>,
    access: &mut A,
) -> DurableIngressOutcome<'owner, 'slots, A::Error>
where
    R: SourceIdentityResolver + ?Sized,
    A: BoundLxmfStoreAccess,
{
    let event_id = lease.id();
    let validated = match validate_application_event(
        lease.event(),
        local_destination,
        limits,
        source_identities,
        stamp_policy,
    ) {
        IngressOutcome::Unrelated(reason) => {
            return DurableIngressOutcome::Retained(RetainedApplicationEvent::new(
                lease,
                DurableIngressRetentionReason::Unrelated(reason),
            ));
        }
        IngressOutcome::Deferred(reason) => {
            return DurableIngressOutcome::Retained(RetainedApplicationEvent::new(
                lease,
                DurableIngressRetentionReason::Deferred(reason),
            ));
        }
        IngressOutcome::Rejected(reason) => {
            return DurableIngressOutcome::Retained(RetainedApplicationEvent::new(
                lease,
                DurableIngressRetentionReason::Rejected(reason),
            ));
        }
        IngressOutcome::Validated(validated) => validated,
    };

    let evidence = validated.evidence();
    let metadata = match metadata_from_evidence(evidence) {
        Ok(metadata) => metadata,
        Err(error) => {
            drop(validated);
            return DurableIngressOutcome::Retained(RetainedApplicationEvent::new(
                lease,
                DurableIngressRetentionReason::Candidate(error),
            ));
        }
    };
    let wire = match metadata.carrier() {
        CarrierProvenance::Opportunistic => NormalizedWire::Opportunistic {
            implied_destination: evidence.destination(),
            carrier_payload: validated.carrier_payload(),
        },
        CarrierProvenance::Complete
        | CarrierProvenance::LinkDataContextNone
        | CarrierProvenance::ResourceComplete => {
            NormalizedWire::Contiguous(validated.carrier_payload())
        }
    };
    let candidate = match InboundMessageCandidate::new(metadata, wire) {
        Ok(candidate) => candidate,
        Err(error) => {
            drop(validated);
            return DurableIngressOutcome::Retained(RetainedApplicationEvent::new(
                lease,
                DurableIngressRetentionReason::Candidate(DurableCandidateError::Candidate(error)),
            ));
        }
    };

    let committed = match store.commit(access, candidate) {
        Ok(outcome) => outcome,
        Err(failure) => {
            let (_, error) = failure.into_parts();
            drop(validated);
            return DurableIngressOutcome::Retained(RetainedApplicationEvent::new(
                lease,
                DurableIngressRetentionReason::Store(error),
            ));
        }
    };
    let (kind, receipt) = match committed {
        LxmfCommitOutcome::Committed(receipt) => (DurableIngressCommitKind::New, receipt),
        LxmfCommitOutcome::AlreadyDurable(receipt) => (DurableIngressCommitKind::Replay, receipt),
    };

    drop(validated);
    drop(lease.acknowledge());
    DurableIngressOutcome::Durable(DurableIngressSuccess {
        event_id,
        receipt,
        kind,
    })
}

fn metadata_from_evidence(
    evidence: ValidatedIngressEvidence,
) -> Result<InboundMessageMetadata, DurableCandidateError> {
    let carrier = match evidence.carrier() {
        CarrierKind::Complete => CarrierProvenance::Complete,
        CarrierKind::Opportunistic => CarrierProvenance::Opportunistic,
        CarrierKind::LinkDataContextNone => CarrierProvenance::LinkDataContextNone,
        CarrierKind::ResourceComplete => CarrierProvenance::ResourceComplete,
    };
    let stamp_admission = match evidence.stamp_admission() {
        StampAdmission::NotRequired { stamp_present } => {
            StampAdmissionProvenance::NotRequired { stamp_present }
        }
        StampAdmission::TrustedPriorTicket => StampAdmissionProvenance::TrustedPriorTicket,
        StampAdmission::ProofOfWork { target_cost, value } => {
            let target_cost = RequiredStampCost::new(target_cost.get())
                .map_err(DurableCandidateError::InvalidStampCost)?;
            StampAdmissionProvenance::ProofOfWork {
                target_cost,
                observed_value: value,
            }
        }
    };
    let lengths = InboundMessageLengths::new(
        evidence.normalized_wire_len(),
        evidence.carrier_payload_len(),
        evidence.title_len(),
        evidence.content_len(),
        evidence.fields_encoded_len(),
    )
    .map_err(DurableCandidateError::LengthOverflow)?;
    InboundMessageMetadata::new(
        MessageId::new(*evidence.message_id()),
        AuthenticatedMaterialFingerprint::new(*evidence.authenticated_material_fingerprint()),
        DestinationHash::new(*evidence.destination()),
        SourceHash::new(*evidence.source()),
        evidence.timestamp_bits(),
        carrier,
        stamp_admission,
        lengths,
    )
    .map_err(DurableCandidateError::CarrierLength)
}
