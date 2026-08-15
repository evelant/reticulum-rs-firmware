use super::*;
use reticulum_device_api::DestinationHash as ApiDestinationHash;

const INCARNATION: [u8; 8] = [0xa5; 8];
const PRINCIPAL: PrincipalId = PrincipalId([0x11; 16]);
const OTHER_PRINCIPAL: PrincipalId = PrincipalId([0x22; 16]);
const DESTINATION: ApiDestinationHash = ApiDestinationHash([0x33; 16]);
const KEY: IdempotencyKey = IdempotencyKey([0x44; 16]);
const OTHER_KEY: IdempotencyKey = IdempotencyKey([0x55; 16]);

fn request(destination: ApiDestinationHash, key: IdempotencyKey) -> ProbeStartRequest {
    ProbeStartRequest::new(destination, key)
}

#[test]
fn start_is_principal_scoped_idempotent_and_one_slot_bounded() {
    let mut state = ProductReticulumProbeState::new();
    let mut port = ProductReticulumProbePort::new(&mut state, INCARNATION, 1_000, true);
    let id = match port.start(PRINCIPAL, request(DESTINATION, KEY)).unwrap() {
        ReticulumProbeStartDisposition::Accepted(id) => id,
        other => panic!("unexpected start result: {other:?}"),
    };
    assert_eq!(&id.as_bytes()[..8], &INCARNATION);
    assert_eq!(&id.as_bytes()[8..], &1_u64.to_be_bytes());
    assert_eq!(
        port.start(PRINCIPAL, request(DESTINATION, KEY)),
        Ok(ReticulumProbeStartDisposition::Replay(id))
    );
    assert_eq!(
        port.start(PRINCIPAL, request(ApiDestinationHash([0x34; 16]), KEY)),
        Ok(ReticulumProbeStartDisposition::IdempotencyConflict)
    );
    assert_eq!(
        port.start(PRINCIPAL, request(DESTINATION, OTHER_KEY)),
        Ok(ReticulumProbeStartDisposition::CapacityExhausted)
    );
    assert_eq!(
        port.start(OTHER_PRINCIPAL, request(DESTINATION, KEY)),
        Ok(ReticulumProbeStartDisposition::CapacityExhausted)
    );
}

#[test]
fn lookup_is_rate_limited_and_uses_distinct_public_failures() {
    let mut state = ProductReticulumProbeState::new();
    let id = {
        let mut port = ProductReticulumProbePort::new(&mut state, INCARNATION, 1_000, true);
        match port.start(PRINCIPAL, request(DESTINATION, KEY)).unwrap() {
            ReticulumProbeStartDisposition::Accepted(id) => id,
            other => panic!("unexpected start result: {other:?}"),
        }
    };
    assert_eq!(
        state.next_drive(1_000),
        ReticulumProbeDrive::ResolveIdentity {
            destination: DestinationHash::new(DESTINATION.0),
            request_path: true,
        }
    );
    state
        .path_request_attempted(DestinationHash::new(DESTINATION.0), 1_000)
        .unwrap();
    assert_eq!(
        state.next_drive(1_001),
        ReticulumProbeDrive::ResolveIdentity {
            destination: DestinationHash::new(DESTINATION.0),
            request_path: false,
        }
    );
    assert_eq!(state.next_drive(61_000), ReticulumProbeDrive::Idle);
    let mut port = ProductReticulumProbePort::new(&mut state, INCARNATION, 61_000, true);
    assert_eq!(
        port.start(PRINCIPAL, request(DESTINATION, KEY)),
        Ok(ReticulumProbeStartDisposition::Replay(id))
    );
    assert_eq!(
        port.start(PRINCIPAL, request(DESTINATION, OTHER_KEY)),
        Ok(ReticulumProbeStartDisposition::CapacityExhausted)
    );
    assert_eq!(
        port.poll(PRINCIPAL, id),
        Ok(Some(ProbePollResponse::Failed(
            ProbeFailure::IdentityUnavailable
        )))
    );
    assert!(matches!(
        port.start(PRINCIPAL, request(DESTINATION, OTHER_KEY)),
        Ok(ReticulumProbeStartDisposition::Accepted(_))
    ));
}

#[test]
fn path_ready_packet_capacity_wait_has_a_terminal_dispatch_deadline() {
    let mut state = ProductReticulumProbeState::new();
    let id = {
        let mut port = ProductReticulumProbePort::new(&mut state, INCARNATION, 1_000, true);
        match port.start(PRINCIPAL, request(DESTINATION, KEY)).unwrap() {
            ReticulumProbeStartDisposition::Accepted(id) => id,
            other => panic!("unexpected start result: {other:?}"),
        }
    };
    let requested = DestinationHash::new(DESTINATION.0);
    let probe = DestinationHash::new([0x66; 16]);
    state.identity_resolved(requested, probe, 2_000).unwrap();
    state.path_resolved(probe, 3_000).unwrap();
    assert_eq!(
        state.next_drive(32_999),
        ReticulumProbeDrive::Prepare { destination: probe }
    );
    assert_eq!(state.next_drive(33_000), ReticulumProbeDrive::Idle);

    let mut port = ProductReticulumProbePort::new(&mut state, INCARNATION, 33_000, true);
    assert_eq!(
        port.poll(PRINCIPAL, id),
        Ok(Some(ProbePollResponse::Failed(ProbeFailure::Dispatch)))
    );
    assert!(matches!(
        port.start(PRINCIPAL, request(DESTINATION, OTHER_KEY)),
        Ok(ReticulumProbeStartDisposition::Accepted(_))
    ));
}

#[test]
fn disabled_and_foreign_poll_do_not_leak_probe_state() {
    let mut state = ProductReticulumProbeState::new();
    let mut disabled = ProductReticulumProbePort::new(&mut state, INCARNATION, 0, false);
    assert_eq!(
        disabled.start(PRINCIPAL, request(DESTINATION, KEY)),
        Err(ReticulumProbePortError::Unavailable)
    );

    let id = {
        let mut available = ProductReticulumProbePort::new(&mut state, INCARNATION, 0, true);
        match available
            .start(PRINCIPAL, request(DESTINATION, KEY))
            .unwrap()
        {
            ReticulumProbeStartDisposition::Accepted(id) => id,
            other => panic!("unexpected start result: {other:?}"),
        }
    };
    let mut available = ProductReticulumProbePort::new(&mut state, INCARNATION, 0, true);
    assert_eq!(available.poll(OTHER_PRINCIPAL, id), Ok(None));
    let other = ProbeId::new([0x99; 16]).unwrap();
    assert_eq!(available.poll(PRINCIPAL, other), Ok(None));
}
