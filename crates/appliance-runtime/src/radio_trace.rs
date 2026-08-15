//! App-facing projection of durable packet-correlated RF traces.

use std::fmt;

use reticulum_appliance_store as core;
use reticulum_device_api as device_api;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use ts_rs::TS;

use super::{
    JsonSafeInteger, MAX_JSON_SAFE_INTEGER, PacketEvidenceView, PhoneLocationObservationView,
    serialize_json_safe_u64, serialize_optional_json_safe_u64,
};

fn core_packet(packet: device_api::RadioTracePacketEvidence) -> core::PacketEvidence {
    core::PacketEvidence::new(
        packet.packet_len(),
        core::EncodedPacketSha256::new(*packet.encoded_packet_sha256().as_bytes()),
    )
    .expect("device radio trace packet evidence is structurally non-empty")
}

fn core_token(token: device_api::RadioTraceAttemptToken) -> core::RnsAttemptToken {
    core::RnsAttemptToken::new(*token.as_bytes())
}

fn core_tx_outcome(outcome: device_api::RadioTraceTxOutcome) -> core::RfTraceTxOutcome {
    match outcome {
        device_api::RadioTraceTxOutcome::Transmitted => core::RfTraceTxOutcome::Transmitted,
        device_api::RadioTraceTxOutcome::AccessRejected => core::RfTraceTxOutcome::AccessRejected,
        device_api::RadioTraceTxOutcome::PermitDenied => core::RfTraceTxOutcome::PermitDenied,
        device_api::RadioTraceTxOutcome::AuthorizationExpired => {
            core::RfTraceTxOutcome::AuthorizationExpired
        }
        device_api::RadioTraceTxOutcome::PostGrantAccessRejected => {
            core::RfTraceTxOutcome::PostGrantAccessRejected
        }
        device_api::RadioTraceTxOutcome::AirtimeRejected => core::RfTraceTxOutcome::AirtimeRejected,
        device_api::RadioTraceTxOutcome::DeadlineConversionOverflow => {
            core::RfTraceTxOutcome::DeadlineConversionOverflow
        }
        device_api::RadioTraceTxOutcome::RadioInactive => core::RfTraceTxOutcome::RadioInactive,
        device_api::RadioTraceTxOutcome::InterfaceConfigurationMismatch => {
            core::RfTraceTxOutcome::InterfaceConfigurationMismatch
        }
        device_api::RadioTraceTxOutcome::RadioConfigurationChangedBeforePermit => {
            core::RfTraceTxOutcome::RadioConfigurationChangedBeforePermit
        }
        device_api::RadioTraceTxOutcome::RadioConfigurationChangedAfterPermit => {
            core::RfTraceTxOutcome::RadioConfigurationChangedAfterPermit
        }
        device_api::RadioTraceTxOutcome::CadFault => core::RfTraceTxOutcome::CadFault,
        device_api::RadioTraceTxOutcome::TxFault => core::RfTraceTxOutcome::TxFault,
        device_api::RadioTraceTxOutcome::ControlPlaneRecovery => {
            core::RfTraceTxOutcome::ControlPlaneRecovery
        }
        device_api::RadioTraceTxOutcome::FrameInvariantRecovery => {
            core::RfTraceTxOutcome::FrameInvariantRecovery
        }
        device_api::RadioTraceTxOutcome::CancelledRadioOperation => {
            core::RfTraceTxOutcome::CancelledRadioOperation
        }
    }
}

fn core_route_resolution(
    resolution: device_api::RouteDiagnosticResolution,
) -> core::RfTraceRouteResolution {
    match resolution {
        device_api::RouteDiagnosticResolution::ExactReady => {
            core::RfTraceRouteResolution::ExactReady
        }
        device_api::RouteDiagnosticResolution::ExactOffline => {
            core::RfTraceRouteResolution::ExactOffline
        }
        device_api::RouteDiagnosticResolution::ExactMissing => {
            core::RfTraceRouteResolution::ExactMissing
        }
        device_api::RouteDiagnosticResolution::BroadcastReady => {
            core::RfTraceRouteResolution::BroadcastReady
        }
        device_api::RouteDiagnosticResolution::BroadcastUnavailable => {
            core::RfTraceRouteResolution::BroadcastUnavailable
        }
    }
}

