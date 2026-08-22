//! Transport-neutral parsing of ordinary LXMF carriers.
//!
//! The API follows Python LXMF: it parses borrowed Reticulum carrier bytes
//! without taking ownership and records whether the signature validated, its
//! source is unknown, or it is invalid. PRNS proof timing and application
//! delivery have already completed before this parser runs.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::fmt;

pub use reticulum_lxmf_model::SignatureVerification;
use reticulum_lxmf_wire::{
    AUTHENTICATED_MATERIAL_FINGERPRINT_LENGTH, IDENTITY_PUBLIC_KEY_LENGTH, MESSAGE_ID_LENGTH,
    WireError,
};
pub use reticulum_lxmf_wire::{CarrierIngress, CarrierKind, StampAdmission, WireLimits};

/// Caller-owned lookup for a public RNS identity announced at one destination.
///
/// The key is returned by value so no mutable PRNS state or cache borrow
/// crosses signature verification. A missing key is recorded as
/// [`SignatureVerification::SourceUnknown`]; it never delays delivery.
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

/// A structural failure to parse an LXMF carrier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RejectedIngress {
    /// Bounded carrier normalization or MessagePack validation failed.
    Wire(WireError),
}

/// Fixed Python-compatible evidence copied from one structurally parsed message.
///
/// Python LXMF delivers parsed messages even when the source identity is
/// unknown or its signature is invalid. [`SignatureVerification`] records that
/// state without turning it into application backpressure or a retained
/// Reticulum event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParsedIngressEvidence {
    message_id: [u8; MESSAGE_ID_LENGTH],
    authenticated_material_fingerprint: [u8; AUTHENTICATED_MATERIAL_FINGERPRINT_LENGTH],
    destination: [u8; 16],
    source: [u8; 16],
    signature_verification: SignatureVerification,
    timestamp_bits: u64,
    carrier: CarrierKind,
    normalized_wire_len: usize,
    carrier_payload_len: usize,
    title_len: usize,
    content_len: usize,
    fields_encoded_len: usize,
    stamp_admission: StampAdmission,
}

impl ParsedIngressEvidence {
    /// Python-compatible LXMF message ID.
    pub const fn message_id(&self) -> &[u8; MESSAGE_ID_LENGTH] {
        &self.message_id
    }

    /// Domain-separated digest of the exact LXMF signature-input material.
    pub const fn authenticated_material_fingerprint(
        &self,
    ) -> &[u8; AUTHENTICATED_MATERIAL_FINGERPRINT_LENGTH] {
        &self.authenticated_material_fingerprint
    }

    /// Local LXMF destination bound by the Reticulum carrier.
    pub const fn destination(&self) -> &[u8; 16] {
        &self.destination
    }

    /// Source `lxmf.delivery` destination claimed by the message.
    pub const fn source(&self) -> &[u8; 16] {
        &self.source
    }

    /// Python-compatible signature result observed during parsing.
    pub const fn signature_verification(&self) -> SignatureVerification {
        self.signature_verification
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

    /// Exact bytes supplied by the Reticulum application callback.
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

    /// Stamp evidence under Python LXMF's non-enforcing receiver policy.
    pub const fn stamp_admission(&self) -> StampAdmission {
        self.stamp_admission
    }
}

/// Borrowed Python-compatible parsed LXMF message.
#[must_use = "the parsed carrier remains caller-owned until explicitly disposed"]
pub struct ParsedIngress<'event> {
    carrier_payload: &'event [u8],
    evidence: ParsedIngressEvidence,
}

impl<'event> ParsedIngress<'event> {
    /// Exact carrier bytes supplied by the caller.
    pub const fn carrier_payload(&self) -> &'event [u8] {
        self.carrier_payload
    }

    /// Fixed evidence copied from the parsed message.
    pub const fn evidence(&self) -> ParsedIngressEvidence {
        self.evidence
    }
}

impl fmt::Debug for ParsedIngress<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ParsedIngress")
            .field("evidence", &self.evidence)
            .field("carrier_payload", &"<redacted>")
            .finish()
    }
}

/// Structural classification of one Python-compatible borrowed carrier.
#[must_use = "classification never disposes the caller-owned carrier"]
pub enum ParsedCarrierOutcome<'event> {
    /// Carrier normalization or MessagePack parsing failed.
    Rejected(RejectedIngress),
    /// The carrier parsed; signature state is evidence, not admission control.
    Parsed(ParsedIngress<'event>),
}

impl fmt::Debug for ParsedCarrierOutcome<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected(reason) => formatter.debug_tuple("Rejected").field(reason).finish(),
            Self::Parsed(message) => formatter.debug_tuple("Parsed").field(message).finish(),
        }
    }
}

/// Parse one carrier with Python LXMF's default inbound signature behavior.
///
/// Structurally invalid carriers are rejected. A missing source identity or an
/// invalid signature is retained in the parsed result and does not manufacture
/// deferred admission. Stamp enforcement is disabled, matching the product's
/// current Python LXMF receiver configuration.
pub fn parse_lxmf_carrier<'event, R>(
    carrier: CarrierIngress<'event>,
    limits: WireLimits,
    source_identities: &R,
) -> ParsedCarrierOutcome<'event>
where
    R: SourceIdentityResolver + ?Sized,
{
    let payload = match carrier {
        CarrierIngress::Opportunistic { payload, .. }
        | CarrierIngress::LinkDataContextNone { payload, .. }
        | CarrierIngress::ResourceComplete { payload, .. } => payload,
    };
    let parsed = match reticulum_lxmf_wire::MessageView::parse_ingress(carrier, limits) {
        Ok(message) => message,
        Err(error) => return ParsedCarrierOutcome::Rejected(RejectedIngress::Wire(error)),
    };
    let signature_verification =
        match source_identities.resolve_source_identity(parsed.source_hash()) {
            None => SignatureVerification::SourceUnknown,
            Some(public_key) => match parsed
                .bind_source_identity(&public_key)
                .and_then(|source| parsed.verify_signature(&source))
            {
                Ok(_) => SignatureVerification::Validated,
                Err(_) => SignatureVerification::Invalid,
            },
        };
    let payload_view = parsed.payload();
    ParsedCarrierOutcome::Parsed(ParsedIngress {
        carrier_payload: payload,
        evidence: ParsedIngressEvidence {
            message_id: parsed.message_id(),
            authenticated_material_fingerprint: parsed.authenticated_material_fingerprint(),
            destination: *parsed.destination_hash(),
            source: *parsed.source_hash(),
            signature_verification,
            timestamp_bits: payload_view.timestamp_bits(),
            carrier: parsed.carrier_kind(),
            normalized_wire_len: parsed.normalized_wire_len(),
            carrier_payload_len: payload.len(),
            title_len: payload_view.title().as_bytes().len(),
            content_len: payload_view.content().as_bytes().len(),
            fields_encoded_len: payload_view.fields().raw().len(),
            stamp_admission: StampAdmission::NotRequired {
                stamp_present: payload_view.stamp().is_some(),
            },
        },
    })
}
