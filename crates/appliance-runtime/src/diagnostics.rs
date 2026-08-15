//! Bounded app-facing projection of node, radio, and retained-route diagnostics.

use std::fmt;

use reticulum_appliance_sync::{DeviceSessionError, LxmfSession};
use reticulum_device_api as api;
use serde::Serialize;
use ts_rs::TS;

use crate::{JsonSafeInteger, MAX_JSON_SAFE_INTEGER, serialize_json_safe_u64};

/// Maximum retained routes returned by one app refresh.
///
/// The initial E290 product retains sixteen routes. This independent host
/// ceiling keeps later device profiles and malformed authenticated peers from
/// turning one UI refresh into unbounded allocation or request traffic.
pub const MAX_RADIO_ROUTE_ENTRIES: usize = 64;

const MAX_ROUTE_PAGE_REQUESTS: usize =
    MAX_RADIO_ROUTE_ENTRIES.div_ceil(api::MAX_ROUTE_DIAGNOSTIC_PAGE_ENTRIES);
const MAX_ROUTE_SNAPSHOT_ATTEMPTS: usize = 2;

/// Stable transport family for one configured interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticInterfaceKindView {
    /// LoRa packet radio.
    Lora,
    /// Reticulum TCP client or server.
    Tcp,
    /// Another transport without a stable app category yet.
    Other,
}

impl From<api::DiagnosticInterfaceKind> for DiagnosticInterfaceKindView {
    fn from(kind: api::DiagnosticInterfaceKind) -> Self {
        match kind {
            api::DiagnosticInterfaceKind::LoRa => Self::Lora,
            api::DiagnosticInterfaceKind::Tcp => Self::Tcp,
            api::DiagnosticInterfaceKind::Other => Self::Other,
        }
    }
}

/// Current local state of one configured interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticInterfaceStateView {
    /// Configured but not currently usable.
    Offline,
    /// Online and eligible for Reticulum traffic.
    Online,
    /// The interface owner has latched a fault.
    Faulted,
}

impl From<api::DiagnosticInterfaceState> for DiagnosticInterfaceStateView {
    fn from(state: api::DiagnosticInterfaceState) -> Self {
        match state {
            api::DiagnosticInterfaceState::Offline => Self::Offline,
            api::DiagnosticInterfaceState::Online => Self::Online,
            api::DiagnosticInterfaceState::Faulted => Self::Faulted,
        }
    }
}

/// One configured local Reticulum interface.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct DiagnosticInterfaceView {
    id: u8,
    kind: DiagnosticInterfaceKindView,
    state: DiagnosticInterfaceStateView,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    generation: u64,
    logical_mtu: u16,
    bitrate: Option<u32>,
}

impl From<api::DiagnosticInterfaceRecord> for DiagnosticInterfaceView {
    fn from(interface: api::DiagnosticInterfaceRecord) -> Self {
        Self {
            id: interface.id(),
            kind: interface.kind().into(),
            state: interface.state().into(),
            generation: json_safe(interface.generation()),
            logical_mtu: interface.logical_mtu(),
            bitrate: interface.bitrate(),
        }
    }
}

/// Terminal category for the most recent local LoRa transmission job.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticLoraTxOutcomeView {
    /// Every physical frame completed.
    Completed,
    /// Channel-access policy rejected the job.
    AccessRejected,
    /// Radio setup, transmission, or completion failed.
    Failed,
}

impl From<api::DiagnosticLoraTxOutcome> for DiagnosticLoraTxOutcomeView {
    fn from(outcome: api::DiagnosticLoraTxOutcome) -> Self {
        match outcome {
            api::DiagnosticLoraTxOutcome::Completed => Self::Completed,
            api::DiagnosticLoraTxOutcome::AccessRejected => Self::AccessRejected,
            api::DiagnosticLoraTxOutcome::Failed => Self::Failed,
        }
    }
}

/// Packet-owner family of one terminal LoRa dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticLoraTxFamilyView {
    /// Destination DATA associated with a durable application attempt.
    Data,
    /// Ordinary Reticulum control or application packet.
    Ordinary,
}