fn core_observation(
    event: device_api::RadioTraceEvent,
) -> Result<core::RfTraceObservation, String> {
    let sequence = core::RfTraceEventSequence::new(event.sequence())
        .ok_or_else(|| "device radio trace returned sequence zero".to_owned())?;
    let kind = match event.kind() {
        device_api::RadioTraceEventKind::RouteSelected(route) => {
            let packet = route.packet();
            let token = packet
                .attempt_token()
                .ok_or_else(|| "route trace omitted its attempt token".to_owned())?;
            let submission = core::SubmissionId::new(route.submission_id().0)
                .map_err(|error| error.to_string())?;
            core::RfTraceObservationKind::RouteSelected(core::RfTraceRouteObservation::new(
                core::DestinationHash::new(route.destination().0),
                route
                    .next_hop_identity()
                    .map(|identity| core::RfTraceIdentityHash::new(*identity.as_bytes())),
                route.hops(),
                core::RfTraceInterfaceId::new(packet.interface_id()),
                core_route_resolution(route.resolution()),
                core_packet(packet),
                core_token(token),
                submission,
            ))
        }
        device_api::RadioTraceEventKind::DataTx(tx) => {
            let packet = tx.packet();
            let token = packet
                .attempt_token()
                .ok_or_else(|| "DATA TX trace omitted its attempt token".to_owned())?;
            let tx = core::RfTraceTxObservation::new(
                core_token(token),
                core::RfTraceInterfaceId::new(packet.interface_id()),
                core_packet(packet),
                core_tx_outcome(tx.outcome()),
                tx.planned_frames(),
                tx.completed_frames(),
                tx.frame_completed_at_us(),
                tx.authorization_observed(),
                None,
            )
            .ok_or_else(|| "DATA TX trace contradicted its physical frame evidence".to_owned())?;
            core::RfTraceObservationKind::DataTx(tx)
        }
        device_api::RadioTraceEventKind::LogicalRx(rx) => {
            let packet = rx.packet();
            core::RfTraceObservationKind::LogicalRx(core::RfTraceRxObservation::new(
                core::RfTraceInterfaceId::new(packet.interface_id()),
                core_packet(packet),
                packet.attempt_token().map(core_token),
                rx.rssi_dbm(),
                rx.snr_db(),
            ))
        }
        device_api::RadioTraceEventKind::AttemptTerminal(terminal) => {
            let ingress = terminal.proof_ingress().map(|ingress| {
                core::RfTraceProofIngress::new(
                    core::RfTraceInterfaceId::new(ingress.interface_id()),
                    ingress
                        .signal()
                        .map(|signal| (signal.rssi_dbm(), signal.snr_db())),
                )
            });
            core::RfTraceObservationKind::AttemptTerminal(core::RfTraceAttemptObservation::new(
                core_token(terminal.attempt_token()),
                match terminal.outcome() {
                    device_api::RadioTraceAttemptOutcome::Delivered => {
                        core::RfTraceAttemptOutcome::Delivered
                    }
                    device_api::RadioTraceAttemptOutcome::DeliveryTimeout => {
                        core::RfTraceAttemptOutcome::DeliveryTimeout
                    }
                    device_api::RadioTraceAttemptOutcome::Unsent => {
                        core::RfTraceAttemptOutcome::Unsent
                    }
                },
                ingress,
            ))
        }
        device_api::RadioTraceEventKind::InboundProof(proof) => {
            let packet = proof.packet().map(|packet| {
                core::PacketEvidence::new(
                    packet.packet_len(),
                    core::EncodedPacketSha256::new(*packet.encoded_packet_sha256().as_bytes()),
                )
                .expect("device inbound proof packet evidence is structurally non-empty")
            });
            let stage = match proof.stage() {
                device_api::RadioTraceInboundProofStage::DataLogicalRx => {
                    core::RfTraceInboundProofStage::DataLogicalRx
                }
                device_api::RadioTraceInboundProofStage::DurableCommit => {
                    core::RfTraceInboundProofStage::DurableCommit
                }
                device_api::RadioTraceInboundProofStage::ProofRetained => {
                    core::RfTraceInboundProofStage::ProofRetained
                }
                device_api::RadioTraceInboundProofStage::ProofStaged => {
                    core::RfTraceInboundProofStage::ProofStaged
                }
                device_api::RadioTraceInboundProofStage::OrdinaryQueued => {
                    core::RfTraceInboundProofStage::OrdinaryQueued
                }
                device_api::RadioTraceInboundProofStage::PhysicalTxDone => {
                    core::RfTraceInboundProofStage::PhysicalTxDone
                }
                device_api::RadioTraceInboundProofStage::PhysicalTxFailed => {
                    core::RfTraceInboundProofStage::PhysicalTxFailed
                }
            };
            core::RfTraceObservationKind::InboundProof(
                core::RfTraceInboundProofObservation::new(
                    core_token(proof.correlation_token()),
                    stage,
                    proof.message_id().map(core::MessageId::new),
                    packet,
                    proof.interface_id().map(core::RfTraceInterfaceId::new),
                    proof
                        .signal()
                        .map(|signal| (signal.rssi_dbm(), signal.snr_db())),
                    proof.dispatch_outcome().map(core_tx_outcome),
                )
                .ok_or_else(|| {
                    "device inbound proof trace contradicted its lifecycle stage".to_owned()
                })?,
            )
        }
        _ => return Err("device returned an unsupported radio trace event kind".to_owned()),
    };
    Ok(core::RfTraceObservation::new(
        sequence,
        event.observed_at_us(),
        kind,
    ))
}

