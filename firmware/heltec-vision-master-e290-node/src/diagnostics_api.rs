//! Pure, allocation-free projection of product telemetry into Device API v1.

use reticulum_device_api::{
    DestinationHash as ApiDestinationHash, DiagnosticInterfaceKind, DiagnosticInterfaceRecord,
    DiagnosticInterfaceState, DiagnosticLoraDataTxEvidence, DiagnosticLoraLastDataTx,
    DiagnosticLoraLastRx, DiagnosticLoraLastTx, DiagnosticLoraTxOutcome,
    EncodedPacketSha256 as ApiEncodedPacketSha256, IdentityHash as ApiIdentityHash,
    InvalidRouteDiagnosticsPage, LoraDiagnostics, MAX_DIAGNOSTIC_INTERFACES,
    MAX_RADIO_TRACE_PAGE_ENTRIES, MAX_ROUTE_DIAGNOSTIC_PAGE_ENTRIES, NodeDiagnosticsSnapshot,
    RadioTraceAppliedLoraProfile, RadioTraceAttemptOutcome as ApiRadioTraceAttemptOutcome,
    RadioTraceAttemptTerminal as ApiRadioTraceAttemptTerminal,
    RadioTraceAttemptToken as ApiRadioTraceAttemptToken, RadioTraceCursor as ApiRadioTraceCursor,
    RadioTraceDataTx as ApiRadioTraceDataTx, RadioTraceEvent as ApiRadioTraceEvent,
    RadioTraceEventKind as ApiRadioTraceEventKind, RadioTraceLogicalRx as ApiRadioTraceLogicalRx,
    RadioTracePacketEvidence, RadioTracePage as ApiRadioTracePage,
    RadioTraceRouteSelected as ApiRadioTraceRouteSelected,
    RadioTraceTxOutcome as ApiRadioTraceTxOutcome, RnsDiagnostics, RouteDiagnosticEntry,
    RouteDiagnosticResolution, RouteDiagnosticsPage, RouteDiagnosticsRequest,
};
use reticulum_interface_router::InterfaceDescriptor;
use reticulum_node_core::{EmbeddedNodeMetrics, MonotonicSeconds};
use reticulum_radio_tx_dispatch::DispatchOutcome;
use reticulum_tx_supervisor::{
    RouteDiagnosticsSnapshot, RouteExpiryState, RouteInterfaceResolution,
};

use crate::radio_diagnostics::{
    AppliedLoRaProfile, LastDataTxObservation, RadioDiagnosticsSnapshot, RadioTxDispatchFamily,
    RadioTxTerminalOutcome,
};
use crate::radio_trace::{
    RadioTraceAttemptOutcome, RadioTraceEventKind, RadioTracePage, RadioTraceRouteResolution,
};

/// Project one current interface-registry descriptor into its stable API record.
///
/// `faulted` is a transport-owner fact that overrides the registry's online
/// bit. A missing descriptor produces no record because the interface is not
/// part of the current product incarnation.
pub fn diagnostic_interface_record(
    kind: DiagnosticInterfaceKind,
    descriptor: Option<InterfaceDescriptor>,
    faulted: bool,
) -> Option<DiagnosticInterfaceRecord> {
    descriptor.map(|descriptor| {
        let state = if faulted {
            DiagnosticInterfaceState::Faulted
        } else if descriptor.is_online() {
            DiagnosticInterfaceState::Online
        } else {
            DiagnosticInterfaceState::Offline
        };
        DiagnosticInterfaceRecord::new(
            descriptor.lease().interface().get(),
            kind,
            state,
            descriptor.lease().generation().get(),
            descriptor.logical_mtu().get(),
            descriptor.advertised_bitrate().map(|bitrate| bitrate.get()),
        )
    })
}