impl From<api::DiagnosticLoraTxFamily> for DiagnosticLoraTxFamilyView {
    fn from(family: api::DiagnosticLoraTxFamily) -> Self {
        match family {
            api::DiagnosticLoraTxFamily::Data => Self::Data,
            api::DiagnosticLoraTxFamily::Ordinary => Self::Ordinary,
        }
    }
}

/// App-correlatable prepared-packet identity for one LoRa DATA dispatch.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct DiagnosticLoraDataTxEvidenceView {
    interface_id: u8,
    encoded_packet_len: u16,
    encoded_packet_sha256: String,
}

impl From<api::DiagnosticLoraDataTxEvidence> for DiagnosticLoraDataTxEvidenceView {
    fn from(evidence: api::DiagnosticLoraDataTxEvidence) -> Self {
        Self {
            interface_id: evidence.interface_id(),
            encoded_packet_len: evidence.encoded_packet_len(),
            encoded_packet_sha256: hex::encode(evidence.encoded_packet_sha256().as_bytes()),
        }
    }
}

/// Most recent local LoRa receive observation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct DiagnosticLoraLastRxView {
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    age_ms: u64,
    rssi_dbm: i16,
    snr_db: i16,
}

impl From<api::DiagnosticLoraLastRx> for DiagnosticLoraLastRxView {
    fn from(last_rx: api::DiagnosticLoraLastRx) -> Self {
        Self {
            age_ms: json_safe(last_rx.age_ms()),
            rssi_dbm: last_rx.rssi_dbm(),
            snr_db: last_rx.snr_db(),
        }
    }
}

/// Most recent terminal local LoRa transmission observation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct DiagnosticLoraLastTxView {
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    age_ms: u64,
    outcome: DiagnosticLoraTxOutcomeView,
    family: Option<DiagnosticLoraTxFamilyView>,
    data_evidence: Option<DiagnosticLoraDataTxEvidenceView>,
}

impl From<api::DiagnosticLoraLastTx> for DiagnosticLoraLastTxView {
    fn from(last_tx: api::DiagnosticLoraLastTx) -> Self {
        Self {
            age_ms: json_safe(last_tx.age_ms()),
            outcome: last_tx.outcome().into(),
            family: last_tx.family().map(Into::into),
            data_evidence: last_tx.data_evidence().map(Into::into),
        }
    }
}

impl From<api::DiagnosticLoraLastDataTx> for DiagnosticLoraLastTxView {
    fn from(last_tx: api::DiagnosticLoraLastDataTx) -> Self {
        Self {
            age_ms: json_safe(last_tx.age_ms()),
            outcome: last_tx.outcome().into(),
            family: Some(DiagnosticLoraTxFamilyView::Data),
            data_evidence: Some(last_tx.data_evidence().into()),
        }
    }
}

/// Applied LoRa profile and bounded local radio/scheduler counters.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct LoraDiagnosticsView {
    applied_tx_power_dbm: i16,
    frequency_hz: u32,
    bandwidth_hz: u32,
    spreading_factor: u8,
    coding_rate_denominator: u8,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    rx_physical_frames: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    rx_packets: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    rx_errors: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    rx_drops: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    tx_terminal_jobs: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    tx_successes: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    tx_completed_frames: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    tx_access_rejects: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    tx_failures: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    cad_busy: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    cad_clear: u64,
    last_rx: Option<DiagnosticLoraLastRxView>,
    last_tx: Option<DiagnosticLoraLastTxView>,
    last_data_tx: Option<DiagnosticLoraLastTxView>,
}

