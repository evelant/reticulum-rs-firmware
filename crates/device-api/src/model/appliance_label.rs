//! Product-owned appliance label protocol.

use crate::MAX_APPLIANCE_LABEL_BYTES;

/// Read the durable product-owned appliance label.
pub const OP_APPLIANCE_LABEL_GET: u16 = 0x0004;
/// Compare-and-swap the durable product-owned appliance label.
pub const OP_APPLIANCE_LABEL_MUTATE: u16 = 0x0005;

/// Borrowed, validated appliance label.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApplianceLabel<'a>(&'a str);

impl<'a> ApplianceLabel<'a> {
    /// Validate one non-empty, single-line UTF-8 label.
    pub fn new(label: &'a str) -> Result<Self, InvalidApplianceLabel> {
        if label.is_empty() {
            return Err(InvalidApplianceLabel::Empty);
        }
        if label.len() > MAX_APPLIANCE_LABEL_BYTES {
            return Err(InvalidApplianceLabel::TooLong {
                actual: label.len(),
                maximum: MAX_APPLIANCE_LABEL_BYTES,
            });
        }
        if label
            .chars()
            .any(|character| character.is_control() || matches!(character, '\u{2028}' | '\u{2029}'))
        {
            return Err(InvalidApplianceLabel::UnsupportedCharacter);
        }
        Ok(Self(label))
    }

    /// Borrow the validated UTF-8 label.
    pub const fn as_str(self) -> &'a str {
        self.0
    }
}

/// Appliance-label validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidApplianceLabel {
    /// An explicitly configured label cannot be empty.
    Empty,
    /// The encoded UTF-8 label exceeds the protocol bound.
    TooLong {
        /// Encoded UTF-8 byte length supplied by the caller.
        actual: usize,
        /// Maximum encoded UTF-8 byte length.
        maximum: usize,
    },
    /// The label contains a control character and is not suitable for display.
    UnsupportedCharacter,
}

/// Owned bounded appliance label used by response snapshots.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct ApplianceLabelSummary {
    bytes: [u8; MAX_APPLIANCE_LABEL_BYTES],
    len: u8,
}

impl ApplianceLabelSummary {
    /// Copy one validated appliance label into the bounded response form.
    pub fn new(label: &str) -> Result<Self, InvalidApplianceLabel> {
        let label = ApplianceLabel::new(label)?;
        let mut bytes = [0; MAX_APPLIANCE_LABEL_BYTES];
        bytes[..label.as_str().len()].copy_from_slice(label.as_str().as_bytes());
        Ok(Self {
            bytes,
            len: label.as_str().len() as u8,
        })
    }

    /// Borrow the label as UTF-8.
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..usize::from(self.len)])
            .expect("ApplianceLabelSummary is constructed from UTF-8")
    }
}

impl core::fmt::Debug for ApplianceLabelSummary {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_tuple("ApplianceLabelSummary")
            .field(&self.as_str())
            .finish()
    }
}

/// Current durable appliance-label state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApplianceLabelSnapshot {
    /// Monotonic durable revision, or zero on erased media.
    pub revision: u64,
    /// Optional user-selected product label.
    pub label: Option<ApplianceLabelSummary>,
}

impl ApplianceLabelSnapshot {
    /// Construct one durable appliance-label snapshot.
    pub const fn new(revision: u64, label: Option<ApplianceLabelSummary>) -> Self {
        Self { revision, label }
    }
}

/// Compare-and-swap request for the product-owned appliance label.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApplianceLabelMutationRequest<'a> {
    /// Revision observed by the client before editing.
    pub expected_revision: u64,
    /// Replacement label, or `None` to restore the identity-derived fallback.
    pub label: Option<ApplianceLabel<'a>>,
}

impl<'a> ApplianceLabelMutationRequest<'a> {
    /// Construct one compare-and-swap label mutation.
    pub const fn new(expected_revision: u64, label: Option<ApplianceLabel<'a>>) -> Self {
        Self {
            expected_revision,
            label,
        }
    }
}

/// Durable appliance-label mutation outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplianceLabelMutationOutcome {
    /// The requested label is durable at this revision.
    Applied {
        /// Current durable revision after the mutation.
        revision: u64,
    },
    /// Another client changed the record after the caller read it.
    RevisionConflict {
        /// Current durable revision to refresh before retrying.
        current_revision: u64,
    },
}