/// Project the complete bounded node snapshot served by the authenticated API.
pub fn node_diagnostics<const PATHS: usize>(
    uptime_ms: u64,
    interfaces: [Option<DiagnosticInterfaceRecord>; MAX_DIAGNOSTIC_INTERFACES],
    radio: RadioDiagnosticsSnapshot,
    metrics: EmbeddedNodeMetrics,
    observed_peer_count: usize,
    routes: &RouteDiagnosticsSnapshot<PATHS>,
) -> NodeDiagnosticsSnapshot {
    let retained_route_count = saturating_u32(routes.len());
    let usable_route_count = saturating_u32(
        routes
            .entries()
            .filter(|entry| entry.resolution().is_usable())
            .count(),
    );
    NodeDiagnosticsSnapshot::new(
        uptime_ms,
        interfaces,
        Some(lora_diagnostics(uptime_ms, radio)),
        rns_diagnostics(metrics),
        saturating_u32(observed_peer_count),
        retained_route_count,
        usable_route_count,
    )
}

/// Project one stable page from a complete caller-owned route snapshot.
pub fn route_diagnostics_page<const PATHS: usize>(
    revision: u64,
    request: RouteDiagnosticsRequest,
    routes: &RouteDiagnosticsSnapshot<PATHS>,
) -> Result<RouteDiagnosticsPage, InvalidRouteDiagnosticsPage> {
    let after = request.after();
    let mut matching = routes.entries().copied().filter(|entry| {
        after.is_none_or(|cursor| entry.route().destination().as_bytes() > &cursor.0)
    });
    let mut entries = [None; MAX_ROUTE_DIAGNOSTIC_PAGE_ENTRIES];
    let mut last_destination = None;
    for slot in &mut entries {
        let Some(entry) = matching.next() else {
            break;
        };
        let projected = route_diagnostic_entry(entry, routes.captured_at());
        last_destination = Some(projected.destination());
        *slot = Some(projected);
    }
    let next_cursor = matching
        .next()
        .is_some()
        .then_some(last_destination)
        .flatten();
    RouteDiagnosticsPage::new(revision, saturating_u32(routes.len()), entries, next_cursor)
}

/// Project one bounded firmware trace page into the authenticated Device API.
pub fn radio_trace_page(
    applied: AppliedLoRaProfile,
    trace: RadioTracePage,
) -> Result<ApiRadioTracePage, reticulum_device_api::InvalidRadioTracePage> {
    let next_sequence = trace
        .next_sequence()
        .expect("a firmware boot cannot exhaust u64 radio trace sequences");
    let oldest_sequence = trace.oldest_retained_sequence().unwrap_or(next_sequence);
    let mut entries = [None; MAX_RADIO_TRACE_PAGE_ENTRIES];
    for (slot, event) in entries.iter_mut().zip(trace.events()) {
        *slot = Some(api_radio_trace_event(event));
    }
    let profile = RadioTraceAppliedLoraProfile::new(
        applied.configuration_fingerprint,
        applied.frequency_hz,
        applied.bandwidth_hz,
        applied.preamble_symbols,
        i16::from(applied.requested_power_dbm),
        applied.spreading_factor,
        applied.coding_rate_denominator,
        applied.explicit_header,
        applied.crc,
        applied.iq_inverted,
    );
    let event_count = trace.len();
    for retained in (0..=event_count).rev() {
        let mut candidate = entries;
        for slot in candidate.iter_mut().skip(retained) {
            *slot = None;
        }
        let has_more = trace.has_more() || retained < event_count;
        let next_cursor = if has_more && retained > 0 {
            candidate[retained - 1]
                .map(|event| ApiRadioTraceCursor::new(trace.boot_sequence(), event.sequence()))
        } else {
            None
        };
        match ApiRadioTracePage::new(
            trace.boot_sequence(),
            profile,
            oldest_sequence,
            next_sequence,
            trace.history_gap()
                || trace.overwritten_events() > 0
                || trace.sequence_exhausted_events() > 0,
            candidate,
            next_cursor,
        ) {
            Ok(page) => return Ok(page),
            Err(reticulum_device_api::InvalidRadioTracePage::EventCombinationExceedsWireBudget)
                if retained > 1 => {}
            Err(error) => return Err(error),
        }
    }
    unreachable!("one radio trace event always fits the frozen API body")
}