impl From<api::LoraDiagnostics> for LoraDiagnosticsView {
    fn from(lora: api::LoraDiagnostics) -> Self {
        let last_tx = lora.last_tx();
        let last_data_tx = lora.last_data_tx().map(Into::into).or_else(|| {
            last_tx
                .filter(|last_tx| {
                    last_tx.family() == Some(api::DiagnosticLoraTxFamily::Data)
                        && last_tx.data_evidence().is_some()
                })
                .map(Into::into)
        });
        Self {
            applied_tx_power_dbm: lora.applied_tx_power_dbm(),
            frequency_hz: lora.frequency_hz(),
            bandwidth_hz: lora.bandwidth_hz(),
            spreading_factor: lora.spreading_factor(),
            coding_rate_denominator: lora.coding_rate_denominator(),
            rx_physical_frames: json_safe(lora.rx_physical_frames()),
            rx_packets: json_safe(lora.rx_packets()),
            rx_errors: json_safe(lora.rx_errors()),
            rx_drops: json_safe(lora.rx_drops()),
            tx_terminal_jobs: json_safe(lora.tx_terminal_jobs()),
            tx_successes: json_safe(lora.tx_successes()),
            tx_completed_frames: json_safe(lora.tx_completed_frames()),
            tx_access_rejects: json_safe(lora.tx_access_rejects()),
            tx_failures: json_safe(lora.tx_failures()),
            cad_busy: json_safe(lora.cad_busy()),
            cad_clear: json_safe(lora.cad_clear()),
            last_rx: lora.last_rx().map(Into::into),
            last_tx: last_tx.map(Into::into),
            last_data_tx,
        }
    }
}

/// Reticulum transport and retained-path counters.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct RnsDiagnosticsView {
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    received: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    forwarded: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    dedup_drops: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    invalid_drops: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    announces_received: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    paths_learned: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    paths_expired: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    links_established: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    links_closed: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    links_failed: u64,
}

impl From<api::RnsDiagnostics> for RnsDiagnosticsView {
    fn from(rns: api::RnsDiagnostics) -> Self {
        Self {
            received: json_safe(rns.received()),
            forwarded: json_safe(rns.forwarded()),
            dedup_drops: json_safe(rns.dedup_drops()),
            invalid_drops: json_safe(rns.invalid_drops()),
            announces_received: json_safe(rns.announces_received()),
            paths_learned: json_safe(rns.paths_learned()),
            paths_expired: json_safe(rns.paths_expired()),
            links_established: json_safe(rns.links_established()),
            links_closed: json_safe(rns.links_closed()),
            links_failed: json_safe(rns.links_failed()),
        }
    }
}

/// Current interpretation of one retained route.
///
/// `BroadcastReady` and `BroadcastUnavailable` describe fallback resolution;
/// they do not mean the destination is a connected or recently heard peer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RouteDiagnosticResolutionView {
    /// The exact retained route is usable through its selected interface.
    ExactReady,
    /// The exact retained route's selected interface is offline or faulted.
    ExactOffline,
    /// The exact retained route lacks a complete next-hop or interface binding.
    ExactMissing,
    /// Broadcast fallback is currently available.
    BroadcastReady,
    /// Neither an exact usable route nor broadcast fallback is available.
    BroadcastUnavailable,
}

impl From<api::RouteDiagnosticResolution> for RouteDiagnosticResolutionView {
    fn from(resolution: api::RouteDiagnosticResolution) -> Self {
        match resolution {
            api::RouteDiagnosticResolution::ExactReady => Self::ExactReady,
            api::RouteDiagnosticResolution::ExactOffline => Self::ExactOffline,
            api::RouteDiagnosticResolution::ExactMissing => Self::ExactMissing,
            api::RouteDiagnosticResolution::BroadcastReady => Self::BroadcastReady,
            api::RouteDiagnosticResolution::BroadcastUnavailable => Self::BroadcastUnavailable,
        }
    }
}

/// One retained Reticulum route.
///
/// `last_local_use_age_ms` is local route-table LRU activity. It is not the
/// time a peer was last heard; recently heard announces remain a separate
/// nearby-peer projection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct RetainedRouteView {
    destination: String,
    next_hop_identity: Option<String>,
    hops: u8,
    retained_interface_id: Option<u8>,
    resolution: RouteDiagnosticResolutionView,
    #[serde(serialize_with = "crate::serialize_optional_json_safe_u64")]
    #[ts(as = "Option<JsonSafeInteger>")]
    learned_age_ms: Option<u64>,
    #[serde(serialize_with = "crate::serialize_optional_json_safe_u64")]
    #[ts(as = "Option<JsonSafeInteger>")]
    last_local_use_age_ms: Option<u64>,
    #[serde(serialize_with = "crate::serialize_optional_json_safe_u64")]
    #[ts(as = "Option<JsonSafeInteger>")]
    expires_in_ms: Option<u64>,
}