pub(crate) fn import_device_page(
    store: &mut core::SqliteChatStore,
    page: device_api::RadioTracePage,
    imported_at_unix_ms: u64,
) -> Result<core::RfTraceImportOutcome, String> {
    let applied = page.applied_lora_profile();
    let profile = core::RfTraceRadioProfile::new(
        applied.configuration_fingerprint(),
        applied.frequency_hz(),
        applied.bandwidth_hz(),
        applied.preamble_symbols(),
        applied.requested_power_dbm(),
        applied.spreading_factor(),
        applied.coding_rate_denominator(),
        applied.explicit_header(),
        applied.crc(),
        applied.iq_inverted(),
    )
    .ok_or_else(|| "device radio trace returned an invalid applied LoRa profile".to_owned())?;
    let observations = page
        .entries()
        .iter()
        .flatten()
        .copied()
        .map(core_observation)
        .collect::<Result<Vec<_>, _>>()?;
    let batch = core::RfTraceImportBatch::new(
        core::RfTraceBootId::new(page.boot_id()),
        profile,
        imported_at_unix_ms,
        page.history_lost(),
        observations,
    )
    .map_err(|error| error.to_string())?;
    core::ChatStore::import_rf_trace_batch(store, batch).map_err(|error| error.to_string())
}

fn deserialize_optional_json_safe_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<u64>::deserialize(deserializer)?;
    if value.is_some_and(|value| value > MAX_JSON_SAFE_INTEGER) {
        return Err(D::Error::custom(
            "integer exceeds the JSON safe-integer contract",
        ));
    }
    Ok(value)
}

/// App-facing bounded newest-first RF trace query.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize, TS)]
#[allow(missing_docs)]
pub struct RadioTracePageRequest {
    #[serde(
        default,
        deserialize_with = "deserialize_optional_json_safe_u64",
        serialize_with = "serialize_optional_json_safe_u64"
    )]
    #[ts(as = "Option<JsonSafeInteger>")]
    before_event_id: Option<u64>,
    limit: u16,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_json_safe_u64",
        serialize_with = "serialize_optional_json_safe_u64"
    )]
    #[ts(as = "Option<JsonSafeInteger>")]
    timeline_sequence: Option<u64>,
}

impl RadioTracePageRequest {
    /// Construct a request for validation at the runtime boundary.
    pub const fn new(
        before_event_id: Option<u64>,
        limit: u16,
        timeline_sequence: Option<u64>,
    ) -> Self {
        Self {
            before_event_id,
            limit,
            timeline_sequence,
        }
    }

    /// Validate the cursor, scope, and shared page-size bound.
    pub fn validate(&self) -> Result<(), RadioTraceRequestError> {
        self.as_core().map(|_| ())
    }

    pub(crate) fn as_core(&self) -> Result<core::RfTracePageRequest, RadioTraceRequestError> {
        let before = self
            .before_event_id
            .map(|value| {
                if value > MAX_JSON_SAFE_INTEGER {
                    return Err(RadioTraceRequestError::InvalidBeforeEventId);
                }
                core::RfTraceEventId::new(value).ok_or(RadioTraceRequestError::InvalidBeforeEventId)
            })
            .transpose()?;
        let scope = self
            .timeline_sequence
            .map(|value| {
                if value > MAX_JSON_SAFE_INTEGER {
                    return Err(RadioTraceRequestError::InvalidTimelineSequence);
                }
                core::TimelineSequence::new(value)
                    .map(core::RfTraceScope::Timeline)
                    .ok_or(RadioTraceRequestError::InvalidTimelineSequence)
            })
            .transpose()?
            .unwrap_or(core::RfTraceScope::All);
        core::RfTracePageRequest::new(scope, before, usize::from(self.limit))
            .map_err(|_| RadioTraceRequestError::InvalidLimit)
    }
}

