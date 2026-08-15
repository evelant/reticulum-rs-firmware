extern crate std;

use core::{
    future::Future,
    ops::{Deref, DerefMut},
    pin::{Pin, pin},
    task::{Context, Poll, Waker},
};
use std::{
    boxed::Box,
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::Wake,
    vec,
    vec::Vec,
};

use embassy_futures::block_on;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use reticulum_interface_router::{
    InterfaceConfigId, InterfaceCost, InterfaceFabric, InterfaceProperties, LogicalMtu,
    OutboundCompletion, OutboundRouter,
};
use reticulum_node_core::{
    AnnounceEmissionTime, DestinationHash, InterfaceSet, MAX_ANNOUNCE_APP_DATA, MAX_DATA_PAYLOAD,
    MonotonicSeconds, NodeConfig, NodeCore, NodeIdentity, NodeInstanceId,
    OrdinaryActionAdmissionRequest, OrdinaryActionOwner, OrdinaryCompletionDisposition,
    OrdinaryPacketBuffer, OrdinaryQuarantineReason, PacketInterfaceId, PrepareDataRequest,
    TxAuthorizationCandidate, TxAuthorizationPolicy, TxCompletionDisposition, TxLeaseDeadline,
    TxPacketBuffer, TxPermitReservation, TxPolicyDecision, TxPolicyDenial, TxRecoveryPriorPhase,
    TxRecoveryReason,
};
use reticulum_radio_interface::{
    CadObservation, FrameSignal, LoRaFrequencyRange, LoRaProfile, LoRaProfileConfig, PacketTxFault,
    PacketTxObservation, RadioConfigurationFingerprint, RnodeTxFrames, SoleRadioFaultSummary,
};
use reticulum_tx_handoff::{
    AuthorizedFrameHandoff, AuthorizedFrameNodeHandoff, DataNodePermitHandoff, DataPermitHandoff,
    DispatcherPermitHandoff, OrdinaryDispatcherPermitHandoff, OrdinaryNodePermitHandoff,
    OrdinaryPermitHandoff,
};
use static_cell::{ConstStaticCell, StaticCell};

use super::*;

type TestNode<const N: usize> = NodeCore<4, 4, 8, 2, N>;
type PortableTestDispatcher = SoleRadioTxDispatcher<MockRadio, CounterRng, NoopRawMutex, 1>;
type TestRouter = OutboundRouter<NoopRawMutex, 1, 1>;

struct TestDispatcher {
    inner: PortableTestDispatcher,
    authorized_frames: AuthorizedFrameNodeHandoff<NoopRawMutex>,
}

impl Deref for TestDispatcher {
    type Target = PortableTestDispatcher;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for TestDispatcher {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl TestDispatcher {
    fn step(&mut self, now_us: u64) -> RadioTxDispatcherStep {
        let result = self.inner.step(now_us);
        if self.inner.phase() != RadioTxDispatcherPhase::AuthorizedFrameAcknowledgementWait {
            return result;
        }
        let Some(observation) = self.authorized_frames.requests().try_receive() else {
            return result;
        };
        if self
            .authorized_frames
            .acknowledgements()
            .try_send(observation)
            .is_err()
        {
            return result;
        }
        let acknowledged = self.inner.step(now_us);
        assert_eq!(acknowledged, RadioTxDispatcherStep::Advanced);
        self.inner.step(now_us)
    }
}

#[derive(Default)]
struct WakeCounter(AtomicUsize);

impl Wake for WakeCounter {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct CounterRng(u64);

impl RngCore for CounterRng {
    fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        self.0
    }

    fn fill_bytes(&mut self, destination: &mut [u8]) {
        for chunk in destination.chunks_mut(8) {
            let bytes = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
    }

    fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(destination);
        Ok(())
    }
}

impl CryptoRng for CounterRng {}

#[derive(Clone, Copy)]
enum CadScript {
    Observation { busy: bool, at_us: u64 },
    Fault,
    Pending,
}

#[derive(Clone, Copy)]
enum TxScript {
    SuccessOne(u64),
    SuccessTwo(u64, u64),
    Fault(PacketTxProgress),
    Pending,
}

#[derive(Clone, Copy)]
enum RxScript {
    Frame {
        len: usize,
        signal: FrameSignal,
        at_us: u64,
    },
    NoPreambleTimeout,
    Fault,
    Pending,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CapturedPacket {
    first: Vec<u8>,
    second: Option<Vec<u8>>,
}

struct MockRadio {
    active: bool,
    fingerprint: RadioConfigurationFingerprint,
    profile: LoRaProfile,
    cad: VecDeque<CadScript>,
    tx: VecDeque<TxScript>,
    rx: VecDeque<RxScript>,
    captured: Vec<CapturedPacket>,
    maximum_receive_operation_us: core::num::NonZeroU64,
    receive_session_invalidations: usize,
}

impl MockRadio {
    fn new(cad: Vec<CadScript>, tx: Vec<TxScript>) -> Self {
        Self {
            active: true,
            fingerprint: RadioConfigurationFingerprint::new([0x91; 16]),
            profile: profile(),
            cad: cad.into(),
            tx: tx.into(),
            rx: VecDeque::new(),
            captured: Vec::new(),
            maximum_receive_operation_us: core::num::NonZeroU64::new(1_500_000).unwrap(),
            receive_session_invalidations: 0,
        }
    }

    fn with_rx(mut self, rx: Vec<RxScript>) -> Self {
        self.rx = rx.into();
        self
    }
}

struct MockCadFuture<'a> {
    radio: &'a mut MockRadio,
    complete: bool,
}

impl Future for MockCadFuture<'_> {
    type Output = Result<CadObservation, SoleRadioFaultSummary>;

    fn poll(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Self::Output> {
        let Some(script) = self.radio.cad.front().copied() else {
            panic!("mock CAD script exhausted")
        };
        if matches!(script, CadScript::Pending) {
            return Poll::Pending;
        }
        let _ = self.radio.cad.pop_front();
        self.complete = true;
        match script {
            CadScript::Observation { busy, at_us } => {
                Poll::Ready(Ok(CadObservation::new(busy, at_us)))
            }
            CadScript::Fault => {
                self.radio.active = false;
                Poll::Ready(Err(SoleRadioFaultSummary::new(
                    SoleRadioFaultPhase::ChannelActivityDetection,
                    SoleRadioFaultClass::Operation,
                )))
            }
            CadScript::Pending => unreachable!(),
        }
    }
}

impl Drop for MockCadFuture<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.radio.active = false;
        }
    }
}

struct MockTxFuture<'a> {
    radio: &'a mut MockRadio,
    complete: bool,
}

impl Future for MockTxFuture<'_> {
    type Output = Result<PacketTxObservation, PacketTxFault<SoleRadioFaultSummary>>;

    fn poll(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Self::Output> {
        let Some(script) = self.radio.tx.front().copied() else {
            panic!("mock TX script exhausted")
        };
        if matches!(script, TxScript::Pending) {
            return Poll::Pending;
        }
        let _ = self.radio.tx.pop_front();
        self.complete = true;
        match script {
            TxScript::SuccessOne(at_us) => Poll::Ready(Ok(PacketTxObservation::one_frame(at_us))),
            TxScript::SuccessTwo(first_us, second_us) => {
                Poll::Ready(Ok(PacketTxObservation::two_frames(first_us, second_us)))
            }
            TxScript::Fault(progress) => {
                self.radio.active = false;
                Poll::Ready(Err(PacketTxFault::new(
                    SoleRadioFaultSummary::new(
                        SoleRadioFaultPhase::FrameTransmission,
                        SoleRadioFaultClass::Operation,
                    ),
                    progress,
                )))
            }
            TxScript::Pending => unreachable!(),
        }
    }
}

impl Drop for MockTxFuture<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.radio.active = false;
        }
    }
}

struct MockRxFuture<'a> {
    radio: &'a mut MockRadio,
    buffer: &'a mut [u8; SX1262_FRAME_MTU],
    complete: bool,
}

impl Future for MockRxFuture<'_> {
    type Output = Result<BoundedRxOutcome, SoleRadioFaultSummary>;

    fn poll(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Self::Output> {
        let Some(script) = self.radio.rx.front().copied() else {
            panic!("mock RX script exhausted")
        };
        if matches!(script, RxScript::Pending) {
            return Poll::Pending;
        }
        let _ = self.radio.rx.pop_front();
        self.complete = true;
        match script {
            RxScript::Frame { len, signal, at_us } => {
                for (index, byte) in self.buffer.iter_mut().take(len).enumerate() {
                    *byte = index as u8;
                }
                self.radio.active = true;
                Poll::Ready(Ok(BoundedRxOutcome::Frame(BoundedRxObservation::new(
                    len, signal, at_us,
                ))))
            }
            RxScript::NoPreambleTimeout => {
                self.radio.active = true;
                Poll::Ready(Ok(BoundedRxOutcome::NoPreambleTimeout))
            }
            RxScript::Fault => Poll::Ready(Err(SoleRadioFaultSummary::new(
                SoleRadioFaultPhase::Receive,
                SoleRadioFaultClass::Operation,
            ))),
            RxScript::Pending => unreachable!(),
        }
    }
}

impl Drop for MockRxFuture<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.radio.active = false;
        }
    }
}

impl SoleRnodeRadio for MockRadio {
    type Fault = SoleRadioFaultSummary;

    fn configuration_fingerprint(&self) -> RadioConfigurationFingerprint {
        self.fingerprint
    }

    fn airtime_profile(&self) -> LoRaProfile {
        self.profile
    }

    fn maximum_receive_operation_us(&self) -> core::num::NonZeroU64 {
        self.maximum_receive_operation_us
    }

    fn receive_bounded<'a>(
        &'a mut self,
        buffer: &'a mut [u8; SX1262_FRAME_MTU],
    ) -> impl Future<Output = Result<BoundedRxOutcome, Self::Fault>> + 'a {
        self.active = false;
        MockRxFuture {
            radio: self,
            buffer,
            complete: false,
        }
    }

    fn receive_continuous_until<'a, SchedulerYield, ProgressDeadline>(
        &'a mut self,
        buffer: &'a mut [u8; SX1262_FRAME_MTU],
        _scheduler_yield: SchedulerYield,
        _progress_deadline: ProgressDeadline,
    ) -> impl Future<Output = Result<BoundedRxOutcome, Self::Fault>> + 'a
    where
        SchedulerYield: Future<Output = ()> + 'a,
        ProgressDeadline: Future<Output = ()> + 'a,
    {
        self.active = false;
        MockRxFuture {
            radio: self,
            buffer,
            complete: false,
        }
    }

    fn invalidate_receive_session(&mut self) {
        self.receive_session_invalidations += 1;
    }

    fn cad(&mut self) -> impl Future<Output = Result<CadObservation, Self::Fault>> + '_ {
        MockCadFuture {
            radio: self,
            complete: false,
        }
    }

    fn transmit<'a>(
        &'a mut self,
        frames: RnodeTxFrames<'a>,
    ) -> impl Future<Output = Result<PacketTxObservation, PacketTxFault<Self::Fault>>> + 'a {
        self.captured.push(CapturedPacket {
            first: frames.first().to_vec(),
            second: frames.second().map(<[u8]>::to_vec),
        });
        MockTxFuture {
            radio: self,
            complete: false,
        }
    }

    fn shutdown(&mut self) {
        self.active = false;
    }

    fn is_active(&self) -> bool {
        self.active
    }
}

const TEST_PERMIT_RESOURCE_BYTES: [u8; 16] = [0x91; 16];
const TEST_INTERFACE: PacketInterfaceId = PacketInterfaceId::new(1);
const TEST_INTERFACE_CONFIG: InterfaceConfigId = InterfaceConfigId::new(1);

fn test_permit_resource() -> TxPermitResourceId {
    TxPermitResourceId::new(TEST_PERMIT_RESOURCE_BYTES)
}

fn exact_requirements(packet_len: u16) -> TxPermitRequirements {
    let required_units = profile()
        .rnode_packet_airtime(packet_len.into())
        .unwrap()
        .aggregate_time_on_air_us();
    TxPermitRequirements::try_new(test_permit_resource(), required_units).unwrap()
}

struct ExactPolicy;

impl TxAuthorizationPolicy for ExactPolicy {
    fn authorize(&mut self, candidate: TxAuthorizationCandidate) -> TxPolicyDecision {
        ExactLoRaAirtimePolicy::new(
            TEST_INTERFACE,
            profile(),
            RadioConfigurationFingerprint::new(TEST_PERMIT_RESOURCE_BYTES),
        )
        .authorize(candidate)
    }
}

struct DenyPolicy;

impl TxAuthorizationPolicy for DenyPolicy {
    fn authorize(&mut self, _candidate: TxAuthorizationCandidate) -> TxPolicyDecision {
        TxPolicyDecision::Deny(TxPolicyDenial::PolicyDenied)
    }
}

fn profile() -> LoRaProfile {
    let range = LoRaFrequencyRange::try_new(863_000_000, 928_000_000).unwrap();
    LoRaProfile::validate(
        LoRaProfileConfig {
            frequency_hz: Some(915_000_000),
            spreading_factor: 7,
            bandwidth_hz: 500_000,
            coding_rate_denominator: 5,
            preamble_symbols: 18,
            explicit_header: true,
            crc: true,
            iq_inverted: false,
        },
        range,
    )
    .unwrap()
}

fn changed_profile() -> LoRaProfile {
    let range = LoRaFrequencyRange::try_new(863_000_000, 928_000_000).unwrap();
    LoRaProfile::validate(
        LoRaProfileConfig {
            frequency_hz: Some(916_000_000),
            spreading_factor: 8,
            bandwidth_hz: 500_000,
            coding_rate_denominator: 5,
            preamble_symbols: 18,
            explicit_header: true,
            crc: true,
            iq_inverted: false,
        },
        range,
    )
    .unwrap()
}

fn policy_candidate(
    packet_len: u16,
    interface: PacketInterfaceId,
    resource: TxPermitResourceId,
    required_units: u64,
) -> TxAuthorizationCandidate {
    TxAuthorizationCandidate {
        interface,
        packet_len,
        requirements: TxPermitRequirements::try_new(resource, required_units).unwrap(),
        now: MonotonicMillis::new(1_000),
        deadline: TxLeaseDeadline::new(MonotonicMillis::new(2_000)),
        may_have_transmitted: false,
    }
}

#[test]
fn exact_policy_recomputes_airtime_and_rejects_wrong_resource_interface_or_units() {
    let packet_len = 300;
    let expected_units = profile()
        .rnode_packet_airtime(packet_len.into())
        .unwrap()
        .aggregate_time_on_air_us();
    let mut policy = ExactPolicy;

    assert_eq!(
        policy.authorize(policy_candidate(
            packet_len,
            TEST_INTERFACE,
            test_permit_resource(),
            expected_units,
        )),
        TxPolicyDecision::Authorize(
            TxPermitReservation::try_new(test_permit_resource(), expected_units).unwrap()
        )
    );
    assert_eq!(
        policy.authorize(policy_candidate(
            packet_len,
            TEST_INTERFACE,
            test_permit_resource(),
            expected_units - 1,
        )),
        TxPolicyDecision::Deny(TxPolicyDenial::PolicyDenied)
    );
    assert_eq!(
        policy.authorize(policy_candidate(
            packet_len,
            TEST_INTERFACE,
            TxPermitResourceId::new([0x92; 16]),
            expected_units,
        )),
        TxPolicyDecision::Deny(TxPolicyDenial::ResourceUnavailable)
    );
    assert_eq!(
        policy.authorize(policy_candidate(
            packet_len,
            PacketInterfaceId::new(2),
            test_permit_resource(),
            expected_units,
        )),
        TxPolicyDecision::Deny(TxPolicyDenial::PolicyDenied)
    );
}

fn config(maximum_cad_attempts: u8) -> RadioTxDispatcherConfig {
    config_with_access(
        LogicalPacketAccessConfig::try_new(maximum_cad_attempts, 10, 20, 100_000, 100, 100, 100)
            .unwrap(),
    )
}

fn config_with_access(channel_access: LogicalPacketAccessConfig) -> RadioTxDispatcherConfig {
    RadioTxDispatcherConfig::new(
        TEST_INTERFACE_CONFIG,
        channel_access,
        500_000,
        3,
        RadioTxCompletionCodes::new(
            TxCompletionCode::new(1),
            TxCompletionCode::new(2),
            TxCompletionCode::new(3),
            TxCompletionCode::new(4),
            TxCompletionCode::new(5),
            TxCompletionCode::new(6),
            TxCompletionCode::new(7),
            TxCompletionCode::new(8),
            TxCompletionCode::new(9),
        ),
    )
}

fn test_dispatcher(
    radio: MockRadio,
    rng: CounterRng,
    data_permits: DispatcherPermitHandoff<NoopRawMutex>,
    ordinary_permits: OrdinaryDispatcherPermitHandoff<NoopRawMutex>,
    dispatcher_config: RadioTxDispatcherConfig,
) -> (TestDispatcher, TestRouter) {
    assert_eq!(
        dispatcher_config.expected_interface_config,
        TEST_INTERFACE_CONFIG
    );
    test_dispatcher_with_registry_config(
        radio,
        rng,
        data_permits,
        ordinary_permits,
        dispatcher_config,
        TEST_INTERFACE_CONFIG,
    )
}