impl From<api::RouteDiagnosticEntry> for RetainedRouteView {
    fn from(route: api::RouteDiagnosticEntry) -> Self {
        Self {
            destination: hex::encode(route.destination().0),
            next_hop_identity: route
                .next_hop_identity()
                .map(|identity| hex::encode(identity.as_bytes())),
            hops: route.hops(),
            retained_interface_id: route.retained_interface(),
            resolution: route.resolution().into(),
            learned_age_ms: route.learned_age_ms().map(json_safe),
            last_local_use_age_ms: route.last_used_age_ms().map(json_safe),
            expires_in_ms: route.expires_in_ms().map(json_safe),
        }
    }
}

/// One coherent app-facing node, radio, and retained-route refresh.
///
/// The route list is bounded and collected only while every page reports the
/// same route-table revision. Retained routes are routing state, not a list of
/// connected or reachable peers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct RadioRoutesStatusView {
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    uptime_ms: u64,
    interfaces: Vec<DiagnosticInterfaceView>,
    lora: Option<LoraDiagnosticsView>,
    rns: RnsDiagnosticsView,
    observed_peer_count: u32,
    retained_route_count: u32,
    usable_route_count: u32,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    route_table_revision: u64,
    routes: Vec<RetainedRouteView>,
}

impl RadioRoutesStatusView {
    fn from_stable_snapshot(
        snapshot: api::NodeDiagnosticsSnapshot,
        route_table_revision: u64,
        routes: Vec<api::RouteDiagnosticEntry>,
    ) -> Self {
        Self {
            uptime_ms: json_safe(snapshot.uptime_ms()),
            interfaces: snapshot
                .interfaces()
                .iter()
                .flatten()
                .copied()
                .map(Into::into)
                .collect(),
            lora: snapshot.lora().map(Into::into),
            rns: snapshot.rns().into(),
            observed_peer_count: snapshot.observed_peer_count(),
            retained_route_count: snapshot.retained_route_count(),
            usable_route_count: snapshot.usable_route_count(),
            route_table_revision: json_safe(route_table_revision),
            routes: routes.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum RadioRoutesReadError {
    Session(String),
    InvalidRouteCounts { retained: u32, usable: u32 },
    RouteLimitExceeded { reported: u32, limit: usize },
    RoutePageLimitExceeded { limit: usize },
    RouteTableChangedRepeatedly,
    NonAdvancingCursor,
    NonIncreasingDestination,
    RouteCountExceeded { reported: u32 },
    RoutePageEndedEarly { reported: u32, received: usize },
}

impl fmt::Display for RadioRoutesReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Session(error) => write!(formatter, "diagnostics request failed: {error}"),
            Self::InvalidRouteCounts { retained, usable } => write!(
                formatter,
                "node diagnostics reported {usable} usable routes but only {retained} retained routes"
            ),
            Self::RouteLimitExceeded { reported, limit } => write!(
                formatter,
                "retained route count {reported} exceeds client limit {limit}"
            ),
            Self::RoutePageLimitExceeded { limit } => {
                write!(
                    formatter,
                    "route diagnostics exceeded {limit} page requests"
                )
            }
            Self::RouteTableChangedRepeatedly => formatter.write_str(
                "retained route table changed repeatedly during one diagnostics refresh",
            ),
            Self::NonAdvancingCursor => {
                formatter.write_str("route diagnostics returned a non-advancing cursor")
            }
            Self::NonIncreasingDestination => formatter
                .write_str("route diagnostics destinations were not globally strictly increasing"),
            Self::RouteCountExceeded { reported } => write!(
                formatter,
                "route diagnostics returned more entries than its reported total {reported}"
            ),
            Self::RoutePageEndedEarly { reported, received } => write!(
                formatter,
                "route diagnostics ended after {received} of {reported} reported entries"
            ),
        }
    }
}