/// Invalid app-facing durable RF trace query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadioTraceRequestError {
    /// Exclusive local event cursor was zero or not JSON-safe.
    InvalidBeforeEventId,
    /// Message timeline scope was zero or not JSON-safe.
    InvalidTimelineSequence,
    /// Page size was outside the shared bounded range.
    InvalidLimit,
}

impl fmt::Display for RadioTraceRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBeforeEventId => {
                formatter.write_str("radio trace cursor must be a JSON-safe non-zero integer")
            }
            Self::InvalidTimelineSequence => formatter
                .write_str("radio trace timeline sequence must be a JSON-safe non-zero integer"),
            Self::InvalidLimit => write!(
                formatter,
                "radio trace page limit must be within 1..={}",
                core::MAX_RF_TRACE_PAGE_SIZE
            ),
        }
    }
}

/// Immutable LoRa configuration applied for one trace-producing boot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[allow(missing_docs)]
pub struct RadioTraceProfileView {
    fingerprint: String,
    frequency_hz: u32,
    bandwidth_hz: u32,
    preamble_symbols: u16,
    requested_power_dbm: i16,
    spreading_factor: u8,
    coding_rate_denominator: u8,
    explicit_header: bool,
    crc: bool,
    iq_inverted: bool,
}

impl From<core::RfTraceRadioProfile> for RadioTraceProfileView {
    fn from(profile: core::RfTraceRadioProfile) -> Self {
        Self {
            fingerprint: hex::encode(profile.fingerprint()),
            frequency_hz: profile.frequency_hz(),
            bandwidth_hz: profile.bandwidth_hz(),
            preamble_symbols: profile.preamble_symbols(),
            requested_power_dbm: profile.requested_power_dbm(),
            spreading_factor: profile.spreading_factor(),
            coding_rate_denominator: profile.coding_rate_denominator(),
            explicit_header: profile.explicit_header(),
            crc: profile.crc(),
            iq_inverted: profile.iq_inverted(),
        }
    }
}

/// Route resolution captured before one concrete dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum RadioTraceRouteResolutionView {
    ExactReady,
    ExactOffline,
    ExactMissing,
    BroadcastReady,
    BroadcastUnavailable,
}

impl From<core::RfTraceRouteResolution> for RadioTraceRouteResolutionView {
    fn from(resolution: core::RfTraceRouteResolution) -> Self {
        match resolution {
            core::RfTraceRouteResolution::ExactReady => Self::ExactReady,
            core::RfTraceRouteResolution::ExactOffline => Self::ExactOffline,
            core::RfTraceRouteResolution::ExactMissing => Self::ExactMissing,
            core::RfTraceRouteResolution::BroadcastReady => Self::BroadcastReady,
            core::RfTraceRouteResolution::BroadcastUnavailable => Self::BroadcastUnavailable,
        }
    }
}

/// Detailed terminal DATA-dispatch result.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum RadioTraceTxOutcomeView {
    Transmitted,
    AccessRejected,
    PermitDenied,
    AuthorizationExpired,
    PostGrantAccessRejected,
    AirtimeRejected,
    DeadlineConversionOverflow,
    RadioInactive,
    InterfaceConfigurationMismatch,
    RadioConfigurationChangedBeforePermit,
    RadioConfigurationChangedAfterPermit,
    CadFault,
    TxFault,
    ControlPlaneRecovery,
    FrameInvariantRecovery,
    CancelledRadioOperation,
}