fn test_dispatcher_with_registry_config(
    radio: MockRadio,
    rng: CounterRng,
    data_permits: DispatcherPermitHandoff<NoopRawMutex>,
    ordinary_permits: OrdinaryDispatcherPermitHandoff<NoopRawMutex>,
    dispatcher_config: RadioTxDispatcherConfig,
    registry_config: InterfaceConfigId,
) -> (TestDispatcher, TestRouter) {
    test_dispatcher_with_registry_config_and_frame_blocker(
        radio,
        rng,
        data_permits,
        ordinary_permits,
        dispatcher_config,
        registry_config,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn test_dispatcher_with_registry_config_and_frame_blocker(
    radio: MockRadio,
    rng: CounterRng,
    data_permits: DispatcherPermitHandoff<NoopRawMutex>,
    ordinary_permits: OrdinaryDispatcherPermitHandoff<NoopRawMutex>,
    dispatcher_config: RadioTxDispatcherConfig,
    registry_config: InterfaceConfigId,
    frame_request_blocker: Option<AuthorizedFrameObservation>,
) -> (TestDispatcher, TestRouter) {
    let authorized_frame_handoff =
        Box::leak(Box::new(AuthorizedFrameHandoff::<NoopRawMutex>::new()));
    let (authorized_frame_node, mut authorized_frame_dispatcher) = authorized_frame_handoff.split();
    if let Some(blocker) = frame_request_blocker {
        assert!(
            authorized_frame_dispatcher
                .requests()
                .try_send(blocker)
                .is_ok()
        );
    }
    let fabric = Box::leak(Box::new(InterfaceFabric::<NoopRawMutex, 1, 1>::new()));
    let (mut router, [actor_handoff]) = fabric.split();
    let (tx_handoff, _unused_ingress_handoff, _unused_lifecycle_handoff) =
        actor_handoff.into_parts();
    router
        .register(
            tx_handoff.queue_id(),
            TEST_INTERFACE,
            InterfaceProperties::new(
                LogicalMtu::try_new(u16::MAX).unwrap(),
                registry_config,
                None,
                InterfaceCost::new(0),
            ),
            true,
        )
        .unwrap();
    (
        TestDispatcher {
            inner: SoleRadioTxDispatcher::new(
                radio,
                rng,
                tx_handoff,
                data_permits,
                ordinary_permits,
                authorized_frame_dispatcher,
                dispatcher_config,
            ),
            authorized_frames: authorized_frame_node,
        },
        router,
    )
}

fn route_data(router: &mut TestRouter, job: RoutedTxJob<'static>) {
    router
        .try_route_data(job)
        .unwrap_or_else(|failure| panic!("DATA route: {:?}", failure));
}

fn route_ordinary(router: &mut TestRouter, job: OrdinaryTxJob<'static>) {
    router
        .try_route_ordinary(job)
        .unwrap_or_else(|failure| panic!("ordinary route: {:?}", failure));
}

fn take_data_completion(router: &mut TestRouter) -> TxCompletion<'static> {
    match router
        .try_receive_completion()
        .unwrap_or_else(|failure| panic!("DATA completion route: {:?}", failure))
        .expect("DATA completion")
    {
        OutboundCompletion::Data(completion) => completion,
        OutboundCompletion::Ordinary(_) => panic!("ordinary completion crossed into DATA"),
    }
}

fn take_ordinary_completion(router: &mut TestRouter) -> OrdinaryTxCompletion<'static> {
    match router
        .try_receive_completion()
        .unwrap_or_else(|failure| panic!("ordinary completion route: {:?}", failure))
        .expect("ordinary completion")
    {
        OutboundCompletion::Ordinary(completion) => completion,
        OutboundCompletion::Data(_) => panic!("DATA completion crossed into ordinary"),
    }
}

fn dispatcher_with_rx(rx: Vec<RxScript>) -> TestDispatcher {
    let data_handoff = Box::leak(Box::new(DataPermitHandoff::<NoopRawMutex>::new()));
    let ordinary_handoff = Box::leak(Box::new(OrdinaryPermitHandoff::<NoopRawMutex>::new()));
    let (_, data_dispatch) = data_handoff.split();
    let (_, ordinary_dispatch) = ordinary_handoff.split();
    test_dispatcher(
        MockRadio::new(vec![], vec![]).with_rx(rx),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(2),
    )
    .0
}

fn identity(tag: u8) -> NodeIdentity {
    NodeIdentity::from_private_key(&[tag; 64]).unwrap()
}

fn node<const N: usize>(tag: u8, aspect: &'static str) -> TestNode<N> {
    TestNode::new(
        identity(tag),
        "reticulum",
        &[aspect],
        NodeInstanceId::new([tag.wrapping_add(0x80); 16]),
        NodeConfig::endpoint(),
    )
    .unwrap()
}

fn prepare_data<const N: usize>(
    sender: &mut TestNode<N>,
    buffer: &'static mut TxPacketBuffer,
    destination: DestinationHash,
    plaintext: &[u8],
    deadline_ms: u64,
) -> RoutedTxJob<'static> {
    sender
        .prepare_data_into_slot(
            buffer,
            PrepareDataRequest {
                destination,
                plaintext,
                rns_now: MonotonicSeconds::new(1),
                owner_now: MonotonicMillis::new(1_000),
                deadline: TxLeaseDeadline::new(MonotonicMillis::new(deadline_ms)),
                enabled_interfaces: InterfaceSet::from_bits(1 << 1),
            },
            &mut CounterRng::default(),
        )
        .unwrap_or_else(|failure| panic!("DATA preparation: {:?}", failure.reason()))
}

fn prepare_ordinary(
    node: &mut TestNode<1>,
    owner: &mut OrdinaryActionOwner<1>,
    refs: &'static mut [&'static mut OrdinaryPacketBuffer; 1],
    app_data_len: usize,
    deadline_ms: u64,
) -> OrdinaryTxJob<'static> {
    owner.register_packet_buffer(refs[0]).unwrap();
    let app_data = [0x42; MAX_ANNOUNCE_APP_DATA];
    node.queue_announce(
        Some(&app_data[..app_data_len]),
        AnnounceEmissionTime::new(1).unwrap(),
        &mut CounterRng::default(),
    )
    .unwrap();
    let actions = node.flush_announces(MonotonicSeconds::new(1), &mut CounterRng::default());
    let mut batch = owner
        .admit(
            actions,
            refs,
            OrdinaryActionAdmissionRequest {
                owner_now: MonotonicMillis::new(1_000),
                deadline: TxLeaseDeadline::new(MonotonicMillis::new(deadline_ms)),
                enabled_interfaces: InterfaceSet::from_bits(1 << 1),
            },
        )
        .unwrap();
    batch.take_next_packet().expect("announce packet missing")
}

fn start_and_clear_data(dispatcher: &mut TestDispatcher, now_us: u64) {
    assert_eq!(dispatcher.step(now_us), RadioTxDispatcherStep::Advanced);
    assert!(matches!(
        dispatcher.step(now_us + 100),
        RadioTxDispatcherStep::NeedCad(DispatchFamily::Data)
    ));
    assert!(matches!(
        block_on(dispatcher.perform_radio_operation(now_us + 101)),
        RadioOperationStep::CadObserved {
            family: DispatchFamily::Data,
            activity_detected: false,
            ..
        }
    ));
}

fn grant_data<const N: usize>(
    dispatcher: &mut TestDispatcher,
    node_ports: &mut DataNodePermitHandoff<NoopRawMutex>,
    owner: &mut TestNode<N>,
    policy: &mut impl TxAuthorizationPolicy,
    authorization_ms: u64,
    dispatcher_now_us: u64,
) {
    assert_eq!(
        dispatcher.step(dispatcher_now_us),
        RadioTxDispatcherStep::Advanced
    );
    let request = node_ports.requests().try_receive().expect("DATA request");
    let reply = owner
        .authorize_tx(request, MonotonicMillis::new(authorization_ms), policy)
        .unwrap_or_else(|failure| panic!("DATA authorize: {:?}", failure.reason()));
    node_ports
        .replies()
        .try_send(reply)
        .unwrap_or_else(|_| panic!("DATA reply full"));
    assert_eq!(
        dispatcher.step(dispatcher_now_us + 1),
        RadioTxDispatcherStep::Advanced
    );
    assert_eq!(
        dispatcher.step(dispatcher_now_us + 2),
        RadioTxDispatcherStep::Advanced
    );
}

fn terminal_data_gate_fixture(
    tag: u8,
    tx: TxScript,
    frame_request_blocker: Option<AuthorizedFrameObservation>,
) -> (TestNode<1>, TestDispatcher, TestRouter, DispatchReport) {
    let mut owner = node::<1>(tag, "authorized-frame-gate-owner");
    let receiver = node::<0>(tag.wrapping_add(1), "authorized-frame-gate-receiver");
    owner
        .register_peer(
            &identity(tag.wrapping_add(1)),
            "reticulum",
            &["authorized-frame-gate-receiver"],
            MonotonicSeconds::new(0),
        )
        .unwrap();
    let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
    owner.register_packet_buffer(buffer).unwrap();
    let job = prepare_data(
        &mut owner,
        buffer,
        receiver.destination_hash(),
        b"durable authorized frame gate",
        100_000,
    );
    let expected_data_packet = DataPacketDispatchObservation::from_job(&job);
    let data_handoff = Box::leak(Box::new(DataPermitHandoff::<NoopRawMutex>::new()));
    let ordinary_handoff = Box::leak(Box::new(OrdinaryPermitHandoff::<NoopRawMutex>::new()));
    let (mut data_node, data_dispatch) = data_handoff.split();
    let (_, ordinary_dispatch) = ordinary_handoff.split();
    let (mut dispatcher, mut router) = test_dispatcher_with_registry_config_and_frame_blocker(
        MockRadio::new(
            vec![CadScript::Observation {
                busy: false,
                at_us: 1_000_100,
            }],
            vec![tx],
        ),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(2),
        TEST_INTERFACE_CONFIG,
        frame_request_blocker,
    );
    route_data(&mut router, job);
    assert_eq!(
        block_on(dispatcher.wait_for_job()),
        RadioTxDispatcherStep::Advanced
    );
    start_and_clear_data(&mut dispatcher, 1_000_000);
    grant_data(
        &mut dispatcher,
        &mut data_node,
        &mut owner,
        &mut ExactPolicy,
        1_001,
        1_000_200,
    );
    let report = match block_on(dispatcher.perform_radio_operation(1_000_300)) {
        RadioOperationStep::Terminal(report) => report,
        other => panic!("DATA TX did not terminate: {other:?}"),
    };
    let authorized_frame = report
        .authorized_frame()
        .expect("authorized frame evidence");
    assert_eq!(report.data_packet(), Some(expected_data_packet));
    assert_eq!(
        authorized_frame.attempt_handle(),
        expected_data_packet.attempt_handle()
    );
    assert_eq!(authorized_frame.attempt(), expected_data_packet.attempt());
    assert_eq!(
        authorized_frame.interface(),
        expected_data_packet.interface()
    );
    assert_eq!(
        authorized_frame.packet_len(),
        usize::from(expected_data_packet.packet_len())
    );
    assert_eq!(
        authorized_frame.encoded_packet_sha256(),
        expected_data_packet.encoded_packet_sha256()
    );
    (owner, dispatcher, router, report)
}

fn assert_no_completion(router: &mut TestRouter) {
    assert!(
        router
            .try_receive_completion()
            .unwrap_or_else(|failure| panic!("completion route failed: {failure:?}"))
            .is_none()
    );
}

#[test]
fn authorized_frame_completion_cannot_overtake_matching_acknowledgement() {
    let (mut owner, mut dispatcher, mut router, report) =
        terminal_data_gate_fixture(82, TxScript::SuccessOne(1_000_500), None);
    let expected = report.authorized_frame().unwrap();
    assert_eq!(
        dispatcher.inner.phase(),
        RadioTxDispatcherPhase::AuthorizedFrameRequest
    );
    assert_eq!(dispatcher.inner.take_last_report(), Some(report));
    assert_eq!(dispatcher.inner.last_report(), None);
    assert_no_completion(&mut router);

    assert_eq!(
        dispatcher.inner.step(1_000_600),
        RadioTxDispatcherStep::Advanced
    );
    assert_eq!(
        dispatcher.inner.phase(),
        RadioTxDispatcherPhase::AuthorizedFrameAcknowledgementWait
    );
    assert_eq!(
        dispatcher.inner.step(1_000_601),
        RadioTxDispatcherStep::NeedAuthorizedFrameAcknowledgement
    );
    assert_no_completion(&mut router);
    {
        let mut wait = pin!(dispatcher.inner.wait_for_authorized_frame_acknowledgement());
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(wait.as_mut().poll(&mut context), Poll::Pending));
    }
    assert_eq!(
        dispatcher.inner.phase(),
        RadioTxDispatcherPhase::AuthorizedFrameAcknowledgementWait
    );
    assert_eq!(
        dispatcher.authorized_frames.requests().try_receive(),
        Some(expected)
    );
    dispatcher
        .authorized_frames
        .acknowledgements()
        .try_send(expected)
        .unwrap_or_else(|_| panic!("matching acknowledgement channel full"));
    assert_eq!(
        block_on(dispatcher.inner.wait_for_authorized_frame_acknowledgement()),
        AuthorizedFrameAcknowledgementProgress::Matched
    );
    assert_eq!(
        dispatcher.inner.phase(),
        RadioTxDispatcherPhase::CompletionReturn(DispatchFamily::Data)
    );
    assert_no_completion(&mut router);
    assert_eq!(
        dispatcher.inner.step(1_000_602),
        RadioTxDispatcherStep::Advanced
    );
    let completion = take_data_completion(&mut router);
    assert!(matches!(
        owner.complete_tx(completion, MonotonicMillis::new(1_001)),
        Ok(TxCompletionDisposition::Available(_))
    ));
    assert_eq!(dispatcher.inner.phase(), RadioTxDispatcherPhase::Idle);
}

#[test]
fn final_txdone_followed_by_cleanup_fault_retains_physical_timestamp() {
    let (mut owner, mut dispatcher, mut router, report) = terminal_data_gate_fixture(
        84,
        TxScript::Fault(PacketTxProgress::first_completed(1_000_500)),
        None,
    );
    assert_eq!(report.frame_count(), 1);
    assert!(matches!(report.outcome(), DispatchOutcome::TxFault { .. }));
    assert_eq!(report.progress().unwrap().completed_frame_count(), 1);
    let expected = report.authorized_frame().unwrap();

    assert_eq!(
        dispatcher.inner.step(1_000_600),
        RadioTxDispatcherStep::Advanced
    );
    assert_eq!(
        dispatcher.authorized_frames.requests().try_receive(),
        Some(expected)
    );
    dispatcher
        .authorized_frames
        .acknowledgements()
        .try_send(expected)
        .unwrap_or_else(|_| panic!("matching acknowledgement channel full"));
    assert_eq!(
        dispatcher.inner.step(60_000_000),
        RadioTxDispatcherStep::Advanced
    );
    assert_eq!(
        dispatcher.inner.step(60_000_001),
        RadioTxDispatcherStep::Disabled(DispatcherFault::RadioUnavailable)
    );
    let completion = take_data_completion(&mut router);
    assert!(matches!(
        owner.complete_tx(completion, MonotonicMillis::new(60_001)),
        Ok(TxCompletionDisposition::Available(_))
    ));

    let mut rng = CounterRng::default();
    let timeout = owner.tick(MonotonicSeconds::new(60), &mut rng);
    assert_eq!(timeout.timed_out_attempts, 1);
}

#[test]
fn missing_txdone_timestamp_uses_post_return_reconciliation_boundary() {
    let (mut owner, mut dispatcher, mut router, report) = terminal_data_gate_fixture(
        85,
        TxScript::Fault(PacketTxProgress::first_completed_timestamp_missing()),
        None,
    );
    assert_eq!(report.frame_count(), 1);
    assert!(matches!(report.outcome(), DispatchOutcome::TxFault { .. }));
    assert_eq!(report.progress().unwrap().completed_frame_count(), 1);
    let expected = report.authorized_frame().unwrap();

    assert_eq!(
        dispatcher.inner.step(1_000_600),
        RadioTxDispatcherStep::Advanced
    );
    assert_eq!(
        dispatcher.authorized_frames.requests().try_receive(),
        Some(expected)
    );
    dispatcher
        .authorized_frames
        .acknowledgements()
        .try_send(expected)
        .unwrap_or_else(|_| panic!("matching acknowledgement channel full"));
    assert_eq!(
        dispatcher.inner.step(60_000_000),
        RadioTxDispatcherStep::Advanced
    );
    assert_eq!(
        dispatcher.inner.step(60_000_001),
        RadioTxDispatcherStep::Disabled(DispatcherFault::RadioUnavailable)
    );
    let completion = take_data_completion(&mut router);
    assert!(matches!(
        owner.complete_tx(completion, MonotonicMillis::new(60_001)),
        Ok(TxCompletionDisposition::Available(_))
    ));

    let mut rng = CounterRng::default();
    let before_reconciliation_deadline = owner.tick(MonotonicSeconds::new(90), &mut rng);
    assert_eq!(before_reconciliation_deadline.timed_out_attempts, 0);
    let after_reconciliation_deadline = owner.tick(MonotonicSeconds::new(92), &mut rng);
    assert_eq!(after_reconciliation_deadline.timed_out_attempts, 1);
}

