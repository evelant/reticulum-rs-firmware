//! App-facing Reticulum proof-probe requests and projections.

use reticulum_device_api as api;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{ClientRequestError, parse_destination, parse_hex};

/// App-facing request to begin one bounded path-and-proof measurement.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, TS)]
#[allow(missing_docs)]
pub struct ReticulumProbeStartRequest {
    destination: String,
    idempotency_key: String,
}

impl ReticulumProbeStartRequest {
    /// Validate the destination and principal-scoped idempotency key.
    pub fn validate(&self) -> Result<(), ClientRequestError> {
        self.as_device_request().map(|_| ())
    }

    pub(crate) fn as_device_request(&self) -> Result<api::ProbeStartRequest, ClientRequestError> {
        let destination = parse_destination(&self.destination)?;
        let idempotency_key = parse_hex::<16>(&self.idempotency_key)
            .map(api::IdempotencyKey)
            .ok_or(ClientRequestError::InvalidIdempotencyKey)?;
        Ok(api::ProbeStartRequest::new(
            api::DestinationHash(*destination.as_bytes()),
            idempotency_key,
        ))
    }
}

/// App-facing request to poll one boot-scoped proof probe.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, TS)]
#[allow(missing_docs)]
pub struct ReticulumProbePollRequest {
    id: String,
}

impl ReticulumProbePollRequest {
    /// Validate the complete opaque boot-scoped probe identifier.
    pub fn validate(&self) -> Result<(), ClientRequestError> {
        self.as_device_id().map(|_| ())
    }

    pub(crate) fn as_device_id(&self) -> Result<api::ProbeId, ClientRequestError> {
        parse_hex::<16>(&self.id)
            .and_then(|bytes| api::ProbeId::new(bytes).ok())
            .ok_or(ClientRequestError::InvalidReticulumProbeId)
    }
}

/// Whether a successful start was newly accepted or replayed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum ReticulumProbeStartOutcome {
    Accepted,
    Replayed,
}

/// App-facing acceptance for one Reticulum proof probe.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[allow(missing_docs)]
pub struct ReticulumProbeStartResponse {
    id: String,
    outcome: ReticulumProbeStartOutcome,
}

impl From<api::ProbeStartAccepted> for ReticulumProbeStartResponse {
    fn from(accepted: api::ProbeStartAccepted) -> Self {
        Self {
            id: hex::encode(accepted.id().as_bytes()),
            outcome: match accepted.outcome() {
                api::ProbeStartOutcome::Accepted => ReticulumProbeStartOutcome::Accepted,
                api::ProbeStartOutcome::Replayed => ReticulumProbeStartOutcome::Replayed,
            },
        }
    }
}

/// Non-terminal progress for an app-facing Reticulum proof probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum ReticulumProbePhase {
    PathLookup,
    AwaitingDispatch,
    AwaitingProof,
}

/// Terminal public failure for an app-facing Reticulum proof probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum ReticulumProbeFailure {
    IdentityUnavailable,
    NoPath,
    Dispatch,
    Timeout,
    Internal,
}

/// Receiver-local physical signal values for the returning proof carrier.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[allow(missing_docs)]
pub struct ReticulumProbeSignalView {
    rssi_dbm: i16,
    snr_db: i16,
}

/// Device-local final-hop evidence for the returning proof.
///
/// This observation belongs to the appliance running the probe. When the
/// proof was relayed, it may describe a relay-to-appliance LoRa hop; it is not
/// the remote destination's RSSI for the original request.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[allow(missing_docs)]
pub struct ReticulumProbeIngressView {
    interface_id: u8,
    signal: Option<ReticulumProbeSignalView>,
}

impl From<api::IngressObservation> for ReticulumProbeIngressView {
    fn from(ingress: api::IngressObservation) -> Self {
        Self {
            interface_id: ingress.interface_id(),
            signal: ingress.signal().map(|signal| ReticulumProbeSignalView {
                rssi_dbm: signal.rssi_dbm(),
                snr_db: signal.snr_db(),
            }),
        }
    }
}

/// Successful end-to-end proof measurement.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[allow(missing_docs)]
pub struct ReticulumProbeSuccessView {
    round_trip_ms: u32,
    hops: u8,
    ingress_observation: ReticulumProbeIngressView,
}

impl From<api::ProbeSuccess> for ReticulumProbeSuccessView {
    fn from(success: api::ProbeSuccess) -> Self {
        Self {
            round_trip_ms: success.round_trip_ms(),
            hops: success.hops(),
            ingress_observation: success.ingress_observation().into(),
        }
    }
}

/// App-facing state returned by polling one proof probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum ReticulumProbePollResponse {
    Pending { phase: ReticulumProbePhase },
    Succeeded { result: ReticulumProbeSuccessView },
    Failed { failure: ReticulumProbeFailure },
}