impl From<core::RfTraceTxOutcome> for RadioTraceTxOutcomeView {
    fn from(outcome: core::RfTraceTxOutcome) -> Self {
        match outcome {
            core::RfTraceTxOutcome::Transmitted => Self::Transmitted,
            core::RfTraceTxOutcome::AccessRejected => Self::AccessRejected,
            core::RfTraceTxOutcome::PermitDenied => Self::PermitDenied,
            core::RfTraceTxOutcome::AuthorizationExpired => Self::AuthorizationExpired,
            core::RfTraceTxOutcome::PostGrantAccessRejected => Self::PostGrantAccessRejected,
            core::RfTraceTxOutcome::AirtimeRejected => Self::AirtimeRejected,
            core::RfTraceTxOutcome::DeadlineConversionOverflow => Self::DeadlineConversionOverflow,
            core::RfTraceTxOutcome::RadioInactive => Self::RadioInactive,
            core::RfTraceTxOutcome::InterfaceConfigurationMismatch => {
                Self::InterfaceConfigurationMismatch
            }
            core::RfTraceTxOutcome::RadioConfigurationChangedBeforePermit => {
                Self::RadioConfigurationChangedBeforePermit
            }
            core::RfTraceTxOutcome::RadioConfigurationChangedAfterPermit => {
                Self::RadioConfigurationChangedAfterPermit
            }
            core::RfTraceTxOutcome::CadFault => Self::CadFault,
            core::RfTraceTxOutcome::TxFault => Self::TxFault,
            core::RfTraceTxOutcome::ControlPlaneRecovery => Self::ControlPlaneRecovery,
            core::RfTraceTxOutcome::FrameInvariantRecovery => Self::FrameInvariantRecovery,
            core::RfTraceTxOutcome::CancelledRadioOperation => Self::CancelledRadioOperation,
        }
    }
}

/// Application-visible terminal state for one proof-correlated attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum RadioTraceAttemptOutcomeView {
    Delivered,
    DeliveryTimeout,
    Unsent,
}

/// Receiver-side durable DATA-to-proof lifecycle stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum RadioTraceInboundProofStageView {
    DataLogicalRx,
    DurableCommit,
    ProofRetained,
    ProofStaged,
    OrdinaryQueued,
    PhysicalTxDone,
    PhysicalTxFailed,
}

impl From<core::RfTraceInboundProofStage> for RadioTraceInboundProofStageView {
    fn from(stage: core::RfTraceInboundProofStage) -> Self {
        match stage {
            core::RfTraceInboundProofStage::DataLogicalRx => Self::DataLogicalRx,
            core::RfTraceInboundProofStage::DurableCommit => Self::DurableCommit,
            core::RfTraceInboundProofStage::ProofRetained => Self::ProofRetained,
            core::RfTraceInboundProofStage::ProofStaged => Self::ProofStaged,
            core::RfTraceInboundProofStage::OrdinaryQueued => Self::OrdinaryQueued,
            core::RfTraceInboundProofStage::PhysicalTxDone => Self::PhysicalTxDone,
            core::RfTraceInboundProofStage::PhysicalTxFailed => Self::PhysicalTxFailed,
        }
    }
}

/// Typed event-specific RF trace evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum RadioTraceEventKindView {
    RouteSelected {
        #[serde(serialize_with = "serialize_json_safe_u64")]
        #[ts(as = "JsonSafeInteger")]
        submission_id: u64,
        destination: String,
        next_hop_identity: Option<String>,
        hops: u8,
        interface_id: u8,
        resolution: RadioTraceRouteResolutionView,
        packet_evidence: PacketEvidenceView,
        rns_attempt_token: String,
    },
    DataTx {
        interface_id: u8,
        packet_evidence: PacketEvidenceView,
        rns_attempt_token: String,
        outcome: RadioTraceTxOutcomeView,
        planned_physical_frames: u8,
        completed_physical_frames: u8,
        #[serde(serialize_with = "serialize_optional_json_safe_u64")]
        #[ts(as = "Option<JsonSafeInteger>")]
        frame_0_completed_at_us: Option<u64>,
        #[serde(serialize_with = "serialize_optional_json_safe_u64")]
        #[ts(as = "Option<JsonSafeInteger>")]
        frame_1_completed_at_us: Option<u64>,
        authorized_frame_observed: bool,
    },
    LogicalRx {
        interface_id: u8,
        packet_evidence: PacketEvidenceView,
        rns_packet_hash: Option<String>,
        rssi_dbm: i16,
        snr_db: i16,
    },
    AttemptTerminal {
        rns_attempt_token: String,
        outcome: RadioTraceAttemptOutcomeView,
        proof_interface_id: Option<u8>,
        proof_rssi_dbm: Option<i16>,
        proof_snr_db: Option<i16>,
    },
    InboundProof {
        correlation_token: String,
        stage: RadioTraceInboundProofStageView,
        message_id: Option<String>,
        packet_evidence: Option<PacketEvidenceView>,
        interface_id: Option<u8>,
        rssi_dbm: Option<i16>,
        snr_db: Option<i16>,
        dispatch_outcome: Option<RadioTraceTxOutcomeView>,
    },
}