#[test]
fn frame_ack_wait_retains_queued_ordinary_job_and_excludes_completion_and_rx() {
    let (mut data_owner, mut dispatcher, mut router, report) =
        terminal_data_gate_fixture(93, TxScript::SuccessOne(1_000_500), None);
    let expected_frame = report.authorized_frame().unwrap();
    assert_eq!(
        dispatcher.inner.step(1_000_600),
        RadioTxDispatcherStep::Advanced
    );
    assert_eq!(
        dispatcher.authorized_frames.requests().try_receive(),
        Some(expected_frame)
    );
    assert_eq!(
        dispatcher.inner.phase(),
        RadioTxDispatcherPhase::AuthorizedFrameAcknowledgementWait
    );

    let mut ordinary_node = node::<1>(94, "frame-ack-wait-ordinary");
    let mut ordinary_owner = ordinary_node.take_ordinary_action_owner::<1>().unwrap();
    let ordinary_buffer = Box::leak(Box::new(OrdinaryPacketBuffer::new()));
    let ordinary_refs = Box::leak(Box::new([ordinary_buffer]));
    let ordinary_job = prepare_ordinary(
        &mut ordinary_node,
        &mut ordinary_owner,
        ordinary_refs,
        4,
        2_000,
    );
    let expected_ordinary = ordinary_job.prepared();
    route_ordinary(&mut router, ordinary_job);

    dispatcher
        .inner
        .radio
        .rx
        .push_back(RxScript::NoPreambleTimeout);
    let mut rx_buffer = [0; SX1262_FRAME_MTU];
    assert!(matches!(
        dispatcher.inner.start_continuous_receive_until(
            &mut rx_buffer,
            core::future::pending(),
            core::future::pending(),
        ),
        Err(RadioReceiveStep::TxPriority(
            RadioTxDispatcherPhase::AuthorizedFrameAcknowledgementWait
        ))
    ));
    assert_eq!(dispatcher.inner.radio.rx.len(), 1);

    // Advancing beyond the queued ordinary owner's deadline cannot make
    // either family escape the exact DATA durability gate.
    for now_us in [2_000_000, 3_000_000, 60_000_000] {
        assert_eq!(
            dispatcher.inner.step(now_us),
            RadioTxDispatcherStep::NeedAuthorizedFrameAcknowledgement
        );
        assert_eq!(
            dispatcher.inner.phase(),
            RadioTxDispatcherPhase::AuthorizedFrameAcknowledgementWait
        );
        assert_no_completion(&mut router);
        assert_eq!(dispatcher.inner.radio.captured.len(), 1);
        assert_eq!(dispatcher.inner.radio.rx.len(), 1);
    }

    dispatcher
        .authorized_frames
        .acknowledgements()
        .try_send(expected_frame)
        .unwrap_or_else(|_| panic!("matching acknowledgement channel full"));
    assert_eq!(
        dispatcher.inner.step(60_000_001),
        RadioTxDispatcherStep::Advanced
    );
    assert_eq!(
        dispatcher.inner.phase(),
        RadioTxDispatcherPhase::CompletionReturn(DispatchFamily::Data)
    );
    assert_eq!(
        dispatcher.inner.step(60_000_002),
        RadioTxDispatcherStep::Advanced
    );
    let data_completion = take_data_completion(&mut router);
    assert!(matches!(
        data_owner.complete_tx(data_completion, MonotonicMillis::new(60_001)),
        Ok(TxCompletionDisposition::Available(_))
    ));
    assert_eq!(dispatcher.inner.phase(), RadioTxDispatcherPhase::Idle);

    // The exact ordinary owner remained queued. Once DATA releases, its
    // now-expired metadata returns unchanged without another RF operation.
    assert_eq!(
        dispatcher.inner.step(60_000_003),
        RadioTxDispatcherStep::Advanced
    );
    assert_eq!(
        dispatcher.inner.phase(),
        RadioTxDispatcherPhase::CompletionReturn(DispatchFamily::Ordinary)
    );
    assert_eq!(
        dispatcher.inner.step(60_000_004),
        RadioTxDispatcherStep::Advanced
    );
    let ordinary_completion = take_ordinary_completion(&mut router);
    assert_eq!(ordinary_completion.prepared(), expected_ordinary);
    assert_eq!(dispatcher.inner.radio.captured.len(), 1);
    assert_eq!(dispatcher.inner.radio.rx.len(), 1);
    assert_eq!(dispatcher.inner.phase(), RadioTxDispatcherPhase::Idle);
}

#[test]
fn authorized_frame_request_backpressure_retains_exact_observation_and_completion() {
    let (_blocker_owner, _blocker_dispatcher, _blocker_router, blocker_report) =
        terminal_data_gate_fixture(84, TxScript::SuccessOne(1_000_500), None);
    let blocker = blocker_report.authorized_frame().unwrap();
    let (mut owner, mut dispatcher, mut router, report) =
        terminal_data_gate_fixture(86, TxScript::SuccessOne(1_000_500), Some(blocker));
    let expected = report.authorized_frame().unwrap();
    assert_ne!(expected, blocker);

    assert_eq!(
        dispatcher.inner.step(1_000_600),
        RadioTxDispatcherStep::Backpressured(
            RadioTxDispatcherChannel::AuthorizedFrameObservationRequest
        )
    );
    assert_eq!(
        dispatcher.inner.phase(),
        RadioTxDispatcherPhase::AuthorizedFrameRequest
    );
    assert_no_completion(&mut router);
    let wake_counter = Arc::new(WakeCounter::default());
    let waker = Waker::from(wake_counter.clone());
    let mut context = Context::from_waker(&waker);
    assert!(matches!(
        dispatcher
            .inner
            .poll_authorized_frame_request_capacity(&mut context),
        Poll::Pending
    ));
    {
        let mut wait = pin!(
            dispatcher
                .inner
                .wait_for_authorized_frame_request_capacity()
        );
        assert!(matches!(wait.as_mut().poll(&mut context), Poll::Pending));
    }
    assert_eq!(
        dispatcher.authorized_frames.requests().try_receive(),
        Some(blocker)
    );
    assert_eq!(wake_counter.0.load(Ordering::SeqCst), 1);
    assert_eq!(
        dispatcher
            .inner
            .poll_authorized_frame_request_capacity(&mut context),
        Poll::Ready(AuthorizedFrameRequestCapacity::Ready)
    );
    assert_eq!(
        dispatcher.inner.step(1_000_601),
        RadioTxDispatcherStep::Advanced
    );
    assert_eq!(
        dispatcher.authorized_frames.requests().try_receive(),
        Some(expected),
        "full-channel retry changed the retained observation"
    );
    assert_no_completion(&mut router);
    dispatcher
        .authorized_frames
        .acknowledgements()
        .try_send(expected)
        .unwrap_or_else(|_| panic!("matching acknowledgement channel full"));
    assert_eq!(
        dispatcher.inner.step(1_000_602),
        RadioTxDispatcherStep::Advanced
    );
    assert_no_completion(&mut router);
    assert_eq!(
        dispatcher.inner.step(1_000_603),
        RadioTxDispatcherStep::Advanced
    );
    let completion = take_data_completion(&mut router);
    assert!(matches!(
        owner.complete_tx(completion, MonotonicMillis::new(1_001)),
        Ok(TxCompletionDisposition::Available(_))
    ));
}

#[test]
fn mismatched_frame_acknowledgement_disables_and_retains_both_observations() {
    let (_other_owner, _other_dispatcher, _other_router, other_report) =
        terminal_data_gate_fixture(88, TxScript::SuccessOne(1_000_500), None);
    let actual = other_report.authorized_frame().unwrap();
    let (_owner, mut dispatcher, mut router, report) =
        terminal_data_gate_fixture(90, TxScript::SuccessOne(1_000_500), None);
    let expected = report.authorized_frame().unwrap();
    assert_ne!(expected, actual);
    assert_eq!(
        dispatcher.inner.step(1_000_600),
        RadioTxDispatcherStep::Advanced
    );
    assert_eq!(
        dispatcher.authorized_frames.requests().try_receive(),
        Some(expected)
    );
    dispatcher
        .authorized_frames
        .acknowledgements()
        .try_send(actual)
        .unwrap_or_else(|_| panic!("mismatched acknowledgement channel full"));
    assert_eq!(
        dispatcher.inner.step(1_000_601),
        RadioTxDispatcherStep::Disabled(DispatcherFault::AuthorizedFrameAcknowledgementMismatch)
    );
    assert_eq!(
        dispatcher.inner.fault(),
        Some(DispatcherFault::AuthorizedFrameAcknowledgementMismatch)
    );
    assert_eq!(
        dispatcher.inner.fault_residue_kind(),
        Some(DispatcherFaultResidueKind::AuthorizedFrameAcknowledgementMismatch)
    );
    let residue = dispatcher
        .inner
        .authorized_frame_acknowledgement_mismatch()
        .unwrap();
    assert_eq!(residue.expected(), expected);
    assert_eq!(residue.actual(), actual);
    assert_no_completion(&mut router);
    assert_eq!(
        dispatcher.inner.step(1_000_602),
        RadioTxDispatcherStep::Disabled(DispatcherFault::AuthorizedFrameAcknowledgementMismatch)
    );
    assert_no_completion(&mut router);
}

#[test]
fn acknowledgement_before_request_is_rejected_even_when_exact() {
    let (_owner, mut dispatcher, mut router, report) =
        terminal_data_gate_fixture(92, TxScript::SuccessOne(1_000_500), None);
    let expected = report.authorized_frame().unwrap();
    dispatcher
        .authorized_frames
        .acknowledgements()
        .try_send(expected)
        .unwrap_or_else(|_| panic!("premature acknowledgement channel full"));
    assert_eq!(
        dispatcher.inner.step(1_000_600),
        RadioTxDispatcherStep::Disabled(DispatcherFault::UnexpectedAuthorizedFrameAcknowledgement)
    );
    assert_eq!(
        dispatcher.inner.fault_residue_kind(),
        Some(DispatcherFaultResidueKind::UnexpectedAuthorizedFrameAcknowledgement)
    );
    let residue = dispatcher
        .inner
        .unexpected_authorized_frame_acknowledgement()
        .unwrap();
    assert_eq!(residue.expected(), expected);
    assert_eq!(residue.actual(), expected);
    assert!(
        dispatcher
            .authorized_frames
            .requests()
            .try_receive()
            .is_none()
    );
    assert_no_completion(&mut router);
}

fn start_and_clear_ordinary(dispatcher: &mut TestDispatcher, now_us: u64) {
    assert_eq!(dispatcher.step(now_us), RadioTxDispatcherStep::Advanced);
    assert!(matches!(
        dispatcher.step(now_us + 100),
        RadioTxDispatcherStep::NeedCad(DispatchFamily::Ordinary)
    ));
    assert!(matches!(
        block_on(dispatcher.perform_radio_operation(now_us + 101)),
        RadioOperationStep::CadObserved {
            family: DispatchFamily::Ordinary,
            activity_detected: false,
            ..
        }
    ));
}

fn grant_ordinary(
    dispatcher: &mut TestDispatcher,
    node_ports: &mut OrdinaryNodePermitHandoff<NoopRawMutex>,
    owner: &mut OrdinaryActionOwner<1>,
    policy: &mut impl TxAuthorizationPolicy,
    now_us: u64,
) {
    assert_eq!(dispatcher.step(now_us), RadioTxDispatcherStep::Advanced);
    let request = node_ports
        .requests()
        .try_receive()
        .expect("ordinary request");
    let reply = owner
        .authorize_tx(request, MonotonicMillis::new(now_us / 1_000), policy)
        .unwrap_or_else(|failure| panic!("ordinary authorize: {:?}", failure.reason()));
    node_ports
        .replies()
        .try_send(reply)
        .unwrap_or_else(|_| panic!("ordinary reply full"));
    assert_eq!(dispatcher.step(now_us + 1), RadioTxDispatcherStep::Advanced);
    assert_eq!(dispatcher.step(now_us + 2), RadioTxDispatcherStep::Advanced);
}

#[test]
fn mismatched_data_interface_config_returns_exact_owner_without_radio_or_permit_access() {
    let mut owner = node::<1>(61, "mismatched-data-config");
    let receiver = node::<0>(62, "mismatched-data-config-receiver");
    owner
        .register_peer(
            &identity(62),
            "reticulum",
            &["mismatched-data-config-receiver"],
            MonotonicSeconds::new(0),
        )
        .unwrap();
    let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
    owner.register_packet_buffer(buffer).unwrap();
    let job = prepare_data(
        &mut owner,
        buffer,
        receiver.destination_hash(),
        b"wrong actor config",
        100_000,
    );
    let data_handoff = Box::leak(Box::new(DataPermitHandoff::<NoopRawMutex>::new()));
    let ordinary_handoff = Box::leak(Box::new(OrdinaryPermitHandoff::<NoopRawMutex>::new()));
    let (mut data_node, data_dispatch) = data_handoff.split();
    let (_, ordinary_dispatch) = ordinary_handoff.split();
    let stamped = InterfaceConfigId::new(2);
    let (mut dispatcher, mut router) = test_dispatcher_with_registry_config(
        MockRadio::new(
            vec![CadScript::Observation {
                busy: false,
                at_us: 1_000_100,
            }],
            vec![TxScript::SuccessOne(1_000_200)],
        ),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(2),
        stamped,
    );
    route_data(&mut router, job);

    assert_eq!(
        block_on(dispatcher.wait_for_job()),
        RadioTxDispatcherStep::Advanced
    );
    assert_eq!(
        dispatcher.last_report().map(DispatchReport::outcome),
        Some(DispatchOutcome::InterfaceConfigurationMismatch {
            expected: TEST_INTERFACE_CONFIG,
            stamped,
        })
    );
    assert!(data_node.requests().try_receive().is_none());
    assert_eq!(dispatcher.radio().cad.len(), 1);
    assert_eq!(dispatcher.radio().tx.len(), 1);
    assert!(dispatcher.radio().captured.is_empty());

    assert_eq!(dispatcher.step(1_000_001), RadioTxDispatcherStep::Advanced);
    let completion = take_data_completion(&mut router);
    assert!(matches!(
        owner.complete_tx(completion, MonotonicMillis::new(1_001)),
        Ok(TxCompletionDisposition::Available(_))
    ));
    assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::Idle);
}

#[test]
fn mismatched_ordinary_interface_config_returns_exact_owner_without_radio_or_permit_access() {
    let mut node = node::<1>(63, "mismatched-ordinary-config");
    let mut owner = node.take_ordinary_action_owner::<1>().unwrap();
    let buffer = Box::leak(Box::new(OrdinaryPacketBuffer::new()));
    let refs = Box::leak(Box::new([buffer]));
    let job = prepare_ordinary(&mut node, &mut owner, refs, 8, 100_000);
    let data_handoff = Box::leak(Box::new(DataPermitHandoff::<NoopRawMutex>::new()));
    let ordinary_handoff = Box::leak(Box::new(OrdinaryPermitHandoff::<NoopRawMutex>::new()));
    let (_, data_dispatch) = data_handoff.split();
    let (mut ordinary_node, ordinary_dispatch) = ordinary_handoff.split();
    let stamped = InterfaceConfigId::new(2);
    let (mut dispatcher, mut router) = test_dispatcher_with_registry_config(
        MockRadio::new(
            vec![CadScript::Observation {
                busy: false,
                at_us: 1_000_100,
            }],
            vec![TxScript::SuccessOne(1_000_200)],
        ),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(2),
        stamped,
    );
    route_ordinary(&mut router, job);

    assert_eq!(dispatcher.step(1_000_000), RadioTxDispatcherStep::Advanced);
    assert_eq!(dispatcher.radio().receive_session_invalidations, 1);
    assert_eq!(
        dispatcher.last_report().map(DispatchReport::outcome),
        Some(DispatchOutcome::InterfaceConfigurationMismatch {
            expected: TEST_INTERFACE_CONFIG,
            stamped,
        })
    );
    assert!(ordinary_node.requests().try_receive().is_none());
    assert_eq!(dispatcher.radio().cad.len(), 1);
    assert_eq!(dispatcher.radio().tx.len(), 1);
    assert!(dispatcher.radio().captured.is_empty());

    assert_eq!(dispatcher.step(1_000_001), RadioTxDispatcherStep::Advanced);
    let completion = take_ordinary_completion(&mut router);
    assert!(matches!(
        owner.complete_tx(completion, MonotonicMillis::new(1_001)),
        Ok(OrdinaryCompletionDisposition::Returned(_))
    ));
    assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::Idle);
}

