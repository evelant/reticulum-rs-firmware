extern crate std;

use core::{
    future::Future,
    pin::pin,
    task::{Context, Poll, Waker},
};

use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use reticulum_appliance_display_model::{
    DisplayCommand, DisplayCompositionState, DisplayHomeSnapshot, DisplayLabel, DisplaySetupState,
    DisplayState, DisplayViewKind, PairingPasskey, PairingWindowSeconds,
};

use super::{
    DisplayBootClearOutcome, DisplayCompletion, DisplayCompletionGateError, DisplayHandoff,
    DisplayHomeTelemetry, DisplayPublisher, DisplayReceiver, DisplayRenderOutcome,
    DisplayRequestIdExhausted, DisplayTelemetryHandoff, E290_DISPLAY_FRAMEBUFFER_CEILING_BYTES,
    E290_MONOCHROME_FRAMEBUFFER_BYTES, overlay_observed_home_telemetry,
};

fn label() -> DisplayLabel {
    DisplayLabel::new("Reticulum E290").expect("test label must fit")
}

fn home() -> DisplayHomeSnapshot {
    DisplayHomeSnapshot::new(
        label(),
        DisplayLabel::new("e13f88").expect("test suffix fits"),
        DisplaySetupState::Paired,
        DisplayCompositionState::Configured,
        DisplayCompositionState::Configured,
        DisplayCompositionState::Configured,
        DisplayCompositionState::Configured,
    )
}

fn show_home() -> DisplayCommand {
    DisplayCommand::ShowHome { snapshot: home() }
}

fn handoff() -> (
    DisplayPublisher<NoopRawMutex>,
    DisplayReceiver<NoopRawMutex>,
) {
    std::boxed::Box::leak(std::boxed::Box::new(DisplayHandoff::new())).split()
}

#[test]
fn home_telemetry_pressure_keeps_only_the_latest_complete_snapshot() {
    let (mut publisher, mut receiver) = std::boxed::Box::leak(std::boxed::Box::new(
        DisplayTelemetryHandoff::<NoopRawMutex>::new(),
    ))
    .split();
    publisher.publish_latest(DisplayHomeTelemetry::new(1));
    publisher.publish_latest(DisplayHomeTelemetry::new(7));
    publisher.publish_latest(DisplayHomeTelemetry::new(12));

    assert_eq!(
        receiver.try_take_latest(),
        Some(DisplayHomeTelemetry::new(12))
    );
    assert!(receiver.try_take_latest().is_none());

    publisher.publish_latest(DisplayHomeTelemetry::new(13));
    assert_eq!(
        embassy_futures::block_on(receiver.next()),
        DisplayHomeTelemetry::new(13)
    );
}

#[test]
fn absent_home_telemetry_preserves_the_durable_boot_count() {
    let durable = home().with_uncollected_messages(7);
    assert_eq!(
        overlay_observed_home_telemetry(durable, None).uncollected_messages(),
        7
    );
    assert_eq!(
        overlay_observed_home_telemetry(durable, Some(DisplayHomeTelemetry::new(0)))
            .uncollected_messages(),
        0
    );
}

#[test]
fn pressure_keeps_only_the_latest_complete_state() {
    let (mut publisher, mut receiver) = handoff();
    let first = publisher
        .publish_latest(DisplayCommand::ShowBooting { label: label() })
        .expect("first request ID");
    let second = publisher
        .publish_latest(show_home())
        .expect("second request ID");
    assert!(first < second);
    let mut newest = second;
    for passkey in 0..64 {
        let request_id = publisher
            .publish_latest(DisplayCommand::ShowPairing {
                label: label(),
                passkey: PairingPasskey::from_number(passkey).expect("six-digit passkey"),
                expires_after_seconds: PairingWindowSeconds::new(60)
                    .expect("nonzero pairing window"),
            })
            .expect("request ID");
        assert!(newest < request_id);
        newest = request_id;
    }

    let request = receiver
        .try_take_latest()
        .expect("one latest state must remain");
    assert_eq!(request.request_id(), newest);
    assert_eq!(request.requested_view(), DisplayViewKind::Pairing);
    let (request_id, command) = request.into_parts();
    assert_eq!(request_id, newest);
    let mut state = DisplayState::new();
    let _ = state.apply(command);
    state
        .view()
        .expose_pairing_passkey(|digits| assert_eq!(digits, Some(b"000063")));
    assert!(receiver.try_take_latest().is_none());
}

#[test]
fn paired_home_supersedes_a_queued_passkey() {
    let (mut publisher, mut receiver) = handoff();
    publisher
        .publish_latest(DisplayCommand::ShowPairing {
            label: label(),
            passkey: PairingPasskey::from_number(123_456).expect("six-digit passkey"),
            expires_after_seconds: PairingWindowSeconds::new(60).expect("nonzero pairing window"),
        })
        .expect("pairing request ID");
    let terminal_id = publisher
        .publish_latest(show_home())
        .expect("terminal request ID");

    let mut state = DisplayState::new();
    let terminal = receiver
        .try_take_latest()
        .expect("terminal state must replace pending passkey");
    assert_eq!(terminal.request_id(), terminal_id);
    let (_, command) = terminal.into_parts();
    let _ = state.apply(command);
    assert_eq!(state.view().kind(), DisplayViewKind::Home);
    assert_eq!(state.view().home(), Some(home()));
    assert!(!state.owns_pairing_secret());
    assert!(receiver.try_take_latest().is_none());
}