fn api_radio_trace_event(event: crate::radio_trace::RadioTraceEvent) -> ApiRadioTraceEvent {
    let kind = match event.kind() {
        RadioTraceEventKind::RouteSelected(route) => {
            let packet = RadioTracePacketEvidence::try_new(
                route.selected_interface(),
                route.packet_len(),
                ApiEncodedPacketSha256::new(route.encoded_packet_sha256()),
                Some(ApiRadioTraceAttemptToken::new(route.rns_attempt_token())),
            )
            .expect("firmware route trace always contains a non-empty packet");
            ApiRadioTraceEventKind::RouteSelected(
                ApiRadioTraceRouteSelected::try_new(
                    reticulum_device_api::SubmissionId(route.submission_id()),
                    ApiDestinationHash(route.destination()),
                    route.next_hop().map(ApiIdentityHash::new),
                    route.hops(),
                    match route.resolution() {
                        RadioTraceRouteResolution::ExactRetainedPath => {
                            RouteDiagnosticResolution::ExactReady
                        }
                        RadioTraceRouteResolution::BroadcastFallback => {
                            RouteDiagnosticResolution::BroadcastReady
                        }
                    },
                    packet,
                )
                .expect("firmware route trace contains submission and token correlation"),
            )
        }
        RadioTraceEventKind::DataTxTerminal(tx) => {
            let packet = RadioTracePacketEvidence::try_new(
                tx.interface(),
                tx.packet_len(),
                ApiEncodedPacketSha256::new(tx.encoded_packet_sha256()),
                Some(ApiRadioTraceAttemptToken::new(tx.rns_attempt_token())),
            )
            .expect("firmware DATA trace always contains a non-empty packet");
            ApiRadioTraceEventKind::DataTx(
                ApiRadioTraceDataTx::try_new(
                    packet,
                    api_radio_trace_tx_outcome(tx.outcome()),
                    tx.planned_frames(),
                    tx.completed_frames(),
                    tx.authorized_frame_observed(),
                    [tx.frame_completed_at_us(0), tx.frame_completed_at_us(1)],
                )
                .expect("firmware dispatcher report preserves physical frame invariants"),
            )
        }
        RadioTraceEventKind::LogicalRx(rx) => {
            let packet = RadioTracePacketEvidence::try_new(
                rx.interface(),
                rx.packet_len(),
                ApiEncodedPacketSha256::new(rx.encoded_packet_sha256()),
                rx.rns_packet_hash().map(ApiRadioTraceAttemptToken::new),
            )
            .expect("firmware RX trace always contains a non-empty packet");
            ApiRadioTraceEventKind::LogicalRx(ApiRadioTraceLogicalRx::new(
                packet,
                rx.rssi_dbm(),
                rx.snr_db(),
            ))
        }
        RadioTraceEventKind::AttemptTerminal(terminal) => {
            let ingress = terminal.proof_ingress().map(|ingress| {
                reticulum_device_api::IngressObservation::new(
                    ingress.interface(),
                    ingress.signal().map(|(rssi_dbm, snr_db)| {
                        reticulum_device_api::IngressSignal::new(rssi_dbm, snr_db)
                    }),
                )
            });
            ApiRadioTraceEventKind::AttemptTerminal(ApiRadioTraceAttemptTerminal::new(
                ApiRadioTraceAttemptToken::new(terminal.rns_attempt_token()),
                match terminal.outcome() {
                    RadioTraceAttemptOutcome::Delivered => ApiRadioTraceAttemptOutcome::Delivered,
                    RadioTraceAttemptOutcome::DeliveryTimeout => {
                        ApiRadioTraceAttemptOutcome::DeliveryTimeout
                    }
                    RadioTraceAttemptOutcome::Unsent => ApiRadioTraceAttemptOutcome::Unsent,
                },
                ingress,
            ))
        }
    };
    ApiRadioTraceEvent::new(event.sequence(), event.observed_at_us(), kind)
}

