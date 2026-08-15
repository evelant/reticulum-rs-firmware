extern crate std;

use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use reticulum_device_api_pairing_control::{ControlRequest, ControlResponse, InitializationStatus};
use reticulum_device_api_pairing_policy::{
    ActiveLowButton, ButtonEffect, ConnectionId, MonotonicMillis as PairingMillis, PairingPolicy,
    PendingState,
};

use crate::pairing_policy::{ActiveLowButtonDebouncer, PhysicalPresencePublicationGuard};

use super::{
    ButtonObservationFlight, ButtonObservationFlightProgress, ButtonObservationReply,
    ExclusiveAcquisitionReply, LifecycleAcknowledgement, PairingControlCommand,
    PairingControlHandoff, PairingControlReply, PairingControlReplyKind,
};

fn connection(value: u64) -> ConnectionId {
    ConnectionId::new(value).expect("test connection must be nonzero")
}

fn handoff() -> (
    super::BearerPairingHandoff<NoopRawMutex>,
    super::NodePairingHandoff<NoopRawMutex>,
) {
    std::boxed::Box::leak(std::boxed::Box::new(PairingControlHandoff::new())).split()
}

#[test]
fn command_pressure_returns_the_exact_unsent_command() {
    let (mut bearer, mut node) = handoff();
    let first = PairingControlCommand::Connected {
        at: PairingMillis::new(10),
        connection: connection(1),
    };
    let second = PairingControlCommand::ObserveButton {
        at: PairingMillis::new(11),
        connection: connection(1),
        level: ActiveLowButton::Low,
    };

    assert_eq!(bearer.command_capacity(), 1);
    assert_eq!(node.command_capacity(), 1);
    assert!(bearer.try_send_command(first).is_ok());
    let retained = bearer
        .try_send_command(second)
        .expect_err("the second command must observe depth-one pressure")
        .into_inner();
    assert_eq!(retained, second);
    assert_eq!(node.try_receive_command(), Some(first));
    assert!(bearer.try_send_command(retained).is_ok());
    assert_eq!(node.try_receive_command(), Some(second));
    assert_eq!(node.try_receive_command(), None);
}

#[test]
fn nonblocking_button_flight_retains_pressure_and_routes_exact_reply() {
    let (mut bearer, mut node) = handoff();
    let active_connection = connection(2);
    let occupied = PairingControlCommand::Connected {
        at: PairingMillis::new(1),
        connection: active_connection,
    };
    assert!(bearer.try_send_command(occupied).is_ok());

    let mut flight = ButtonObservationFlight::new();
    assert_eq!(
        flight.try_schedule_and_poll(
            &mut bearer,
            PairingMillis::new(2),
            active_connection,
            ActiveLowButton::High,
        ),
        Ok(Some(ButtonObservationFlightProgress::Pending))
    );
    assert_eq!(node.try_receive_command(), Some(occupied));
    assert_eq!(
        flight.poll(&mut bearer),
        Ok(ButtonObservationFlightProgress::Pending)
    );
    assert_eq!(
        node.try_receive_command(),
        Some(PairingControlCommand::ObserveButton {
            at: PairingMillis::new(2),
            connection: active_connection,
            level: ActiveLowButton::High,
        })
    );

    let stale = PairingControlReply::new(
        connection(1),
        PairingControlReplyKind::Button(ButtonObservationReply::Observed),
    );
    assert!(node.try_send_reply(stale).is_ok());
    assert_eq!(
        flight.poll(&mut bearer),
        Ok(ButtonObservationFlightProgress::Pending)
    );
    let exact = PairingControlReply::new(
        active_connection,
        PairingControlReplyKind::Button(ButtonObservationReply::AcquireExclusive),
    );
    assert!(node.try_send_reply(exact).is_ok());
    assert_eq!(
        flight.poll(&mut bearer),
        Ok(ButtonObservationFlightProgress::AcquireExclusive)
    );
    assert!(!flight.is_pending());
    assert_eq!(flight.started_at(), None);
}

