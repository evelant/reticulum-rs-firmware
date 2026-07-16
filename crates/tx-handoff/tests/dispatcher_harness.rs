//! Deterministic integration of node-core's TX typestates with the bounded
//! Embassy handoff.
//!
//! This harness deliberately has no radio, HAL, executor, or firmware
//! dependency. `NoRfInspector` borrows authorized frames only long enough to
//! record scalar test evidence; it cannot perform I/O.

use core::ptr;

use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use rand_core::{CryptoRng, RngCore};
use reticulum_node_core::{
    AttemptOutcome, AttemptUnsentReason, DestinationHash, InterfaceSet, MonotonicMillis,
    MonotonicSeconds, NodeConfig, NodeCore, NodeIdentity, NodeInstanceId, PacketInterfaceId,
    PermitResolution, PrepareDataRequest, RoutedTxJob, TxAuthorizationCandidate,
    TxAuthorizationPolicy, TxCompletionCode, TxCompletionDisposition, TxFrame, TxFrameError,
    TxLeaseDeadline, TxPacketBuffer, TxPermitDenialReason, TxPolicyDecision, TxPolicyDenial,
    TxRecoveryPriorPhase, TxRecoveryReason,
};
use reticulum_tx_handoff::{ChannelFull, TxHandoff, TxOwnerReturn};
use static_cell::ConstStaticCell;

type TestNode<const BUFFERS: usize> = NodeCore<4, 2, 8, 2, BUFFERS>;

const AUTHORIZED_NO_RF_INSPECTION: TxCompletionCode = TxCompletionCode::new(0x4e52);
const AUTHORIZED_GRANT_EXPIRED: TxCompletionCode = TxCompletionCode::new(0x4e45);
const DEFINITELY_UNPERMITTED: TxCompletionCode = TxCompletionCode::new(0x4e55);

trait MustFit {
    fn must_fit(self, message: &str);
}

impl<T> MustFit for Result<(), ChannelFull<T>> {
    fn must_fit(self, message: &str) {
        if self.is_err() {
            panic!("{message}");
        }
    }
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

struct RecordingPolicy {
    decision: TxPolicyDecision,
    candidates: std::vec::Vec<TxAuthorizationCandidate>,
}

impl RecordingPolicy {
    fn allowing() -> Self {
        Self {
            decision: TxPolicyDecision::Authorize,
            candidates: std::vec::Vec::new(),
        }
    }

