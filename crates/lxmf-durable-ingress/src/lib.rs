//! Durable ownership of an ordinary borrowed LXMF carrier.
//!
//! This handoff follows Python LXMF parsing semantics, streams normalized bytes
//! into the mounted store, and records signature verification as durable
//! metadata. Reticulum delivery and immediate proof emission are already
//! complete before this application boundary runs.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use reticulum_lxmf_ingress::{
    CarrierIngress, CarrierKind, ParsedCarrierOutcome, ParsedIngressEvidence, RejectedIngress,
    SourceIdentityResolver, StampAdmission, WireLimits, parse_lxmf_carrier,
};
use reticulum_lxmf_model::{
    AuthenticatedMaterialFingerprint, CandidateError, CarrierLengthMismatch, CarrierProvenance,
    DestinationHash, DurableMessageReceipt, InboundMessageCandidate, InboundMessageLengths,
    InboundMessageMetadata, InboundTransportObservation, MessageId, MessageLengthOverflow,
    NormalizedWire, SourceHash, StampAdmissionProvenance,
};
use reticulum_lxmf_store::{
    BoundLxmfStoreAccess, LxmfCommitError, LxmfCommitOutcome, MountedLxmfStore,
};

/// Whether this carrier created a durable message or replayed one already owned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DurableIngressCommitKind {
    /// Exact message bytes and metadata became durable during this handoff.
    New,
    /// The authenticated logical message was already durable.
    Replay,
}

/// Successful durable ownership transfer for one transport-neutral carrier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DurableCarrierSuccess {
    receipt: DurableMessageReceipt,
    kind: DurableIngressCommitKind,
}

impl DurableCarrierSuccess {
    /// Stable receipt for the durably owned logical message.
    pub const fn receipt(self) -> DurableMessageReceipt {
        self.receipt
    }

    /// Whether this handoff committed new durable state or observed a replay.
    pub const fn kind(self) -> DurableIngressCommitKind {
        self.kind
    }
}

/// Failure while translating parsed ingress evidence into a durable candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DurableCandidateError {
    /// One exact host-size length did not fit the portable durable model.
    LengthOverflow(MessageLengthOverflow),
    /// Carrier provenance contradicted normalized and physical byte counts.
    CarrierLength(CarrierLengthMismatch),
    /// Borrowed exact bytes contradicted their immutable metadata.
    Candidate(CandidateError),
}

/// Why a carrier did not transfer to durable ownership.
#[derive(Debug)]
pub enum DurableCarrierRetentionReason<E> {
    /// Carrier normalization or MessagePack parsing failed.
    Rejected(RejectedIngress),
    /// Parsed evidence could not form a self-consistent durable candidate.
    Candidate(DurableCandidateError),
    /// The mounted durable store blocked or failed without committing the carrier.
    Store(LxmfCommitError<E>),
}

/// Complete durable-ingress outcome for an ordinary borrowed LXMF carrier.
///
/// A retained result never disposes the caller's owned payload. PRNS proof and
/// delivery behavior has already completed independently of this operation.
#[must_use = "a retained outcome leaves the caller's carrier payload undisposed"]
#[derive(Debug)]
pub enum DurableCarrierOutcome<E> {
    /// Store ownership completed or the authenticated message was already durable.
    Durable(DurableCarrierSuccess),
    /// The caller still owns the carrier and decides whether to retry or discard it.
    Retained(DurableCarrierRetentionReason<E>),
}

/// Parse and durably own one carrier with Python LXMF's inbound semantics.
///
/// Structurally valid messages commit immediately whether their signature is
/// validated, source-unknown, or invalid. That state is durable application
/// metadata; it never becomes a retained PRNS event or delayed proof.
pub fn commit_parsed_carrier<R, A>(
    carrier: CarrierIngress<'_>,
    ingress: Option<InboundTransportObservation>,
    limits: WireLimits,
    source_identities: &R,
    store: &mut MountedLxmfStore<'_>,
    access: &mut A,
) -> DurableCarrierOutcome<A::Error>
where
    R: SourceIdentityResolver + ?Sized,
    A: BoundLxmfStoreAccess,
{
    let parsed = match parse_lxmf_carrier(carrier, limits, source_identities) {
        ParsedCarrierOutcome::Rejected(reason) => {
            return DurableCarrierOutcome::Retained(DurableCarrierRetentionReason::Rejected(
                reason,
            ));
        }
        ParsedCarrierOutcome::Parsed(parsed) => parsed,
    };
    let evidence = parsed.evidence();
    let metadata = match metadata_from_parsed_evidence(evidence, ingress) {
        Ok(metadata) => metadata,
        Err(error) => {
            return DurableCarrierOutcome::Retained(DurableCarrierRetentionReason::Candidate(
                error,
            ));
        }
    };
    let wire = match evidence.carrier() {
        CarrierKind::Opportunistic => NormalizedWire::Opportunistic {
            implied_destination: evidence.destination(),
            carrier_payload: parsed.carrier_payload(),
        },
        CarrierKind::Complete
        | CarrierKind::LinkDataContextNone
        | CarrierKind::ResourceComplete => NormalizedWire::Contiguous(parsed.carrier_payload()),
    };
    let candidate = match InboundMessageCandidate::new(metadata, wire) {
        Ok(candidate) => candidate,
        Err(error) => {
            return DurableCarrierOutcome::Retained(DurableCarrierRetentionReason::Candidate(
                DurableCandidateError::Candidate(error),
            ));
        }
    };
    let committed = match store.commit(access, candidate) {
        Ok(outcome) => outcome,
        Err(failure) => {
            let (_, error) = failure.into_parts();
            return DurableCarrierOutcome::Retained(DurableCarrierRetentionReason::Store(error));
        }
    };
    let (kind, receipt) = commit_parts(committed);
    DurableCarrierOutcome::Durable(DurableCarrierSuccess { receipt, kind })
}

fn commit_parts(outcome: LxmfCommitOutcome) -> (DurableIngressCommitKind, DurableMessageReceipt) {
    match outcome {
        LxmfCommitOutcome::Committed(receipt) => (DurableIngressCommitKind::New, receipt),
        LxmfCommitOutcome::AlreadyDurable(receipt) => (DurableIngressCommitKind::Replay, receipt),
    }
}

fn metadata_from_parsed_evidence(
    evidence: ParsedIngressEvidence,
    ingress: Option<InboundTransportObservation>,
) -> Result<InboundMessageMetadata, DurableCandidateError> {
    let carrier = match evidence.carrier() {
        CarrierKind::Complete => CarrierProvenance::Complete,
        CarrierKind::Opportunistic => CarrierProvenance::Opportunistic,
        CarrierKind::LinkDataContextNone => CarrierProvenance::LinkDataContextNone,
        CarrierKind::ResourceComplete => CarrierProvenance::ResourceComplete,
    };
    let StampAdmission::NotRequired { stamp_present } = evidence.stamp_admission() else {
        unreachable!("Python-compatible parsing never enforces a receiver stamp")
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
        evidence.signature_verification(),
        evidence.timestamp_bits(),
        carrier,
        StampAdmissionProvenance::NotRequired { stamp_present },
        lengths,
    )
    .map(|metadata| metadata.with_ingress_observation(ingress))
    .map_err(DurableCandidateError::CarrierLength)
}
