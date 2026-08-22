//! Bounded app-facing projection of node, radio, and retained-route diagnostics.

use std::fmt;

use reticulum_appliance_sync::{DeviceSessionError, LxmfSession};
use reticulum_device_api as api;
use serde::Serialize;
use ts_rs::TS;

use crate::{JsonSafeInteger, MAX_JSON_SAFE_INTEGER, serialize_json_safe_u64};

/// Maximum retained routes returned by one app refresh.
///
/// This matches the current E290 product path-table capacity. The independent
/// host ceiling keeps later device profiles and malformed authenticated peers
/// from turning one UI refresh into unbounded allocation or request traffic.
pub const MAX_RADIO_ROUTE_ENTRIES: usize = 256;

const MAX_ROUTE_PAGE_REQUESTS: usize =
    MAX_RADIO_ROUTE_ENTRIES.div_ceil(api::MAX_ROUTE_DIAGNOSTIC_PAGE_ENTRIES);

/// PRNS forwarding mode for one interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticInterfaceModeView {
    /// Ordinary full interface.
    Full,
    /// Point-to-point interface.
    PointToPoint,
    /// Access-point interface.
    AccessPoint,
    /// Roaming interface.
    Roaming,
    /// Boundary interface.
    Boundary,
    /// Gateway interface.
    Gateway,
    /// Internal transport interface.
    Internal,
}

impl From<api::DiagnosticInterfaceMode> for DiagnosticInterfaceModeView {
    fn from(mode: api::DiagnosticInterfaceMode) -> Self {
        match mode {
            api::DiagnosticInterfaceMode::Full => Self::Full,
            api::DiagnosticInterfaceMode::PointToPoint => Self::PointToPoint,
            api::DiagnosticInterfaceMode::AccessPoint => Self::AccessPoint,
            api::DiagnosticInterfaceMode::Roaming => Self::Roaming,
            api::DiagnosticInterfaceMode::Boundary => Self::Boundary,
            api::DiagnosticInterfaceMode::Gateway => Self::Gateway,
            api::DiagnosticInterfaceMode::Internal => Self::Internal,
        }
    }
}

/// Current local state of one configured interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticInterfaceStateView {
    /// Interface owner is starting.
    Initializing,
    /// Interface is connected.
    Connected,
    /// Interface is usable with degraded service.
    Degraded,
    /// Interface is reconnecting.
    Reconnecting,
    /// Interface owner failed.
    Failed,
    /// Interface is disconnected.
    Disconnected,
    /// Interface is deliberately disabled.
    Disabled,
    /// Interface state is not classified.
    Unknown,
}

impl From<api::DiagnosticInterfaceState> for DiagnosticInterfaceStateView {
    fn from(state: api::DiagnosticInterfaceState) -> Self {
        match state {
            api::DiagnosticInterfaceState::Initializing => Self::Initializing,
            api::DiagnosticInterfaceState::Connected => Self::Connected,
            api::DiagnosticInterfaceState::Degraded => Self::Degraded,
            api::DiagnosticInterfaceState::Reconnecting => Self::Reconnecting,
            api::DiagnosticInterfaceState::Failed => Self::Failed,
            api::DiagnosticInterfaceState::Disconnected => Self::Disconnected,
            api::DiagnosticInterfaceState::Disabled => Self::Disabled,
            api::DiagnosticInterfaceState::Unknown => Self::Unknown,
        }
    }
}

/// One configured local Reticulum interface.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct DiagnosticInterfaceView {
    id: [u8; 8],
    mode: DiagnosticInterfaceModeView,
    state: DiagnosticInterfaceStateView,
    failure_reason: Option<String>,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    rx_bytes: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    tx_bytes: u64,
    destinations: u32,
    links: u32,
    transported_links: u32,
    supervisor: Option<[u8; 8]>,
}