#[test]
fn rendered_and_faulted_completions_round_trip_without_secret_data() {
    let (mut publisher, mut receiver) = handoff();
    let home_id = publisher
        .publish_latest(show_home())
        .expect("home request ID");
    let request = receiver.try_take_latest().expect("home request");
    assert_eq!(request.request_id(), home_id);
    receiver.report_completion(DisplayCompletion::new(
        home_id,
        request.requested_view(),
        DisplayRenderOutcome::Rendered,
    ));
    drop(request);

    let rendered = publisher
        .try_take_completion()
        .expect("rendered completion");
    assert_eq!(rendered.request_id(), home_id);
    assert_eq!(rendered.view(), DisplayViewKind::Home);
    assert_eq!(rendered.outcome(), DisplayRenderOutcome::Rendered);
    assert!(publisher.try_take_completion().is_none());

    let booting_id = publisher
        .publish_latest(DisplayCommand::ShowBooting { label: label() })
        .expect("booting request ID");
    let request = receiver.try_take_latest().expect("booting request");
    receiver.report_completion(DisplayCompletion::new(
        booting_id,
        request.requested_view(),
        DisplayRenderOutcome::Faulted,
    ));
    drop(request);

    let faulted = embassy_futures::block_on(publisher.next_completion());
    assert_eq!(faulted.request_id(), booting_id);
    assert_eq!(faulted.view(), DisplayViewKind::Booting);
    assert_eq!(faulted.outcome(), DisplayRenderOutcome::Faulted);
}

#[test]
fn completion_pressure_keeps_only_the_latest_non_secret_report() {
    let (mut publisher, mut receiver) = handoff();
    let first = publisher
        .publish_latest(DisplayCommand::ShowBooting { label: label() })
        .expect("first request ID");
    let _ = receiver.try_take_latest().expect("first request");
    let second = publisher
        .publish_latest(show_home())
        .expect("second request ID");
    let _ = receiver.try_take_latest().expect("second request");

    receiver.report_completion(DisplayCompletion::new(
        first,
        DisplayViewKind::Booting,
        DisplayRenderOutcome::Rendered,
    ));
    receiver.report_completion(DisplayCompletion::new(
        second,
        DisplayViewKind::Home,
        DisplayRenderOutcome::Faulted,
    ));

    assert_eq!(
        publisher.try_take_completion(),
        Some(DisplayCompletion::new(
            second,
            DisplayViewKind::Home,
            DisplayRenderOutcome::Faulted,
        ))
    );
    assert!(publisher.try_take_completion().is_none());
}

#[test]
fn async_request_and_completion_waits_take_preexisting_values() {
    let (mut publisher, mut receiver) = handoff();
    let expected = publisher.publish_latest(show_home()).expect("request ID");
    let request = embassy_futures::block_on(receiver.next());
    assert_eq!(request.request_id(), expected);
    receiver.report_completion(DisplayCompletion::new(
        expected,
        request.requested_view(),
        DisplayRenderOutcome::Rendered,
    ));
    drop(request);
    assert_eq!(
        embassy_futures::block_on(publisher.next_completion()).request_id(),
        expected
    );
}

#[test]
fn exact_rendered_completion_satisfies_the_physical_gate() {
    let (mut publisher, mut receiver) = handoff();
    let expected = publisher.publish_latest(show_home()).expect("request ID");
    receiver.report_completion(DisplayCompletion::new(
        expected,
        DisplayViewKind::Home,
        DisplayRenderOutcome::Rendered,
    ));

    assert_eq!(
        embassy_futures::block_on(
            publisher.wait_for_rendered_completion(expected, DisplayViewKind::Home)
        ),
        Ok(DisplayCompletion::new(
            expected,
            DisplayViewKind::Home,
            DisplayRenderOutcome::Rendered,
        ))
    );
}

#[test]
fn physical_gate_ignores_stale_completion_before_exact_render() {
    let (mut publisher, mut receiver) = handoff();
    let stale = publisher
        .publish_latest(DisplayCommand::ShowBooting { label: label() })
        .expect("stale request ID");
    let expected = publisher
        .publish_latest(show_home())
        .expect("expected request ID");
    receiver.report_completion(DisplayCompletion::new(
        stale,
        DisplayViewKind::Booting,
        DisplayRenderOutcome::Rendered,
    ));

    let mut wait = pin!(publisher.wait_for_rendered_completion(expected, DisplayViewKind::Home));
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    assert!(matches!(wait.as_mut().poll(&mut context), Poll::Pending));

    receiver.report_completion(DisplayCompletion::new(
        expected,
        DisplayViewKind::Home,
        DisplayRenderOutcome::Rendered,
    ));
    assert_eq!(
        wait.as_mut().poll(&mut context),
        Poll::Ready(Ok(DisplayCompletion::new(
            expected,
            DisplayViewKind::Home,
            DisplayRenderOutcome::Rendered,
        )))
    );
}