#[derive(Debug)]
enum StableRouteRead {
    Complete {
        revision: u64,
        routes: Vec<api::RouteDiagnosticEntry>,
    },
    Changed,
}

pub(crate) fn read_radio_routes(
    session: &mut (dyn LxmfSession<Error = DeviceSessionError> + Send),
) -> Result<RadioRoutesStatusView, RadioRoutesReadError> {
    read_radio_routes_from(
        session,
        |session| {
            session
                .node_diagnostics()
                .map_err(|error| error.to_string())
        },
        |session, request| {
            session
                .route_diagnostics_page(request)
                .map_err(|error| error.to_string())
        },
    )
}

fn read_radio_routes_from<S: ?Sized>(
    source: &mut S,
    mut read_node: impl FnMut(&mut S) -> Result<api::NodeDiagnosticsSnapshot, String>,
    mut read_page: impl FnMut(
        &mut S,
        api::RouteDiagnosticsRequest,
    ) -> Result<api::RouteDiagnosticsPage, String>,
) -> Result<RadioRoutesStatusView, RadioRoutesReadError> {
    for attempt in 0..MAX_ROUTE_SNAPSHOT_ATTEMPTS {
        let snapshot = read_node(source).map_err(RadioRoutesReadError::Session)?;
        match read_stable_routes_with(snapshot, |request| read_page(source, request))? {
            StableRouteRead::Complete { revision, routes } => {
                return Ok(RadioRoutesStatusView::from_stable_snapshot(
                    snapshot, revision, routes,
                ));
            }
            StableRouteRead::Changed if attempt + 1 < MAX_ROUTE_SNAPSHOT_ATTEMPTS => {}
            StableRouteRead::Changed => {
                return Err(RadioRoutesReadError::RouteTableChangedRepeatedly);
            }
        }
    }
    unreachable!("the fixed diagnostics attempt loop always returns")
}

fn read_stable_routes_with(
    snapshot: api::NodeDiagnosticsSnapshot,
    mut request_page: impl FnMut(
        api::RouteDiagnosticsRequest,
    ) -> Result<api::RouteDiagnosticsPage, String>,
) -> Result<StableRouteRead, RadioRoutesReadError> {
    let expected_revision = snapshot.rns().route_revision();
    let expected_total = snapshot.retained_route_count();
    if snapshot.usable_route_count() > expected_total {
        return Err(RadioRoutesReadError::InvalidRouteCounts {
            retained: expected_total,
            usable: snapshot.usable_route_count(),
        });
    }
    if expected_total as usize > MAX_RADIO_ROUTE_ENTRIES {
        return Err(RadioRoutesReadError::RouteLimitExceeded {
            reported: expected_total,
            limit: MAX_RADIO_ROUTE_ENTRIES,
        });
    }

    let mut routes = Vec::with_capacity(expected_total as usize);
    let mut after = None;

    for _ in 0..MAX_ROUTE_PAGE_REQUESTS {
        let requested_after = after;
        let page = request_page(api::RouteDiagnosticsRequest::new(requested_after))
            .map_err(RadioRoutesReadError::Session)?;
        if page.revision() != expected_revision || page.total_count() != expected_total {
            return Ok(StableRouteRead::Changed);
        }

        for route in page.entries().iter().flatten().copied() {
            let destination = route.destination();
            if requested_after.is_some_and(|cursor| destination <= cursor)
                || after.is_some_and(|cursor| destination <= cursor)
            {
                return Err(RadioRoutesReadError::NonIncreasingDestination);
            }
            routes.push(route);
            if routes.len() > expected_total as usize {
                return Err(RadioRoutesReadError::RouteCountExceeded {
                    reported: expected_total,
                });
            }
            after = Some(destination);
        }

        match page.next_cursor() {
            Some(next) => {
                if Some(next) != after
                    || requested_after.is_some_and(|previous| next <= previous)
                    || routes.len() >= expected_total as usize
                {
                    return Err(RadioRoutesReadError::NonAdvancingCursor);
                }
            }
            None if routes.len() == expected_total as usize => {
                return Ok(StableRouteRead::Complete {
                    revision: expected_revision,
                    routes,
                });
            }
            None => {
                return Err(RadioRoutesReadError::RoutePageEndedEarly {
                    reported: expected_total,
                    received: routes.len(),
                });
            }
        }
    }

    Err(RadioRoutesReadError::RoutePageLimitExceeded {
        limit: MAX_ROUTE_PAGE_REQUESTS,
    })
}

