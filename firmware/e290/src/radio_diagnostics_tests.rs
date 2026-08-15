use core::mem;

use reticulum_board_e290_radio::{
    E290_NA915_DEFAULT_22_DBM_CONFIGURATION, E290_NA915_DEFAULT_CONFIGURATION,
};
use reticulum_node_core::{EncodedPacketSha256, PacketInterfaceId};
use reticulum_radio_interface::{FrameSignal, LogicalPacketAccessRejection, RxDiagnostics};
use reticulum_radio_tx_dispatch::DispatchOutcome;

use crate::radio_trace::{
    RadioTraceAttemptOutcome, RadioTraceAttemptTerminal, RadioTraceEventKind,
    RadioTraceProofIngress, RadioTraceRouteResolution, RadioTraceRouteSelected,
};

use super::{
    AcceptedLoRaPacketObservation, LastDataTxObservation, MAXIMUM_RADIO_DIAGNOSTICS_SNAPSHOT_BYTES,
    RadioDiagnosticsCell, RadioDiagnosticsSnapshot, RadioRuntimeState, RadioTxDispatchFamily,
    RadioTxTerminalOutcome, classify_dispatch_outcome,
};

#[test]
fn construction_captures_the_exact_applied_profile_and_power() {
    let cell = RadioDiagnosticsCell::new(E290_NA915_DEFAULT_22_DBM_CONFIGURATION);
    let snapshot = cell.snapshot();

    assert_eq!(snapshot.state, RadioRuntimeState::Offline);
    assert_eq!(
        snapshot.applied.configuration_fingerprint,
        E290_NA915_DEFAULT_22_DBM_CONFIGURATION
            .fingerprint()
            .as_bytes()
    );
    assert_eq!(snapshot.applied.frequency_hz, 915_000_000);
    assert_eq!(snapshot.applied.bandwidth_hz, 125_000);
    assert_eq!(snapshot.applied.spreading_factor, 7);
    assert_eq!(snapshot.applied.coding_rate_denominator, 5);
    assert_eq!(snapshot.applied.preamble_symbols, 24);
    assert_eq!(snapshot.applied.requested_power_dbm, 22);
    assert!(snapshot.applied.explicit_header);
    assert!(snapshot.applied.crc);
    assert!(!snapshot.applied.iq_inverted);
    assert!(snapshot.receive.last_accepted_at_us().is_none());
    assert!(snapshot.transmit.last_outcome().is_none());
}

#[test]
fn receive_projection_updates_counters_and_accepted_packet_atomically() {
    let cell = RadioDiagnosticsCell::new(E290_NA915_DEFAULT_CONFIGURATION);
    let pipeline = RxDiagnostics {
        frames_seen: 4,
        framing_errors: 1,
        pending_started: 2,
        pending_replaced: 1,
        pending_expired: 1,
        pending_discarded: 0,
        packets_completed: 2,
        packets_accepted: 1,
        packets_too_long: 1,
        last_frame_len: 12,
        last_packet_len: 501,
        last_frame_signal: Some(FrameSignal::new(-101, -4)),
        last_packet_signal: Some(FrameSignal::new(-105, -8)),
    };
    cell.record_receive_pipeline(
        pipeline,
        Some(AcceptedLoRaPacketObservation::new(
            42,
            128,
            FrameSignal::new(-97, 3),
        )),
    );

    let snapshot = cell.snapshot();
    assert_eq!(snapshot.receive.frames_seen, 4);
    assert_eq!(snapshot.receive.framing_errors, 1);
    assert_eq!(snapshot.receive.pending_started, 2);
    assert_eq!(snapshot.receive.pending_replaced, 1);
    assert_eq!(snapshot.receive.pending_expired, 1);
    assert_eq!(snapshot.receive.packets_completed, 2);
    assert_eq!(snapshot.receive.packets_accepted, 1);
    assert_eq!(snapshot.receive.packets_too_long, 1);
    assert_eq!(snapshot.receive.last_accepted_at_us(), Some(42));
    assert_eq!(snapshot.receive.last_accepted_packet_len(), Some(128));
    assert_eq!(
        snapshot.receive.last_accepted_signal(),
        Some(FrameSignal::new(-97, 3))
    );
}

#[test]
fn shared_cell_records_packet_correlated_rx_route_and_terminal_events() {
    let cell = RadioDiagnosticsCell::new(E290_NA915_DEFAULT_CONFIGURATION);
    cell.set_trace_boot_sequence(41);
    let packet = [0_u8; 19];
    let rx = cell
        .record_logical_rx(100, 1, &packet, FrameSignal::new(-98, 4))
        .expect("RX trace sequence");
    let route = RadioTraceRouteSelected::new(
        9,
        [0x11; 16],
        None,
        1,
        1,
        RadioTraceRouteResolution::ExactRetainedPath,
        19,
        [0x22; 32],
        [0x33; 32],
    );
    let selected = cell
        .record_route_selected(200, route)
        .expect("route trace sequence");
    let terminal = RadioTraceAttemptTerminal::new(
        [0x33; 32],
        RadioTraceAttemptOutcome::Delivered,
        Some(RadioTraceProofIngress::new(1, Some((-101, 2)))),
    );
    let completed = cell
        .record_attempt_terminal(300, terminal)
        .expect("terminal trace sequence");

    assert_eq!(
        (rx.sequence(), selected.sequence(), completed.sequence()),
        (1, 2, 3)
    );
    let page = cell.radio_trace_page(None);
    assert_eq!(page.boot_sequence(), 41);
    let events = page.events().collect::<std::vec::Vec<_>>();
    let RadioTraceEventKind::LogicalRx(rx) = events[0].kind() else {
        panic!("first event must be logical RX")
    };
    assert_eq!(rx.packet_len(), 19);
    assert!(rx.rns_packet_hash().is_some());
    assert_eq!(rx.rssi_dbm(), -98);
    assert_eq!(events[1].kind(), RadioTraceEventKind::RouteSelected(route));
    assert_eq!(
        events[2].kind(),
        RadioTraceEventKind::AttemptTerminal(terminal)
    );
}

