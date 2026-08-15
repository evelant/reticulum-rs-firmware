//! Transport-neutral admission of LXMF application events.
//!
//! This crate joins the project-owned application-event boundary to the
//! allocation-free LXMF wire validator without taking ownership of either the
//! event or its payload. The caller remains solely responsible for retaining,
//! durably committing, explicitly discarding, or quarantining the event.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::fmt;

use reticulum_lxmf_wire::{
    AUTHENTICATED_MATERIAL_FINGERPRINT_LENGTH, CarrierIngress, IDENTITY_PUBLIC_KEY_LENGTH,
    MESSAGE_ID_LENGTH, SignatureError, StampError, StampPolicyError, WireError,
};
pub use reticulum_lxmf_wire::{
    CarrierKind, RequiredStampCost, StampAdmission, StampPolicy, WireLimits,
};
use reticulum_node_core::{
    APPLICATION_LINK_CONTEXT_NONE, ApplicationEvent, ApplicationEventKind, ApplicationLinkRole,
};

/// Complete hash of the one locally owned `lxmf.delivery` destination admitted
/// by an ingress instance.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LocalDeliveryDestination([u8; 16]);

impl LocalDeliveryDestination {
    /// Construct an owned local delivery destination from all hash bytes.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Borrow the complete destination hash.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Caller-owned lookup for a public RNS identity announced at one destination.
///
/// The key is returned by value so no mutable Reticulum node state or cache
/// borrow crosses signature verification. A missing key is a deferred outcome,
/// not evidence that the message is invalid.
pub trait SourceIdentityResolver {
    /// Recall the complete 64-byte RNS public key for `source_destination`.
    fn resolve_source_identity(
        &self,
        source_destination: &[u8; 16],
    ) -> Option<[u8; IDENTITY_PUBLIC_KEY_LENGTH]>;
}

impl<F> SourceIdentityResolver for F
where
    F: Fn(&[u8; 16]) -> Option<[u8; IDENTITY_PUBLIC_KEY_LENGTH]>,
{
    fn resolve_source_identity(
        &self,
        source_destination: &[u8; 16],
    ) -> Option<[u8; IDENTITY_PUBLIC_KEY_LENGTH]> {
        self(source_destination)
    }
}

/// Why an application event does not belong to this LXMF delivery destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnrelatedEvent {
    /// Destination DATA was addressed to another local application service.
    OtherDestination {
        /// Destination owned by this ingress instance.
        expected: LocalDeliveryDestination,
        /// Destination carried by the application event.
        actual: [u8; 16],
    },
    /// Link DATA uses an RNS context other than ordinary application bytes and
    /// therefore belongs to another Link service.
    OtherLinkContext {
        /// Reticulum Link identifier.
        link: [u8; 16],
        /// Non-NONE RNS Link DATA context.
        context: u8,
    },
    /// Link DATA arrived on a locally initiated Link and therefore belongs to
    /// the local client for that remote destination, not a local delivery
    /// service.
    OtherLinkRole {
        /// Reticulum Link identifier.
        link: [u8; 16],
        /// Role retained by the authenticated Link.
        role: ApplicationLinkRole,
    },
    /// This event kind is not an LXMF complete-message carrier.
    OtherKind(ApplicationEventKind),
}

/// Complete-message carrier deliberately not admitted by this tranche.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsupportedCarrier {
    /// Completed Resources need the same Link destination binding, and native
    /// Resource ingress remains disabled until bounded admission is qualified.
    ResourceComplete {
        /// Reticulum Link identifier.
        link: [u8; 16],
        /// Truncated Resource hash.
        resource_hash: [u8; 16],
    },
}