impl From<api::ProbePollResponse> for ReticulumProbePollResponse {
    fn from(response: api::ProbePollResponse) -> Self {
        match response {
            api::ProbePollResponse::Pending(phase) => Self::Pending {
                phase: match phase {
                    api::ProbePhase::PathLookup => ReticulumProbePhase::PathLookup,
                    api::ProbePhase::AwaitingDispatch => ReticulumProbePhase::AwaitingDispatch,
                    api::ProbePhase::AwaitingProof => ReticulumProbePhase::AwaitingProof,
                },
            },
            api::ProbePollResponse::Succeeded(result) => Self::Succeeded {
                result: result.into(),
            },
            api::ProbePollResponse::Failed(failure) => Self::Failed {
                failure: match failure {
                    api::ProbeFailure::IdentityUnavailable => {
                        ReticulumProbeFailure::IdentityUnavailable
                    }
                    api::ProbeFailure::NoPath => ReticulumProbeFailure::NoPath,
                    api::ProbeFailure::Dispatch => ReticulumProbeFailure::Dispatch,
                    api::ProbeFailure::Timeout => ReticulumProbeFailure::Timeout,
                    api::ProbeFailure::Internal => ReticulumProbeFailure::Internal,
                },
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start_request(destination: &str, idempotency_key: &str) -> ReticulumProbeStartRequest {
        serde_json::from_value(serde_json::json!({
            "destination": destination,
            "idempotency_key": idempotency_key,
        }))
        .unwrap()
    }

    #[test]
    fn requests_validate_and_preserve_exact_device_semantics() {
        let request = start_request(&"11".repeat(16), &"22".repeat(16));
        let device = request.as_device_request().unwrap();
        assert_eq!(device.destination(), api::DestinationHash([0x11; 16]));
        assert_eq!(device.idempotency_key(), api::IdempotencyKey([0x22; 16]));

        assert_eq!(
            start_request("not-a-destination", &"22".repeat(16)).as_device_request(),
            Err(ClientRequestError::InvalidDestination)
        );
        assert_eq!(
            start_request(&"11".repeat(16), "not-a-key").as_device_request(),
            Err(ClientRequestError::InvalidIdempotencyKey)
        );

        let valid = api::ProbeId::new([0x33; 16]).unwrap();
        let request: ReticulumProbePollRequest = serde_json::from_value(serde_json::json!({
            "id": hex::encode(valid.as_bytes()),
        }))
        .unwrap();
        assert_eq!(request.as_device_id(), Ok(valid));
        for id in ["not-hex".to_owned(), "00".repeat(16)] {
            let request: ReticulumProbePollRequest =
                serde_json::from_value(serde_json::json!({ "id": id })).unwrap();
            assert_eq!(
                request.as_device_id(),
                Err(ClientRequestError::InvalidReticulumProbeId)
            );
        }
    }

    #[test]
    fn device_results_map_to_stable_receiver_local_json() {
        let id = api::ProbeId::new([0x44; 16]).unwrap();
        assert_eq!(
            serde_json::to_value(ReticulumProbeStartResponse::from(
                api::ProbeStartAccepted::new(id, api::ProbeStartOutcome::Replayed)
            ))
            .unwrap(),
            serde_json::json!({
                "id": hex::encode(id.as_bytes()),
                "outcome": "replayed",
            })
        );
        assert_eq!(
            serde_json::to_value(ReticulumProbePollResponse::from(
                api::ProbePollResponse::Pending(api::ProbePhase::AwaitingProof)
            ))
            .unwrap(),
            serde_json::json!({
                "state": "pending",
                "phase": "awaiting_proof",
            })
        );
        assert_eq!(
            serde_json::to_value(ReticulumProbePollResponse::from(
                api::ProbePollResponse::Succeeded(api::ProbeSuccess::new(
                    1_234,
                    2,
                    api::IngressObservation::new(7, Some(api::IngressSignal::new(-91, 7)),),
                ))
            ))
            .unwrap(),
            serde_json::json!({
                "state": "succeeded",
                "result": {
                    "round_trip_ms": 1_234,
                    "hops": 2,
                    "ingress_observation": {
                        "interface_id": 7,
                        "signal": {
                            "rssi_dbm": -91,
                            "snr_db": 7,
                        },
                    },
                },
            })
        );
        assert_eq!(
            serde_json::to_value(ReticulumProbePollResponse::from(
                api::ProbePollResponse::Failed(api::ProbeFailure::NoPath)
            ))
            .unwrap(),
            serde_json::json!({
                "state": "failed",
                "failure": "no_path",
            })
        );
    }
}