fn api_radio_trace_tx_outcome(outcome: DispatchOutcome) -> ApiRadioTraceTxOutcome {
    match outcome {
        DispatchOutcome::Transmitted => ApiRadioTraceTxOutcome::Transmitted,
        DispatchOutcome::AccessRejected(_) => ApiRadioTraceTxOutcome::AccessRejected,
        DispatchOutcome::PermitDenied => ApiRadioTraceTxOutcome::PermitDenied,
        DispatchOutcome::AuthorizationExpired => ApiRadioTraceTxOutcome::AuthorizationExpired,
        DispatchOutcome::PostGrantAccessRejected(_) => {
            ApiRadioTraceTxOutcome::PostGrantAccessRejected
        }
        DispatchOutcome::AirtimeRejected(_) => ApiRadioTraceTxOutcome::AirtimeRejected,
        DispatchOutcome::DeadlineConversionOverflow => {
            ApiRadioTraceTxOutcome::DeadlineConversionOverflow
        }
        DispatchOutcome::RadioInactive => ApiRadioTraceTxOutcome::RadioInactive,
        DispatchOutcome::InterfaceConfigurationMismatch { .. } => {
            ApiRadioTraceTxOutcome::InterfaceConfigurationMismatch
        }
        DispatchOutcome::RadioConfigurationChangedBeforePermit => {
            ApiRadioTraceTxOutcome::RadioConfigurationChangedBeforePermit
        }
        DispatchOutcome::RadioConfigurationChangedAfterPermit => {
            ApiRadioTraceTxOutcome::RadioConfigurationChangedAfterPermit
        }
        DispatchOutcome::CadFault { .. } => ApiRadioTraceTxOutcome::CadFault,
        DispatchOutcome::TxFault { .. } => ApiRadioTraceTxOutcome::TxFault,
        DispatchOutcome::ControlPlaneRecovery => ApiRadioTraceTxOutcome::ControlPlaneRecovery,
        DispatchOutcome::FrameInvariantRecovery => ApiRadioTraceTxOutcome::FrameInvariantRecovery,
        DispatchOutcome::CancelledRadioOperation => ApiRadioTraceTxOutcome::CancelledRadioOperation,
    }
}

fn lora_diagnostics(uptime_ms: u64, snapshot: RadioDiagnosticsSnapshot) -> LoraDiagnostics {
    let now_us = uptime_ms.saturating_mul(1_000);
    let receive = snapshot.receive;
    let transmit = snapshot.transmit;
    let last_rx = receive
        .last_accepted_at_us()
        .zip(receive.last_accepted_signal())
        .map(|(observed_at_us, signal)| {
            DiagnosticLoraLastRx::new(
                now_us.saturating_sub(observed_at_us) / 1_000,
                signal.rssi_dbm,
                signal.snr_db,
            )
        });
    let last_tx = transmit
        .last_terminal_at_us()
        .zip(transmit.last_outcome())
        .zip(transmit.last_family())
        .map(|((observed_at_us, outcome), family)| {
            let age_ms = now_us.saturating_sub(observed_at_us) / 1_000;
            match family {
                RadioTxDispatchFamily::Data => transmit
                    .last_data()
                    .filter(|data| {
                        data.observed_at_us() == observed_at_us && data.outcome() == outcome
                    })
                    .map(|data| diagnostic_data_tx(age_ms, data))
                    .unwrap_or_else(|| DiagnosticLoraLastTx::new(age_ms, api_tx_outcome(outcome))),
                RadioTxDispatchFamily::Ordinary => {
                    DiagnosticLoraLastTx::ordinary(age_ms, api_tx_outcome(outcome))
                }
            }
        });
    let last_data_tx = (transmit.last_family() != Some(RadioTxDispatchFamily::Data))
        .then(|| transmit.last_data())
        .flatten()
        .map(|data| {
            DiagnosticLoraLastDataTx::new(
                now_us.saturating_sub(data.observed_at_us()) / 1_000,
                api_tx_outcome(data.outcome()),
                diagnostic_data_evidence(data),
            )
        });
    let rx_errors = receive
        .framing_errors
        .saturating_add(snapshot.invalid_physical_frames);
    let rx_drops = receive
        .pending_replaced
        .saturating_add(receive.pending_expired)
        .saturating_add(receive.pending_discarded)
        .saturating_add(receive.packets_too_long)
        .saturating_add(snapshot.ingress.failed);
    LoraDiagnostics::new(
        i16::from(snapshot.applied.requested_power_dbm),
        snapshot.applied.frequency_hz,
        snapshot.applied.bandwidth_hz,
        snapshot.applied.spreading_factor,
        snapshot.applied.coding_rate_denominator,
        receive
            .frames_seen
            .saturating_add(snapshot.invalid_physical_frames),
        receive.packets_accepted,
        rx_errors,
        rx_drops,
        transmit.terminal_jobs,
        transmit.transmitted_jobs,
        transmit.completed_physical_frames,
        transmit.access_rejected_jobs,
        transmit.failed_jobs,
        snapshot.cad.busy,
        snapshot.cad.clear,
        last_rx,
        last_tx,
        last_data_tx,
    )
}