#[test]
fn physical_gate_rejects_later_mismatched_and_faulted_completions() {
    let (mut publisher, mut receiver) = handoff();
    let expected = publisher
        .publish_latest(DisplayCommand::ShowBooting { label: label() })
        .expect("expected request ID");
    let later = publisher
        .publish_latest(show_home())
        .expect("later request ID");
    receiver.report_completion(DisplayCompletion::new(
        later,
        DisplayViewKind::Home,
        DisplayRenderOutcome::Rendered,
    ));
    assert_eq!(
        embassy_futures::block_on(
            publisher.wait_for_rendered_completion(expected, DisplayViewKind::Booting)
        ),
        Err(DisplayCompletionGateError::Superseded {
            expected,
            observed: later,
        })
    );

    let (mut publisher, mut receiver) = handoff();
    let expected = publisher
        .publish_latest(show_home())
        .expect("expected request ID");
    receiver.report_completion(DisplayCompletion::new(
        expected,
        DisplayViewKind::Booting,
        DisplayRenderOutcome::Rendered,
    ));
    assert_eq!(
        embassy_futures::block_on(
            publisher.wait_for_rendered_completion(expected, DisplayViewKind::Home)
        ),
        Err(DisplayCompletionGateError::ViewMismatch {
            expected: DisplayViewKind::Home,
            observed: DisplayViewKind::Booting,
        })
    );

    let (mut publisher, mut receiver) = handoff();
    let expected = publisher
        .publish_latest(show_home())
        .expect("expected request ID");
    receiver.report_completion(DisplayCompletion::new(
        expected,
        DisplayViewKind::Home,
        DisplayRenderOutcome::Faulted,
    ));
    assert_eq!(
        embassy_futures::block_on(
            publisher.wait_for_rendered_completion(expected, DisplayViewKind::Home)
        ),
        Err(DisplayCompletionGateError::Faulted {
            request_id: expected,
            view: DisplayViewKind::Home,
        })
    );
}

#[test]
fn boot_clear_ready_and_faulted_acknowledgements_are_bounded() {
    let (mut publisher, mut receiver) = handoff();
    assert!(publisher.try_take_boot_clear().is_none());

    receiver.report_boot_clear(DisplayBootClearOutcome::Faulted);
    receiver.report_boot_clear(DisplayBootClearOutcome::Ready);
    assert_eq!(
        publisher.try_take_boot_clear(),
        Some(DisplayBootClearOutcome::Ready)
    );
    assert!(publisher.try_take_boot_clear().is_none());

    receiver.report_boot_clear(DisplayBootClearOutcome::Faulted);
    assert_eq!(
        embassy_futures::block_on(publisher.wait_for_boot_clear()),
        DisplayBootClearOutcome::Faulted
    );
}

#[test]
fn boot_clear_acknowledgement_is_independent_of_requests_and_completions() {
    let (mut publisher, mut receiver) = handoff();
    let request_id = publisher.publish_latest(show_home()).expect("request ID");
    receiver.report_boot_clear(DisplayBootClearOutcome::Ready);
    receiver.report_completion(DisplayCompletion::new(
        request_id,
        DisplayViewKind::Home,
        DisplayRenderOutcome::Rendered,
    ));

    assert_eq!(
        publisher.try_take_boot_clear(),
        Some(DisplayBootClearOutcome::Ready)
    );
    assert_eq!(
        publisher
            .try_take_completion()
            .expect("completion")
            .request_id(),
        request_id
    );
    assert_eq!(
        receiver.try_take_latest().expect("request").request_id(),
        request_id
    );
}

#[test]
fn request_id_exhaustion_rejects_without_replacing_the_pending_request() {
    let (mut publisher, mut receiver) = handoff();
    publisher.next_request_id = Some(u64::MAX);
    let final_id = publisher
        .publish_latest(show_home())
        .expect("last representable request ID");
    assert_eq!(final_id.sequence(), u64::MAX);

    assert_eq!(
        publisher.publish_latest(DisplayCommand::ShowPairing {
            label: label(),
            passkey: PairingPasskey::from_number(654_321).expect("six-digit passkey"),
            expires_after_seconds: PairingWindowSeconds::new(60).expect("nonzero pairing window"),
        }),
        Err(DisplayRequestIdExhausted)
    );

    let retained = receiver
        .try_take_latest()
        .expect("rejection must preserve the prior request");
    assert_eq!(retained.request_id(), final_id);
    assert_eq!(retained.requested_view(), DisplayViewKind::Home);
    assert!(receiver.try_take_latest().is_none());
}

#[test]
fn e290_framebuffer_exactly_meets_the_product_ceiling() {
    assert_eq!(E290_MONOCHROME_FRAMEBUFFER_BYTES, 4_736);
    assert_eq!(E290_DISPLAY_FRAMEBUFFER_CEILING_BYTES, 4_736);
}