#[test]
fn data_permit_capacity_wake_cancel_retries_and_reconciles_exact_request() {
    let mut owner = node::<2>(65, "data-permit-capacity");
    let receiver = node::<0>(66, "data-permit-capacity-receiver");
    owner
        .register_peer(
            &identity(66),
            "reticulum",
            &["data-permit-capacity-receiver"],
            MonotonicSeconds::new(0),
        )
        .unwrap();
    let blocker_buffer = Box::leak(Box::new(TxPacketBuffer::new()));
    owner.register_packet_buffer(blocker_buffer).unwrap();
    let active_buffer = Box::leak(Box::new(TxPacketBuffer::new()));
    owner.register_packet_buffer(active_buffer).unwrap();
    let blocker_job = prepare_data(
        &mut owner,
        blocker_buffer,
        receiver.destination_hash(),
        b"permit request blocker",
        100_000,
    );
    let active_job = prepare_data(
        &mut owner,
        active_buffer,
        receiver.destination_hash(),
        b"retained exact request",
        100_000,
    );
    let blocker_requirements = exact_requirements(blocker_job.packet_len());
    let (blocker_pending, blocker_request) = blocker_job.begin_permit(blocker_requirements);

    let data_handoff = Box::leak(Box::new(DataPermitHandoff::<NoopRawMutex>::new()));
    let ordinary_handoff = Box::leak(Box::new(OrdinaryPermitHandoff::<NoopRawMutex>::new()));
    let (mut data_node, mut data_dispatch) = data_handoff.split();
    data_dispatch
        .requests()
        .try_send(blocker_request)
        .unwrap_or_else(|_| panic!("empty DATA request channel rejected blocker"));
    let (_, ordinary_dispatch) = ordinary_handoff.split();
    let (mut dispatcher, mut router) = test_dispatcher(
        MockRadio::new(
            vec![CadScript::Observation {
                busy: false,
                at_us: 1_000_100,
            }],
            vec![],
        ),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(2),
    );
    route_data(&mut router, active_job);
    start_and_clear_data(&mut dispatcher, 1_000_000);
    assert_eq!(
        dispatcher.step(1_000_200),
        RadioTxDispatcherStep::Backpressured(RadioTxDispatcherChannel::DataPermitRequest)
    );
    let grace_deadline_us = 100_500_000;

    let wake_counter = Arc::new(WakeCounter::default());
    let waker = Waker::from(Arc::clone(&wake_counter));
    let mut context = Context::from_waker(&waker);
    assert_eq!(
        dispatcher.poll_permit_request_capacity(&mut context),
        PermitRequestCapacity::Pending {
            family: DispatchFamily::Data,
            grace_deadline_us,
        }
    );
    let mut capacity_wait = Box::pin(dispatcher.wait_for_permit_request_capacity());
    assert!(matches!(
        capacity_wait.as_mut().poll(&mut context),
        Poll::Pending
    ));

    let blocker_request = data_node
        .requests()
        .try_receive()
        .expect("DATA blocker request");
    assert!(wake_counter.0.load(Ordering::SeqCst) > 0);
    let blocker_reply = owner
        .authorize_tx(
            blocker_request,
            MonotonicMillis::new(1_001),
            &mut DenyPolicy,
        )
        .unwrap_or_else(|failure| panic!("DATA blocker authorize: {:?}", failure.reason()));
    let blocker = match blocker_pending
        .resolve(blocker_reply, MonotonicMillis::new(1_001))
        .unwrap_or_else(|_| panic!("DATA blocker reply crossed owners"))
    {
        PermitResolution::Unpermitted(owner) => owner.complete(TxCompletionCode::new(0x81)),
        _ => panic!("denied DATA blocker became authorized"),
    };
    assert!(matches!(
        owner.complete_tx(blocker, MonotonicMillis::new(1_001)),
        Ok(TxCompletionDisposition::Available(_))
    ));

    // Cancellation after the wake cannot move the active request or
    // reserve the newly available scalar-control slot.
    drop(capacity_wait);
    assert_eq!(
        dispatcher.poll_permit_request_capacity(&mut context),
        PermitRequestCapacity::Ready {
            family: DispatchFamily::Data,
            grace_deadline_us,
        }
    );
    grant_data(
        &mut dispatcher,
        &mut data_node,
        &mut owner,
        &mut DenyPolicy,
        1_001,
        1_000_201,
    );
    assert_eq!(dispatcher.step(1_000_204), RadioTxDispatcherStep::Advanced);
    assert_eq!(dispatcher.step(1_000_205), RadioTxDispatcherStep::Advanced);
    let completion = take_data_completion(&mut router);
    assert!(matches!(
        owner.complete_tx(completion, MonotonicMillis::new(1_001)),
        Ok(TxCompletionDisposition::Available(_))
    ));
    assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::Idle);
}

#[test]
fn ordinary_permit_capacity_wake_cancel_retries_and_reconciles_exact_request() {
    let mut blocker_node = node::<1>(67, "ordinary-permit-capacity-blocker");
    let mut blocker_owner = blocker_node.take_ordinary_action_owner::<1>().unwrap();
    let blocker_buffer = Box::leak(Box::new(OrdinaryPacketBuffer::new()));
    let blocker_refs = Box::leak(Box::new([blocker_buffer]));
    let blocker_job = prepare_ordinary(
        &mut blocker_node,
        &mut blocker_owner,
        blocker_refs,
        8,
        100_000,
    );
    let blocker_requirements = exact_requirements(blocker_job.packet_len());
    let (blocker_pending, blocker_request) = blocker_job.begin_permit(blocker_requirements);

    let mut node = node::<1>(68, "ordinary-permit-capacity");
    let mut owner = node.take_ordinary_action_owner::<1>().unwrap();
    let active_buffer = Box::leak(Box::new(OrdinaryPacketBuffer::new()));
    let active_refs = Box::leak(Box::new([active_buffer]));
    let active_job = prepare_ordinary(&mut node, &mut owner, active_refs, 8, 100_000);

    let data_handoff = Box::leak(Box::new(DataPermitHandoff::<NoopRawMutex>::new()));
    let ordinary_handoff = Box::leak(Box::new(OrdinaryPermitHandoff::<NoopRawMutex>::new()));
    let (_, data_dispatch) = data_handoff.split();
    let (mut ordinary_node, mut ordinary_dispatch) = ordinary_handoff.split();
    ordinary_dispatch
        .requests()
        .try_send(blocker_request)
        .unwrap_or_else(|_| panic!("empty ordinary request channel rejected blocker"));
    let (mut dispatcher, mut router) = test_dispatcher(
        MockRadio::new(
            vec![CadScript::Observation {
                busy: false,
                at_us: 1_000_100,
            }],
            vec![],
        ),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(2),
    );
    route_ordinary(&mut router, active_job);
    start_and_clear_ordinary(&mut dispatcher, 1_000_000);
    assert_eq!(
        dispatcher.step(1_000_200),
        RadioTxDispatcherStep::Backpressured(RadioTxDispatcherChannel::OrdinaryPermitRequest)
    );
    let grace_deadline_us = 100_500_000;

    let wake_counter = Arc::new(WakeCounter::default());
    let waker = Waker::from(Arc::clone(&wake_counter));
    let mut context = Context::from_waker(&waker);
    assert_eq!(
        dispatcher.poll_permit_request_capacity(&mut context),
        PermitRequestCapacity::Pending {
            family: DispatchFamily::Ordinary,
            grace_deadline_us,
        }
    );
    let mut capacity_wait = Box::pin(dispatcher.wait_for_permit_request_capacity());
    assert!(matches!(
        capacity_wait.as_mut().poll(&mut context),
        Poll::Pending
    ));

    let blocker_request = ordinary_node
        .requests()
        .try_receive()
        .expect("ordinary blocker request");
    assert!(wake_counter.0.load(Ordering::SeqCst) > 0);
    let blocker_reply = blocker_owner
        .authorize_tx(
            blocker_request,
            MonotonicMillis::new(1_001),
            &mut DenyPolicy,
        )
        .unwrap_or_else(|failure| panic!("ordinary blocker authorize: {:?}", failure.reason()));
    let blocker = match blocker_pending
        .resolve(blocker_reply, MonotonicMillis::new(1_001))
        .unwrap_or_else(|_| panic!("ordinary blocker reply crossed owners"))
    {
        OrdinaryPermitResolution::Unpermitted(owner) => owner.complete(TxCompletionCode::new(0x82)),
        _ => panic!("denied ordinary blocker became authorized"),
    };
    assert!(matches!(
        blocker_owner.complete_tx(blocker, MonotonicMillis::new(1_001)),
        Ok(OrdinaryCompletionDisposition::Returned(_))
    ));

    drop(capacity_wait);
    assert_eq!(
        dispatcher.poll_permit_request_capacity(&mut context),
        PermitRequestCapacity::Ready {
            family: DispatchFamily::Ordinary,
            grace_deadline_us,
        }
    );
    grant_ordinary(
        &mut dispatcher,
        &mut ordinary_node,
        &mut owner,
        &mut DenyPolicy,
        1_000_201,
    );
    assert_eq!(dispatcher.step(1_000_204), RadioTxDispatcherStep::Advanced);
    assert_eq!(dispatcher.step(1_000_205), RadioTxDispatcherStep::Advanced);
    let completion = take_ordinary_completion(&mut router);
    assert!(matches!(
        owner.complete_tx(completion, MonotonicMillis::new(1_001)),
        Ok(OrdinaryCompletionDisposition::Returned(_))
    ));
    assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::Idle);
}

static SUCCESS_DATA_BUFFER: ConstStaticCell<TxPacketBuffer> =
    ConstStaticCell::new(TxPacketBuffer::new());
static SUCCESS_ORDINARY_BUFFER: ConstStaticCell<OrdinaryPacketBuffer> =
    ConstStaticCell::new(OrdinaryPacketBuffer::new());