const fn json_safe(value: u64) -> u64 {
    if value > MAX_JSON_SAFE_INTEGER {
        MAX_JSON_SAFE_INTEGER
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use serde_json::json;

    use super::*;

    fn destination(value: u8) -> api::DestinationHash {
        api::DestinationHash([value; 16])
    }

    fn route(value: u8, resolution: api::RouteDiagnosticResolution) -> api::RouteDiagnosticEntry {
        api::RouteDiagnosticEntry::new(
            destination(value),
            Some(api::IdentityHash::new([value.wrapping_add(1); 16])),
            2,
            Some(1),
            resolution,
            Some(u64::from(value) * 1_000),
            Some(u64::from(value) * 2_000),
            Some(u64::from(value) * 3_000),
        )
    }

    fn node(
        revision_learned: u64,
        revision_expired: u64,
        retained: u32,
    ) -> api::NodeDiagnosticsSnapshot {
        api::NodeDiagnosticsSnapshot::new(
            12_345,
            [
                Some(api::DiagnosticInterfaceRecord::new(
                    1,
                    api::DiagnosticInterfaceKind::LoRa,
                    api::DiagnosticInterfaceState::Online,
                    7,
                    500,
                    Some(5_470),
                )),
                None,
                None,
                None,
            ],
            Some(api::LoraDiagnostics::new(
                22,
                915_000_000,
                125_000,
                7,
                5,
                10,
                8,
                1,
                1,
                3,
                2,
                6,
                1,
                0,
                4,
                9,
                Some(api::DiagnosticLoraLastRx::new(500, -91, 7)),
                Some(api::DiagnosticLoraLastTx::data(
                    700,
                    api::DiagnosticLoraTxOutcome::Completed,
                    api::DiagnosticLoraDataTxEvidence::try_new(
                        1,
                        183,
                        api::EncodedPacketSha256::new([0xab; 32]),
                    )
                    .unwrap(),
                )),
                None,
            )),
            api::RnsDiagnostics::new(20, 5, 2, 1, 8, revision_learned, revision_expired, 3, 1, 1),
            4,
            retained,
            retained,
        )
    }

    fn page(
        revision: u64,
        total: u32,
        entries: &[api::RouteDiagnosticEntry],
        next: Option<api::DestinationHash>,
    ) -> api::RouteDiagnosticsPage {
        let mut slots = [None; api::MAX_ROUTE_DIAGNOSTIC_PAGE_ENTRIES];
        for (slot, entry) in slots.iter_mut().zip(entries) {
            *slot = Some(*entry);
        }
        api::RouteDiagnosticsPage::new(revision, total, slots, next).unwrap()
    }

    #[test]
    fn coherent_pages_project_lowercase_hashes_and_explicit_local_lru_semantics() {
        let first = [
            route(0x0a, api::RouteDiagnosticResolution::ExactReady),
            route(0x0b, api::RouteDiagnosticResolution::ExactOffline),
            route(0x0c, api::RouteDiagnosticResolution::BroadcastReady),
            route(0x0d, api::RouteDiagnosticResolution::ExactMissing),
        ];
        let second = [route(
            0xaa,
            api::RouteDiagnosticResolution::BroadcastUnavailable,
        )];
        let mut pages = VecDeque::from([
            page(9, 5, &first, Some(destination(0x0d))),
            page(9, 5, &second, None),
        ]);
        let snapshot = node(7, 2, 5);
        let StableRouteRead::Complete { revision, routes } =
            read_stable_routes_with(snapshot, |request| {
                let expected_after = if pages.len() == 2 {
                    None
                } else {
                    Some(destination(0x0d))
                };
                assert_eq!(request.after(), expected_after);
                Ok(pages.pop_front().unwrap())
            })
            .unwrap()
        else {
            panic!("stable scripted pages must complete")
        };
        let view = RadioRoutesStatusView::from_stable_snapshot(snapshot, revision, routes);
        let encoded = serde_json::to_value(view).unwrap();

        assert_eq!(encoded["route_table_revision"], 9);
        assert_eq!(encoded["retained_route_count"], 5);
        assert_eq!(encoded["interfaces"][0]["kind"], "lora");
        assert_eq!(encoded["lora"]["applied_tx_power_dbm"], 22);
        assert_eq!(encoded["lora"]["last_rx"]["rssi_dbm"], -91);
        assert_eq!(encoded["lora"]["last_tx"]["family"], "data");
        assert_eq!(
            encoded["lora"]["last_data_tx"]["data_evidence"]["encoded_packet_len"],
            183
        );
        assert_eq!(
            encoded["lora"]["last_data_tx"]["data_evidence"]["encoded_packet_sha256"],
            "ab".repeat(32)
        );
        assert_eq!(
            encoded["routes"][0]["destination"],
            "0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a"
        );
        assert_eq!(
            encoded["routes"][0]["next_hop_identity"],
            "0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b"
        );
        assert_eq!(encoded["routes"][0]["last_local_use_age_ms"], 20_000);
        assert_eq!(encoded["routes"][2]["resolution"], json!("broadcast_ready"));
    }

    #[test]
    fn route_revision_or_total_change_requests_a_whole_snapshot_retry() {
        let snapshot = node(7, 2, 1);
        let changed_revision = read_stable_routes_with(snapshot, |_| {
            Ok(page(
                10,
                1,
                &[route(1, api::RouteDiagnosticResolution::ExactReady)],
                None,
            ))
        })
        .unwrap();
        assert!(matches!(changed_revision, StableRouteRead::Changed));

        let changed_total = read_stable_routes_with(snapshot, |_| {
            Ok(page(
                9,
                2,
                &[route(1, api::RouteDiagnosticResolution::ExactReady)],
                None,
            ))
        })
        .unwrap();
        assert!(matches!(changed_total, StableRouteRead::Changed));
    }

    #[test]
    fn one_mid_read_change_restarts_the_complete_node_and_route_snapshot() {
        struct Script {
            node_reads: usize,
            page_reads: usize,
        }

        let mut script = Script {
            node_reads: 0,
            page_reads: 0,
        };
        let view = read_radio_routes_from(
            &mut script,
            |script| {
                script.node_reads += 1;
                Ok(if script.node_reads == 1 {
                    node(9, 0, 1)
                } else {
                    node(10, 0, 1)
                })
            },
            |script, request| {
                script.page_reads += 1;
                assert_eq!(request.after(), None);
                Ok(page(
                    10,
                    1,
                    &[route(1, api::RouteDiagnosticResolution::ExactReady)],
                    None,
                ))
            },
        )
        .unwrap();

        assert_eq!(script.node_reads, 2);
        assert_eq!(script.page_reads, 2);
        assert_eq!(
            serde_json::to_value(view).unwrap()["route_table_revision"],
            10
        );
    }

    #[test]
    fn repeated_mid_read_change_is_a_bounded_visible_failure() {
        let mut calls = 0_usize;
        let error = read_radio_routes_from(
            &mut (),
            |_| Ok(node(9, 0, 1)),
            |_, _| {
                calls += 1;
                Ok(page(
                    10,
                    1,
                    &[route(1, api::RouteDiagnosticResolution::ExactReady)],
                    None,
                ))
            },
        )
        .unwrap_err();
        assert_eq!(calls, MAX_ROUTE_SNAPSHOT_ATTEMPTS);
        assert_eq!(error, RadioRoutesReadError::RouteTableChangedRepeatedly);
    }

    #[test]
    fn route_ceiling_accepts_the_configured_full_table_and_rejects_more() {
        let snapshot = node(1, 0, MAX_RADIO_ROUTE_ENTRIES as u32);
        let mut page_reads = 0_usize;
        let StableRouteRead::Complete { routes, .. } =
            read_stable_routes_with(snapshot, |request| {
                page_reads += 1;
                let first = request
                    .after()
                    .map_or(0, |destination| destination.0[0] + 1);
                let entries = [
                    route(first, api::RouteDiagnosticResolution::ExactReady),
                    route(first + 1, api::RouteDiagnosticResolution::ExactReady),
                    route(first + 2, api::RouteDiagnosticResolution::ExactReady),
                    route(first + 3, api::RouteDiagnosticResolution::ExactReady),
                ];
                let next = (usize::from(first) + entries.len() < MAX_RADIO_ROUTE_ENTRIES)
                    .then(|| destination(first + 3));
                Ok(page(1, MAX_RADIO_ROUTE_ENTRIES as u32, &entries, next))
            })
            .unwrap()
        else {
            panic!("the maximum bounded table must complete")
        };
        assert_eq!(routes.len(), MAX_RADIO_ROUTE_ENTRIES);
        assert_eq!(page_reads, MAX_ROUTE_PAGE_REQUESTS);

        let over_limit = node(1, 0, MAX_RADIO_ROUTE_ENTRIES as u32 + 1);
        let error = read_stable_routes_with(over_limit, |_| {
            panic!("an over-limit snapshot must be rejected before paging")
        })
        .unwrap_err();
        assert_eq!(
            error,
            RadioRoutesReadError::RouteLimitExceeded {
                reported: MAX_RADIO_ROUTE_ENTRIES as u32 + 1,
                limit: MAX_RADIO_ROUTE_ENTRIES,
            }
        );
    }

    #[test]
    fn contradictory_usable_route_count_is_rejected_before_projection() {
        let valid = node(0, 0, 0);
        let invalid = api::NodeDiagnosticsSnapshot::new(
            valid.uptime_ms(),
            *valid.interfaces(),
            valid.lora(),
            valid.rns(),
            valid.observed_peer_count(),
            0,
            1,
        );
        let error = read_stable_routes_with(invalid, |_| {
            panic!("an invalid node snapshot must be rejected before paging")
        })
        .unwrap_err();
        assert_eq!(
            error,
            RadioRoutesReadError::InvalidRouteCounts {
                retained: 0,
                usable: 1,
            }
        );
    }

    #[test]
    fn route_aggregation_rejects_nonadvancing_and_truncated_pages() {
        let snapshot = node(9, 0, 2);
        let first_route = route(1, api::RouteDiagnosticResolution::ExactReady);
        let nonadvancing = read_stable_routes_with(snapshot, |_| {
            Ok(page(9, 2, &[first_route], Some(destination(1))))
        });
        // The page itself advances from the initial cursor; repeating it is
        // rejected on the second request.
        assert_eq!(
            nonadvancing.unwrap_err(),
            RadioRoutesReadError::NonIncreasingDestination
        );

        let truncated = read_stable_routes_with(snapshot, |_| Ok(page(9, 2, &[first_route], None)));
        assert_eq!(
            truncated.unwrap_err(),
            RadioRoutesReadError::RoutePageEndedEarly {
                reported: 2,
                received: 1,
            }
        );
    }

    #[test]
    fn every_u64_projection_saturates_at_the_json_safe_integer_boundary() {
        let snapshot = api::NodeDiagnosticsSnapshot::new(
            u64::MAX,
            [None; api::MAX_DIAGNOSTIC_INTERFACES],
            None,
            api::RnsDiagnostics::new(
                u64::MAX,
                u64::MAX,
                u64::MAX,
                u64::MAX,
                u64::MAX,
                u64::MAX,
                0,
                u64::MAX,
                u64::MAX,
                u64::MAX,
            ),
            0,
            0,
            0,
        );
        let view = RadioRoutesStatusView::from_stable_snapshot(snapshot, u64::MAX, Vec::new());
        let encoded = serde_json::to_value(view).unwrap();
        assert_eq!(encoded["uptime_ms"], MAX_JSON_SAFE_INTEGER);
        assert_eq!(encoded["route_table_revision"], MAX_JSON_SAFE_INTEGER);
        assert_eq!(encoded["rns"]["received"], MAX_JSON_SAFE_INTEGER);
    }
}
