//! App-facing NomadNet fetch requests and projections.

use reticulum_device_api as api;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{
    ClientRequestError, JsonSafeInteger, deserialize_json_safe_u64, parse_destination, parse_hex,
};

/// App-facing request to begin one bounded anonymous NomadNet page fetch.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, TS)]
#[allow(missing_docs)]
pub struct NomadFetchStartRequest {
    destination: String,
    path: String,
    #[serde(deserialize_with = "deserialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    timestamp_unix_ms: u64,
    idempotency_key: String,
}

impl NomadFetchStartRequest {
    /// Validate all bounded device semantics without starting a fetch.
    pub fn validate(&self) -> Result<(), ClientRequestError> {
        self.as_device_request().map(|_| ())
    }

    pub(crate) fn as_device_request(
        &self,
    ) -> Result<api::NomadFetchStartRequest<'_>, ClientRequestError> {
        let destination = parse_destination(&self.destination)?;
        let path = api::NomadPagePath::new(&self.path).map_err(|error| match error {
            api::InvalidNomadPagePath::Invalid => ClientRequestError::InvalidNomadPath,
            api::InvalidNomadPagePath::TooLong { .. } => ClientRequestError::NomadPathTooLong,
        })?;
        let timestamp = api::NomadRequestTimestampUnixMs::new(self.timestamp_unix_ms)
            .map_err(|_| ClientRequestError::InvalidTimestamp)?;
        let idempotency_key = parse_hex::<16>(&self.idempotency_key)
            .map(api::IdempotencyKey)
            .ok_or(ClientRequestError::InvalidIdempotencyKey)?;
        Ok(api::NomadFetchStartRequest::new(
            api::DestinationHash(*destination.as_bytes()),
            path,
            timestamp,
            idempotency_key,
        ))
    }
}

/// App-facing request to poll one boot-scoped NomadNet fetch.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, TS)]
#[allow(missing_docs)]
pub struct NomadFetchPollRequest {
    id: String,
}

impl NomadFetchPollRequest {
    /// Validate the complete opaque boot-scoped fetch identifier.
    pub fn validate(&self) -> Result<(), ClientRequestError> {
        self.as_device_id().map(|_| ())
    }

    pub(crate) fn as_device_id(&self) -> Result<api::NomadFetchId, ClientRequestError> {
        parse_hex::<16>(&self.id)
            .and_then(|bytes| api::NomadFetchId::from_bytes(bytes).ok())
            .ok_or(ClientRequestError::InvalidNomadFetchId)
    }
}

/// Whether a successful fetch start was newly accepted or replayed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum NomadFetchStartOutcome {
    Accepted,
    Replayed,
}

/// App-facing acceptance for one NomadNet fetch.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[allow(missing_docs)]
pub struct NomadFetchStartResponse {
    id: String,
    outcome: NomadFetchStartOutcome,
}

impl From<api::NomadFetchStartAccepted> for NomadFetchStartResponse {
    fn from(accepted: api::NomadFetchStartAccepted) -> Self {
        Self {
            id: hex::encode(accepted.id.as_bytes()),
            outcome: match accepted.outcome {
                api::NomadFetchStartOutcome::Accepted => NomadFetchStartOutcome::Accepted,
                api::NomadFetchStartOutcome::Replayed => NomadFetchStartOutcome::Replayed,
            },
        }
    }
}

/// Non-terminal progress for an app-facing NomadNet fetch.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum NomadFetchPhase {
    PathLookup,
    LinkEstablishment,
    RequestPreparation,
    AwaitingDispatchConfirmation,
    AwaitingResponse,
}

/// Terminal app-facing NomadNet fetch failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum NomadFetchFailure {
    NoPath,
    Link,
    Request,
    Timeout,
    PageTooLarge,
    InvalidUtf8,
    Internal,
}

/// App-facing state returned by polling one NomadNet fetch.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum NomadFetchPollResponse {
    Pending { phase: NomadFetchPhase },
    Ready { page: String },
    Failed { failure: NomadFetchFailure },
}