static SUCCESS_ORDINARY_REFS: StaticCell<[&'static mut OrdinaryPacketBuffer; 1]> =
    StaticCell::new();
static SUCCESS_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(DataPermitHandoff::new());
static SUCCESS_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(OrdinaryPermitHandoff::new());

#[test]
fn data_and_ordinary_success_share_one_wrapping_sequence_and_exact_permits() {
    let mut owner = node::<1>(1, "success-sender");
    let receiver = node::<0>(2, "success-receiver");
    owner
        .register_peer(
            &identity(2),
            "reticulum",
            &["success-receiver"],
            MonotonicSeconds::new(0),
        )
        .unwrap();
    let data_buffer = SUCCESS_DATA_BUFFER.take();
    owner.register_packet_buffer(data_buffer).unwrap();
    let data_job = prepare_data(
        &mut owner,
        data_buffer,
        receiver.destination_hash(),
        b"one frame",
        100_000,
    );
    let expected_data = data_job.prepared();
    let expected_data_interface = data_job.interface();
    let mut ordinary_owner = owner.take_ordinary_action_owner::<1>().unwrap();
    let ordinary_buffer = SUCCESS_ORDINARY_BUFFER.take();
    let ordinary_refs = SUCCESS_ORDINARY_REFS.init([ordinary_buffer]);
    let ordinary_job = prepare_ordinary(&mut owner, &mut ordinary_owner, ordinary_refs, 8, 100_000);
    let (mut data_node, data_dispatch) = SUCCESS_DATA_HANDOFF.take().split();
    let (mut ordinary_node, ordinary_dispatch) = SUCCESS_ORDINARY_HANDOFF.take().split();
    let radio = MockRadio::new(
        vec![
            CadScript::Observation {
                busy: false,
                at_us: 1_000_100,
            },
            CadScript::Observation {
                busy: false,
                at_us: 2_000_100,
            },
        ],
        vec![
            TxScript::SuccessOne(1_000_500),
            TxScript::SuccessOne(2_000_500),
        ],
    );
    let (mut dispatcher, mut router) = test_dispatcher(
        radio,
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(3),
    );
    route_data(&mut router, data_job);

    assert_eq!(
        block_on(dispatcher.wait_for_job()),
        RadioTxDispatcherStep::Advanced
    );
    route_ordinary(&mut router, ordinary_job);
    start_and_clear_data(&mut dispatcher, 1_000_000);
    grant_data(
        &mut dispatcher,
        &mut data_node,
        &mut owner,
        &mut ExactPolicy,
        1_001,
        1_000_200,
    );
    let report = match block_on(dispatcher.perform_radio_operation(1_000_300)) {
        RadioOperationStep::Terminal(report) => report,
        other => panic!("DATA TX did not terminate: {other:?}"),
    };
    assert_eq!(report.outcome(), DispatchOutcome::Transmitted);
    assert_eq!(report.frame_count(), 1);
    assert_eq!(report.progress().unwrap().completed_frame_count(), 1);
    let authorized_frame = report
        .authorized_frame()
        .expect("DATA byte exposure must be observable");
    assert_eq!(authorized_frame.attempt_handle(), expected_data.handle());
    assert_eq!(authorized_frame.attempt(), expected_data.attempt());
    assert_eq!(authorized_frame.interface(), expected_data_interface);
    assert_eq!(
        authorized_frame.packet_len(),
        usize::from(expected_data.packet_len())
    );
    assert_eq!(
        authorized_frame.encoded_packet_sha256(),
        expected_data.encoded_packet_sha256()
    );
    assert_eq!(dispatcher.step(1_000_600), RadioTxDispatcherStep::Advanced);
    let data_completion = take_data_completion(&mut router);
    assert!(matches!(
        owner.complete_tx(data_completion, MonotonicMillis::new(1_002)),
        Ok(TxCompletionDisposition::Available(_))
    ));

    start_and_clear_ordinary(&mut dispatcher, 2_000_000);
    grant_ordinary(
        &mut dispatcher,
        &mut ordinary_node,
        &mut ordinary_owner,
        &mut ExactPolicy,
        2_000_200,
    );
    let report = match block_on(dispatcher.perform_radio_operation(2_000_300)) {
        RadioOperationStep::Terminal(report) => report,
        other => panic!("ordinary TX did not terminate: {other:?}"),
    };
    assert_eq!(report.family(), DispatchFamily::Ordinary);
    assert_eq!(report.outcome(), DispatchOutcome::Transmitted);
    assert_eq!(report.authorized_frame(), None);
    assert_eq!(dispatcher.step(2_000_600), RadioTxDispatcherStep::Advanced);
    let completion = take_ordinary_completion(&mut router);
    assert_eq!(
        completion.transmission_outcome(),
        reticulum_node_core::OrdinaryTransmissionOutcome::Transmitted
    );
    assert!(matches!(
        ordinary_owner.complete_tx(completion, MonotonicMillis::new(2_001)),
        Ok(OrdinaryCompletionDisposition::Returned(_))
    ));

    let captured = &dispatcher.radio().captured;
    assert_eq!(captured.len(), 2);
    assert_eq!(captured[0].first[0] >> 4, 3);
    assert_eq!(captured[1].first[0] >> 4, 4);
    assert!(captured.iter().all(|packet| packet.second.is_none()));
}

static BUSY_DATA_BUFFER: ConstStaticCell<TxPacketBuffer> =
    ConstStaticCell::new(TxPacketBuffer::new());
static BUSY_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(DataPermitHandoff::new());
static BUSY_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(OrdinaryPermitHandoff::new());
static BUSY_RECOVERY_ORDINARY_BUFFER: ConstStaticCell<OrdinaryPacketBuffer> =
    ConstStaticCell::new(OrdinaryPacketBuffer::new());
static BUSY_RECOVERY_ORDINARY_REFS: StaticCell<[&'static mut OrdinaryPacketBuffer; 1]> =
    StaticCell::new();
static BUSY_RECOVERY_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(DataPermitHandoff::new());
static BUSY_RECOVERY_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(OrdinaryPermitHandoff::new());

#[test]
fn ordinary_control_packet_survives_one_transient_busy_frame() {
    let mut owner = node::<1>(93, "busy-recovery-ordinary");
    let mut ordinary_owner = owner.take_ordinary_action_owner::<1>().unwrap();
    let ordinary_buffer = BUSY_RECOVERY_ORDINARY_BUFFER.take();
    let ordinary_refs = BUSY_RECOVERY_ORDINARY_REFS.init([ordinary_buffer]);
    let ordinary_job = prepare_ordinary(&mut owner, &mut ordinary_owner, ordinary_refs, 8, 100_000);
    let (_, data_dispatch) = BUSY_RECOVERY_DATA_HANDOFF.take().split();
    let (mut ordinary_node, ordinary_dispatch) = BUSY_RECOVERY_ORDINARY_HANDOFF.take().split();
    let channel_access = LogicalPacketAccessConfig::try_new_with_busy_retry_holdoff(
        3, 10, 20, 500_000, 100_000, 100, 100, 100,
    )
    .unwrap();
    let (mut dispatcher, mut router) = test_dispatcher(
        MockRadio::new(
            vec![
                CadScript::Observation {
                    busy: true,
                    at_us: 1_000_100,
                },
                CadScript::Observation {
                    busy: false,
                    at_us: 1_500_300,
                },
            ],
            vec![TxScript::SuccessOne(1_500_700)],
        ),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config_with_access(channel_access),
    );
    route_ordinary(&mut router, ordinary_job);

    assert_eq!(dispatcher.step(1_000_000), RadioTxDispatcherStep::Advanced);
    assert!(matches!(
        dispatcher.step(1_000_050),
        RadioTxDispatcherStep::NeedCad(DispatchFamily::Ordinary)
    ));
    assert!(matches!(
        block_on(dispatcher.perform_radio_operation(1_000_051)),
        RadioOperationStep::CadObserved {
            family: DispatchFamily::Ordinary,
            activity_detected: true,
            observed_at_us: 1_000_100,
        }
    ));
    let retry_at_us = match dispatcher.step(1_100_000) {
        RadioTxDispatcherStep::WaitUntil {
            family: DispatchFamily::Ordinary,
            retry_at_us,
        } => retry_at_us,
        other => panic!("ordinary retry did not retain its busy holdoff: {other:?}"),
    };
    assert!(retry_at_us > 1_500_100);
    assert!(retry_at_us <= 1_500_120);
    assert!(matches!(
        dispatcher.step(1_500_200),
        RadioTxDispatcherStep::NeedCad(DispatchFamily::Ordinary)
    ));
    assert!(matches!(
        block_on(dispatcher.perform_radio_operation(1_500_201)),
        RadioOperationStep::CadObserved {
            family: DispatchFamily::Ordinary,
            activity_detected: false,
            observed_at_us: 1_500_300,
        }
    ));

    grant_ordinary(
        &mut dispatcher,
        &mut ordinary_node,
        &mut ordinary_owner,
        &mut ExactPolicy,
        1_500_400,
    );
    let report = match block_on(dispatcher.perform_radio_operation(1_500_500)) {
        RadioOperationStep::Terminal(report) => report,
        other => panic!("ordinary TX did not terminate: {other:?}"),
    };
    assert_eq!(report.family(), DispatchFamily::Ordinary);
    assert_eq!(report.outcome(), DispatchOutcome::Transmitted);
    assert_eq!(dispatcher.step(1_500_800), RadioTxDispatcherStep::Advanced);
    let completion = take_ordinary_completion(&mut router);
    assert_eq!(
        completion.transmission_outcome(),
        reticulum_node_core::OrdinaryTransmissionOutcome::Transmitted
    );
    assert!(matches!(
        ordinary_owner.complete_tx(completion, MonotonicMillis::new(1_501)),
        Ok(OrdinaryCompletionDisposition::Returned(_))
    ));
    assert_eq!(dispatcher.radio().captured.len(), 1);
}

#[test]
fn busy_retry_exhaustion_never_requests_a_permit_or_transmits() {
    let mut owner = node::<1>(3, "busy-sender");
    let receiver = node::<0>(4, "busy-receiver");
    owner
        .register_peer(
            &identity(4),
            "reticulum",
            &["busy-receiver"],
            MonotonicSeconds::new(0),
        )
        .unwrap();
    let buffer = BUSY_DATA_BUFFER.take();
    owner.register_packet_buffer(buffer).unwrap();
    let job = prepare_data(
        &mut owner,
        buffer,
        receiver.destination_hash(),
        b"busy",
        100_000,
    );
    let expected_data_packet = DataPacketDispatchObservation::from_job(&job);
    let (mut data_node, data_dispatch) = BUSY_DATA_HANDOFF.take().split();
    let (_, ordinary_dispatch) = BUSY_ORDINARY_HANDOFF.take().split();
    let (mut dispatcher, mut router) = test_dispatcher(
        MockRadio::new(
            vec![
                CadScript::Observation {
                    busy: true,
                    at_us: 1_000_100,
                },
                CadScript::Observation {
                    busy: true,
                    at_us: 1_000_300,
                },
            ],
            vec![],
        ),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(2),
    );
    route_data(&mut router, job);
    assert_eq!(dispatcher.step(1_000_000), RadioTxDispatcherStep::Advanced);
    assert_eq!(dispatcher.radio().receive_session_invalidations, 1);
    assert!(matches!(
        dispatcher.step(1_000_050),
        RadioTxDispatcherStep::NeedCad(DispatchFamily::Data)
    ));
    let _ = block_on(dispatcher.perform_radio_operation(1_000_051));
    assert!(matches!(
        dispatcher.step(1_000_250),
        RadioTxDispatcherStep::NeedCad(DispatchFamily::Data)
    ));
    let terminal = block_on(dispatcher.perform_radio_operation(1_000_251));
    assert!(matches!(terminal, RadioOperationStep::Terminal(_)));
    let report = dispatcher.last_report().expect("busy report");
    assert_eq!(
        report.outcome(),
        DispatchOutcome::AccessRejected(LogicalPacketAccessRejection::ChannelBusyExhausted {
            attempts: 2
        })
    );
    assert_eq!(report.data_packet(), Some(expected_data_packet));
    assert_eq!(report.authorized_frame(), None);
    assert!(data_node.requests().try_receive().is_none());
    assert!(dispatcher.radio().captured.is_empty());
    assert_eq!(dispatcher.step(1_000_400), RadioTxDispatcherStep::Advanced);
    let _ = take_data_completion(&mut router);
}

static CAD_CANCEL_DATA_BUFFER: ConstStaticCell<TxPacketBuffer> =
    ConstStaticCell::new(TxPacketBuffer::new());
static CAD_CANCEL_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(DataPermitHandoff::new());
static CAD_CANCEL_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(OrdinaryPermitHandoff::new());

#[test]
fn dropped_cad_future_returns_unpermitted_owner_then_disables_radio() {
    let mut owner = node::<1>(23, "cad-cancel-sender");
    let receiver = node::<0>(24, "cad-cancel-receiver");
    owner
        .register_peer(
            &identity(24),
            "reticulum",
            &["cad-cancel-receiver"],
            MonotonicSeconds::new(0),
        )
        .unwrap();
    let buffer = CAD_CANCEL_DATA_BUFFER.take();
    owner.register_packet_buffer(buffer).unwrap();
    let job = prepare_data(
        &mut owner,
        buffer,
        receiver.destination_hash(),
        b"cancel CAD",
        100_000,
    );
    let (mut data_node, data_dispatch) = CAD_CANCEL_DATA_HANDOFF.take().split();
    let (_, ordinary_dispatch) = CAD_CANCEL_ORDINARY_HANDOFF.take().split();
    let (mut dispatcher, mut router) = test_dispatcher(
        MockRadio::new(vec![CadScript::Pending], vec![]),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(2),
    );
    route_data(&mut router, job);

    assert_eq!(dispatcher.step(1_000_000), RadioTxDispatcherStep::Advanced);
    assert_eq!(
        dispatcher.step(1_000_100),
        RadioTxDispatcherStep::NeedCad(DispatchFamily::Data)
    );
    {
        let mut operation = pin!(dispatcher.perform_radio_operation(1_000_101));
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(
            operation.as_mut().poll(&mut context),
            Poll::Pending
        ));
    }
    assert_eq!(
        dispatcher.phase(),
        RadioTxDispatcherPhase::CadInFlight(DispatchFamily::Data)
    );
    assert!(!dispatcher.radio().is_active());
    assert_eq!(
        block_on(dispatcher.perform_radio_operation(1_000_102)),
        RadioOperationStep::CancelledFutureNeedsRecovery(DispatchFamily::Data)
    );
    assert!(data_node.requests().try_receive().is_none());
    assert!(dispatcher.radio().captured.is_empty());
    assert_eq!(
        dispatcher.recover_cancelled_radio_operation(),
        RadioTxDispatcherStep::Advanced
    );
    assert_eq!(
        dispatcher.last_report().unwrap().outcome(),
        DispatchOutcome::CancelledRadioOperation
    );
    assert_eq!(dispatcher.last_report().unwrap().progress(), None);
    assert_eq!(
        dispatcher.step(1_000_200),
        RadioTxDispatcherStep::Disabled(DispatcherFault::RadioUnavailable)
    );
    let completion = take_data_completion(&mut router);
    assert!(matches!(
        owner.complete_tx(completion, MonotonicMillis::new(1_001)),
        Ok(TxCompletionDisposition::Available(_))
    ));
}

static DENY_DATA_BUFFER: ConstStaticCell<TxPacketBuffer> =
    ConstStaticCell::new(TxPacketBuffer::new());
static DENY_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(DataPermitHandoff::new());
static DENY_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(OrdinaryPermitHandoff::new());

#[test]
fn policy_denial_returns_definitely_unpermitted_without_byte_access() {
    let mut owner = node::<1>(5, "deny-sender");
    let receiver = node::<0>(6, "deny-receiver");
    owner
        .register_peer(
            &identity(6),
            "reticulum",
            &["deny-receiver"],
            MonotonicSeconds::new(0),
        )
        .unwrap();
    let buffer = DENY_DATA_BUFFER.take();
    owner.register_packet_buffer(buffer).unwrap();
    let job = prepare_data(
        &mut owner,
        buffer,
        receiver.destination_hash(),
        b"deny",
        100_000,
    );
    let (mut data_node, data_dispatch) = DENY_DATA_HANDOFF.take().split();
    let (_, ordinary_dispatch) = DENY_ORDINARY_HANDOFF.take().split();
    let (mut dispatcher, mut router) = test_dispatcher(
        MockRadio::new(
            vec![CadScript::Observation {
                busy: false,
                at_us: 1_000_100,
            }],
            vec![],
        ),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(2),
    );
    route_data(&mut router, job);
    start_and_clear_data(&mut dispatcher, 1_000_000);
    grant_data(
        &mut dispatcher,
        &mut data_node,
        &mut owner,
        &mut DenyPolicy,
        1_001,
        1_000_200,
    );
    assert_eq!(dispatcher.step(1_000_300), RadioTxDispatcherStep::Advanced);
    assert_eq!(
        dispatcher.last_report().unwrap().outcome(),
        DispatchOutcome::PermitDenied
    );
    assert!(dispatcher.radio().captured.is_empty());
    assert_eq!(dispatcher.step(1_000_301), RadioTxDispatcherStep::Advanced);
    let _ = take_data_completion(&mut router);
}

static EXPIRED_DATA_BUFFER: ConstStaticCell<TxPacketBuffer> =
    ConstStaticCell::new(TxPacketBuffer::new());
static EXPIRED_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(DataPermitHandoff::new());
static EXPIRED_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(OrdinaryPermitHandoff::new());

#[test]
fn grant_issued_before_deadline_but_delivered_at_deadline_stays_authorized() {
    let mut owner = node::<1>(7, "expired-sender");
    let receiver = node::<0>(8, "expired-receiver");
    owner
        .register_peer(
            &identity(8),
            "reticulum",
            &["expired-receiver"],
            MonotonicSeconds::new(0),
        )
        .unwrap();
    let buffer = EXPIRED_DATA_BUFFER.take();
    owner.register_packet_buffer(buffer).unwrap();
    let job = prepare_data(
        &mut owner,
        buffer,
        receiver.destination_hash(),
        b"late grant",
        1_100,
    );
    let (mut data_node, data_dispatch) = EXPIRED_DATA_HANDOFF.take().split();
    let (_, ordinary_dispatch) = EXPIRED_ORDINARY_HANDOFF.take().split();
    let (mut dispatcher, mut router) = test_dispatcher(
        MockRadio::new(
            vec![CadScript::Observation {
                busy: false,
                at_us: 1_000_100,
            }],
            vec![],
        ),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(2),
    );
    route_data(&mut router, job);
    start_and_clear_data(&mut dispatcher, 1_000_000);
    assert_eq!(dispatcher.step(1_000_200), RadioTxDispatcherStep::Advanced);
    let request = data_node.requests().try_receive().unwrap();
    let reply = owner
        .authorize_tx(request, MonotonicMillis::new(1_050), &mut ExactPolicy)
        .unwrap_or_else(|failure| panic!("late DATA authorize: {:?}", failure.reason()));
    data_node
        .replies()
        .try_send(reply)
        .unwrap_or_else(|_| panic!("reply full"));
    assert_eq!(dispatcher.step(1_100_000), RadioTxDispatcherStep::Advanced);
    assert_eq!(dispatcher.step(1_100_000), RadioTxDispatcherStep::Advanced);
    assert_eq!(dispatcher.step(1_100_001), RadioTxDispatcherStep::Advanced);
    assert_eq!(
        dispatcher.last_report().unwrap().outcome(),
        DispatchOutcome::AuthorizationExpired
    );
    assert!(dispatcher.radio().captured.is_empty());
    assert_eq!(dispatcher.step(1_100_002), RadioTxDispatcherStep::Advanced);
    let _ = take_data_completion(&mut router);
}

static DATA_GRACE_BUFFER: ConstStaticCell<TxPacketBuffer> =
    ConstStaticCell::new(TxPacketBuffer::new());
static DATA_GRACE_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(DataPermitHandoff::new());
static DATA_GRACE_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(OrdinaryPermitHandoff::new());

#[test]
fn lost_data_permit_reply_returns_exact_recovery_then_disables() {
    let mut owner = node::<1>(25, "data-grace-sender");
    let receiver = node::<0>(26, "data-grace-receiver");
    owner
        .register_peer(
            &identity(26),
            "reticulum",
            &["data-grace-receiver"],
            MonotonicSeconds::new(0),
        )
        .unwrap();
    let buffer = DATA_GRACE_BUFFER.take();
    owner.register_packet_buffer(buffer).unwrap();
    let job = prepare_data(
        &mut owner,
        buffer,
        receiver.destination_hash(),
        b"lost DATA reply",
        1_100,
    );
    let (mut data_node, data_dispatch) = DATA_GRACE_HANDOFF.take().split();
    let (_, ordinary_dispatch) = DATA_GRACE_ORDINARY_HANDOFF.take().split();
    let (mut dispatcher, mut router) = test_dispatcher(
        MockRadio::new(
            vec![CadScript::Observation {
                busy: false,
                at_us: 1_000_100,
            }],
            vec![],
        ),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(2),
    );
    route_data(&mut router, job);

    start_and_clear_data(&mut dispatcher, 1_000_000);
    assert_eq!(dispatcher.step(1_000_200), RadioTxDispatcherStep::Advanced);
    let request = data_node.requests().try_receive().expect("DATA request");
    let lost_reply = owner
        .authorize_tx(request, MonotonicMillis::new(1_001), &mut ExactPolicy)
        .unwrap_or_else(|failure| panic!("DATA authorize: {:?}", failure.reason()));
    drop(lost_reply);
    assert_eq!(
        dispatcher.step(1_599_999),
        RadioTxDispatcherStep::NeedPermitReply {
            family: DispatchFamily::Data,
            grace_deadline_us: 1_600_000,
        }
    );
    assert_eq!(dispatcher.step(1_600_000), RadioTxDispatcherStep::Advanced);
    assert_eq!(
        dispatcher.last_report().unwrap().outcome(),
        DispatchOutcome::ControlPlaneRecovery
    );
    assert_eq!(
        dispatcher.step(1_600_001),
        RadioTxDispatcherStep::Disabled(DispatcherFault::PermitReplyGraceExpired(
            DispatchFamily::Data
        ))
    );
    let completion = take_data_completion(&mut router);
    let quarantine = match owner
        .complete_tx(completion, MonotonicMillis::new(1_600))
        .unwrap_or_else(|failure| panic!("DATA recovery correlation: {:?}", failure.reason()))
    {
        TxCompletionDisposition::Quarantined(quarantine) => quarantine,
        _ => panic!("lost DATA reply did not quarantine"),
    };
    assert_eq!(
        quarantine.record().reason(),
        TxRecoveryReason::CompletionFault(TxCompletionCode::new(7))
    );
    assert_eq!(
        quarantine.record().prior_phase(),
        TxRecoveryPriorPhase::Authorized
    );
}

static ORDINARY_GRACE_BUFFER: ConstStaticCell<OrdinaryPacketBuffer> =
    ConstStaticCell::new(OrdinaryPacketBuffer::new());
static ORDINARY_GRACE_REFS: StaticCell<[&'static mut OrdinaryPacketBuffer; 1]> = StaticCell::new();
static ORDINARY_GRACE_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(DataPermitHandoff::new());
static ORDINARY_GRACE_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(OrdinaryPermitHandoff::new());

#[test]
fn lost_ordinary_permit_reply_returns_exact_recovery_then_disables() {
    let mut node = node::<1>(27, "ordinary-grace");
    let mut owner = node.take_ordinary_action_owner::<1>().unwrap();
    let buffer = ORDINARY_GRACE_BUFFER.take();
    let refs = ORDINARY_GRACE_REFS.init([buffer]);
    let job = prepare_ordinary(&mut node, &mut owner, refs, 8, 1_100);
    let (_, data_dispatch) = ORDINARY_GRACE_DATA_HANDOFF.take().split();
    let (mut ordinary_node, ordinary_dispatch) = ORDINARY_GRACE_HANDOFF.take().split();
    let (mut dispatcher, mut router) = test_dispatcher(
        MockRadio::new(
            vec![CadScript::Observation {
                busy: false,
                at_us: 1_000_100,
            }],
            vec![],
        ),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(2),
    );
    route_ordinary(&mut router, job);

    start_and_clear_ordinary(&mut dispatcher, 1_000_000);
    assert_eq!(dispatcher.step(1_000_200), RadioTxDispatcherStep::Advanced);
    let request = ordinary_node
        .requests()
        .try_receive()
        .expect("ordinary request");
    let lost_reply = owner
        .authorize_tx(request, MonotonicMillis::new(1_001), &mut ExactPolicy)
        .unwrap_or_else(|failure| panic!("ordinary authorize: {:?}", failure.reason()));
    drop(lost_reply);
    assert_eq!(
        dispatcher.step(1_599_999),
        RadioTxDispatcherStep::NeedPermitReply {
            family: DispatchFamily::Ordinary,
            grace_deadline_us: 1_600_000,
        }
    );
    assert_eq!(dispatcher.step(1_600_000), RadioTxDispatcherStep::Advanced);
    assert_eq!(
        dispatcher.last_report().unwrap().outcome(),
        DispatchOutcome::ControlPlaneRecovery
    );
    assert_eq!(
        dispatcher.step(1_600_001),
        RadioTxDispatcherStep::Disabled(DispatcherFault::PermitReplyGraceExpired(
            DispatchFamily::Ordinary
        ))
    );
    let completion = take_ordinary_completion(&mut router);
    let quarantine = match owner
        .complete_tx(completion, MonotonicMillis::new(1_600))
        .expect("ordinary recovery correlation")
    {
        OrdinaryCompletionDisposition::Quarantined(quarantine) => quarantine,
        _ => panic!("lost ordinary reply did not quarantine"),
    };
    assert_eq!(
        quarantine.reason(),
        OrdinaryQuarantineReason::RecoveryFault(TxCompletionCode::new(7))
    );
}

static STALE_CLEAR_DATA_BUFFER: ConstStaticCell<TxPacketBuffer> =
    ConstStaticCell::new(TxPacketBuffer::new());
static STALE_CLEAR_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(DataPermitHandoff::new());
static STALE_CLEAR_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(OrdinaryPermitHandoff::new());

#[test]
fn post_grant_stale_clear_rejects_without_byte_access_and_resumes() {
    let mut owner = node::<1>(28, "stale-clear-sender");
    let receiver = node::<0>(29, "stale-clear-receiver");
    owner
        .register_peer(
            &identity(29),
            "reticulum",
            &["stale-clear-receiver"],
            MonotonicSeconds::new(0),
        )
        .unwrap();
    let buffer = STALE_CLEAR_DATA_BUFFER.take();
    owner.register_packet_buffer(buffer).unwrap();
    let job = prepare_data(
        &mut owner,
        buffer,
        receiver.destination_hash(),
        b"stale clear",
        100_000,
    );
    let (mut data_node, data_dispatch) = STALE_CLEAR_DATA_HANDOFF.take().split();
    let (_, ordinary_dispatch) = STALE_CLEAR_ORDINARY_HANDOFF.take().split();
    let (mut dispatcher, mut router) = test_dispatcher(
        MockRadio::new(
            vec![CadScript::Observation {
                busy: false,
                at_us: 1_000_100,
            }],
            vec![],
        ),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(2),
    );
    route_data(&mut router, job);
    start_and_clear_data(&mut dispatcher, 1_000_000);
    grant_data(
        &mut dispatcher,
        &mut data_node,
        &mut owner,
        &mut ExactPolicy,
        1_001,
        1_000_200,
    );

    let report = match block_on(dispatcher.perform_radio_operation(1_100_001)) {
        RadioOperationStep::Terminal(report) => report,
        other => panic!("stale clear did not terminate: {other:?}"),
    };
    assert_eq!(
        report.outcome(),
        DispatchOutcome::PostGrantAccessRejected(
            LogicalPacketAccessRejection::ClearObservationTooOld {
                clear_observed_at_us: 1_000_100,
                dispatch_start_observed_at_us: 1_100_001,
                predicted_first_rf_start_us: 1_100_101,
                observed_age_us: 100_001,
                maximum_age_us: 100_000,
            }
        )
    );
    assert_eq!(report.progress(), None);
    assert!(dispatcher.radio().is_active());
    assert!(dispatcher.radio().captured.is_empty());
    assert_eq!(dispatcher.step(1_100_002), RadioTxDispatcherStep::Advanced);
    let completion = take_data_completion(&mut router);
    assert!(matches!(
        owner.complete_tx(completion, MonotonicMillis::new(1_101)),
        Ok(TxCompletionDisposition::Available(_))
    ));
    assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::Idle);
}

static PRE_PERMIT_DRIFT_DATA_BUFFER: ConstStaticCell<TxPacketBuffer> =
    ConstStaticCell::new(TxPacketBuffer::new());
static PRE_PERMIT_DRIFT_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(DataPermitHandoff::new());
static PRE_PERMIT_DRIFT_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(OrdinaryPermitHandoff::new());
static FINGERPRINT_DRIFT_DATA_BUFFER: ConstStaticCell<TxPacketBuffer> =
    ConstStaticCell::new(TxPacketBuffer::new());
static FINGERPRINT_DRIFT_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(DataPermitHandoff::new());
static FINGERPRINT_DRIFT_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(OrdinaryPermitHandoff::new());

#[test]
fn pre_permit_fingerprint_drift_returns_unpermitted_without_request_or_transmit() {
    let mut owner = node::<1>(34, "pre-permit-drift-sender");
    let receiver = node::<0>(35, "pre-permit-drift-receiver");
    owner
        .register_peer(
            &identity(35),
            "reticulum",
            &["pre-permit-drift-receiver"],
            MonotonicSeconds::new(0),
        )
        .unwrap();
    let buffer = PRE_PERMIT_DRIFT_DATA_BUFFER.take();
    owner.register_packet_buffer(buffer).unwrap();
    let job = prepare_data(
        &mut owner,
        buffer,
        receiver.destination_hash(),
        b"pre-permit fingerprint drift",
        100_000,
    );
    let (mut data_node, data_dispatch) = PRE_PERMIT_DRIFT_DATA_HANDOFF.take().split();
    let (_, ordinary_dispatch) = PRE_PERMIT_DRIFT_ORDINARY_HANDOFF.take().split();
    let (mut dispatcher, mut router) = test_dispatcher(
        MockRadio::new(
            vec![CadScript::Observation {
                busy: false,
                at_us: 1_000_100,
            }],
            vec![],
        ),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(2),
    );
    route_data(&mut router, job);

    assert_eq!(dispatcher.step(1_000_000), RadioTxDispatcherStep::Advanced);
    assert!(matches!(
        dispatcher.step(1_000_100),
        RadioTxDispatcherStep::NeedCad(DispatchFamily::Data)
    ));
    dispatcher.radio.fingerprint = RadioConfigurationFingerprint::new([0x92; 16]);
    let report = match block_on(dispatcher.perform_radio_operation(1_000_101)) {
        RadioOperationStep::Terminal(report) => report,
        other => panic!("pre-permit drift did not terminate: {other:?}"),
    };

    assert_eq!(
        report.outcome(),
        DispatchOutcome::RadioConfigurationChangedBeforePermit
    );
    assert_eq!(report.progress(), None);
    assert!(data_node.requests().try_receive().is_none());
    assert!(!dispatcher.radio().is_active());
    assert!(dispatcher.radio().captured.is_empty());
    assert_eq!(
        dispatcher.step(1_000_102),
        RadioTxDispatcherStep::Disabled(DispatcherFault::RadioUnavailable)
    );
    let completion = take_data_completion(&mut router);
    assert!(matches!(
        owner.complete_tx(completion, MonotonicMillis::new(1_001)),
        Ok(TxCompletionDisposition::Available(_))
    ));
}

#[test]
fn post_grant_fingerprint_drift_shuts_down_without_transmit() {
    let mut owner = node::<1>(30, "fingerprint-drift-sender");
    let receiver = node::<0>(31, "fingerprint-drift-receiver");
    owner
        .register_peer(
            &identity(31),
            "reticulum",
            &["fingerprint-drift-receiver"],
            MonotonicSeconds::new(0),
        )
        .unwrap();
    let buffer = FINGERPRINT_DRIFT_DATA_BUFFER.take();
    owner.register_packet_buffer(buffer).unwrap();
    let job = prepare_data(
        &mut owner,
        buffer,
        receiver.destination_hash(),
        b"fingerprint drift",
        100_000,
    );
    let (mut data_node, data_dispatch) = FINGERPRINT_DRIFT_DATA_HANDOFF.take().split();
    let (_, ordinary_dispatch) = FINGERPRINT_DRIFT_ORDINARY_HANDOFF.take().split();
    let (mut dispatcher, mut router) = test_dispatcher(
        MockRadio::new(
            vec![CadScript::Observation {
                busy: false,
                at_us: 1_000_100,
            }],
            vec![],
        ),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(2),
    );
    route_data(&mut router, job);
    start_and_clear_data(&mut dispatcher, 1_000_000);
    grant_data(
        &mut dispatcher,
        &mut data_node,
        &mut owner,
        &mut ExactPolicy,
        1_001,
        1_000_200,
    );
    dispatcher.radio.fingerprint = RadioConfigurationFingerprint::new([0x92; 16]);

    let report = match block_on(dispatcher.perform_radio_operation(1_000_300)) {
        RadioOperationStep::Terminal(report) => report,
        other => panic!("fingerprint drift did not terminate: {other:?}"),
    };
    assert_eq!(
        report.outcome(),
        DispatchOutcome::RadioConfigurationChangedAfterPermit
    );
    assert_eq!(report.progress(), None);
    assert!(!dispatcher.radio().is_active());
    assert!(dispatcher.radio().captured.is_empty());
    assert_eq!(
        dispatcher.step(1_000_301),
        RadioTxDispatcherStep::Disabled(DispatcherFault::RadioUnavailable)
    );
    let completion = take_data_completion(&mut router);
    assert!(matches!(
        owner.complete_tx(completion, MonotonicMillis::new(1_001)),
        Ok(TxCompletionDisposition::Available(_))
    ));
}

static PROFILE_DRIFT_ORDINARY_BUFFER: ConstStaticCell<OrdinaryPacketBuffer> =
    ConstStaticCell::new(OrdinaryPacketBuffer::new());
static PROFILE_DRIFT_ORDINARY_REFS: StaticCell<[&'static mut OrdinaryPacketBuffer; 1]> =
    StaticCell::new();
static PROFILE_DRIFT_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(DataPermitHandoff::new());
static PROFILE_DRIFT_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(OrdinaryPermitHandoff::new());

#[test]
fn post_grant_profile_drift_shuts_down_ordinary_without_transmit() {
    let mut node = node::<1>(32, "profile-drift");
    let mut owner = node.take_ordinary_action_owner::<1>().unwrap();
    let buffer = PROFILE_DRIFT_ORDINARY_BUFFER.take();
    let refs = PROFILE_DRIFT_ORDINARY_REFS.init([buffer]);
    let job = prepare_ordinary(&mut node, &mut owner, refs, 8, 100_000);
    let (_, data_dispatch) = PROFILE_DRIFT_DATA_HANDOFF.take().split();
    let (mut ordinary_node, ordinary_dispatch) = PROFILE_DRIFT_ORDINARY_HANDOFF.take().split();
    let (mut dispatcher, mut router) = test_dispatcher(
        MockRadio::new(
            vec![CadScript::Observation {
                busy: false,
                at_us: 1_000_100,
            }],
            vec![],
        ),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(2),
    );
    route_ordinary(&mut router, job);
    start_and_clear_ordinary(&mut dispatcher, 1_000_000);
    grant_ordinary(
        &mut dispatcher,
        &mut ordinary_node,
        &mut owner,
        &mut ExactPolicy,
        1_000_200,
    );
    dispatcher.radio.profile = changed_profile();

    let report = match block_on(dispatcher.perform_radio_operation(1_000_300)) {
        RadioOperationStep::Terminal(report) => report,
        other => panic!("profile drift did not terminate: {other:?}"),
    };
    assert_eq!(report.family(), DispatchFamily::Ordinary);
    assert_eq!(
        report.outcome(),
        DispatchOutcome::RadioConfigurationChangedAfterPermit
    );
    assert_eq!(report.progress(), None);
    assert!(!dispatcher.radio().is_active());
    assert!(dispatcher.radio().captured.is_empty());
    assert_eq!(
        dispatcher.step(1_000_301),
        RadioTxDispatcherStep::Disabled(DispatcherFault::RadioUnavailable)
    );
    let completion = take_ordinary_completion(&mut router);
    assert!(matches!(
        owner.complete_tx(completion, MonotonicMillis::new(1_001)),
        Ok(OrdinaryCompletionDisposition::Returned(_))
    ));
}

static DATA_RETURN_PRESSURE_BUFFER: ConstStaticCell<TxPacketBuffer> =
    ConstStaticCell::new(TxPacketBuffer::new());
static DATA_RETURN_PRESSURE_BLOCKER: ConstStaticCell<TxPacketBuffer> =
    ConstStaticCell::new(TxPacketBuffer::new());
static DATA_RETURN_PRESSURE_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(DataPermitHandoff::new());
static DATA_RETURN_PRESSURE_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(OrdinaryPermitHandoff::new());

#[test]
fn data_completion_return_survives_channel_backpressure_and_retry() {
    let mut owner = node::<2>(33, "data-return-pressure-sender");
    let receiver = node::<0>(34, "data-return-pressure-receiver");
    owner
        .register_peer(
            &identity(34),
            "reticulum",
            &["data-return-pressure-receiver"],
            MonotonicSeconds::new(0),
        )
        .unwrap();
    let buffer = DATA_RETURN_PRESSURE_BUFFER.take();
    owner.register_packet_buffer(buffer).unwrap();
    let blocker_buffer = DATA_RETURN_PRESSURE_BLOCKER.take();
    owner.register_packet_buffer(blocker_buffer).unwrap();
    let blocker_job = prepare_data(
        &mut owner,
        blocker_buffer,
        receiver.destination_hash(),
        b"DATA completion blocker",
        u64::MAX,
    );
    let job = prepare_data(
        &mut owner,
        buffer,
        receiver.destination_hash(),
        b"DATA return pressure",
        100_000,
    );
    let (mut data_node, data_dispatch) = DATA_RETURN_PRESSURE_HANDOFF.take().split();
    let (_, ordinary_dispatch) = DATA_RETURN_PRESSURE_ORDINARY_HANDOFF.take().split();
    let (mut dispatcher, mut router) = test_dispatcher(
        MockRadio::new(
            vec![CadScript::Observation {
                busy: false,
                at_us: 1_000_100,
            }],
            vec![],
        ),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(2),
    );
    route_data(&mut router, blocker_job);
    assert_eq!(dispatcher.step(900_000), RadioTxDispatcherStep::Advanced);
    assert_eq!(dispatcher.step(900_001), RadioTxDispatcherStep::Advanced);
    route_data(&mut router, job);
    start_and_clear_data(&mut dispatcher, 1_000_000);
    grant_data(
        &mut dispatcher,
        &mut data_node,
        &mut owner,
        &mut DenyPolicy,
        1_001,
        1_000_200,
    );
    assert_eq!(dispatcher.step(1_000_300), RadioTxDispatcherStep::Advanced);
    assert_eq!(
        dispatcher.step(1_000_301),
        RadioTxDispatcherStep::Backpressured(RadioTxDispatcherChannel::InterfaceCompletion(
            DispatchFamily::Data
        ))
    );
    assert_eq!(
        dispatcher.phase(),
        RadioTxDispatcherPhase::CompletionReturn(DispatchFamily::Data)
    );

    let wake_counter = Arc::new(WakeCounter::default());
    let waker = Waker::from(Arc::clone(&wake_counter));
    let mut context = Context::from_waker(&waker);
    let mut capacity_wait = Box::pin(dispatcher.wait_for_interface_completion_capacity());
    assert!(matches!(
        capacity_wait.as_mut().poll(&mut context),
        Poll::Pending
    ));

    let blocker = take_data_completion(&mut router);
    assert!(wake_counter.0.load(Ordering::SeqCst) > 0);
    assert!(matches!(
        owner.complete_tx(blocker, MonotonicMillis::new(1_001)),
        Ok(TxCompletionDisposition::Available(_))
    ));

    // Cancellation after the wake cannot move the stored completion or
    // reserve the newly available queue slot.
    drop(capacity_wait);
    assert_eq!(
        dispatcher.poll_interface_completion_capacity(&mut context),
        Poll::Ready(InterfaceCompletionCapacity::Ready(DispatchFamily::Data))
    );
    assert_eq!(dispatcher.step(1_000_302), RadioTxDispatcherStep::Advanced);
    let completion = take_data_completion(&mut router);
    assert!(matches!(
        owner.complete_tx(completion, MonotonicMillis::new(1_001)),
        Ok(TxCompletionDisposition::Available(_))
    ));
    assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::Idle);
}

static ORDINARY_RETURN_PRESSURE_ACTIVE_BUFFER: ConstStaticCell<OrdinaryPacketBuffer> =
    ConstStaticCell::new(OrdinaryPacketBuffer::new());
static ORDINARY_RETURN_PRESSURE_ACTIVE_REFS: StaticCell<[&'static mut OrdinaryPacketBuffer; 1]> =
    StaticCell::new();
static ORDINARY_RETURN_PRESSURE_BLOCKER_BUFFER: ConstStaticCell<OrdinaryPacketBuffer> =
    ConstStaticCell::new(OrdinaryPacketBuffer::new());
static ORDINARY_RETURN_PRESSURE_BLOCKER_REFS: StaticCell<[&'static mut OrdinaryPacketBuffer; 1]> =
    StaticCell::new();
static ORDINARY_RETURN_PRESSURE_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(DataPermitHandoff::new());
static ORDINARY_RETURN_PRESSURE_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(OrdinaryPermitHandoff::new());

#[test]
fn ordinary_completion_return_survives_channel_backpressure_and_retry() {
    let mut blocker_node = node::<1>(35, "ordinary-return-blocker");
    let mut blocker_owner = blocker_node.take_ordinary_action_owner::<1>().unwrap();
    let blocker_refs = ORDINARY_RETURN_PRESSURE_BLOCKER_REFS
        .init([ORDINARY_RETURN_PRESSURE_BLOCKER_BUFFER.take()]);
    let blocker_job = prepare_ordinary(
        &mut blocker_node,
        &mut blocker_owner,
        blocker_refs,
        8,
        u64::MAX,
    );

    let mut node = node::<1>(36, "ordinary-return-pressure");
    let mut owner = node.take_ordinary_action_owner::<1>().unwrap();
    let refs =
        ORDINARY_RETURN_PRESSURE_ACTIVE_REFS.init([ORDINARY_RETURN_PRESSURE_ACTIVE_BUFFER.take()]);
    let job = prepare_ordinary(&mut node, &mut owner, refs, 8, 100_000);
    let (_, data_dispatch) = ORDINARY_RETURN_PRESSURE_DATA_HANDOFF.take().split();
    let (mut ordinary_node, ordinary_dispatch) = ORDINARY_RETURN_PRESSURE_HANDOFF.take().split();
    let (mut dispatcher, mut router) = test_dispatcher(
        MockRadio::new(
            vec![CadScript::Observation {
                busy: false,
                at_us: 1_000_100,
            }],
            vec![],
        ),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(2),
    );
    route_ordinary(&mut router, blocker_job);
    assert_eq!(dispatcher.step(900_000), RadioTxDispatcherStep::Advanced);
    assert_eq!(dispatcher.step(900_001), RadioTxDispatcherStep::Advanced);
    route_ordinary(&mut router, job);
    start_and_clear_ordinary(&mut dispatcher, 1_000_000);
    grant_ordinary(
        &mut dispatcher,
        &mut ordinary_node,
        &mut owner,
        &mut DenyPolicy,
        1_000_200,
    );
    assert_eq!(dispatcher.step(1_000_300), RadioTxDispatcherStep::Advanced);
    assert_eq!(
        dispatcher.step(1_000_301),
        RadioTxDispatcherStep::Backpressured(RadioTxDispatcherChannel::InterfaceCompletion(
            DispatchFamily::Ordinary
        ))
    );
    assert_eq!(
        dispatcher.phase(),
        RadioTxDispatcherPhase::CompletionReturn(DispatchFamily::Ordinary)
    );
    let blocker = take_ordinary_completion(&mut router);
    assert!(matches!(
        blocker_owner.complete_tx(blocker, MonotonicMillis::new(1_001)),
        Ok(OrdinaryCompletionDisposition::Returned(_))
    ));
    assert_eq!(dispatcher.step(1_000_302), RadioTxDispatcherStep::Advanced);
    let completion = take_ordinary_completion(&mut router);
    assert!(matches!(
        owner.complete_tx(completion, MonotonicMillis::new(1_001)),
        Ok(OrdinaryCompletionDisposition::Returned(_))
    ));
    assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::Idle);
}

static OVERFLOW_DATA_BUFFER: ConstStaticCell<TxPacketBuffer> =
    ConstStaticCell::new(TxPacketBuffer::new());
static OVERFLOW_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(DataPermitHandoff::new());
static OVERFLOW_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(OrdinaryPermitHandoff::new());

#[test]
fn deadline_conversion_overflow_preserves_planned_frame_count() {
    let mut owner = node::<1>(19, "overflow-sender");
    let receiver = node::<0>(20, "overflow-receiver");
    owner
        .register_peer(
            &identity(20),
            "reticulum",
            &["overflow-receiver"],
            MonotonicSeconds::new(0),
        )
        .unwrap();
    let buffer = OVERFLOW_DATA_BUFFER.take();
    owner.register_packet_buffer(buffer).unwrap();
    let job = prepare_data(
        &mut owner,
        buffer,
        receiver.destination_hash(),
        b"overflow",
        u64::MAX,
    );
    let expected_frames = profile()
        .rnode_packet_airtime(job.packet_len().into())
        .unwrap()
        .frame_count();
    let (_data_node, data_dispatch) = OVERFLOW_DATA_HANDOFF.take().split();
    let (_, ordinary_dispatch) = OVERFLOW_ORDINARY_HANDOFF.take().split();
    let (mut dispatcher, mut router) = test_dispatcher(
        MockRadio::new(vec![], vec![]),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(2),
    );
    route_data(&mut router, job);

    assert_eq!(dispatcher.step(1_000_000), RadioTxDispatcherStep::Advanced);
    let report = dispatcher.last_report().expect("overflow report");
    assert_eq!(
        report.outcome(),
        DispatchOutcome::DeadlineConversionOverflow
    );
    assert_eq!(report.frame_count(), expected_frames);
    assert_eq!(dispatcher.step(1_000_001), RadioTxDispatcherStep::Advanced);
    let _ = take_data_completion(&mut router);
    assert!(dispatcher.radio().captured.is_empty());
}

static PARTIAL_DATA_BUFFER: ConstStaticCell<TxPacketBuffer> =
    ConstStaticCell::new(TxPacketBuffer::new());
static PARTIAL_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(DataPermitHandoff::new());
static PARTIAL_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(OrdinaryPermitHandoff::new());

#[test]
fn split_frame_tx_fault_preserves_first_frame_progress_and_one_sequence() {
    let mut owner = node::<1>(9, "partial-sender");
    let receiver = node::<0>(10, "partial-receiver");
    owner
        .register_peer(
            &identity(10),
            "reticulum",
            &["partial-receiver"],
            MonotonicSeconds::new(0),
        )
        .unwrap();
    let buffer = PARTIAL_DATA_BUFFER.take();
    owner.register_packet_buffer(buffer).unwrap();
    let plaintext = [0x5a; MAX_DATA_PAYLOAD];
    let job = prepare_data(
        &mut owner,
        buffer,
        receiver.destination_hash(),
        &plaintext,
        100_000,
    );
    assert!(job.packet_len() > 254);
    let (mut data_node, data_dispatch) = PARTIAL_DATA_HANDOFF.take().split();
    let (_, ordinary_dispatch) = PARTIAL_ORDINARY_HANDOFF.take().split();
    let (mut dispatcher, mut router) = test_dispatcher(
        MockRadio::new(
            vec![CadScript::Observation {
                busy: false,
                at_us: 1_000_100,
            }],
            vec![TxScript::Fault(PacketTxProgress::first_completed(
                1_000_500,
            ))],
        ),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(2),
    );
    route_data(&mut router, job);
    start_and_clear_data(&mut dispatcher, 1_000_000);
    grant_data(
        &mut dispatcher,
        &mut data_node,
        &mut owner,
        &mut ExactPolicy,
        1_001,
        1_000_200,
    );
    let report = match block_on(dispatcher.perform_radio_operation(1_000_300)) {
        RadioOperationStep::Terminal(report) => report,
        other => panic!("partial TX did not terminate: {other:?}"),
    };
    assert_eq!(report.frame_count(), 2);
    assert!(matches!(report.outcome(), DispatchOutcome::TxFault { .. }));
    assert_eq!(report.progress().unwrap().completed_frame_count(), 1);
    let packet = &dispatcher.radio().captured[0];
    assert!(packet.second.is_some());
    assert_eq!(packet.first[0], packet.second.as_ref().unwrap()[0]);
    assert_eq!(packet.first[0] >> 4, 3);
}

static FAULT_OVERCOUNT_DATA_BUFFER: ConstStaticCell<TxPacketBuffer> =
    ConstStaticCell::new(TxPacketBuffer::new());
static FAULT_OVERCOUNT_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(DataPermitHandoff::new());
static FAULT_OVERCOUNT_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(OrdinaryPermitHandoff::new());

#[test]
fn one_frame_tx_fault_with_two_completed_frames_fails_closed() {
    let mut owner = node::<1>(21, "fault-overcount-sender");
    let receiver = node::<0>(22, "fault-overcount-receiver");
    owner
        .register_peer(
            &identity(22),
            "reticulum",
            &["fault-overcount-receiver"],
            MonotonicSeconds::new(0),
        )
        .unwrap();
    let buffer = FAULT_OVERCOUNT_DATA_BUFFER.take();
    owner.register_packet_buffer(buffer).unwrap();
    let job = prepare_data(
        &mut owner,
        buffer,
        receiver.destination_hash(),
        b"one frame",
        100_000,
    );
    assert!(job.packet_len() <= 254);
    let (mut data_node, data_dispatch) = FAULT_OVERCOUNT_DATA_HANDOFF.take().split();
    let (_, ordinary_dispatch) = FAULT_OVERCOUNT_ORDINARY_HANDOFF.take().split();
    let (mut dispatcher, mut router) = test_dispatcher(
        MockRadio::new(
            vec![CadScript::Observation {
                busy: false,
                at_us: 1_000_100,
            }],
            vec![TxScript::Fault(PacketTxProgress::both_completed(
                1_000_500, 1_000_700,
            ))],
        ),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(2),
    );
    route_data(&mut router, job);
    start_and_clear_data(&mut dispatcher, 1_000_000);
    grant_data(
        &mut dispatcher,
        &mut data_node,
        &mut owner,
        &mut ExactPolicy,
        1_001,
        1_000_200,
    );
    let report = match block_on(dispatcher.perform_radio_operation(1_000_300)) {
        RadioOperationStep::Terminal(report) => report,
        other => panic!("fault overcount did not terminate: {other:?}"),
    };
    assert_eq!(report.outcome(), DispatchOutcome::FrameInvariantRecovery);
    assert_eq!(report.frame_count(), 1);
    assert_eq!(report.progress().unwrap().completed_frame_count(), 2);
    assert!(!dispatcher.radio().is_active());
    assert_eq!(
        dispatcher.step(1_000_800),
        RadioTxDispatcherStep::Disabled(DispatcherFault::InternalInvariant)
    );
    let completion = take_data_completion(&mut router);
    let quarantine = match owner
        .complete_tx(completion, MonotonicMillis::new(1_001))
        .unwrap_or_else(|failure| panic!("fault-overcount correlation: {:?}", failure.reason()))
    {
        TxCompletionDisposition::Quarantined(quarantine) => quarantine,
        _ => panic!("fault overcount did not quarantine"),
    };
    assert_eq!(
        quarantine.record().reason(),
        TxRecoveryReason::CompletionFault(TxCompletionCode::new(8))
    );
    assert_eq!(
        quarantine.record().prior_phase(),
        TxRecoveryPriorPhase::Authorized
    );
}

static SPLIT_SUCCESS_DATA_BUFFER: ConstStaticCell<TxPacketBuffer> =
    ConstStaticCell::new(TxPacketBuffer::new());
static SPLIT_SUCCESS_ORDINARY_BUFFER: ConstStaticCell<OrdinaryPacketBuffer> =
    ConstStaticCell::new(OrdinaryPacketBuffer::new());
static SPLIT_SUCCESS_ORDINARY_REFS: StaticCell<[&'static mut OrdinaryPacketBuffer; 1]> =
    StaticCell::new();
static SPLIT_SUCCESS_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(DataPermitHandoff::new());
static SPLIT_SUCCESS_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(OrdinaryPermitHandoff::new());

#[test]
fn split_success_requires_two_completions_and_wrong_ordinary_count_fails_closed() {
    let mut owner = node::<1>(17, "split-success-sender");
    let receiver = node::<0>(18, "split-success-receiver");
    owner
        .register_peer(
            &identity(18),
            "reticulum",
            &["split-success-receiver"],
            MonotonicSeconds::new(0),
        )
        .unwrap();
    let data_buffer = SPLIT_SUCCESS_DATA_BUFFER.take();
    owner.register_packet_buffer(data_buffer).unwrap();
    let plaintext = [0x6b; MAX_DATA_PAYLOAD];
    let data_job = prepare_data(
        &mut owner,
        data_buffer,
        receiver.destination_hash(),
        &plaintext,
        100_000,
    );
    assert!(data_job.packet_len() > 254);
    let mut ordinary_owner = owner.take_ordinary_action_owner::<1>().unwrap();
    let ordinary_buffer = SPLIT_SUCCESS_ORDINARY_BUFFER.take();
    let ordinary_refs = SPLIT_SUCCESS_ORDINARY_REFS.init([ordinary_buffer]);
    let ordinary_job = prepare_ordinary(&mut owner, &mut ordinary_owner, ordinary_refs, 8, 100_000);
    assert!(ordinary_job.packet_len() <= 254);
    let (mut data_node, data_dispatch) = SPLIT_SUCCESS_DATA_HANDOFF.take().split();
    let (mut ordinary_node, ordinary_dispatch) = SPLIT_SUCCESS_ORDINARY_HANDOFF.take().split();
    let (mut dispatcher, mut router) = test_dispatcher(
        MockRadio::new(
            vec![
                CadScript::Observation {
                    busy: false,
                    at_us: 1_000_100,
                },
                CadScript::Observation {
                    busy: false,
                    at_us: 2_000_100,
                },
            ],
            vec![
                TxScript::SuccessTwo(1_000_500, 1_000_700),
                TxScript::SuccessTwo(2_000_500, 2_000_700),
            ],
        ),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(3),
    );
    route_data(&mut router, data_job);

    start_and_clear_data(&mut dispatcher, 1_000_000);
    route_ordinary(&mut router, ordinary_job);
    grant_data(
        &mut dispatcher,
        &mut data_node,
        &mut owner,
        &mut ExactPolicy,
        1_001,
        1_000_200,
    );
    let report = match block_on(dispatcher.perform_radio_operation(1_000_300)) {
        RadioOperationStep::Terminal(report) => report,
        other => panic!("split success did not terminate: {other:?}"),
    };
    assert_eq!(report.outcome(), DispatchOutcome::Transmitted);
    assert_eq!(report.frame_count(), 2);
    assert_eq!(report.progress().unwrap().completed_frame_count(), 2);
    assert_eq!(dispatcher.step(1_000_800), RadioTxDispatcherStep::Advanced);
    let data_completion = take_data_completion(&mut router);
    assert!(matches!(
        owner.complete_tx(data_completion, MonotonicMillis::new(1_002)),
        Ok(TxCompletionDisposition::Available(_))
    ));

    start_and_clear_ordinary(&mut dispatcher, 2_000_000);
    grant_ordinary(
        &mut dispatcher,
        &mut ordinary_node,
        &mut ordinary_owner,
        &mut ExactPolicy,
        2_000_200,
    );
    let report = match block_on(dispatcher.perform_radio_operation(2_000_300)) {
        RadioOperationStep::Terminal(report) => report,
        other => panic!("ordinary mismatch did not terminate: {other:?}"),
    };
    assert_eq!(report.family(), DispatchFamily::Ordinary);
    assert_eq!(report.outcome(), DispatchOutcome::FrameInvariantRecovery);
    assert_eq!(report.frame_count(), 1);
    assert_eq!(report.progress().unwrap().completed_frame_count(), 2);
    assert!(!dispatcher.radio().is_active());
    assert_eq!(
        dispatcher.step(2_000_800),
        RadioTxDispatcherStep::Disabled(DispatcherFault::InternalInvariant)
    );
    let completion = take_ordinary_completion(&mut router);
    assert_eq!(
        completion.transmission_outcome(),
        reticulum_node_core::OrdinaryTransmissionOutcome::NotConfirmed
    );
    let quarantine = match ordinary_owner
        .complete_tx(completion, MonotonicMillis::new(2_001))
        .expect("ordinary frame-count correlation")
    {
        OrdinaryCompletionDisposition::Quarantined(quarantine) => quarantine,
        _ => panic!("ordinary frame-count mismatch did not quarantine"),
    };
    assert_eq!(
        quarantine.reason(),
        OrdinaryQuarantineReason::RecoveryFault(TxCompletionCode::new(8))
    );
}

static CANCEL_DATA_BUFFER: ConstStaticCell<TxPacketBuffer> =
    ConstStaticCell::new(TxPacketBuffer::new());
static CANCEL_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(DataPermitHandoff::new());
static CANCEL_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(OrdinaryPermitHandoff::new());

#[test]
fn dropped_tx_future_retains_authorized_owner_until_explicit_recovery() {
    let mut owner = node::<1>(11, "cancel-sender");
    let receiver = node::<0>(12, "cancel-receiver");
    owner
        .register_peer(
            &identity(12),
            "reticulum",
            &["cancel-receiver"],
            MonotonicSeconds::new(0),
        )
        .unwrap();
    let buffer = CANCEL_DATA_BUFFER.take();
    owner.register_packet_buffer(buffer).unwrap();
    let job = prepare_data(
        &mut owner,
        buffer,
        receiver.destination_hash(),
        b"cancel",
        100_000,
    );
    let expected_data = job.prepared();
    let expected_data_interface = job.interface();
    let (mut data_node, data_dispatch) = CANCEL_DATA_HANDOFF.take().split();
    let (_, ordinary_dispatch) = CANCEL_ORDINARY_HANDOFF.take().split();
    let (mut dispatcher, mut router) = test_dispatcher(
        MockRadio::new(
            vec![CadScript::Observation {
                busy: false,
                at_us: 1_000_100,
            }],
            vec![TxScript::Pending],
        ),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(2),
    );
    route_data(&mut router, job);
    start_and_clear_data(&mut dispatcher, 1_000_000);
    grant_data(
        &mut dispatcher,
        &mut data_node,
        &mut owner,
        &mut ExactPolicy,
        1_001,
        1_000_200,
    );
    {
        let mut operation = pin!(dispatcher.perform_radio_operation(1_000_300));
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(
            operation.as_mut().poll(&mut context),
            Poll::Pending
        ));
    }
    assert_eq!(
        dispatcher.phase(),
        RadioTxDispatcherPhase::TransmitInFlight(DispatchFamily::Data)
    );
    assert!(!dispatcher.radio().is_active());
    assert_eq!(
        dispatcher.recover_cancelled_radio_operation(),
        RadioTxDispatcherStep::Advanced
    );
    let report = dispatcher.last_report().unwrap();
    assert_eq!(report.outcome(), DispatchOutcome::CancelledRadioOperation);
    assert_eq!(report.progress(), None);
    let authorized_frame = report
        .authorized_frame()
        .expect("cancelled TX must retain prior authorized byte exposure");
    assert_eq!(authorized_frame.attempt_handle(), expected_data.handle());
    assert_eq!(authorized_frame.attempt(), expected_data.attempt());
    assert_eq!(authorized_frame.interface(), expected_data_interface);
    assert_eq!(
        authorized_frame.packet_len(),
        usize::from(expected_data.packet_len())
    );
    assert_eq!(
        authorized_frame.encoded_packet_sha256(),
        expected_data.encoded_packet_sha256()
    );
    assert_eq!(
        dispatcher.inner.phase(),
        RadioTxDispatcherPhase::AuthorizedFrameRequest
    );
    assert_no_completion(&mut router);
    assert_eq!(
        dispatcher.inner.step(1_000_400),
        RadioTxDispatcherStep::Advanced
    );
    assert_eq!(
        dispatcher.authorized_frames.requests().try_receive(),
        Some(authorized_frame)
    );
    assert_no_completion(&mut router);
    dispatcher
        .authorized_frames
        .acknowledgements()
        .try_send(authorized_frame)
        .unwrap_or_else(|_| panic!("cancelled frame acknowledgement channel full"));
    assert_eq!(
        dispatcher.inner.step(1_000_401),
        RadioTxDispatcherStep::Advanced
    );
    assert_no_completion(&mut router);
    assert_eq!(
        dispatcher.inner.step(1_000_402),
        RadioTxDispatcherStep::Disabled(DispatcherFault::RadioUnavailable)
    );
    let _ = take_data_completion(&mut router);
}

static STALE_SOURCE_BUFFER: ConstStaticCell<TxPacketBuffer> =
    ConstStaticCell::new(TxPacketBuffer::new());
static STALE_ACTIVE_BUFFER: ConstStaticCell<TxPacketBuffer> =
    ConstStaticCell::new(TxPacketBuffer::new());
static STALE_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(DataPermitHandoff::new());
static STALE_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
    ConstStaticCell::new(OrdinaryPermitHandoff::new());

#[test]
fn stale_reply_is_retained_and_matching_pending_owner_returns_recovery() {
    let mut stale_owner = node::<1>(13, "stale-source");
    let stale_receiver = node::<0>(14, "stale-source-receiver");
    stale_owner
        .register_peer(
            &identity(14),
            "reticulum",
            &["stale-source-receiver"],
            MonotonicSeconds::new(0),
        )
        .unwrap();
    let stale_buffer = STALE_SOURCE_BUFFER.take();
    stale_owner.register_packet_buffer(stale_buffer).unwrap();
    let stale_job = prepare_data(
        &mut stale_owner,
        stale_buffer,
        stale_receiver.destination_hash(),
        b"stale",
        100_000,
    );
    let stale_airtime = profile()
        .rnode_packet_airtime(stale_job.packet_len().into())
        .unwrap();
    let stale_requirements = TxPermitRequirements::try_new(
        test_permit_resource(),
        stale_airtime.aggregate_time_on_air_us(),
    )
    .unwrap();
    let (stale_pending, stale_request) = stale_job.begin_permit(stale_requirements);
    let stale_reply = stale_owner
        .authorize_tx(stale_request, MonotonicMillis::new(1_001), &mut ExactPolicy)
        .unwrap_or_else(|failure| panic!("stale source authorize: {:?}", failure.reason()));
    let stale_completion = stale_pending.recovery_fault(TxCompletionCode::new(0x77));
    assert!(
        stale_owner
            .complete_tx(stale_completion, MonotonicMillis::new(1_002))
            .is_ok()
    );

    let mut active_owner = node::<1>(15, "stale-active");
    let active_receiver = node::<0>(16, "stale-active-receiver");
    active_owner
        .register_peer(
            &identity(16),
            "reticulum",
            &["stale-active-receiver"],
            MonotonicSeconds::new(0),
        )
        .unwrap();
    let active_buffer = STALE_ACTIVE_BUFFER.take();
    active_owner.register_packet_buffer(active_buffer).unwrap();
    let active_job = prepare_data(
        &mut active_owner,
        active_buffer,
        active_receiver.destination_hash(),
        b"active",
        100_000,
    );
    let (mut data_node, data_dispatch) = STALE_DATA_HANDOFF.take().split();
    let (_, ordinary_dispatch) = STALE_ORDINARY_HANDOFF.take().split();
    let (mut dispatcher, mut router) = test_dispatcher(
        MockRadio::new(
            vec![CadScript::Observation {
                busy: false,
                at_us: 1_000_100,
            }],
            vec![],
        ),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(2),
    );
    route_data(&mut router, active_job);
    start_and_clear_data(&mut dispatcher, 1_000_000);
    assert_eq!(dispatcher.step(1_000_200), RadioTxDispatcherStep::Advanced);
    assert!(!data_node.requests().is_empty());
    data_node
        .replies()
        .try_send(stale_reply)
        .unwrap_or_else(|_| panic!("stale reply full"));
    assert_eq!(dispatcher.step(1_000_201), RadioTxDispatcherStep::Advanced);
    assert_eq!(dispatcher.step(1_000_202), RadioTxDispatcherStep::Advanced);
    assert_eq!(
        dispatcher.fault(),
        Some(DispatcherFault::PermitReplyMismatch(DispatchFamily::Data))
    );
    assert_eq!(
        dispatcher.fault_residue_kind(),
        Some(DispatcherFaultResidueKind::DataPermitReply)
    );
    assert_eq!(
        dispatcher.step(1_000_203),
        RadioTxDispatcherStep::Disabled(DispatcherFault::PermitReplyMismatch(DispatchFamily::Data))
    );
    let _ = take_data_completion(&mut router);
    assert!(dispatcher.radio().captured.is_empty());
}

#[test]
fn bounded_receive_timeout_frame_and_zero_length_are_reusable() {
    let signal = FrameSignal::new(-87, 9);
    let mut dispatcher = dispatcher_with_rx(vec![
        RxScript::NoPreambleTimeout,
        RxScript::Frame {
            len: 3,
            signal,
            at_us: 1_234_567,
        },
        RxScript::Frame {
            len: SX1262_FRAME_MTU,
            signal: FrameSignal::new(-95, 3),
            at_us: 1_234_888,
        },
        RxScript::Frame {
            len: 0,
            signal: FrameSignal::new(-101, -2),
            at_us: 1_234_999,
        },
    ]);
    let mut buffer = [0xa5; SX1262_FRAME_MTU];
    assert_eq!(dispatcher.maximum_receive_operation_us().get(), 1_500_000);

    let timeout = dispatcher
        .start_continuous_receive_until(
            &mut buffer,
            core::future::pending(),
            core::future::pending(),
        )
        .unwrap_or_else(|step| panic!("RX did not start: {step:?}"));
    assert_eq!(block_on(timeout), RadioReceiveStep::SchedulerYield);
    assert_eq!(buffer, [0xa5; SX1262_FRAME_MTU]);
    assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::Idle);
    assert!(dispatcher.radio().is_active());

    let receive = dispatcher
        .start_continuous_receive_until(
            &mut buffer,
            core::future::pending(),
            core::future::pending(),
        )
        .unwrap_or_else(|step| panic!("second RX did not start: {step:?}"));
    let observation = match block_on(receive) {
        RadioReceiveStep::Frame(observation) => observation,
        step => panic!("unexpected frame result: {step:?}"),
    };
    assert_eq!(observation.len(), 3);
    assert_eq!(observation.signal(), signal);
    assert_eq!(observation.received_at_us(), 1_234_567);
    assert_eq!(observation.payload(&buffer), Some(&[0, 1, 2][..]));
    assert_eq!(&buffer[3..], &[0xa5; SX1262_FRAME_MTU - 3]);
    assert_eq!(
        dispatcher.last_receive_step(),
        Some(RadioReceiveStep::Frame(observation))
    );

    let maximum = dispatcher
        .start_continuous_receive_until(
            &mut buffer,
            core::future::pending(),
            core::future::pending(),
        )
        .unwrap_or_else(|step| panic!("maximum-length RX did not start: {step:?}"));
    let maximum = match block_on(maximum) {
        RadioReceiveStep::Frame(observation) => observation,
        step => panic!("unexpected maximum-length result: {step:?}"),
    };
    assert_eq!(maximum.len(), SX1262_FRAME_MTU);
    assert_eq!(maximum.payload(&buffer).map(<[u8]>::len), Some(255));
    assert_eq!(buffer[254], 254);

    let before_zero = buffer;
    let zero = dispatcher
        .start_continuous_receive_until(
            &mut buffer,
            core::future::pending(),
            core::future::pending(),
        )
        .unwrap_or_else(|step| panic!("zero-length RX did not start: {step:?}"));
    let zero = match block_on(zero) {
        RadioReceiveStep::Frame(observation) => observation,
        step => panic!("unexpected zero-length result: {step:?}"),
    };
    assert!(zero.is_empty());
    assert_eq!(zero.payload(&buffer), Some(&[][..]));
    assert_eq!(buffer, before_zero);
    assert!(dispatcher.radio().is_active());
    assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::Idle);
}