impl From<core::RfTraceObservationKind> for RadioTraceEventKindView {
    fn from(kind: core::RfTraceObservationKind) -> Self {
        match kind {
            core::RfTraceObservationKind::RouteSelected(route) => Self::RouteSelected {
                submission_id: route.submission_id().get(),
                destination: hex::encode(route.destination().as_bytes()),
                next_hop_identity: route.next_hop().map(|hash| hex::encode(hash.as_bytes())),
                hops: route.hops(),
                interface_id: route.selected_interface().get(),
                resolution: route.resolution().into(),
                packet_evidence: route.packet_evidence().into(),
                rns_attempt_token: hex::encode(route.rns_attempt_token().as_bytes()),
            },
            core::RfTraceObservationKind::DataTx(tx) => {
                let completed = tx.frame_completed_at_us();
                Self::DataTx {
                    interface_id: tx.interface().get(),
                    packet_evidence: tx.packet_evidence().into(),
                    rns_attempt_token: hex::encode(tx.rns_attempt_token().as_bytes()),
                    outcome: tx.outcome().into(),
                    planned_physical_frames: tx.planned_physical_frames(),
                    completed_physical_frames: tx.completed_physical_frames(),
                    frame_0_completed_at_us: completed[0],
                    frame_1_completed_at_us: completed[1],
                    authorized_frame_observed: tx.authorized_frame_observed(),
                }
            }
            core::RfTraceObservationKind::LogicalRx(rx) => Self::LogicalRx {
                interface_id: rx.interface().get(),
                packet_evidence: rx.packet_evidence().into(),
                rns_packet_hash: rx
                    .rns_packet_hash()
                    .map(|hash| hex::encode(hash.as_bytes())),
                rssi_dbm: rx.rssi_dbm(),
                snr_db: rx.snr_db(),
            },
            core::RfTraceObservationKind::AttemptTerminal(terminal) => {
                let ingress = terminal.proof_ingress();
                let signal = ingress.and_then(|ingress| ingress.signal());
                Self::AttemptTerminal {
                    rns_attempt_token: hex::encode(terminal.rns_attempt_token().as_bytes()),
                    outcome: match terminal.outcome() {
                        core::RfTraceAttemptOutcome::Delivered => {
                            RadioTraceAttemptOutcomeView::Delivered
                        }
                        core::RfTraceAttemptOutcome::DeliveryTimeout => {
                            RadioTraceAttemptOutcomeView::DeliveryTimeout
                        }
                        core::RfTraceAttemptOutcome::Unsent => RadioTraceAttemptOutcomeView::Unsent,
                    },
                    proof_interface_id: ingress.map(|ingress| ingress.interface().get()),
                    proof_rssi_dbm: signal.map(|signal| signal.0),
                    proof_snr_db: signal.map(|signal| signal.1),
                }
            }
            core::RfTraceObservationKind::InboundProof(proof) => {
                let signal = proof.signal();
                Self::InboundProof {
                    correlation_token: hex::encode(proof.rns_attempt_token().as_bytes()),
                    stage: proof.stage().into(),
                    message_id: proof
                        .message_id()
                        .map(|message_id| hex::encode(message_id.as_bytes())),
                    packet_evidence: proof.packet_evidence().map(Into::into),
                    interface_id: proof.interface().map(|interface| interface.get()),
                    rssi_dbm: signal.map(|signal| signal.0),
                    snr_db: signal.map(|signal| signal.1),
                    dispatch_outcome: proof.dispatch_outcome().map(Into::into),
                }
            }
        }
    }
}

/// Durable message-attempt association for a traced RF event.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[allow(missing_docs)]
pub struct RadioTraceMessageCorrelationView {
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    timeline_sequence: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    outbox_id: u64,
    attempt_number: u32,
    attempt_location: PhoneLocationObservationView,
}

impl From<core::RfTraceMessageCorrelation> for RadioTraceMessageCorrelationView {
    fn from(correlation: core::RfTraceMessageCorrelation) -> Self {
        Self {
            timeline_sequence: correlation.timeline_sequence().get(),
            outbox_id: correlation.outbox_id().get(),
            attempt_number: correlation.attempt_number().get(),
            attempt_location: correlation.attempt_location().into(),
        }
    }
}