    fn denying(reason: TxPolicyDenial) -> Self {
        Self {
            decision: TxPolicyDecision::Deny(reason),
            candidates: std::vec::Vec::new(),
        }
    }
}

impl TxAuthorizationPolicy for RecordingPolicy {
    fn authorize(&mut self, candidate: TxAuthorizationCandidate) -> TxPolicyDecision {
        self.candidates.push(candidate);
        self.decision
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NoRfObservation {
    attempt: reticulum_node_core::AttemptToken,
    interface: PacketInterfaceId,
    packet_len: usize,
    wrapping_checksum: u8,
}

#[derive(Default)]
struct NoRfInspector {
    observations: std::vec::Vec<NoRfObservation>,
}

impl NoRfInspector {
    fn inspect(&mut self, frame: &TxFrame<'_>) -> TxCompletionCode {
        self.observations.push(NoRfObservation {
            attempt: frame.attempt(),
            interface: frame.interface(),
            packet_len: frame.bytes().len(),
            wrapping_checksum: frame
                .bytes()
                .iter()
                .fold(0, |checksum, byte| checksum.wrapping_add(*byte)),
        });
        AUTHORIZED_NO_RF_INSPECTION
    }
}

fn identity(tag: u8) -> NodeIdentity {
    NodeIdentity::from_private_key(&[tag; 64]).expect("test identity must be valid")
}

fn node<const BUFFERS: usize>(tag: u8, aspect: &str) -> TestNode<BUFFERS> {
    TestNode::new(
        identity(tag),
        "reticulum",
        &[aspect],
        NodeInstanceId::new([tag.wrapping_add(0x80); 16]),
        NodeConfig::endpoint(),
    )
    .expect("test node must construct")
}

fn register_peer<const BUFFERS: usize>(sender: &mut TestNode<BUFFERS>, tag: u8, aspect: &str) {
    sender
        .register_peer(
            &identity(tag),
            "reticulum",
            &[aspect],
            MonotonicSeconds::new(0),
        )
        .expect("test peer must register");
}

#[allow(clippy::too_many_arguments)]
fn prepare<'a, const BUFFERS: usize>(
    sender: &mut TestNode<BUFFERS>,
    buffer: &'a mut TxPacketBuffer,
    destination: DestinationHash,
    plaintext: &[u8],
    rns_now: u64,
    owner_now: u64,
    deadline: u64,
    interfaces: InterfaceSet,
    rng: &mut CounterRng,
) -> RoutedTxJob<'a> {
    match sender.prepare_data_into_slot(
        buffer,
        PrepareDataRequest {
            destination,
            plaintext,
            rns_now: MonotonicSeconds::new(rns_now),
            owner_now: MonotonicMillis::new(owner_now),
            deadline: TxLeaseDeadline::new(MonotonicMillis::new(deadline)),
            enabled_interfaces: interfaces,
        },
        rng,
    ) {
        Ok(job) => job,
        Err(failure) => panic!("test preparation failed: {:?}", failure.reason()),
    }
}

fn interfaces(ids: &[u8]) -> InterfaceSet {
    ids.iter().fold(InterfaceSet::empty(), |set, id| {
        set.with(PacketInterfaceId::new(*id))
            .expect("test interface must fit the compact profile")
    })
}

static AUTHORIZED_BUFFER: ConstStaticCell<TxPacketBuffer> =
    ConstStaticCell::new(TxPacketBuffer::new());
static AUTHORIZED_HANDOFF: ConstStaticCell<TxHandoff<NoopRawMutex, 1>> =
    ConstStaticCell::new(TxHandoff::new());

#[test]
fn authorized_job_crosses_every_handoff_and_exposes_one_no_rf_frame() {
    let mut owner = node::<1>(1, "authorized-owner");
    let receiver = node::<0>(2, "authorized-receiver");
    register_peer(&mut owner, 2, "authorized-receiver");
    let buffer = AUTHORIZED_BUFFER.take();
    let pointer = ptr::from_ref(&*buffer);
    let slot = owner
        .register_packet_buffer(buffer)
        .expect("buffer must register");
    let mut rng = CounterRng::default();
    let job = prepare(
        &mut owner,
        buffer,
        receiver.destination_hash(),
        b"authorized handoff",
        1,
        1_000,
        1_500,
        interfaces(&[1]),
        &mut rng,
    );
    let attempt = job.attempt();
    let packet_len = usize::from(job.packet_len());

    let (mut node, mut dispatcher) = AUTHORIZED_HANDOFF.take().split();
    node.jobs
        .try_send(job)
        .must_fit("job handoff must have room");
    let job = dispatcher.jobs.try_receive().expect("job must arrive");
    assert_eq!(job.slot_id(), slot);
    assert_eq!(job.interface(), PacketInterfaceId::new(1));
    let (pending, request) = job.begin_permit();
    dispatcher
        .permit_requests
        .try_send(request)
        .must_fit("request handoff must have room");

    let mut policy = RecordingPolicy::allowing();
    let request = node
        .permit_requests
        .try_receive()
        .expect("request must arrive");
    let reply = owner
        .authorize_tx(request, MonotonicMillis::new(1_100), &mut policy)
        .unwrap_or_else(|failure| panic!("authorization failed: {:?}", failure.reason()));
    node.permit_replies
        .try_send(reply)
        .must_fit("reply handoff must have room");
    let reply = dispatcher
        .permit_replies
        .try_receive()
        .expect("reply must arrive");
    let mut authorized = match pending.resolve(reply, MonotonicMillis::new(1_101)) {
        Ok(PermitResolution::Authorized(owner)) => owner,
        Ok(PermitResolution::Expired(_)) => panic!("fresh grant expired"),
        Ok(PermitResolution::Unpermitted(_)) => panic!("allowed grant was denied"),
        Err(_) => panic!("matching reply mismatched"),
    };

    let mut inspector = NoRfInspector::default();
    let completion_code = {
        let frame = authorized
            .frame(MonotonicMillis::new(1_102))
            .expect("authorized frame must be borrowable once");
        inspector.inspect(&frame)
    };
    assert!(matches!(
        authorized.frame(MonotonicMillis::new(1_102)),
        Err(TxFrameError::AlreadyTaken)
    ));
    dispatcher
        .returns
        .try_send(authorized.complete(completion_code).into())
        .must_fit("completion handoff must have room");
    let completion = match node.returns.try_receive().expect("completion must arrive") {
        TxOwnerReturn::Completion(completion) => completion,
        TxOwnerReturn::Available(_) => panic!("completion changed variant"),
    };
    let returned = match owner
        .complete_tx(completion, MonotonicMillis::new(1_200))
        .unwrap_or_else(|failure| panic!("completion failed: {:?}", failure.reason()))
    {
        TxCompletionDisposition::Available(buffer) => buffer,
        TxCompletionDisposition::Next(_) => panic!("single route fanned out"),
        TxCompletionDisposition::Recovered { .. } => panic!("fresh return recovered"),
        TxCompletionDisposition::Quarantined(_) => panic!("valid return quarantined"),
    };

    assert_eq!(ptr::from_ref(&*returned), pointer);
    assert_eq!(returned.slot_id(), Some(slot));
    assert_eq!(policy.candidates.len(), 1);
    assert_eq!(policy.candidates[0].interface, PacketInterfaceId::new(1));
    assert_eq!(usize::from(policy.candidates[0].packet_len), packet_len);
    assert!(!policy.candidates[0].may_have_transmitted);
    assert_eq!(inspector.observations.len(), 1);
    assert_eq!(inspector.observations[0].attempt, attempt);
    assert_eq!(inspector.observations[0].packet_len, packet_len);
    assert_eq!(owner.capacities().dispatches_used, 0);
    assert_eq!(owner.capacities().receipts_used, 1);
    assert_eq!(owner.capacities().attempts_active, 1);
    assert!(node.jobs.is_empty());
    assert!(node.returns.is_empty());
    assert!(node.permit_requests.is_empty());
    assert!(node.permit_replies.is_empty());
}

static DENIED_BUFFER: ConstStaticCell<TxPacketBuffer> = ConstStaticCell::new(TxPacketBuffer::new());
static DENIED_HANDOFF: ConstStaticCell<TxHandoff<NoopRawMutex, 1>> =
    ConstStaticCell::new(TxHandoff::new());

#[test]
fn policy_denial_crosses_control_plane_without_exposing_frame_bytes() {
    let mut owner = node::<1>(3, "denied-owner");
    let receiver = node::<0>(4, "denied-receiver");
    register_peer(&mut owner, 4, "denied-receiver");
    let buffer = DENIED_BUFFER.take();
    let pointer = ptr::from_ref(&*buffer);
    owner
        .register_packet_buffer(buffer)
        .expect("buffer must register");
    let mut rng = CounterRng::default();
    let job = prepare(
        &mut owner,
        buffer,
        receiver.destination_hash(),
        b"denied handoff",
        1,
        1_000,
        1_500,
        interfaces(&[2]),
        &mut rng,
    );
    let handle = job.attempt_handle();

    let (mut node, mut dispatcher) = DENIED_HANDOFF.take().split();
    node.jobs
        .try_send(job)
        .must_fit("job handoff must have room");
    let job = dispatcher.jobs.try_receive().expect("job must arrive");
    let (pending, request) = job.begin_permit();
    dispatcher
        .permit_requests
        .try_send(request)
        .must_fit("request handoff must have room");
    let mut policy = RecordingPolicy::denying(TxPolicyDenial::RegionalProfileUnavailable);
    let request = node
        .permit_requests
        .try_receive()
        .expect("request must arrive");
    let reply = owner
        .authorize_tx(request, MonotonicMillis::new(1_100), &mut policy)
        .unwrap_or_else(|failure| panic!("policy denial failed: {:?}", failure.reason()));
    node.permit_replies
        .try_send(reply)
        .must_fit("reply handoff must have room");
    let reply = dispatcher
        .permit_replies
        .try_receive()
        .expect("reply must arrive");
    let unpermitted = match pending.resolve(reply, MonotonicMillis::new(1_101)) {
        Ok(PermitResolution::Unpermitted(owner)) => owner,
        Ok(PermitResolution::Authorized(_)) => panic!("policy denial exposed an owner"),
        Ok(PermitResolution::Expired(_)) => panic!("policy denial became expired grant"),
        Err(_) => panic!("matching denial mismatched"),
    };
    assert_eq!(
        unpermitted.denial(),
        Some(TxPermitDenialReason::Policy(
            TxPolicyDenial::RegionalProfileUnavailable
        ))
    );

    let inspector = NoRfInspector::default();
    dispatcher
        .returns
        .try_send(unpermitted.complete(DEFINITELY_UNPERMITTED).into())
        .must_fit("completion handoff must have room");
    let completion = match node.returns.try_receive().expect("completion must arrive") {
        TxOwnerReturn::Completion(completion) => completion,
        TxOwnerReturn::Available(_) => panic!("completion changed variant"),
    };
    let returned = match owner
        .complete_tx(completion, MonotonicMillis::new(1_200))
        .unwrap_or_else(|failure| panic!("denied completion failed: {:?}", failure.reason()))
    {
        TxCompletionDisposition::Available(buffer) => buffer,
        TxCompletionDisposition::Next(_) => panic!("single route fanned out"),
        TxCompletionDisposition::Recovered { .. } => panic!("fresh denial recovered"),
        TxCompletionDisposition::Quarantined(_) => panic!("valid denial quarantined"),
    };

    assert_eq!(ptr::from_ref(&*returned), pointer);
    assert_eq!(policy.candidates.len(), 1);
    assert!(inspector.observations.is_empty());
    assert_eq!(owner.capacities().dispatches_used, 0);
    assert_eq!(owner.capacities().receipts_used, 0);
    let terminal = owner
        .acknowledge_terminal(handle)
        .expect("returned denied owner must permit terminal acknowledgement");
    assert_eq!(
        terminal.outcome(),
        AttemptOutcome::Unsent(AttemptUnsentReason::PolicyDenied(
            TxPolicyDenial::RegionalProfileUnavailable
        ))
    );
    assert_eq!(owner.capacities().attempts_used, 0);
}

static EXPIRED_BUFFER: ConstStaticCell<TxPacketBuffer> =
    ConstStaticCell::new(TxPacketBuffer::new());
static EXPIRED_HANDOFF: ConstStaticCell<TxHandoff<NoopRawMutex, 1>> =
    ConstStaticCell::new(TxHandoff::new());

#[test]
fn grant_resolved_at_exact_deadline_returns_recovery_without_frame_access() {
    let mut owner = node::<1>(5, "expired-owner");
    let receiver = node::<0>(6, "expired-receiver");
    register_peer(&mut owner, 6, "expired-receiver");
    let buffer = EXPIRED_BUFFER.take();
    let pointer = ptr::from_ref(&*buffer);
    owner
        .register_packet_buffer(buffer)
        .expect("buffer must register");
    let mut rng = CounterRng::default();
    let job = prepare(
        &mut owner,
        buffer,
        receiver.destination_hash(),
        b"delayed grant",
        1,
        1_000,
        1_500,
        interfaces(&[3]),
        &mut rng,
    );
    let handle = job.attempt_handle();
    let attempt = job.attempt();

    let (mut node, mut dispatcher) = EXPIRED_HANDOFF.take().split();
    node.jobs
        .try_send(job)
        .must_fit("job handoff must have room");
    let job = dispatcher.jobs.try_receive().expect("job must arrive");
    let (pending, request) = job.begin_permit();
    dispatcher
        .permit_requests
        .try_send(request)
        .must_fit("request handoff must have room");
    let mut policy = RecordingPolicy::allowing();
    let request = node
        .permit_requests
        .try_receive()
        .expect("request must arrive");
    let reply = owner
        .authorize_tx(request, MonotonicMillis::new(1_499), &mut policy)
        .unwrap_or_else(|failure| panic!("grant failed: {:?}", failure.reason()));
    node.permit_replies
        .try_send(reply)
        .must_fit("reply handoff must have room");
    let reply = dispatcher
        .permit_replies
        .try_receive()
        .expect("reply must arrive");
    let expired = match pending.resolve(reply, MonotonicMillis::new(1_500)) {
        Ok(PermitResolution::Expired(owner)) => owner,
        Ok(PermitResolution::Authorized(_)) => panic!("deadline exposed an authorized owner"),
        Ok(PermitResolution::Unpermitted(_)) => panic!("issued grant became unpermitted"),
        Err(_) => panic!("matching grant mismatched"),
    };
    assert_eq!(
        expired.deadline(),
        TxLeaseDeadline::new(MonotonicMillis::new(1_500))
    );

    let inspector = NoRfInspector::default();
    dispatcher
        .returns
        .try_send(expired.complete(AUTHORIZED_GRANT_EXPIRED).into())
        .must_fit("completion handoff must have room");
    let completion = match node.returns.try_receive().expect("completion must arrive") {
        TxOwnerReturn::Completion(completion) => completion,
        TxOwnerReturn::Available(_) => panic!("completion changed variant"),
    };
    let (returned, observation) = match owner
        .complete_tx(completion, MonotonicMillis::new(1_500))
        .unwrap_or_else(|failure| panic!("expired completion failed: {:?}", failure.reason()))
    {
        TxCompletionDisposition::Recovered {
            buffer,
            observation,
        } => (buffer, observation),
        TxCompletionDisposition::Available(_) => panic!("deadline recovery was hidden"),
        TxCompletionDisposition::Next(_) => panic!("expired grant fanned out"),
        TxCompletionDisposition::Quarantined(_) => panic!("matching late owner quarantined"),
    };

    assert_eq!(observation.attempt_handle(), handle);
    assert_eq!(observation.attempt(), attempt);
    let record = observation.record();
    assert_eq!(ptr::from_ref(&*returned), pointer);
    assert_eq!(record.interface(), Some(PacketInterfaceId::new(3)));
    assert_eq!(record.prior_phase(), TxRecoveryPriorPhase::Authorized);
    assert!(record.may_have_transmitted());
    assert_eq!(record.reason(), TxRecoveryReason::DeadlineExpired);
    assert_eq!(record.observed_at(), MonotonicMillis::new(1_500));
    assert_eq!(policy.candidates.len(), 1);
    assert!(inspector.observations.is_empty());
    assert_eq!(owner.capacities().dispatches_used, 0);
    assert_eq!(owner.capacities().receipts_used, 1);
    assert_eq!(owner.capacities().attempts_active, 1);
}

static FANOUT_BUFFER: ConstStaticCell<TxPacketBuffer> = ConstStaticCell::new(TxPacketBuffer::new());
static FANOUT_HANDOFF: ConstStaticCell<TxHandoff<NoopRawMutex, 1>> =
    ConstStaticCell::new(TxHandoff::new());

#[test]
fn serialized_fanout_requeues_same_owner_and_authorizes_interfaces_in_order() {
    let mut owner = node::<1>(7, "fanout-owner");
    let receiver = node::<0>(8, "fanout-receiver");
    register_peer(&mut owner, 8, "fanout-receiver");
    let buffer = FANOUT_BUFFER.take();
    let pointer = ptr::from_ref(&*buffer);
    let slot = owner
        .register_packet_buffer(buffer)
        .expect("buffer must register");
    let mut rng = CounterRng::default();
    let job = prepare(
        &mut owner,
        buffer,
        receiver.destination_hash(),
        b"serialized fanout",
        1,
        1_000,
        1_800,
        interfaces(&[4, 1]),
        &mut rng,
    );
    let attempt = job.attempt();
    let packet_len = usize::from(job.packet_len());

    let (mut node, mut dispatcher) = FANOUT_HANDOFF.take().split();
    let mut policy = RecordingPolicy::allowing();
    let mut inspector = NoRfInspector::default();
    node.jobs
        .try_send(job)
        .must_fit("job handoff must have room");

    let first = dispatcher
        .jobs
        .try_receive()
        .expect("first job must arrive");
    assert_eq!(first.interface(), PacketInterfaceId::new(1));
    let (pending, request) = first.begin_permit();
    dispatcher
        .permit_requests
        .try_send(request)
        .must_fit("first request must fit");
    let request = node
        .permit_requests
        .try_receive()
        .expect("first request must arrive");
    let reply = owner
        .authorize_tx(request, MonotonicMillis::new(1_100), &mut policy)
        .unwrap_or_else(|failure| panic!("first authorization failed: {:?}", failure.reason()));
    node.permit_replies
        .try_send(reply)
        .must_fit("first reply must fit");
    let reply = dispatcher
        .permit_replies
        .try_receive()
        .expect("first reply must arrive");
    let mut authorized = match pending.resolve(reply, MonotonicMillis::new(1_101)) {
        Ok(PermitResolution::Authorized(owner)) => owner,
        Ok(_) => panic!("first route did not authorize"),
        Err(_) => panic!("first reply mismatched"),
    };
    let code = {
        let frame = authorized
            .frame(MonotonicMillis::new(1_102))
            .expect("first frame must be borrowable");
        inspector.inspect(&frame)
    };
    dispatcher
        .returns
        .try_send(authorized.complete(code).into())
        .must_fit("first completion must fit");
    let completion = match node
        .returns
        .try_receive()
        .expect("first completion must arrive")
    {
        TxOwnerReturn::Completion(completion) => completion,
        TxOwnerReturn::Available(_) => panic!("first completion changed variant"),
    };
    let second = match owner
        .complete_tx(completion, MonotonicMillis::new(1_150))
        .unwrap_or_else(|failure| panic!("first completion failed: {:?}", failure.reason()))
    {
        TxCompletionDisposition::Next(job) => job,
        TxCompletionDisposition::Available(_) => panic!("fanout stopped after first route"),
        TxCompletionDisposition::Recovered { .. } => panic!("fresh fanout recovered"),
        TxCompletionDisposition::Quarantined(_) => panic!("valid fanout quarantined"),
    };
    assert_eq!(second.slot_id(), slot);
    assert_eq!(second.attempt(), attempt);
    assert_eq!(second.interface(), PacketInterfaceId::new(4));
    node.jobs
        .try_send(second)
        .must_fit("next job handoff must have room");

    let second = dispatcher
        .jobs
        .try_receive()
        .expect("second job must arrive");
    let (pending, request) = second.begin_permit();
    dispatcher
        .permit_requests
        .try_send(request)
        .must_fit("second request must fit");
    let request = node
        .permit_requests
        .try_receive()
        .expect("second request must arrive");
    let reply = owner
        .authorize_tx(request, MonotonicMillis::new(1_200), &mut policy)
        .unwrap_or_else(|failure| panic!("second authorization failed: {:?}", failure.reason()));
    node.permit_replies
        .try_send(reply)
        .must_fit("second reply must fit");
    let reply = dispatcher
        .permit_replies
        .try_receive()
        .expect("second reply must arrive");
    let mut authorized = match pending.resolve(reply, MonotonicMillis::new(1_201)) {
        Ok(PermitResolution::Authorized(owner)) => owner,
        Ok(_) => panic!("second route did not authorize"),
        Err(_) => panic!("second reply mismatched"),
    };
    let code = {
        let frame = authorized
            .frame(MonotonicMillis::new(1_202))
            .expect("second frame must be borrowable");
        inspector.inspect(&frame)
    };
    dispatcher
        .returns
        .try_send(authorized.complete(code).into())
        .must_fit("second completion must fit");
    let completion = match node
        .returns
        .try_receive()
        .expect("second completion must arrive")
    {
        TxOwnerReturn::Completion(completion) => completion,
        TxOwnerReturn::Available(_) => panic!("second completion changed variant"),
    };
    let returned = match owner
        .complete_tx(completion, MonotonicMillis::new(1_250))
        .unwrap_or_else(|failure| panic!("second completion failed: {:?}", failure.reason()))
    {
        TxCompletionDisposition::Available(buffer) => buffer,
        TxCompletionDisposition::Next(_) => panic!("fanout continued past final route"),
        TxCompletionDisposition::Recovered { .. } => panic!("fresh return recovered"),
        TxCompletionDisposition::Quarantined(_) => panic!("valid return quarantined"),
    };

    assert_eq!(ptr::from_ref(&*returned), pointer);
    assert_eq!(inspector.observations.len(), 2);
    assert_eq!(inspector.observations[0].attempt, attempt);
    assert_eq!(inspector.observations[1].attempt, attempt);
    assert_eq!(inspector.observations[0].packet_len, packet_len);
    assert_eq!(inspector.observations[1].packet_len, packet_len);
    assert_eq!(
        inspector.observations[0].wrapping_checksum,
        inspector.observations[1].wrapping_checksum
    );
    assert_eq!(
        inspector.observations[0].interface,
        PacketInterfaceId::new(1)
    );
    assert_eq!(
        inspector.observations[1].interface,
        PacketInterfaceId::new(4)
    );
    assert_eq!(policy.candidates.len(), 2);
    assert!(!policy.candidates[0].may_have_transmitted);
    assert!(policy.candidates[1].may_have_transmitted);
    assert_eq!(owner.capacities().dispatches_used, 0);
    assert_eq!(owner.capacities().receipts_used, 1);
    assert_eq!(owner.capacities().attempts_active, 1);
}

static TERMINAL_BUFFER: ConstStaticCell<TxPacketBuffer> =
    ConstStaticCell::new(TxPacketBuffer::new());
static TERMINAL_HANDOFF: ConstStaticCell<TxHandoff<NoopRawMutex, 1>> =
    ConstStaticCell::new(TxHandoff::new());

#[test]
fn terminal_attempt_before_authorization_bypasses_policy_and_stops_fanout() {
    let mut owner = node::<1>(9, "terminal-owner");
    let receiver = node::<0>(10, "terminal-receiver");
    register_peer(&mut owner, 10, "terminal-receiver");
    let buffer = TERMINAL_BUFFER.take();
    let pointer = ptr::from_ref(&*buffer);
    owner
        .register_packet_buffer(buffer)
        .expect("buffer must register");
    let mut rng = CounterRng::default();
    let job = prepare(
        &mut owner,
        buffer,
        receiver.destination_hash(),
        b"terminal before permit",
        100,
        100_000,
        200_000,
        interfaces(&[1, 4]),
        &mut rng,
    );
    let handle = job.attempt_handle();

    let (mut node, mut dispatcher) = TERMINAL_HANDOFF.take().split();
    node.jobs
        .try_send(job)
        .must_fit("job handoff must have room");
    let job = dispatcher.jobs.try_receive().expect("job must arrive");
    let (pending, request) = job.begin_permit();
    dispatcher
        .permit_requests
        .try_send(request)
        .must_fit("request handoff must have room");

    let report = owner.tick(MonotonicSeconds::new(132), &mut rng);
    assert_eq!(report.timed_out_attempts, 1);
    assert_eq!(report.correlation_fault, None);
    assert_eq!(owner.capacities().attempts_terminal, 1);
    let mut policy = RecordingPolicy::allowing();
    let request = node
        .permit_requests
        .try_receive()
        .expect("request must arrive");
    let reply = owner
        .authorize_tx(request, MonotonicMillis::new(100_100), &mut policy)
        .unwrap_or_else(|failure| panic!("terminal denial failed: {:?}", failure.reason()));
    node.permit_replies
        .try_send(reply)
        .must_fit("reply handoff must have room");
    let reply = dispatcher
        .permit_replies
        .try_receive()
        .expect("reply must arrive");
    let unpermitted = match pending.resolve(reply, MonotonicMillis::new(100_101)) {
        Ok(PermitResolution::Unpermitted(owner)) => owner,
        Ok(PermitResolution::Authorized(_)) => panic!("terminal attempt authorized"),
        Ok(PermitResolution::Expired(_)) => panic!("terminal denial became expired grant"),
        Err(_) => panic!("matching terminal denial mismatched"),
    };
    assert_eq!(
        unpermitted.denial(),
        Some(TxPermitDenialReason::AttemptTerminal(
            AttemptOutcome::DeliveryTimeout
        ))
    );
    assert!(policy.candidates.is_empty());

    dispatcher
        .returns
        .try_send(unpermitted.complete(DEFINITELY_UNPERMITTED).into())
        .must_fit("completion handoff must have room");
    let completion = match node.returns.try_receive().expect("completion must arrive") {
        TxOwnerReturn::Completion(completion) => completion,
        TxOwnerReturn::Available(_) => panic!("completion changed variant"),
    };
    let returned = match owner
        .complete_tx(completion, MonotonicMillis::new(100_200))
        .unwrap_or_else(|failure| panic!("terminal completion failed: {:?}", failure.reason()))
    {
        TxCompletionDisposition::Available(buffer) => buffer,
        TxCompletionDisposition::Next(_) => panic!("terminal attempt continued fanout"),
        TxCompletionDisposition::Recovered { .. } => panic!("fresh terminal return recovered"),
        TxCompletionDisposition::Quarantined(_) => panic!("valid terminal return quarantined"),
    };
    assert_eq!(ptr::from_ref(&*returned), pointer);
    let terminal = owner
        .acknowledge_terminal(handle)
        .expect("unbound terminal must acknowledge");
    assert_eq!(terminal.outcome(), AttemptOutcome::DeliveryTimeout);
    assert_eq!(owner.capacities().dispatches_used, 0);
    assert_eq!(owner.capacities().receipts_used, 0);
    assert_eq!(owner.capacities().attempts_used, 0);
}