#[test]
fn scheduler_can_service_receive_before_queued_data_then_select_tx() {
    let mut sender = node::<1>(41, "rx-priority-data");
    let receiver = node::<0>(42, "rx-priority-data-receiver");
    sender
        .register_peer(
            &identity(42),
            "reticulum",
            &["rx-priority-data-receiver"],
            MonotonicSeconds::new(0),
        )
        .unwrap();
    let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
    sender.register_packet_buffer(buffer).unwrap();
    let job = prepare_data(
        &mut sender,
        buffer,
        receiver.destination_hash(),
        b"priority",
        100_000,
    );
    let data_handoff = Box::leak(Box::new(DataPermitHandoff::<NoopRawMutex>::new()));
    let ordinary_handoff = Box::leak(Box::new(OrdinaryPermitHandoff::<NoopRawMutex>::new()));
    let (_data_node, data_dispatch) = data_handoff.split();
    let (_, ordinary_dispatch) = ordinary_handoff.split();
    let (mut dispatcher, mut router) = test_dispatcher(
        MockRadio::new(vec![], vec![]).with_rx(vec![RxScript::NoPreambleTimeout]),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(2),
    );
    route_data(&mut router, job);
    let mut rx_buffer = [0; SX1262_FRAME_MTU];

    let receive = dispatcher
        .start_continuous_receive_until(
            &mut rx_buffer,
            core::future::pending(),
            core::future::pending(),
        )
        .unwrap_or_else(|step| panic!("scheduler-selected RX did not start: {step:?}"));
    assert_eq!(block_on(receive), RadioReceiveStep::SchedulerYield);
    assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::Idle);
    assert_eq!(dispatcher.step(1_000_000), RadioTxDispatcherStep::Advanced);
    assert_eq!(
        dispatcher.phase(),
        RadioTxDispatcherPhase::BackingOff(DispatchFamily::Data)
    );
    assert!(dispatcher.radio().is_active());
    assert!(matches!(
        dispatcher.start_continuous_receive_until(
            &mut rx_buffer,
            core::future::pending(),
            core::future::pending(),
        ),
        Err(RadioReceiveStep::TxPriority(
            RadioTxDispatcherPhase::BackingOff(DispatchFamily::Data)
        ))
    ));
}