/// Why a possibly valid LXMF event must remain owned for later work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeferredIngress {
    /// The source destination parsed correctly, but its announced public key is
    /// not currently available through the caller's identity authority.
    SourceIdentityUnavailable {
        /// Parsed `lxmf.delivery` source destination hash.
        source: [u8; 16],
    },
    /// Stamp calculation could not complete under the caller's parameters or
    /// work budget. No stamp-validity decision was made.
    StampValidationUnavailable(StampError),
    /// The wire crate supports this complete-message carrier, but its bounded
    /// native ownership path is not enabled. This classification does not
    /// establish LXMF ownership: a global dispatcher must retain or quarantine
    /// the event until that carrier has a qualified application boundary.
    UnsupportedCarrier(UnsupportedCarrier),
}

impl DeferredIngress {
    /// Whether a change in the identity cache alone can make the same event
    /// immediately admissible under the same product configuration.
    pub const fn retryable_after_identity_update(self) -> bool {
        matches!(self, Self::SourceIdentityUnavailable { .. })
    }
}

/// A fully classified failure to authenticate or admit an LXMF message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RejectedIngress {
    /// Bounded carrier normalization or MessagePack validation failed.
    Wire(WireError),
    /// Source binding or Ed25519 verification failed.
    Signature(SignatureError),
    /// Receiver-owned destination or stamp policy rejected the message.
    StampPolicy(StampPolicyError),
}

/// Fixed scalar evidence copied from one fully validated message.
///
/// This value is useful for correlation and a future semantic durable model.
/// It is not, by itself, a durable record and never substitutes for retaining
/// the exact signed message bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValidatedIngressEvidence {
    message_id: [u8; MESSAGE_ID_LENGTH],
    authenticated_material_fingerprint: [u8; AUTHENTICATED_MATERIAL_FINGERPRINT_LENGTH],
    destination: [u8; 16],
    source: [u8; 16],
    timestamp_bits: u64,
    carrier: CarrierKind,
    normalized_wire_len: usize,
    carrier_payload_len: usize,
    title_len: usize,
    content_len: usize,
    fields_encoded_len: usize,
    stamp_admission: StampAdmission,
}

impl ValidatedIngressEvidence {
    /// Python-compatible LXMF message ID.
    pub const fn message_id(&self) -> &[u8; MESSAGE_ID_LENGTH] {
        &self.message_id
    }

    /// Domain-separated digest of the exact destination, source, and
    /// Python-compatible payload-without-stamp bytes authenticated by the
    /// message signature.
    ///
    /// Durable stores use this independently of the protocol message ID when
    /// distinguishing a replay (including a different valid stamp) from a
    /// theoretical same-ID/different-preimage collision.
    pub const fn authenticated_material_fingerprint(
        &self,
    ) -> &[u8; AUTHENTICATED_MATERIAL_FINGERPRINT_LENGTH] {
        &self.authenticated_material_fingerprint
    }

    /// Local LXMF destination hash authenticated by the carrier and signature.
    pub const fn destination(&self) -> &[u8; 16] {
        &self.destination
    }

    /// Authenticated source `lxmf.delivery` destination hash.
    pub const fn source(&self) -> &[u8; 16] {
        &self.source
    }

    /// Exact IEEE-754 timestamp bits retained from MessagePack.
    pub const fn timestamp_bits(&self) -> u64 {
        self.timestamp_bits
    }

    /// Carrier kind established during normalization.
    pub const fn carrier(&self) -> CarrierKind {
        self.carrier
    }

    /// Complete normalized wire length, including the implied destination.
    pub const fn normalized_wire_len(&self) -> usize {
        self.normalized_wire_len
    }

    /// Exact payload bytes retained by the application event.
    pub const fn carrier_payload_len(&self) -> usize {
        self.carrier_payload_len
    }

    /// Decoded binary title length.
    pub const fn title_len(&self) -> usize {
        self.title_len
    }

    /// Decoded binary content length.
    pub const fn content_len(&self) -> usize {
        self.content_len
    }

    /// Exact encoded MessagePack fields-map length.
    pub const fn fields_encoded_len(&self) -> usize {
        self.fields_encoded_len
    }