fn diagnostic_data_tx(age_ms: u64, data: LastDataTxObservation) -> DiagnosticLoraLastTx {
    DiagnosticLoraLastTx::data(
        age_ms,
        api_tx_outcome(data.outcome()),
        diagnostic_data_evidence(data),
    )
}

fn diagnostic_data_evidence(data: LastDataTxObservation) -> DiagnosticLoraDataTxEvidence {
    DiagnosticLoraDataTxEvidence::try_new(
        data.interface().get(),
        data.packet_len(),
        ApiEncodedPacketSha256::new(*data.encoded_packet_sha256().as_bytes()),
    )
    .expect("prepared Reticulum DATA packet is nonempty")
}

fn api_tx_outcome(outcome: RadioTxTerminalOutcome) -> DiagnosticLoraTxOutcome {
    match outcome {
        RadioTxTerminalOutcome::Transmitted => DiagnosticLoraTxOutcome::Completed,
        RadioTxTerminalOutcome::AccessRejected
        | RadioTxTerminalOutcome::PostGrantAccessRejected => {
            DiagnosticLoraTxOutcome::AccessRejected
        }
        _ => DiagnosticLoraTxOutcome::Failed,
    }
}

fn rns_diagnostics(metrics: EmbeddedNodeMetrics) -> RnsDiagnostics {
    let transport = metrics.transport;
    RnsDiagnostics::new(
        transport.packets_received,
        transport.packets_forwarded,
        transport.packets_dropped_dedup,
        transport.packets_dropped_invalid,
        transport.announces_received,
        transport.paths_learned,
        transport.paths_expired,
        transport.links_established,
        transport.links_closed,
        transport.links_failed,
    )
}

fn route_diagnostic_entry(
    entry: reticulum_tx_supervisor::RouteDiagnosticsEntry,
    captured_at: MonotonicSeconds,
) -> RouteDiagnosticEntry {
    let route = entry.route();
    let resolution = match entry.resolution() {
        RouteInterfaceResolution::BroadcastFallback { usable: true } => {
            RouteDiagnosticResolution::BroadcastReady
        }
        RouteInterfaceResolution::BroadcastFallback { usable: false } => {
            RouteDiagnosticResolution::BroadcastUnavailable
        }
        RouteInterfaceResolution::Exact { usable: true, .. } => {
            RouteDiagnosticResolution::ExactReady
        }
        RouteInterfaceResolution::Exact {
            current: Some(current),
            usable: false,
            ..
        } if !current.is_online() => RouteDiagnosticResolution::ExactOffline,
        RouteInterfaceResolution::Exact { usable: false, .. } => {
            RouteDiagnosticResolution::ExactMissing
        }
    };
    let expires_in_ms = match entry.expiry_state(captured_at) {
        RouteExpiryState::Retained { remaining_seconds } => {
            Some(seconds_to_millis(remaining_seconds))
        }
        RouteExpiryState::ExpiredPendingMaintenance { .. } => Some(0),
        RouteExpiryState::ClockRegression => None,
    };
    RouteDiagnosticEntry::new(
        ApiDestinationHash(*route.destination().as_bytes()),
        route.via().map(ApiIdentityHash::new),
        route.hops(),
        route.received_on().map(|interface| interface.get()),
        resolution,
        entry
            .learned_age_seconds(captured_at)
            .map(seconds_to_millis),
        entry.lru_age_seconds(captured_at).map(seconds_to_millis),
        expires_in_ms,
    )
}