#[test]
fn scheduler_can_service_receive_before_queued_ordinary_then_select_tx() {
    let mut node = node::<1>(43, "rx-priority-ordinary");
    let mut owner = node.take_ordinary_action_owner::<1>().unwrap();
    let ordinary_buffer = Box::leak(Box::new(OrdinaryPacketBuffer::new()));
    let refs = Box::leak(Box::new([ordinary_buffer]));
    let job = prepare_ordinary(&mut node, &mut owner, refs, 4, 100_000);
    let data_handoff = Box::leak(Box::new(DataPermitHandoff::<NoopRawMutex>::new()));
    let ordinary_handoff = Box::leak(Box::new(OrdinaryPermitHandoff::<NoopRawMutex>::new()));
    let (_, data_dispatch) = data_handoff.split();
    let (_ordinary_node, ordinary_dispatch) = ordinary_handoff.split();
    let (mut dispatcher, mut router) = test_dispatcher(
        MockRadio::new(vec![], vec![]).with_rx(vec![RxScript::NoPreambleTimeout]),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(2),
    );
    route_ordinary(&mut router, job);
    let mut rx_buffer = [0; SX1262_FRAME_MTU];

    let receive = dispatcher
        .start_continuous_receive_until(
            &mut rx_buffer,
            core::future::pending(),
            core::future::pending(),
        )
        .unwrap_or_else(|step| panic!("scheduler-selected RX did not start: {step:?}"));
    assert_eq!(block_on(receive), RadioReceiveStep::SchedulerYield);
    assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::Idle);
    assert_eq!(dispatcher.step(1_000_000), RadioTxDispatcherStep::Advanced);
    assert_eq!(
        dispatcher.phase(),
        RadioTxDispatcherPhase::BackingOff(DispatchFamily::Ordinary)
    );
    assert!(dispatcher.radio().is_active());
}

