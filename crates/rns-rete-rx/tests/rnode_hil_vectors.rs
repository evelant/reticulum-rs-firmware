use core::num::NonZeroU64;

use rand_core::{CryptoRng, RngCore};
use reticulum_radio_interface::{
    FrameSignal, RNODE_LORA_DATA_PER_FRAME, RNODE_LORA_SPLIT_FLAG, RNS_MTU, RawReceivedFrame,
    SX1262_FRAME_MTU, TimedReceiveError,
};
use reticulum_rns_rete::{DestType, IngressDisposition, IngressDropReason};
use reticulum_rns_rete_rx::{
    ReceiveOnlyClockSample, ReceiveOnlyIngressOutcome, ReceiveOnlyInterfaceId, ReceiveOnlyRete,
    ReceiveOnlyWake, receive_only_identity_from_private_key,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const CORPUS_JSON: &str = include_str!("../../../interop/vectors/rnode-hil-v1.json");
const FRAGMENT_TIMEOUT_TICKS: NonZeroU64 = NonZeroU64::new(5_000).unwrap();
const MAINTENANCE_INTERVAL_TICKS: NonZeroU64 = NonZeroU64::new(u64::MAX).unwrap();
const SIGNAL: FrameSignal = FrameSignal::new(-90, 5);

#[derive(Debug, Deserialize)]
struct Corpus {
    schema: u64,
    scenarios: Vec<Scenario>,
}

#[derive(Debug, Deserialize)]
struct Scenario {
    name: String,
    steps: Vec<Step>,
    unstalled_reference_deltas: ExpectedDeltas,
    required_target_feature: Option<String>,
    target_expectations: Option<TargetExpectations>,
}

#[derive(Debug, Deserialize)]
struct Step {
    mode: StepMode,
    payload_hex: String,
    payload_len: usize,
    payload_sha256: String,
    wait_after: WaitAfter,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StepMode {
    RnodePacket,
    RawLoraFrame,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WaitAfter {
    Fixed { milliseconds: u64 },
    ReceiverFragmentTimeout { margin_ms: u64 },
}

#[derive(Debug, Deserialize)]
struct ExpectedDeltas {
    completed_packets: Vec<ExpectedPacket>,
    pending_started: u64,
    pending_replaced: u64,
    pending_discarded: u64,
    pending_expired: u64,
    packets_too_long: u64,
}

#[derive(Debug, Deserialize)]
struct ExpectedPacket {
    packet_len: usize,
    packet_sha256: Option<String>,
    rns_admitted: bool,
    rete_disposition: ExpectedDisposition,
}

#[derive(Debug, Deserialize)]
struct InitialBootExpectation {
    reset_reason: String,
    retained_streak: u64,
    retained_total: u64,
    retained_pending: bool,
    next_action: String,
    radio_activation_follows: bool,
}

#[derive(Debug, Deserialize)]
struct PostFaultBootExpectation {
    after_invocation: u64,
    reset_reason: String,
    retained_streak: u64,
    retained_total: u64,
    retained_pending: bool,
    pending_fault_acknowledged_without_increment: bool,
    next_action: String,
    radio_activation_follows: bool,
    quarantine_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum TargetExpectations {
    Backpressure {
        artifact_mode: String,
        trigger: String,
        peer_frames_observed: u64,
        offered_during_stall: u64,
        queued_during_stall: u64,
        dropped_during_stall: u64,
        pending_expired: u64,
        queued_frames_rejected_by_expiry_watermark: u64,
        completed_packets: u64,
        rete_ingress_calls: u64,
        tracker_transmissions: u64,
    },
    ReturnedFault {
        artifact_mode: String,
        trigger: String,
        policy: String,
        peer_frames_observed: u64,
        set_rx_forwarded: bool,
        get_irq_status_rejected_before_spi: bool,
        evidence_fired: bool,
        fault_phase: String,
        fault_operation: String,
        fault_primary: String,
        fault_cleanup: String,
        retained_fault_committed_before_reset: bool,
        core_software_reset: bool,
        post_reset_radio_constructed: bool,
        post_reset_spi_constructed: bool,
        post_reset_supervisor_watchdog_enabled: bool,
        completed_packets: u64,
        rete_ingress_calls: u64,
        tracker_transmissions: u64,
    },
    ReturnedFaultRepeatUntilQuarantine {
        artifact_mode: String,
        trigger: String,
        policy: String,
        same_powered_session: bool,
        invocations_required: u64,
        peer_frames_per_invocation: u64,
        peer_frames_observed: u64,
        set_rx_forwarded_each_fault: bool,
        get_irq_status_rejected_before_spi_each_fault: bool,
        evidence_fired_each_fault: bool,
        fault_phase: String,
        fault_operation: String,
        fault_primary: String,
        fault_cleanup: String,
        retained_fault_committed_before_each_reset: bool,
        initial_boot: InitialBootExpectation,
        post_fault_boots: Vec<PostFaultBootExpectation>,
        core_software_resets: u64,
        radio_activations: u64,
        quarantine_before_fourth_radio_activation: bool,
        quarantine_radio_constructed: bool,
        quarantine_spi_constructed: bool,
        quarantine_supervisor_watchdog_enabled: bool,
        completed_packets: u64,
        rete_ingress_calls: u64,
        tracker_transmissions: u64,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ExpectedDisposition {
    Processed,
    Duplicate,
    Invalid,
    NoObservableOutcome,
    InvalidLinkRequest,
    RnsPacketTooLong,
}

#[derive(Default)]
struct CounterRng(u8);

impl RngCore for CounterRng {
    fn next_u32(&mut self) -> u32 {
        let mut bytes = [0; 4];
        self.fill_bytes(&mut bytes);
        u32::from_le_bytes(bytes)
    }

    fn next_u64(&mut self) -> u64 {
        let mut bytes = [0; 8];
        self.fill_bytes(&mut bytes);
        u64::from_le_bytes(bytes)
    }

    fn fill_bytes(&mut self, destination: &mut [u8]) {
        for byte in destination {
            self.0 = self.0.wrapping_add(1);
            *byte = self.0;
        }
    }

    fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(destination);
        Ok(())
    }
}

impl CryptoRng for CounterRng {}

#[test]
fn committed_rnode_hil_corpus_replays_through_receive_only_ingress() {
    let corpus: Corpus = serde_json::from_str(CORPUS_JSON).expect("valid RNode HIL corpus");
    assert_eq!(corpus.schema, 3);
    assert!(!corpus.scenarios.is_empty());

    for scenario in corpus.scenarios {
        replay_scenario(&scenario);
    }
}

fn assert_target_expectations(scenario: &Scenario) {
    match (
        scenario.required_target_feature.as_deref(),
        scenario.target_expectations.as_ref(),
    ) {
        (None, None) => {}
        (
            Some("lab-rx-backpressure"),
            Some(TargetExpectations::Backpressure {
                artifact_mode,
                trigger,
                peer_frames_observed,
                offered_during_stall,
                queued_during_stall,
                dropped_during_stall,
                pending_expired,
                queued_frames_rejected_by_expiry_watermark,
                completed_packets,
                rete_ingress_calls,
                tracker_transmissions,
            }),
        ) => {
            assert_eq!(scenario.name, "raw-backpressure-four-frame");
            assert_eq!(artifact_mode, "lab-rx-backpressure-hil");
            assert_eq!(trigger, "first_awaiting_continuation");
            assert_eq!(*peer_frames_observed, 4);
            assert_eq!(*offered_during_stall, 3);
            assert_eq!(*queued_during_stall, 2);
            assert_eq!(*dropped_during_stall, 1);
            assert_eq!(*pending_expired, 1);
            assert_eq!(*queued_frames_rejected_by_expiry_watermark, 2);
            assert_eq!(*completed_packets, 0);
            assert_eq!(*rete_ingress_calls, 0);
            assert_eq!(*tracker_transmissions, 0);
        }
        (
            Some("lab-rx-returned-fault-hil"),
            Some(TargetExpectations::ReturnedFault {
                artifact_mode,
                trigger,
                policy,
                peer_frames_observed,
                set_rx_forwarded,
                get_irq_status_rejected_before_spi,
                evidence_fired,
                fault_phase,
                fault_operation,
                fault_primary,
                fault_cleanup,
                retained_fault_committed_before_reset,
                core_software_reset,
                post_reset_radio_constructed,
                post_reset_spi_constructed,
                post_reset_supervisor_watchdog_enabled,
                completed_packets,
                rete_ingress_calls,
                tracker_transmissions,
            }),
        ) => {
            assert_eq!(scenario.name, "raw-returned-fault-trigger");
            assert_eq!(
                artifact_mode,
                "lab-rx-returned-fault-hil;trigger=get-irq-status-after-set-rx;policy=one-boot"
            );
            assert_eq!(trigger, "get-irq-status-after-set-rx");
            assert_eq!(policy, "one-boot");
            assert_eq!(*peer_frames_observed, 1);
            assert!(*set_rx_forwarded);
            assert!(*get_irq_status_rejected_before_spi);
            assert!(*evidence_fired);
            assert_eq!(fault_phase, "receive");
            assert_eq!(fault_operation, "receive");
            assert_eq!(fault_primary, "radio_spi");
            assert_eq!(fault_cleanup, "none");
            assert!(*retained_fault_committed_before_reset);
            assert!(*core_software_reset);
            assert!(!*post_reset_radio_constructed);
            assert!(!*post_reset_spi_constructed);
            assert!(!*post_reset_supervisor_watchdog_enabled);
            assert_eq!(*completed_packets, 0);
            assert_eq!(*rete_ingress_calls, 0);
            assert_eq!(*tracker_transmissions, 0);
        }
        (
            Some("lab-rx-returned-fault-hil"),
            Some(TargetExpectations::ReturnedFaultRepeatUntilQuarantine {
                artifact_mode,
                trigger,
                policy,
                same_powered_session,
                invocations_required,
                peer_frames_per_invocation,
                peer_frames_observed,
                set_rx_forwarded_each_fault,
                get_irq_status_rejected_before_spi_each_fault,
                evidence_fired_each_fault,
                fault_phase,
                fault_operation,
                fault_primary,
                fault_cleanup,
                retained_fault_committed_before_each_reset,
                initial_boot,
                post_fault_boots,
                core_software_resets,
                radio_activations,
                quarantine_before_fourth_radio_activation,
                quarantine_radio_constructed,
                quarantine_spi_constructed,
                quarantine_supervisor_watchdog_enabled,
                completed_packets,
                rete_ingress_calls,
                tracker_transmissions,
            }),
        ) => {
            assert_eq!(scenario.name, "raw-returned-fault-repeat-until-quarantine");
            assert_eq!(
                artifact_mode,
                "lab-rx-returned-fault-hil;trigger=get-irq-status-after-set-rx;policy=repeat-until-quarantine"
            );
            assert_eq!(trigger, "get-irq-status-after-set-rx");
            assert_eq!(policy, "repeat-until-quarantine");
            assert!(*same_powered_session);
            assert_eq!(*invocations_required, 3);
            assert_eq!(*peer_frames_per_invocation, 1);
            assert_eq!(*peer_frames_observed, 3);
            assert!(*set_rx_forwarded_each_fault);
            assert!(*get_irq_status_rejected_before_spi_each_fault);
            assert!(*evidence_fired_each_fault);
            assert_eq!(fault_phase, "receive");
            assert_eq!(fault_operation, "receive");
            assert_eq!(fault_primary, "radio_spi");
            assert_eq!(fault_cleanup, "none");
            assert!(*retained_fault_committed_before_each_reset);

            assert_eq!(initial_boot.reset_reason, "chip_power_on");
            assert_eq!(initial_boot.retained_streak, 0);
            assert_eq!(initial_boot.retained_total, 0);
            assert!(!initial_boot.retained_pending);
            assert_eq!(initial_boot.next_action, "arm_radio");
            assert!(initial_boot.radio_activation_follows);

            let expected_boots = [
                (1, 1, 1, "rearm_radio", true, None),
                (2, 2, 2, "rearm_radio", true, None),
                (3, 3, 3, "quarantine", false, Some("fault_streak")),
            ];
            assert_eq!(post_fault_boots.len(), expected_boots.len());
            for (boot, expected) in post_fault_boots.iter().zip(expected_boots) {
                assert_eq!(boot.after_invocation, expected.0);
                assert_eq!(boot.reset_reason, "core_software");
                assert_eq!(boot.retained_streak, expected.1);
                assert_eq!(boot.retained_total, expected.2);
                assert!(!boot.retained_pending);
                assert!(boot.pending_fault_acknowledged_without_increment);
                assert_eq!(boot.next_action, expected.3);
                assert_eq!(boot.radio_activation_follows, expected.4);
                assert_eq!(boot.quarantine_reason.as_deref(), expected.5);
            }

            assert_eq!(*core_software_resets, 3);
            assert_eq!(*radio_activations, 3);
            assert!(*quarantine_before_fourth_radio_activation);
            assert!(!*quarantine_radio_constructed);
            assert!(!*quarantine_spi_constructed);
            assert!(!*quarantine_supervisor_watchdog_enabled);
            assert_eq!(*completed_packets, 0);
            assert_eq!(*rete_ingress_calls, 0);
            assert_eq!(*tracker_transmissions, 0);
        }
        (feature, expectations) => panic!(
            "{}: unsupported target expectation binding {feature:?}/{expectations:?}",
            scenario.name
        ),
    }
}

fn replay_scenario(scenario: &Scenario) {
    assert_target_expectations(scenario);
    let identity_seed = Sha256::digest(format!("rnode-hil-corpus:{}", scenario.name).as_bytes());
    let mut private_key = [0; 64];
    private_key[..32].copy_from_slice(&identity_seed);
    private_key[32..].copy_from_slice(&identity_seed);
    let mut ingress = ReceiveOnlyRete::<16, 4, 32, 2>::new(
        receive_only_identity_from_private_key(&private_key).unwrap(),
        "reticulum-rs-firmware",
        &["rnode-hil-corpus"],
        FRAGMENT_TIMEOUT_TICKS,
        0,
        MAINTENANCE_INTERVAL_TICKS,
        ReceiveOnlyInterfaceId(7),
    )
    .unwrap();
    let mut rng = CounterRng::default();
    let mut now_ticks = 1_u64;
    let mut physical_frames = 0_u64;
    let mut completed_packets = 0_usize;
    let mut expected_node = ExpectedNodeCounters::default();

    for (step_index, step) in scenario.steps.iter().enumerate() {
        let payload = hex::decode(&step.payload_hex).unwrap_or_else(|error| {
            panic!("{} step {step_index}: invalid hex: {error}", scenario.name)
        });
        assert_eq!(
            payload.len(),
            step.payload_len,
            "{} step {step_index}: payload length",
            scenario.name
        );
        assert_eq!(
            hex::encode(Sha256::digest(&payload)),
            step.payload_sha256,
            "{} step {step_index}: payload SHA-256",
            scenario.name
        );

        let frames = physical_frames_for_step(step.mode, &payload, step_index);
        for frame in frames {
            physical_frames += 1;
            let raw = raw_received_frame(&frame, now_ticks);
            let result = ingress.on_wake(ReceiveOnlyWake::Frame(&raw), clock(now_ticks), &mut rng);
            assert!(
                result.expired_fragment.is_none(),
                "{} step {step_index}: fragment expired before its explicit timeout wait",
                scenario.name
            );
            assert!(
                result.maintenance.is_none(),
                "{} step {step_index}: maintenance unexpectedly ran",
                scenario.name
            );
            let outcome = result.frame.expect("frame wake must have an outcome");
            if matches!(
                outcome,
                ReceiveOnlyIngressOutcome::Packet { .. }
                    | ReceiveOnlyIngressOutcome::Rejected(
                        TimedReceiveError::RnsPacketTooLong { .. }
                    )
            ) {
                let expected = scenario
                    .unstalled_reference_deltas
                    .completed_packets
                    .get(completed_packets)
                    .unwrap_or_else(|| {
                        panic!(
                            "{} step {step_index}: unexpected completed packet {completed_packets}",
                            scenario.name
                        )
                    });
                if let Some(disposition) =
                    assert_completed_packet(scenario, completed_packets, expected, outcome)
                {
                    expected_node.record(&disposition);
                }
                completed_packets += 1;
            } else {
                assert!(
                    matches!(
                        outcome,
                        ReceiveOnlyIngressOutcome::AwaitingContinuation { .. }
                    ),
                    "{} step {step_index}: unexpected ingress outcome {outcome:?}",
                    scenario.name
                );
            }
            now_ticks = now_ticks.saturating_add(1);
        }

        match step.wait_after {
            WaitAfter::Fixed { milliseconds } => {
                assert!(
                    milliseconds < FRAGMENT_TIMEOUT_TICKS.get(),
                    "{} step {step_index}: fixed wait would implicitly expire a fragment",
                    scenario.name
                );
                now_ticks = now_ticks.saturating_add(milliseconds);
            }
            WaitAfter::ReceiverFragmentTimeout { margin_ms } => {
                let deadline = ingress.fragment_deadline_ticks().unwrap_or_else(|| {
                    panic!(
                        "{} step {step_index}: timeout wait has no pending fragment",
                        scenario.name
                    )
                });
                let result = ingress.on_wake(ReceiveOnlyWake::Timer, clock(deadline), &mut rng);
                assert!(
                    result.expired_fragment.is_some(),
                    "{} step {step_index}: timer did not expire pending fragment",
                    scenario.name
                );
                assert!(result.frame.is_none());
                assert!(result.maintenance.is_none());
                now_ticks = deadline.saturating_add(margin_ms);
            }
        }
    }

    assert_eq!(
        completed_packets,
        scenario.unstalled_reference_deltas.completed_packets.len(),
        "{}: completed packet count",
        scenario.name
    );
    assert!(
        ingress.fragment_deadline_ticks().is_none(),
        "{}: scenario left a fragment pending",
        scenario.name
    );

    let metrics = ingress.metrics();
    let expected = &scenario.unstalled_reference_deltas;
    assert_eq!(
        metrics.frames_handed_off, physical_frames,
        "{}",
        scenario.name
    );
    assert_eq!(
        metrics.receive.frames_seen, physical_frames,
        "{}",
        scenario.name
    );
    assert_eq!(
        metrics.receive.packets_completed,
        u64::try_from(expected.completed_packets.len()).unwrap(),
        "{}",
        scenario.name
    );
    assert_eq!(
        metrics.receive.packets_accepted,
        expected
            .completed_packets
            .iter()
            .filter(|packet| packet.rns_admitted)
            .count() as u64,
        "{}",
        scenario.name
    );
    assert_eq!(
        metrics.receive.pending_started, expected.pending_started,
        "{}",
        scenario.name
    );
    assert_eq!(
        metrics.receive.pending_replaced, expected.pending_replaced,
        "{}",
        scenario.name
    );
    assert_eq!(
        metrics.receive.pending_discarded, expected.pending_discarded,
        "{}",
        scenario.name
    );
    assert_eq!(
        metrics.receive.pending_expired, expected.pending_expired,
        "{}",
        scenario.name
    );
    assert_eq!(
        metrics.receive.packets_too_long, expected.packets_too_long,
        "{}",
        scenario.name
    );
    assert_eq!(metrics.receive.framing_errors, 0, "{}", scenario.name);
    assert_eq!(metrics.raw_frames_dropped, 0, "{}", scenario.name);
    assert_eq!(
        metrics.rete_ingress_calls, metrics.receive.packets_accepted,
        "{}",
        scenario.name
    );
    let expected_last_sha256 = expected
        .completed_packets
        .iter()
        .rev()
        .find(|packet| packet.rns_admitted)
        .map(|packet| {
            let decoded = hex::decode(packet.packet_sha256.as_ref().unwrap()).unwrap();
            <[u8; 32]>::try_from(decoded).unwrap()
        });
    assert_eq!(
        metrics.last_raw_packet_sha256, expected_last_sha256,
        "{}: last heartbeat packet digest",
        scenario.name
    );
    assert_eq!(
        metrics.node.ingress.seen, expected_node.seen,
        "{}",
        scenario.name
    );
    assert_eq!(
        metrics.node.ingress.admitted, expected_node.admitted,
        "{}",
        scenario.name
    );
    assert_eq!(
        metrics.node.ingress.rejected, expected_node.rejected,
        "{}",
        scenario.name
    );
    assert_eq!(
        metrics.node.ingress.native_duplicate, expected_node.native_duplicate,
        "{}",
        scenario.name
    );
    assert_eq!(
        metrics.node.ingress.native_invalid, expected_node.native_invalid,
        "{}",
        scenario.name
    );
    assert_eq!(
        metrics.node.ingress.native_no_outcome, expected_node.native_no_outcome,
        "{}",
        scenario.name
    );
    assert_eq!(
        metrics.node.ingress.malformed, expected_node.malformed,
        "{}",
        scenario.name
    );
    assert_eq!(
        metrics.node.ingress.invalid_link_request, expected_node.invalid_link_request,
        "{}",
        scenario.name
    );
}

#[derive(Default)]
struct ExpectedNodeCounters {
    seen: u64,
    admitted: u64,
    rejected: u64,
    native_duplicate: u64,
    native_invalid: u64,
    native_no_outcome: u64,
    malformed: u64,
    invalid_link_request: u64,
}

impl ExpectedNodeCounters {
    fn record(&mut self, disposition: &IngressDisposition) {
        self.seen += 1;
        match disposition {
            IngressDisposition::Processed => self.admitted += 1,
            IngressDisposition::NativeDuplicate => {
                self.admitted += 1;
                self.native_duplicate += 1;
            }
            IngressDisposition::NativeInvalid => {
                self.admitted += 1;
                self.native_invalid += 1;
            }
            IngressDisposition::NoObservableOutcome => {
                self.admitted += 1;
                self.native_no_outcome += 1;
            }
            IngressDisposition::Rejected(reason) => {
                self.rejected += 1;
                match reason {
                    IngressDropReason::Malformed(_) => self.malformed += 1,
                    IngressDropReason::LinkRequestDestinationType(_)
                    | IngressDropReason::LinkRequestContext(_)
                    | IngressDropReason::LinkRequestPayloadLength(_)
                    | IngressDropReason::DestinationDoesNotAcceptLinks => {
                        self.invalid_link_request += 1;
                    }
                    _ => {}
                }
            }
        }
    }
}

fn physical_frames_for_step(mode: StepMode, payload: &[u8], step_index: usize) -> Vec<Vec<u8>> {
    match mode {
        StepMode::RawLoraFrame => vec![payload.to_vec()],
        StepMode::RnodePacket => {
            assert!(
                payload.len() <= 508,
                "ordinary RNode payload exceeds hardware MTU"
            );
            let sequence = u8::try_from(step_index % 16).unwrap() << 4;
            if payload.len() <= RNODE_LORA_DATA_PER_FRAME {
                let mut frame = Vec::with_capacity(payload.len() + 1);
                frame.push(sequence);
                frame.extend_from_slice(payload);
                vec![frame]
            } else {
                let header = sequence | RNODE_LORA_SPLIT_FLAG;
                let mut first = Vec::with_capacity(SX1262_FRAME_MTU);
                first.push(header);
                first.extend_from_slice(&payload[..RNODE_LORA_DATA_PER_FRAME]);
                let mut second = Vec::with_capacity(payload.len() - RNODE_LORA_DATA_PER_FRAME + 1);
                second.push(header);
                second.extend_from_slice(&payload[RNODE_LORA_DATA_PER_FRAME..]);
                vec![first, second]
            }
        }
    }
}

fn raw_received_frame(frame: &[u8], now_ticks: u64) -> RawReceivedFrame {
    assert!(frame.len() <= SX1262_FRAME_MTU);
    let mut bytes = [0; SX1262_FRAME_MTU];
    bytes[..frame.len()].copy_from_slice(frame);
    RawReceivedFrame::new(bytes, u8::try_from(frame.len()).unwrap(), SIGNAL, now_ticks)
}

fn clock(ticks: u64) -> ReceiveOnlyClockSample {
    ReceiveOnlyClockSample {
        ticks,
        transport_seconds: ticks / 1_000,
    }
}

fn assert_completed_packet(
    scenario: &Scenario,
    packet_index: usize,
    expected: &ExpectedPacket,
    outcome: ReceiveOnlyIngressOutcome,
) -> Option<IngressDisposition> {
    match outcome {
        ReceiveOnlyIngressOutcome::Packet {
            packet_len,
            raw_packet_sha256,
            disposition,
            ..
        } => {
            assert!(
                expected.rns_admitted,
                "{} packet {packet_index}",
                scenario.name
            );
            assert_eq!(
                packet_len, expected.packet_len,
                "{} packet {packet_index}",
                scenario.name
            );
            let expected_sha256 = expected.packet_sha256.as_deref().unwrap_or_else(|| {
                panic!(
                    "{} packet {packet_index}: admitted packet has no hash",
                    scenario.name
                )
            });
            assert_eq!(
                hex::encode(raw_packet_sha256),
                expected_sha256,
                "{} packet {packet_index}: reassembled packet SHA-256",
                scenario.name
            );
            assert_disposition(
                scenario,
                packet_index,
                expected.rete_disposition,
                disposition.clone(),
            );
            Some(disposition)
        }
        ReceiveOnlyIngressOutcome::Rejected(TimedReceiveError::RnsPacketTooLong {
            actual,
            maximum,
        }) => {
            assert!(
                !expected.rns_admitted,
                "{} packet {packet_index}",
                scenario.name
            );
            assert_eq!(
                actual, expected.packet_len,
                "{} packet {packet_index}",
                scenario.name
            );
            assert_eq!(maximum, RNS_MTU, "{} packet {packet_index}", scenario.name);
            assert!(
                expected.packet_sha256.is_none(),
                "{} packet {packet_index}",
                scenario.name
            );
            assert_eq!(
                expected.rete_disposition,
                ExpectedDisposition::RnsPacketTooLong,
                "{} packet {packet_index}",
                scenario.name
            );
            None
        }
        other => panic!(
            "{} packet {packet_index}: unexpected completed outcome {other:?}",
            scenario.name
        ),
    }
}

fn assert_disposition(
    scenario: &Scenario,
    packet_index: usize,
    expected: ExpectedDisposition,
    actual: IngressDisposition,
) {
    let matches = match expected {
        ExpectedDisposition::Processed => actual == IngressDisposition::Processed,
        ExpectedDisposition::Duplicate => actual == IngressDisposition::NativeDuplicate,
        // The corpus deliberately uses `invalid` as the common class for a
        // native parser rejection and a more specific owning-boundary guard.
        ExpectedDisposition::Invalid => matches!(
            actual,
            IngressDisposition::NativeInvalid | IngressDisposition::Rejected(_)
        ),
        ExpectedDisposition::NoObservableOutcome => {
            actual == IngressDisposition::NoObservableOutcome
        }
        ExpectedDisposition::InvalidLinkRequest => {
            actual
                == IngressDisposition::Rejected(IngressDropReason::LinkRequestDestinationType(
                    DestType::Group,
                ))
        }
        ExpectedDisposition::RnsPacketTooLong => false,
    };
    assert!(
        matches,
        "{} packet {packet_index}: expected {expected:?}, got {actual:?}",
        scenario.name
    );
}