#[test]
fn scheduling_enqueues_before_node_time_can_overtake_the_observation() {
    let connection = connection(8);

    let mut delayed_policy = PairingPolicy::new(PendingState::None);
    assert_eq!(
        delayed_policy.connected(PairingMillis::new(0), connection),
        Ok(None)
    );
    let _ = delayed_policy.poll_timeout(PairingMillis::new(2));
    assert!(matches!(
        delayed_policy.observe_button(PairingMillis::new(1), ActiveLowButton::High),
        ButtonEffect::Fault(_)
    ));

    let (mut bearer, mut node) = handoff();
    let mut flight = ButtonObservationFlight::new();
    assert_eq!(
        flight.try_schedule_and_poll(
            &mut bearer,
            PairingMillis::new(1),
            connection,
            ActiveLowButton::High,
        ),
        Ok(Some(ButtonObservationFlightProgress::Pending))
    );
    let command = node
        .try_receive_command()
        .expect("the timestamped observation must be visible before the bearer yields");
    let PairingControlCommand::ObserveButton {
        at,
        connection: routed,
        level,
    } = command
    else {
        panic!("the immediate command must be the scheduled observation");
    };
    assert_eq!(at, PairingMillis::new(1));
    assert_eq!(routed, connection);
    assert_eq!(level, ActiveLowButton::High);

    let mut ordered_policy = PairingPolicy::new(PendingState::None);
    assert_eq!(
        ordered_policy.connected(PairingMillis::new(0), connection),
        Ok(None)
    );
    assert!(matches!(
        ordered_policy.observe_button(at, level),
        ButtonEffect::None
    ));
    assert!(!matches!(
        ordered_policy.poll_timeout(PairingMillis::new(2)),
        reticulum_device_api_pairing_policy::PolicyEvent::Fault(_)
    ));
}

#[test]
fn delayed_button_replies_do_not_pause_raw_sampling_or_hide_one_hold() {
    let (mut bearer, mut node) = handoff();
    let connection = connection(3);
    let mut policy = PairingPolicy::new(PendingState::None);
    assert_eq!(
        policy.connected(PairingMillis::new(0), connection),
        Ok(None)
    );
    let mut debouncer = ActiveLowButtonDebouncer::new(0, ActiveLowButton::High);
    let mut publication = PhysicalPresencePublicationGuard::new();
    let mut flight = ButtonObservationFlight::new();
    let mut last_publication = 0_u64;
    let mut acquisitions = 0_u8;

    assert_eq!(
        flight.try_schedule_and_poll(
            &mut bearer,
            PairingMillis::new(0),
            connection,
            ActiveLowButton::High,
        ),
        Ok(Some(ButtonObservationFlightProgress::Pending))
    );
    publication.publication_queued();
    let initial = node
        .try_receive_command()
        .expect("the initial release publication must cross the handoff");
    let PairingControlCommand::ObserveButton {
        at,
        connection: routed,
        level,
    } = initial
    else {
        panic!("the flight may only publish a button command");
    };
    assert_eq!(routed, connection);
    assert!(matches!(
        policy.observe_button(at, level),
        ButtonEffect::None
    ));
    let mut delayed_reply = Some((
        650,
        PairingControlReply::new(
            connection,
            PairingControlReplyKind::Button(ButtonObservationReply::Observed),
        ),
    ));

    for now in 1_u64..=3_200 {
        let raw = if now <= 650 {
            ActiveLowButton::High
        } else {
            ActiveLowButton::Low
        };
        let observation = debouncer
            .observe(now, raw)
            .expect("the monotonic millisecond sampler remains valid");
        assert!(
            !observation.continuity_lost(),
            "an outstanding node reply must not pause raw GPIO sampling"
        );
        publication.observe(observation);

        if delayed_reply
            .as_ref()
            .is_some_and(|(deliver_at, _)| *deliver_at <= now)
        {
            let (_, reply) = delayed_reply
                .take()
                .expect("the due reply remains exactly owned");
            assert!(node.try_send_reply(reply).is_ok());
        }

        match flight
            .poll(&mut bearer)
            .expect("every routed reply matches the button flight")
        {
            ButtonObservationFlightProgress::AcquireExclusive => {
                acquisitions = acquisitions.saturating_add(1);
            }
            ButtonObservationFlightProgress::Idle
            | ButtonObservationFlightProgress::Pending
            | ButtonObservationFlightProgress::Observed => {}
        }

        if !flight.is_pending()
            && acquisitions == 0
            && (publication.publication_due(now.saturating_sub(last_publication) >= 20))
        {
            let level = publication.policy_level(debouncer.current());
            assert_eq!(
                flight.try_schedule_and_poll(
                    &mut bearer,
                    PairingMillis::new(now),
                    connection,
                    level,
                ),
                Ok(Some(ButtonObservationFlightProgress::Pending))
            );
            publication.publication_queued();
            last_publication = now;
            let command = node
                .try_receive_command()
                .expect("each scheduled observation must cross immediately");
            let PairingControlCommand::ObserveButton {
                at,
                connection: routed,
                level,
            } = command
            else {
                panic!("the flight may only publish a button command");
            };
            assert_eq!(routed, connection);
            let reply = match policy.observe_button(at, level) {
                ButtonEffect::AcquirePairingExclusive(_) => {
                    PairingControlReplyKind::Button(ButtonObservationReply::AcquireExclusive)
                }
                ButtonEffect::None | ButtonEffect::Closed(_) | ButtonEffect::Fault(_) => {
                    PairingControlReplyKind::Button(ButtonObservationReply::Observed)
                }
            };
            assert!(delayed_reply.is_none());
            delayed_reply = Some((
                now.saturating_add(50),
                PairingControlReply::new(connection, reply),
            ));
        }
    }

    assert_eq!(
        acquisitions, 1,
        "a fresh release and continuous two-second hold must acquire exactly once"
    );
}