#[test]
fn dropping_unpolled_receive_shuts_down_and_requires_explicit_recovery() {
    let mut dispatcher = dispatcher_with_rx(vec![RxScript::Pending]);
    let mut buffer = [0; SX1262_FRAME_MTU];
    let receive = dispatcher
        .start_continuous_receive_until(
            &mut buffer,
            core::future::pending(),
            core::future::pending(),
        )
        .unwrap_or_else(|step| panic!("RX did not start: {step:?}"));
    drop(receive);

    assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::ReceiveInFlight);
    assert!(!dispatcher.radio().is_active());
    assert_eq!(dispatcher.last_receive_step(), None);
    assert_eq!(
        block_on(dispatcher.perform_radio_operation(1_000_001)),
        RadioOperationStep::ReceiveFutureNeedsRecovery
    );
    assert_eq!(
        dispatcher.recover_cancelled_radio_operation(),
        RadioTxDispatcherStep::Disabled(DispatcherFault::ReceiveCancelled)
    );
    assert_eq!(
        dispatcher.last_receive_step(),
        Some(RadioReceiveStep::Disabled(
            DispatcherFault::ReceiveCancelled
        ))
    );
    assert_eq!(
        dispatcher.step(1_000_002),
        RadioTxDispatcherStep::Disabled(DispatcherFault::ReceiveCancelled)
    );
}

#[test]
fn pending_receive_gates_the_shared_queued_job_wait_until_recovery() {
    let mut sender = node::<1>(44, "rx-cancel-data");
    let receiver = node::<0>(45, "rx-cancel-data-receiver");
    sender
        .register_peer(
            &identity(45),
            "reticulum",
            &["rx-cancel-data-receiver"],
            MonotonicSeconds::new(0),
        )
        .unwrap();
    let data_buffer = Box::leak(Box::new(TxPacketBuffer::new()));
    sender.register_packet_buffer(data_buffer).unwrap();
    let data_job = prepare_data(
        &mut sender,
        data_buffer,
        receiver.destination_hash(),
        b"queued while rx cancelled",
        100_000,
    );
    let mut ordinary_node_core = node::<1>(46, "rx-cancel-ordinary");
    let mut ordinary_owner = ordinary_node_core
        .take_ordinary_action_owner::<1>()
        .unwrap();
    let ordinary_buffer = Box::leak(Box::new(OrdinaryPacketBuffer::new()));
    let ordinary_refs = Box::leak(Box::new([ordinary_buffer]));
    let ordinary_job = prepare_ordinary(
        &mut ordinary_node_core,
        &mut ordinary_owner,
        ordinary_refs,
        4,
        100_000,
    );
    let data_handoff = Box::leak(Box::new(DataPermitHandoff::<NoopRawMutex>::new()));
    let ordinary_handoff = Box::leak(Box::new(OrdinaryPermitHandoff::<NoopRawMutex>::new()));
    let (_data_node, data_dispatch) = data_handoff.split();
    let (_ordinary_node, ordinary_dispatch) = ordinary_handoff.split();
    let (mut dispatcher, mut router) = test_dispatcher(
        MockRadio::new(vec![], vec![]).with_rx(vec![RxScript::Pending]),
        CounterRng::default(),
        data_dispatch,
        ordinary_dispatch,
        config(2),
    );
    let mut rx_buffer = [0; SX1262_FRAME_MTU];
    {
        let mut receive = pin!(
            dispatcher
                .start_continuous_receive_until(
                    &mut rx_buffer,
                    core::future::pending(),
                    core::future::pending(),
                )
                .unwrap_or_else(|step| panic!("RX did not start: {step:?}"))
        );
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(receive.as_mut().poll(&mut context), Poll::Pending));
    }
    route_data(&mut router, data_job);
    let ordinary_job = match router.try_route_ordinary(ordinary_job) {
        Err(failure) => failure.into_job(),
        Ok(_) => panic!("shared interface queue accepted an over-capacity ordinary job"),
    };

    assert_eq!(
        block_on(dispatcher.wait_for_job()),
        RadioTxDispatcherStep::NeedReceiveRecovery
    );
    let ordinary_job = match router.try_route_ordinary(ordinary_job) {
        Err(failure) => failure.into_job(),
        Ok(_) => panic!("RX-gated DATA owner left the interface queue"),
    };
    assert_eq!(
        block_on(dispatcher.wait_for_permit_reply()),
        RadioTxDispatcherStep::NeedReceiveRecovery
    );
    assert_eq!(
        dispatcher.step(1_000_001),
        RadioTxDispatcherStep::NeedReceiveRecovery
    );
    assert!(!dispatcher.radio().is_active());
    assert_eq!(
        dispatcher.recover_cancelled_radio_operation(),
        RadioTxDispatcherStep::Disabled(DispatcherFault::ReceiveCancelled)
    );
    assert!(router.try_route_ordinary(ordinary_job).is_err());
}

#[test]
fn receive_fault_disables_radio_and_retains_phase_and_class() {
    let mut dispatcher = dispatcher_with_rx(vec![RxScript::Fault]);
    let mut buffer = [0; SX1262_FRAME_MTU];
    let receive = dispatcher
        .start_continuous_receive_until(
            &mut buffer,
            core::future::pending(),
            core::future::pending(),
        )
        .unwrap_or_else(|step| panic!("RX did not start: {step:?}"));
    let expected = RadioReceiveStep::Fault {
        phase: SoleRadioFaultPhase::Receive,
        class: SoleRadioFaultClass::Operation,
    };
    assert_eq!(block_on(receive), expected);
    assert!(!dispatcher.radio().is_active());
    assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::Disabled);
    assert_eq!(dispatcher.last_receive_step(), Some(expected));
    assert_eq!(
        dispatcher.step(1_000_001),
        RadioTxDispatcherStep::Disabled(DispatcherFault::RadioUnavailable)
    );
    assert_eq!(dispatcher.take_last_receive_step(), Some(expected));
}

#[test]
fn invalid_receive_length_fails_closed_and_remains_diagnosable() {
    let invalid_len = SX1262_FRAME_MTU + 1;
    let mut dispatcher = dispatcher_with_rx(vec![RxScript::Frame {
        len: invalid_len,
        signal: FrameSignal::new(-80, 4),
        at_us: 7,
    }]);
    let mut buffer = [0; SX1262_FRAME_MTU];
    let receive = dispatcher
        .start_continuous_receive_until(
            &mut buffer,
            core::future::pending(),
            core::future::pending(),
        )
        .unwrap_or_else(|step| panic!("RX did not start: {step:?}"));
    let expected = RadioReceiveStep::InvalidObservation { len: invalid_len };
    assert_eq!(block_on(receive), expected);
    assert!(!dispatcher.radio().is_active());
    assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::Disabled);
    assert_eq!(dispatcher.last_receive_step(), Some(expected));
    assert_eq!(
        dispatcher.step(1_000_001),
        RadioTxDispatcherStep::Disabled(DispatcherFault::ReceiveObservationInvalid)
    );
}

#[test]
fn embassy_helpers_have_the_expected_absolute_microsecond_shape() {
    let _now: fn() -> u64 = embassy_now_us;
    let wait = embassy_wait_until_us(0);
    drop(wait);
    let _unused_fault_script = CadScript::Fault;
    let _unused_two_frame_success = TxScript::SuccessTwo(1, 2);
    assert_eq!(monotonic_millis_ceiling(0).get(), 0);
    assert_eq!(monotonic_millis_ceiling(1).get(), 1);
    assert_eq!(monotonic_millis_ceiling(1_000).get(), 1);
    assert_eq!(
        monotonic_millis_ceiling(u64::MAX).get(),
        18_446_744_073_709_552
    );
}