/// One durable packet-correlated RF trace event.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[allow(missing_docs)]
pub struct RadioTraceEventView {
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    event_id: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    boot_id: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    event_sequence: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    observed_at_us: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    imported_at_unix_ms: u64,
    profile: RadioTraceProfileView,
    correlation: Option<RadioTraceMessageCorrelationView>,
    event: RadioTraceEventKindView,
}

impl From<core::RfTraceEvent> for RadioTraceEventView {
    fn from(event: core::RfTraceEvent) -> Self {
        let observation = event.observation();
        Self {
            event_id: event.id().get(),
            boot_id: event.boot_id().get(),
            event_sequence: observation.event_sequence().get(),
            observed_at_us: observation.observed_at_us(),
            imported_at_unix_ms: event.imported_at_unix_ms(),
            profile: event.profile().into(),
            correlation: event.message_correlation().map(Into::into),
            event: observation.kind().into(),
        }
    }
}

/// One bounded newest-first page of durable RF trace events.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[allow(missing_docs)]
pub struct RadioTracePageView {
    events: Vec<RadioTraceEventView>,
    #[serde(serialize_with = "serialize_optional_json_safe_u64")]
    #[ts(as = "Option<JsonSafeInteger>")]
    next_before_event_id: Option<u64>,
    history_incomplete: bool,
}

impl From<core::RfTracePage> for RadioTracePageView {
    fn from(page: core::RfTracePage) -> Self {
        Self {
            events: page.events().iter().copied().map(Into::into).collect(),
            next_before_event_id: page.next_before().map(core::RfTraceEventId::get),
            history_incomplete: page.history_incomplete(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbound_proof_device_event_preserves_correlation_and_terminal_evidence() {
        let packet = device_api::RadioTraceInboundProofPacket::try_new(
            123,
            device_api::EncodedPacketSha256::new([0x33; 32]),
        )
        .unwrap();
        let proof = device_api::RadioTraceInboundProof::try_new(
            device_api::RadioTraceAttemptToken::new([0x11; 32]),
            device_api::RadioTraceInboundProofStage::PhysicalTxFailed,
            Some([0x22; 32]),
            Some(packet),
            Some(7),
            Some(device_api::IngressSignal::new(-104, 7)),
            Some(device_api::RadioTraceTxOutcome::TxFault),
        )
        .unwrap();
        let observation = core_observation(device_api::RadioTraceEvent::new(
            3,
            4_000,
            device_api::RadioTraceEventKind::InboundProof(proof),
        ))
        .unwrap();

        let core::RfTraceObservationKind::InboundProof(proof) = observation.kind() else {
            panic!("device inbound proof must remain a typed receiver lifecycle event");
        };
        assert_eq!(proof.rns_attempt_token().as_bytes(), &[0x11; 32]);
        assert_eq!(
            proof.stage(),
            core::RfTraceInboundProofStage::PhysicalTxFailed
        );
        assert_eq!(proof.message_id().unwrap().as_bytes(), &[0x22; 32]);
        assert_eq!(proof.packet_evidence().unwrap().encoded_packet_len(), 123);
        assert_eq!(proof.interface().unwrap().get(), 7);
        assert_eq!(proof.signal(), Some((-104, 7)));
        assert_eq!(
            proof.dispatch_outcome(),
            Some(core::RfTraceTxOutcome::TxFault)
        );

        let value = serde_json::to_value(RadioTraceEventKindView::from(
            core::RfTraceObservationKind::InboundProof(proof),
        ))
        .unwrap();
        assert_eq!(value["kind"], "inbound_proof");
        assert_eq!(value["stage"], "physical_tx_failed");
        assert_eq!(value["correlation_token"], "11".repeat(32));
        assert_eq!(value["message_id"], "22".repeat(32));
        assert_eq!(value["packet_evidence"]["encoded_packet_len"], 123);
        assert_eq!(
            value["packet_evidence"]["encoded_packet_sha256"],
            "33".repeat(32)
        );
        assert_eq!(value["interface_id"], 7);
        assert_eq!(value["rssi_dbm"], -104);
        assert_eq!(value["snr_db"], 7);
        assert_eq!(value["dispatch_outcome"], "tx_fault");
    }
}