impl From<api::DiagnosticInterfaceRecord> for DiagnosticInterfaceView {
    fn from(interface: api::DiagnosticInterfaceRecord) -> Self {
        Self {
            id: *interface.id().as_bytes(),
            mode: interface.mode().into(),
            state: interface.state().into(),
            failure_reason: interface
                .failure_reason()
                .map(|reason| reason.as_str().to_owned()),
            rx_bytes: json_safe(interface.rx_bytes()),
            tx_bytes: json_safe(interface.tx_bytes()),
            destinations: interface.destinations(),
            links: interface.links(),
            transported_links: interface.transported_links(),
            supervisor: interface
                .supervisor()
                .map(|supervisor| *supervisor.as_bytes()),
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
    interface_id: [u8; 8],
    encoded_packet_len: u16,
    encoded_packet_sha256: String,
}

impl From<api::DiagnosticLoraDataTxEvidence> for DiagnosticLoraDataTxEvidenceView {
    fn from(evidence: api::DiagnosticLoraDataTxEvidence) -> Self {
        Self {
            interface_id: *evidence.interface_id().as_bytes(),
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

/// PRNS next-hop decision for one live route.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RouteNextHopView {
    /// Destination is directly reachable on the selected interface.
    Direct,
    /// Destination is reached through a transport identity.
    Via {
        /// Lowercase Reticulum transport identity hash.
        transport_identity: String,
    },
}

impl From<api::RouteDiagnosticNextHop> for RouteNextHopView {
    fn from(next_hop: api::RouteDiagnosticNextHop) -> Self {
        match next_hop {
            api::RouteDiagnosticNextHop::Direct => Self::Direct,
            api::RouteDiagnosticNextHop::Via(identity) => Self::Via {
                transport_identity: hex::encode(identity.as_bytes()),
            },
        }
    }
}

/// One retained Reticulum route.
///
/// `last_activity_age_ms` is local route-table LRU activity. It is not the
/// time a peer was last heard; recently heard announces remain a separate
/// nearby-peer projection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct RetainedRouteView {
    destination: String,
    next_hop: RouteNextHopView,
    hops: u8,
    interface_id: [u8; 8],
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    learned_age_ms: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    last_activity_age_ms: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    expires_in_ms: u64,
}

impl From<api::RouteDiagnosticEntry> for RetainedRouteView {
    fn from(route: api::RouteDiagnosticEntry) -> Self {
        Self {
            destination: hex::encode(route.destination().0),
            next_hop: route.next_hop().into(),
            hops: route.hops(),
            interface_id: *route.interface().as_bytes(),
            learned_age_ms: json_safe(route.learned_age_ms()),
            last_activity_age_ms: json_safe(route.last_activity_age_ms()),
            expires_in_ms: json_safe(route.expires_in_ms()),
        }
    }
}

/// One bounded app-facing view of PRNS node, interface, and live-route state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct RadioRoutesStatusView {
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    uptime_ms: u64,
    interfaces: Vec<DiagnosticInterfaceView>,
    lora: Option<LoraDiagnosticsView>,
    route_count: u32,
    link_count: u32,
    routes: Vec<RetainedRouteView>,
}

impl RadioRoutesStatusView {
    fn from_snapshot(
        snapshot: api::NodeDiagnosticsSnapshot,
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
            route_count: snapshot.route_count(),
            link_count: snapshot.link_count(),
            routes: routes.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum RadioRoutesReadError {
    Session(String),
    RouteLimitExceeded { reported: u32, limit: usize },
    RoutePageLimitExceeded { limit: usize },
    NonAdvancingCursor,
    NonIncreasingDestination,
}

impl fmt::Display for RadioRoutesReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Session(error) => write!(formatter, "diagnostics request failed: {error}"),
            Self::RouteLimitExceeded { reported, limit } => write!(
                formatter,
                "PRNS route count {reported} exceeds client limit {limit}"
            ),
            Self::RoutePageLimitExceeded { limit } => {
                write!(
                    formatter,
                    "route diagnostics exceeded {limit} page requests"
                )
            }
            Self::NonAdvancingCursor => {
                formatter.write_str("route diagnostics returned a non-advancing cursor")
            }
            Self::NonIncreasingDestination => formatter
                .write_str("route diagnostics destinations were not globally strictly increasing"),
        }
    }
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
    let snapshot = read_node(source).map_err(RadioRoutesReadError::Session)?;
    let routes = read_routes_with(snapshot.route_count(), |request| read_page(source, request))?;
    Ok(RadioRoutesStatusView::from_snapshot(snapshot, routes))
}

fn read_routes_with(
    reported_route_count: u32,
    mut request_page: impl FnMut(
        api::RouteDiagnosticsRequest,
    ) -> Result<api::RouteDiagnosticsPage, String>,
) -> Result<Vec<api::RouteDiagnosticEntry>, RadioRoutesReadError> {
    if reported_route_count as usize > MAX_RADIO_ROUTE_ENTRIES {
        return Err(RadioRoutesReadError::RouteLimitExceeded {
            reported: reported_route_count,
            limit: MAX_RADIO_ROUTE_ENTRIES,
        });
    }

    let mut routes = Vec::with_capacity(reported_route_count as usize);
    let mut after = None;

    for _ in 0..MAX_ROUTE_PAGE_REQUESTS {
        let requested_after = after;
        let page = request_page(api::RouteDiagnosticsRequest::new(requested_after))
            .map_err(RadioRoutesReadError::Session)?;

        for route in page.entries().iter().flatten().copied() {
            let destination = route.destination();
            if requested_after.is_some_and(|cursor| destination <= cursor)
                || after.is_some_and(|cursor| destination <= cursor)
            {
                return Err(RadioRoutesReadError::NonIncreasingDestination);
            }
            routes.push(route);
            if routes.len() > MAX_RADIO_ROUTE_ENTRIES {
                return Err(RadioRoutesReadError::RoutePageLimitExceeded {
                    limit: MAX_ROUTE_PAGE_REQUESTS,
                });
            }
            after = Some(destination);
        }

        match page.next_cursor() {
            Some(next) => {
                if Some(next) != after || requested_after.is_some_and(|previous| next <= previous) {
                    return Err(RadioRoutesReadError::NonAdvancingCursor);
                }
            }
            None => return Ok(routes),
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

    fn interface() -> api::ReticulumInterfaceId {
        api::ReticulumInterfaceId::new([14, 1, 2, 3, 4, 5, 6, 7])
    }

    fn route(value: u8) -> api::RouteDiagnosticEntry {
        api::RouteDiagnosticEntry::new(
            destination(value),
            api::RouteDiagnosticNextHop::Via(api::IdentityHash::new([value.wrapping_add(1); 16])),
            2,
            interface(),
            u64::from(value) * 1_000,
            u64::from(value) * 2_000,
            u64::from(value) * 3_000,
        )
    }

    fn page(
        entries: &[api::RouteDiagnosticEntry],
        next: Option<api::DestinationHash>,
    ) -> api::RouteDiagnosticsPage {
        let mut slots = [None; api::MAX_ROUTE_DIAGNOSTIC_PAGE_ENTRIES];
        for (slot, entry) in slots.iter_mut().zip(entries) {
            *slot = Some(*entry);
        }
        api::RouteDiagnosticsPage::new(slots, next).unwrap()
    }

    fn node(route_count: u32) -> api::NodeDiagnosticsSnapshot {
        api::NodeDiagnosticsSnapshot::new(
            12_345,
            [
                Some(api::DiagnosticInterfaceRecord::new(
                    interface(),
                    api::DiagnosticInterfaceMode::Internal,
                    api::DiagnosticInterfaceState::Connected,
                    None,
                    7,
                    500,
                    4,
                    3,
                    2,
                    None,
                )),
                None,
                None,
                None,
            ],
            None,
            route_count,
            3,
        )
    }

    #[test]
    fn live_prns_routes_project_without_product_resolution_policy() {
        let first = [route(0x0a), route(0x0b), route(0x0c), route(0x0d)];
        let second = [route(0xaa)];
        let mut pages =
            VecDeque::from([page(&first, Some(destination(0x0d))), page(&second, None)]);
        let mut source = ();
        let snapshot = read_radio_routes_from(
            &mut source,
            |_| Ok(node(5)),
            |_, request| {
                let expected_after = if pages.len() == 2 {
                    None
                } else {
                    Some(destination(0x0d))
                };
                assert_eq!(request.after(), expected_after);
                Ok(pages.pop_front().unwrap())
            },
        )
        .unwrap();
        let encoded = serde_json::to_value(snapshot).unwrap();

        assert_eq!(encoded["route_count"], 5);
        assert_eq!(encoded["link_count"], 3);
        assert_eq!(encoded["interfaces"][0]["mode"], "internal");
        assert_eq!(encoded["interfaces"][0]["state"], "connected");
        assert_eq!(
            encoded["routes"][0]["destination"],
            "0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a"
        );
        assert_eq!(encoded["routes"][0]["next_hop"]["kind"], "via");
        assert_eq!(
            encoded["routes"][0]["next_hop"]["transport_identity"],
            "0b".repeat(16)
        );
        assert_eq!(encoded["routes"][0]["last_activity_age_ms"], 20_000);
    }

    #[test]
    fn a_route_table_change_does_not_require_a_synthetic_revision() {
        let routes = read_routes_with(2, |_| Ok(page(&[route(1)], None))).unwrap();
        assert_eq!(routes.len(), 1);
    }

    #[test]
    fn paging_rejects_repeated_destinations_and_nonadvancing_cursors() {
        let mut calls = 0;
        let repeated = read_routes_with(2, |_| {
            calls += 1;
            Ok(page(&[route(1)], Some(destination(1))))
        });
        assert_eq!(calls, 2);
        assert_eq!(
            repeated.unwrap_err(),
            RadioRoutesReadError::NonIncreasingDestination
        );

        let invalid = api::RouteDiagnosticsPage::new(
            [Some(route(1)), None, None, None],
            Some(destination(2)),
        );
        assert!(
            invalid.is_err(),
            "the wire model rejects an unrelated cursor"
        );
    }

    #[test]
    fn reported_route_count_enforces_the_host_allocation_ceiling() {
        let error = read_routes_with(MAX_RADIO_ROUTE_ENTRIES as u32 + 1, |_| {
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
    fn json_safe_fields_saturate_and_direct_routes_are_explicit() {
        let direct = api::RouteDiagnosticEntry::new(
            destination(1),
            api::RouteDiagnosticNextHop::Direct,
            1,
            interface(),
            u64::MAX,
            u64::MAX,
            u64::MAX,
        );
        let snapshot = api::NodeDiagnosticsSnapshot::new(
            u64::MAX,
            [None; api::MAX_DIAGNOSTIC_INTERFACES],
            None,
            1,
            0,
        );
        let view = RadioRoutesStatusView::from_snapshot(snapshot, vec![direct]);
        let encoded = serde_json::to_value(view).unwrap();
        assert_eq!(encoded["uptime_ms"], MAX_JSON_SAFE_INTEGER);
        assert_eq!(
            encoded["routes"][0]["learned_age_ms"],
            MAX_JSON_SAFE_INTEGER
        );
        assert_eq!(
            encoded["routes"][0]["next_hop"],
            json!({ "kind": "direct" })
        );
    }
}