    /// Receiver-owned stamp-policy evidence used for admission.
    pub const fn stamp_admission(&self) -> StampAdmission {
        self.stamp_admission
    }
}

/// Borrowed validated LXMF message and its fixed scalar evidence.
///
/// The view borrows the exact `Vec` inside an [`ApplicationEvent`]. It cannot
/// outlive that event and performs no payload copy or ownership transition.
#[must_use = "the validated event remains caller-owned until explicitly disposed"]
pub struct ValidatedIngress<'event> {
    carrier_payload: &'event [u8],
    evidence: ValidatedIngressEvidence,
}

impl<'event> ValidatedIngress<'event> {
    /// Exact admitted carrier payload owned by the application event.
    ///
    /// The branded borrow is constructed only after complete wire, source,
    /// signature, destination, and stamp-policy validation. Reparse it through
    /// `reticulum-lxmf-wire` when detailed borrowed fields are needed; this
    /// ingress boundary deliberately does not retain that crate's larger parser
    /// typestate in every outcome value.
    pub const fn carrier_payload(&self) -> &'event [u8] {
        self.carrier_payload
    }

    /// Fixed copied correlation evidence.
    pub const fn evidence(&self) -> ValidatedIngressEvidence {
        self.evidence
    }
}

impl fmt::Debug for ValidatedIngress<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ValidatedIngress")
            .field("evidence", &self.evidence)
            .field("carrier_payload", &"<redacted>")
            .finish()
    }
}

/// Complete classification of one borrowed application event.
#[must_use = "classification never disposes the caller-owned application event"]
pub enum IngressOutcome<'event> {
    /// Event belongs to another application consumer.
    Unrelated(UnrelatedEvent),
    /// Event may be valid but needs identity state or an unimplemented bounded
    /// carrier adapter; retain or quarantine it explicitly.
    Deferred(DeferredIngress),
    /// Event was conclusively rejected under the supplied validation policy.
    Rejected(RejectedIngress),
    /// Event is a fully validated admitted LXMF complete-message carrier.
    Validated(ValidatedIngress<'event>),
}

impl fmt::Debug for IngressOutcome<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unrelated(reason) => formatter.debug_tuple("Unrelated").field(reason).finish(),
            Self::Deferred(reason) => formatter.debug_tuple("Deferred").field(reason).finish(),
            Self::Rejected(reason) => formatter.debug_tuple("Rejected").field(reason).finish(),
            Self::Validated(message) => formatter.debug_tuple("Validated").field(message).finish(),
        }
    }
}