impl From<api::NomadFetchPollResponse> for NomadFetchPollResponse {
    fn from(response: api::NomadFetchPollResponse) -> Self {
        match response {
            api::NomadFetchPollResponse::Pending(phase) => Self::Pending {
                phase: match phase {
                    api::NomadFetchPhase::PathLookup => NomadFetchPhase::PathLookup,
                    api::NomadFetchPhase::LinkEstablishment => NomadFetchPhase::LinkEstablishment,
                    api::NomadFetchPhase::RequestPreparation => NomadFetchPhase::RequestPreparation,
                    api::NomadFetchPhase::AwaitingDispatchConfirmation => {
                        NomadFetchPhase::AwaitingDispatchConfirmation
                    }
                    api::NomadFetchPhase::AwaitingResponse => NomadFetchPhase::AwaitingResponse,
                },
            },
            api::NomadFetchPollResponse::Ready(page) => Self::Ready {
                page: page.as_str().to_owned(),
            },
            api::NomadFetchPollResponse::Failed(failure) => Self::Failed {
                failure: match failure {
                    api::NomadFetchFailure::NoPath => NomadFetchFailure::NoPath,
                    api::NomadFetchFailure::Link => NomadFetchFailure::Link,
                    api::NomadFetchFailure::Request => NomadFetchFailure::Request,
                    api::NomadFetchFailure::Timeout => NomadFetchFailure::Timeout,
                    api::NomadFetchFailure::PageTooLarge => NomadFetchFailure::PageTooLarge,
                    api::NomadFetchFailure::InvalidUtf8 => NomadFetchFailure::InvalidUtf8,
                    api::NomadFetchFailure::Internal => NomadFetchFailure::Internal,
                },
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start_request(path: &str, timestamp_unix_ms: u64) -> NomadFetchStartRequest {
        serde_json::from_value(serde_json::json!({
            "destination": "11".repeat(16),
            "path": path,
            "timestamp_unix_ms": timestamp_unix_ms,
            "idempotency_key": "22".repeat(16),
        }))
        .unwrap()
    }

    #[test]
    fn start_request_validates_and_preserves_exact_device_semantics() {
        let request = start_request("/page/index.mu", 1_784_732_100_001);
        let device = request.as_device_request().unwrap();
        assert_eq!(device.destination(), api::DestinationHash([0x11; 16]));
        assert_eq!(device.path().as_str(), "/page/index.mu");
        assert_eq!(device.timestamp_unix_ms().get(), 1_784_732_100_001);
        assert_eq!(device.idempotency_key(), api::IdempotencyKey([0x22; 16]));

        assert_eq!(
            start_request("relative", 1).as_device_request(),
            Err(ClientRequestError::InvalidNomadPath)
        );
        assert_eq!(
            start_request(
                &format!("/{}", "x".repeat(api::MAX_NOMAD_PAGE_PATH_BYTES)),
                1,
            )
            .as_device_request(),
            Err(ClientRequestError::NomadPathTooLong)
        );
        assert_eq!(
            start_request("/", 0).as_device_request(),
            Err(ClientRequestError::InvalidTimestamp)
        );
    }

    #[test]
    fn poll_request_rejects_malformed_and_zero_sequence_ids() {
        let valid = api::NomadFetchId::new([0x33; 8], 7).unwrap();
        let request: NomadFetchPollRequest = serde_json::from_value(serde_json::json!({
            "id": hex::encode(valid.as_bytes()),
        }))
        .unwrap();
        assert_eq!(request.as_device_id(), Ok(valid));

        for id in ["not-hex".to_owned(), "00".repeat(16)] {
            let request: NomadFetchPollRequest =
                serde_json::from_value(serde_json::json!({ "id": id })).unwrap();
            assert_eq!(
                request.as_device_id(),
                Err(ClientRequestError::InvalidNomadFetchId)
            );
        }
    }

    #[test]
    fn device_results_map_to_stable_tagged_json() {
        let id = api::NomadFetchId::new([0x44; 8], 9).unwrap();
        assert_eq!(
            serde_json::to_value(NomadFetchStartResponse::from(
                api::NomadFetchStartAccepted {
                    id,
                    outcome: api::NomadFetchStartOutcome::Replayed,
                }
            ))
            .unwrap(),
            serde_json::json!({
                "id": hex::encode(id.as_bytes()),
                "outcome": "replayed",
            })
        );
        assert_eq!(
            serde_json::to_value(NomadFetchPollResponse::from(
                api::NomadFetchPollResponse::Pending(api::NomadFetchPhase::AwaitingResponse)
            ))
            .unwrap(),
            serde_json::json!({
                "state": "pending",
                "phase": "awaiting_response",
            })
        );
        assert_eq!(
            serde_json::to_value(NomadFetchPollResponse::from(
                api::NomadFetchPollResponse::Ready(api::NomadPage::new(b">Metalbeard").unwrap())
            ))
            .unwrap(),
            serde_json::json!({
                "state": "ready",
                "page": ">Metalbeard",
            })
        );
        assert_eq!(
            serde_json::to_value(NomadFetchPollResponse::from(
                api::NomadFetchPollResponse::Failed(api::NomadFetchFailure::NoPath)
            ))
            .unwrap(),
            serde_json::json!({
                "state": "failed",
                "failure": "no_path",
            })
        );
    }
}