const fn seconds_to_millis(seconds: u64) -> u64 {
    seconds.saturating_mul(1_000)
}

const fn saturating_u32(value: usize) -> u32 {
    if value > u32::MAX as usize {
        u32::MAX
    } else {
        value as u32
    }
}

#[cfg(test)]
mod tests {
    use reticulum_board_heltec_vision_master_e290_radio::E290_NA915_DEV_22_DBM_CONFIGURATION;
    use reticulum_radio_interface::{FrameSignal, RxDiagnostics};

    use super::{lora_diagnostics, radio_trace_page};
    use crate::radio_diagnostics::{AcceptedLoRaPacketObservation, RadioDiagnosticsCell};
    use crate::radio_trace::{
        RadioTraceCursor, RadioTraceRouteResolution, RadioTraceRouteSelected,
    };

    #[test]
    fn lora_projection_uses_the_last_accepted_packet_and_saturating_drop_groups() {
        let cell = RadioDiagnosticsCell::new(E290_NA915_DEV_22_DBM_CONFIGURATION);
        cell.record_receive_pipeline(
            RxDiagnostics {
                frames_seen: 7,
                framing_errors: 2,
                pending_replaced: 3,
                pending_expired: 5,
                pending_discarded: 7,
                packets_accepted: 11,
                packets_too_long: 13,
                ..RxDiagnostics::default()
            },
            Some(AcceptedLoRaPacketObservation::new(
                1_250_000,
                128,
                FrameSignal::new(-97, 4),
            )),
        );
        cell.record_invalid_physical_frame();
        cell.record_ingress_failed();
        cell.record_cad(true);
        cell.record_cad(false);

        let projected = lora_diagnostics(2_000, cell.snapshot());
        assert_eq!(projected.applied_tx_power_dbm(), 22);
        assert_eq!(projected.rx_physical_frames(), 8);
        assert_eq!(projected.rx_packets(), 11);
        assert_eq!(projected.rx_errors(), 3);
        assert_eq!(projected.rx_drops(), 29);
        assert_eq!(projected.cad_busy(), 1);
        assert_eq!(projected.cad_clear(), 1);
        let last_rx = projected.last_rx().expect("accepted packet observation");
        assert_eq!(last_rx.age_ms(), 750);
        assert_eq!(last_rx.rssi_dbm(), -97);
        assert_eq!(last_rx.snr_db(), 4);
    }

    #[test]
    fn trace_projection_shortens_an_over_budget_page_and_ends_the_final_page() {
        let cell = RadioDiagnosticsCell::new(E290_NA915_DEV_22_DBM_CONFIGURATION);
        cell.set_trace_boot_sequence(77);
        for tag in 1_u8..=3 {
            cell.record_route_selected(
                u64::from(tag),
                RadioTraceRouteSelected::new(
                    u64::from(tag),
                    [tag; 16],
                    Some([tag.wrapping_add(1); 16]),
                    tag,
                    1,
                    RadioTraceRouteResolution::ExactRetainedPath,
                    255,
                    [tag; 32],
                    [tag.wrapping_add(2); 32],
                ),
            )
            .unwrap();
        }

        let applied = cell.snapshot().applied;
        let first = radio_trace_page(applied, cell.radio_trace_page(None)).unwrap();
        assert_eq!(first.entries()[0].unwrap().sequence(), 1);
        assert_eq!(first.entries()[1].unwrap().sequence(), 2);
        assert_eq!(first.entries()[2], None);
        let cursor = first
            .next_cursor()
            .expect("the omitted third route remains");
        assert_eq!(cursor.boot_id(), 77);
        assert_eq!(cursor.after_sequence(), 2);

        let second = radio_trace_page(
            applied,
            cell.radio_trace_page(Some(RadioTraceCursor::new(
                cursor.boot_id(),
                cursor.after_sequence(),
            ))),
        )
        .unwrap();
        assert_eq!(second.entries()[0].unwrap().sequence(), 3);
        assert_eq!(second.entries()[1], None);
        assert_eq!(second.next_cursor(), None);
    }
}