/// Borrow and validate one application event without consuming or copying its
/// payload.
///
/// Opportunistic destination DATA and responder-side context-NONE Link DATA
/// addressed to `local_destination` are admitted. Link ownership is established
/// by the event's opaque Rete-derived Link binding before the complete LXMF wire
/// is parsed; the wire destination must independently agree with that binding.
/// The source resolver is consulted only after bounded structural parsing
/// exposes the exact source destination. Non-NONE and initiator-side Link DATA
/// are unrelated. Resource completion remains explicitly deferred because
/// bounded native Resource ownership is not enabled. Proof-of-work policies
/// execute the wire crate's synchronous protocol work; callers must schedule
/// such policies outside the sole network actor.
pub fn validate_application_event<'event, R>(
    event: &'event ApplicationEvent,
    local_destination: LocalDeliveryDestination,
    limits: WireLimits,
    source_identities: &R,
    stamp_policy: StampPolicy<'_>,
) -> IngressOutcome<'event>
where
    R: SourceIdentityResolver + ?Sized,
{
    let (destination, payload, expected_carrier) = match event {
        ApplicationEvent::DataReceived {
            destination,
            payload,
            ..
        } => (destination, payload.as_slice(), CarrierKind::Opportunistic),
        ApplicationEvent::LinkData {
            binding, context, ..
        } if *context != APPLICATION_LINK_CONTEXT_NONE => {
            return IngressOutcome::Unrelated(UnrelatedEvent::OtherLinkContext {
                link: *binding.link(),
                context: *context,
            });
        }
        ApplicationEvent::LinkData { binding, data, .. } => match binding.role() {
            ApplicationLinkRole::Initiator => {
                return IngressOutcome::Unrelated(UnrelatedEvent::OtherLinkRole {
                    link: *binding.link(),
                    role: ApplicationLinkRole::Initiator,
                });
            }
            ApplicationLinkRole::Responder => (
                binding.destination(),
                data.as_slice(),
                CarrierKind::LinkDataContextNone,
            ),
        },
        ApplicationEvent::ResourceComplete {
            link,
            resource_hash,
            ..
        } => {
            return IngressOutcome::Deferred(DeferredIngress::UnsupportedCarrier(
                UnsupportedCarrier::ResourceComplete {
                    link: *link,
                    resource_hash: *resource_hash,
                },
            ));
        }
        other => {
            return IngressOutcome::Unrelated(UnrelatedEvent::OtherKind(other.kind()));
        }
    };

    if destination != local_destination.as_bytes() {
        return IngressOutcome::Unrelated(UnrelatedEvent::OtherDestination {
            expected: local_destination,
            actual: *destination,
        });
    }

    let carrier = match expected_carrier {
        CarrierKind::Opportunistic => CarrierIngress::Opportunistic {
            implied_destination: destination,
            payload,
        },
        CarrierKind::LinkDataContextNone => CarrierIngress::LinkDataContextNone {
            expected_destination: destination,
            payload,
        },
        CarrierKind::Complete | CarrierKind::ResourceComplete => {
            unreachable!("application event matching only selects admitted carriers")
        }
    };
    let parsed = match reticulum_lxmf_wire::MessageView::parse_ingress(carrier, limits) {
        Ok(message) => message,
        Err(error) => return IngressOutcome::Rejected(RejectedIngress::Wire(error)),
    };

    let Some(public_key) = source_identities.resolve_source_identity(parsed.source_hash()) else {
        return IngressOutcome::Deferred(DeferredIngress::SourceIdentityUnavailable {
            source: *parsed.source_hash(),
        });
    };
    let bound_source = match parsed.bind_source_identity(&public_key) {
        Ok(source) => source,
        Err(error) => return IngressOutcome::Rejected(RejectedIngress::Signature(error)),
    };
    let signature_verified = match parsed.verify_signature(&bound_source) {
        Ok(message) => message,
        Err(error) => return IngressOutcome::Rejected(RejectedIngress::Signature(error)),
    };
    let message = match signature_verified.apply_stamp_policy(stamp_policy) {
        Ok(message) => message,
        Err(StampPolicyError::Validation(error)) => {
            return IngressOutcome::Deferred(DeferredIngress::StampValidationUnavailable(error));
        }
        Err(error) => return IngressOutcome::Rejected(RejectedIngress::StampPolicy(error)),
    };
    let view = message.message();
    let payload_view = view.payload();
    let evidence = ValidatedIngressEvidence {
        message_id: view.message_id(),
        authenticated_material_fingerprint: view.authenticated_material_fingerprint(),
        destination: *view.destination_hash(),
        source: *view.source_hash(),
        timestamp_bits: payload_view.timestamp_bits(),
        carrier: view.carrier_kind(),
        normalized_wire_len: view.normalized_wire_len(),
        carrier_payload_len: payload.len(),
        title_len: payload_view.title().as_bytes().len(),
        content_len: payload_view.content().as_bytes().len(),
        fields_encoded_len: payload_view.fields().raw().len(),
        stamp_admission: message.stamp_admission(),
    };
    debug_assert_eq!(evidence.carrier, expected_carrier);
    IngressOutcome::Validated(ValidatedIngress {
        carrier_payload: payload,
        evidence,
    })
}