#[test]
fn reply_pressure_returns_the_exact_routed_reply() {
    let (mut bearer, mut node) = handoff();
    let first = PairingControlReply::new(
        connection(4),
        PairingControlReplyKind::Lifecycle(LifecycleAcknowledgement::Connected),
    );
    let response = ControlResponse::status(99, InitializationStatus::InFlight);
    let second =
        PairingControlReply::new(connection(4), PairingControlReplyKind::Control(response));

    assert_eq!(bearer.reply_capacity(), 1);
    assert_eq!(node.reply_capacity(), 1);
    assert!(node.try_send_reply(first).is_ok());
    let retained = node
        .try_send_reply(second)
        .expect_err("the second reply must observe depth-one pressure")
        .into_inner();
    assert_eq!(retained, second);
    assert_eq!(bearer.try_receive_reply(), Some(first));
    assert!(node.try_send_reply(retained).is_ok());
    assert_eq!(bearer.try_receive_reply(), Some(second));
    assert_eq!(bearer.try_receive_reply(), None);
}

#[test]
fn every_command_preserves_its_event_time_and_connection() {
    let at = PairingMillis::new(0x1122_3344);
    let connection = connection(7);
    let commands = [
        PairingControlCommand::Connected { at, connection },
        PairingControlCommand::Disconnected { at, connection },
        PairingControlCommand::ObserveButton {
            at,
            connection,
            level: ActiveLowButton::High,
        },
        PairingControlCommand::ExclusiveAcquired { at, connection },
        PairingControlCommand::Control {
            at,
            connection,
            request: ControlRequest::initialize(12),
        },
    ];

    for command in commands {
        assert_eq!(command.connection(), connection);
        assert_eq!(command.at(), at);
    }
}

#[test]
fn reply_shapes_are_connection_routed_and_capability_free() {
    let connection = connection(21);
    let kinds = [
        PairingControlReplyKind::Lifecycle(LifecycleAcknowledgement::Connected),
        PairingControlReplyKind::Lifecycle(LifecycleAcknowledgement::Disconnected),
        PairingControlReplyKind::Button(ButtonObservationReply::Observed),
        PairingControlReplyKind::Button(ButtonObservationReply::AcquireExclusive),
        PairingControlReplyKind::Exclusive(ExclusiveAcquisitionReply::Opened),
        PairingControlReplyKind::Exclusive(ExclusiveAcquisitionReply::Closed),
        PairingControlReplyKind::Exclusive(ExclusiveAcquisitionReply::Refused),
        PairingControlReplyKind::Control(ControlResponse::status(
            3,
            InitializationStatus::Completed,
        )),
    ];

    for kind in kinds {
        let reply = PairingControlReply::new(connection, kind);
        assert_eq!(reply.connection(), connection);
        assert_eq!(reply.kind(), &kind);
        assert_eq!(reply.into_kind(), kind);
    }
}
