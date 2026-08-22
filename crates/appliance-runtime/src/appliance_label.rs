//! App-facing product-owned appliance label.

use std::fmt;

use reticulum_device_api as device_api;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{JsonSafeInteger, deserialize_json_safe_u64, serialize_json_safe_u64};

/// Current durable appliance-label state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct ApplianceLabelView {
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    revision: u64,
    label: Option<String>,
}

impl ApplianceLabelView {
    /// Durable settings revision.
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Optional user-selected appliance label.
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }
}

impl From<device_api::ApplianceLabelSnapshot> for ApplianceLabelView {
    fn from(snapshot: device_api::ApplianceLabelSnapshot) -> Self {
        Self {
            revision: snapshot.revision,
            label: snapshot.label.map(|label| label.as_str().to_owned()),
        }
    }
}

/// Compare-and-swap request for the product-owned appliance label.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, TS)]
pub struct ApplianceLabelMutationRequest {
    #[serde(deserialize_with = "deserialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    expected_revision: u64,
    label: Option<String>,
}

impl ApplianceLabelMutationRequest {
    /// Replacement label requested by the app, or `None` to clear it.
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// Validate the request and invoke a borrowed device-API mutation.
    pub fn with_device_request<T>(
        self,
        invoke: impl FnOnce(device_api::ApplianceLabelMutationRequest<'_>) -> T,
    ) -> Result<T, ApplianceLabelRequestError> {
        let label = self
            .label
            .as_deref()
            .map(device_api::ApplianceLabel::new)
            .transpose()
            .map_err(|_| ApplianceLabelRequestError::InvalidLabel)?;
        Ok(invoke(device_api::ApplianceLabelMutationRequest::new(
            self.expected_revision,
            label,
        )))
    }
}

/// Safe app-facing validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplianceLabelRequestError {
    /// The label is empty, too long, or contains a control character.
    InvalidLabel,
}

impl fmt::Display for ApplianceLabelRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "appliance label must contain between 1 and 32 UTF-8 bytes without control characters",
        )
    }
}

impl std::error::Error for ApplianceLabelRequestError {}

/// Durable appliance-label mutation outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ApplianceLabelMutationOutcome {
    /// The requested value is durable at this revision.
    Applied {
        /// Current durable revision.
        #[serde(serialize_with = "serialize_json_safe_u64")]
        #[ts(as = "JsonSafeInteger")]
        revision: u64,
    },
    /// The caller edited a stale revision.
    RevisionConflict {
        /// Current durable revision to refresh before retrying.
        #[serde(serialize_with = "serialize_json_safe_u64")]
        #[ts(as = "JsonSafeInteger")]
        current_revision: u64,
    },
}

impl From<device_api::ApplianceLabelMutationOutcome> for ApplianceLabelMutationOutcome {
    fn from(outcome: device_api::ApplianceLabelMutationOutcome) -> Self {
        match outcome {
            device_api::ApplianceLabelMutationOutcome::Applied { revision } => {
                Self::Applied { revision }
            }
            device_api::ApplianceLabelMutationOutcome::RevisionConflict { current_revision } => {
                Self::RevisionConflict { current_revision }
            }
        }
    }
}