#[test]
fn terminal_classification_separates_access_rejection_from_failures() {
    assert_eq!(
        classify_dispatch_outcome(DispatchOutcome::Transmitted),
        RadioTxTerminalOutcome::Transmitted
    );
    assert_eq!(
        classify_dispatch_outcome(DispatchOutcome::AccessRejected(
            LogicalPacketAccessRejection::TimestampOverflow {
                start_us: 1,
                interval_us: 2,
            }
        )),
        RadioTxTerminalOutcome::AccessRejected
    );
    assert_eq!(
        classify_dispatch_outcome(DispatchOutcome::PermitDenied),
        RadioTxTerminalOutcome::PermitDenied
    );
}

#[test]
fn terminal_counters_partition_success_access_rejection_and_failure() {
    let cell = RadioDiagnosticsCell::new(E290_NA915_DEFAULT_CONFIGURATION);
    cell.update(|state| {
        state.transmit.record_terminal(
            RadioTxTerminalOutcome::Transmitted,
            RadioTxDispatchFamily::Ordinary,
            None,
            2,
            10,
        );
        state.transmit.record_terminal(
            RadioTxTerminalOutcome::AccessRejected,
            RadioTxDispatchFamily::Ordinary,
            None,
            0,
            20,
        );
        state.transmit.record_terminal(
            RadioTxTerminalOutcome::PermitDenied,
            RadioTxDispatchFamily::Ordinary,
            None,
            0,
            30,
        );
    });

    let transmit = cell.snapshot().transmit;
    assert_eq!(transmit.terminal_jobs, 3);
    assert_eq!(transmit.transmitted_jobs, 1);
    assert_eq!(transmit.access_rejected_jobs, 1);
    assert_eq!(transmit.failed_jobs, 1);
    assert_eq!(transmit.completed_physical_frames, 2);
    assert_eq!(transmit.last_terminal_at_us(), Some(30));
    assert_eq!(
        transmit.last_outcome(),
        Some(RadioTxTerminalOutcome::PermitDenied)
    );
    assert_eq!(
        transmit.last_family(),
        Some(RadioTxDispatchFamily::Ordinary)
    );
}

#[test]
fn ordinary_terminal_report_does_not_replace_last_data_packet_evidence() {
    let cell = RadioDiagnosticsCell::new(E290_NA915_DEFAULT_CONFIGURATION);
    let data = LastDataTxObservation {
        observed_at_plus_one_us: 11,
        outcome: RadioTxTerminalOutcome::AccessRejected,
        interface: PacketInterfaceId::new(7),
        packet_len: 183,
        encoded_packet_sha256: EncodedPacketSha256::new([0xab; 32]),
    };
    cell.update(|state| {
        state.transmit.record_data(data);
        state.transmit.record_terminal(
            RadioTxTerminalOutcome::Transmitted,
            RadioTxDispatchFamily::Ordinary,
            None,
            1,
            20,
        );
    });

    let transmit = cell.snapshot().transmit;
    assert_eq!(transmit.last_terminal_at_us(), Some(20));
    assert_eq!(
        transmit.last_outcome(),
        Some(RadioTxTerminalOutcome::Transmitted)
    );
    assert_eq!(
        transmit.last_family(),
        Some(RadioTxDispatchFamily::Ordinary)
    );
    assert_eq!(transmit.last_data(), Some(data));
}

#[test]
fn counters_saturate_and_state_transitions_remain_copy_coherent() {
    let cell = RadioDiagnosticsCell::new(E290_NA915_DEFAULT_CONFIGURATION);
    cell.update(|state| {
        state.invalid_physical_frames = u64::MAX;
        state.ingress.enqueued = u64::MAX;
        state.cad.busy = u64::MAX;
        state.transmit.terminal_jobs = u64::MAX;
        state.transmit.completed_physical_frames = u64::MAX;
    });

    cell.record_invalid_physical_frame();
    cell.record_ingress_enqueued();
    cell.record_cad(true);
    cell.update(|state| {
        state.transmit.record_terminal(
            RadioTxTerminalOutcome::Transmitted,
            RadioTxDispatchFamily::Ordinary,
            None,
            2,
            99,
        );
    });
    cell.mark_online();
    assert_eq!(cell.snapshot().state, RadioRuntimeState::Online);
    cell.mark_faulted();

    let snapshot = cell.snapshot();
    assert_eq!(snapshot.state, RadioRuntimeState::Faulted);
    assert_eq!(snapshot.invalid_physical_frames, u64::MAX);
    assert_eq!(snapshot.ingress.enqueued, u64::MAX);
    assert_eq!(snapshot.cad.busy, u64::MAX);
    assert_eq!(snapshot.transmit.terminal_jobs, u64::MAX);
    assert_eq!(snapshot.transmit.completed_physical_frames, u64::MAX);
    assert_eq!(snapshot.transmit.transmitted_jobs, 1);
    assert_eq!(snapshot.transmit.last_terminal_at_us(), Some(99));
    assert_eq!(
        snapshot.transmit.last_outcome(),
        Some(RadioTxTerminalOutcome::Transmitted)
    );
}

#[test]
fn snapshot_stays_within_the_fixed_copy_budget() {
    assert!(mem::size_of::<RadioDiagnosticsSnapshot>() <= MAXIMUM_RADIO_DIAGNOSTICS_SNAPSHOT_BYTES);
}
