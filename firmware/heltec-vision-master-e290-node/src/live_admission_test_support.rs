use core::future::Future;
use core::num::NonZeroU64;

use crate::{
    config,
    durability_policy::{AuthorizedFrameDurability, DurabilityServiceState},
    node_journal_binding, storage_device_id_from_eui48,
};
use embassy_futures::block_on;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
    check_erase, check_read, check_write,
};
use rand_core::{CryptoRng, RngCore};
use reticulum_board_heltec_vision_master_e290_radio::{
    E290_NA915_DEV_CONFIGURATION_FINGERPRINT, E290_NA915_DEV_PROFILE,
};
use reticulum_device_api::CapabilityAvailability;
use reticulum_device_api_adapter::{SubmissionAcceptance, SubmissionPort, SubmissionPortError};
use reticulum_interface_router::{
    InterfaceDescriptor, InterfaceFabric, InterfaceLifecycleActorHandoff, InterfaceLifecycleState,
};
use reticulum_node_core::{
    AnnounceEmissionTime, ApplicationEventDiscardReason, ApplicationEventOwner,
    ApplicationEventSlot, AuthorizedFrameObservation, DestinationHash, MonotonicMillis,
    MonotonicSeconds, NodeConfig, NodeCore, NodeIdentity, NodeInstanceId, OrdinaryBufferPool,
    OrdinaryPacketBuffer, PacketInterfaceId, TxLeaseDeadline, TxPacketBuffer,
};
use reticulum_radio_interface::{
    BoundedRxOutcome, CadObservation, LabRxProfile, PacketTxFault, PacketTxObservation,
    RadioConfigurationFingerprint, RnodeTxFrames, SX1262_FRAME_MTU, SoleRadioFaultSummary,
    SoleRnodeRadio,
};
use reticulum_radio_tx_dispatch::{
    DispatchFamily, DispatchOutcome, DispatchReport, ExactLoRaAirtimePolicy, RadioOperationStep,
    RadioTxDispatcherPhase, RadioTxDispatcherStep, SoleRadioTxDispatcher,
};
use reticulum_storage_actor::{
    AcceptanceProgress, BoundJournal, DriveError, MountError, ProjectorOperationError,
};
use reticulum_storage_journal::{ERASE_SIZE, PARTITION_SIZE, format_erased};
use reticulum_storage_model::{
    AcceptOutcome, AcceptanceCandidate, LifecycleState, PrincipalId, SubmissionId,
};
use reticulum_submission_runtime::{
    FrameOfferProgress, RecoveryStep, RuntimeControlError, RuntimeError, RuntimeStep,
    SubmissionRuntime,
};
use reticulum_tx_handoff::{
    AuthorizedFrameHandoff, AuthorizedFrameNodeHandoff, DataPermitHandoff, OrdinaryPermitHandoff,
};
use reticulum_tx_supervisor::{
    DataRouterCoordinator, NodeInterfaceAnnounceFlushResult, NodeInterfaceApplicationEventDrain,
    NodeInterfaceSupervisor, NodeInterfaceSupervisorTransition, OrdinaryRouterCoordinator,
    OrdinaryRouterStep,
};

use std::{boxed::Box, vec, vec::Vec};

pub const LORA_INTERFACE: PacketInterfaceId = PacketInterfaceId::new(1);
const FIRST_SUBMISSION: SubmissionId = SubmissionId::new(1);

pub type ProductSupervisor = NodeInterfaceSupervisor<
    NoopRawMutex,
    ExactLoRaAirtimePolicy,
    { config::PATHS },
    { config::ANNOUNCES },
    { config::DEDUPLICATION },
    { config::LINKS },
    { config::DATA_BUFFERS },
    { config::ORDINARY_BUFFERS },
    { config::INTERFACE_SLOTS },
    { config::INTERFACE_QUEUE_DEPTH },
>;

pub type ProductDispatcher = SoleRadioTxDispatcher<
    ScriptedRadio,
    CounterRng,
    NoopRawMutex,
    { config::INTERFACE_QUEUE_DEPTH },
>;

#[derive(Clone, Debug, Default)]
pub struct CounterRng(u8);

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FakeNorError {
    Bounds,
    Alignment,
}

impl NorFlashError for FakeNorError {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::Bounds => NorFlashErrorKind::OutOfBounds,
            Self::Alignment => NorFlashErrorKind::NotAligned,
        }
    }
}

pub struct ScriptedNor {
    bytes: Vec<u8>,
    write_attempts: usize,
}

impl ScriptedNor {
    fn formatted() -> Self {
        let mut flash = Self {
            bytes: vec![0xff; PARTITION_SIZE],
            write_attempts: 0,
        };
        format_erased(&mut flash).expect("erased scripted NOR formats");
        flash
    }

    pub fn write_attempts(&self) -> usize {
        self.write_attempts
    }
}

impl ErrorType for ScriptedNor {
    type Error = FakeNorError;
}

impl ReadNorFlash for ScriptedNor {
    const READ_SIZE: usize = 4;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        check_read(self, offset, bytes.len()).map_err(map_check_error)?;
        let offset = offset as usize;
        bytes.copy_from_slice(&self.bytes[offset..offset + bytes.len()]);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.bytes.len()
    }
}

impl NorFlash for ScriptedNor {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = ERASE_SIZE;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        check_erase(self, from, to).map_err(map_check_error)?;
        self.bytes[from as usize..to as usize].fill(0xff);
        Ok(())
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        check_write(self, offset, bytes.len()).map_err(map_check_error)?;
        self.write_attempts += 1;
        let offset = offset as usize;
        for (stored, supplied) in self.bytes[offset..offset + bytes.len()]
            .iter_mut()
            .zip(bytes)
        {
            *stored &= *supplied;
        }
        Ok(())
    }
}

impl MultiwriteNorFlash for ScriptedNor {}

fn map_check_error(error: NorFlashErrorKind) -> FakeNorError {
    match error {
        NorFlashErrorKind::OutOfBounds => FakeNorError::Bounds,
        _ => FakeNorError::Alignment,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedPacket {
    pub first: Vec<u8>,
    pub second: Option<Vec<u8>>,
}

pub struct ScriptedRadio {
    active: bool,
    cad_calls: usize,
    tx_calls: usize,
    rx_calls: usize,
    captured: Vec<CapturedPacket>,
}

impl ScriptedRadio {
    fn new() -> Self {
        Self {
            active: true,
            cad_calls: 0,
            tx_calls: 0,
            rx_calls: 0,
            captured: Vec::new(),
        }
    }

    pub fn tx_calls(&self) -> usize {
        self.tx_calls
    }

    pub fn rx_calls(&self) -> usize {
        self.rx_calls
    }

    pub fn captured(&self) -> &[CapturedPacket] {
        &self.captured
    }
}

impl SoleRnodeRadio for ScriptedRadio {
    type Fault = SoleRadioFaultSummary;

    fn configuration_fingerprint(&self) -> RadioConfigurationFingerprint {
        E290_NA915_DEV_CONFIGURATION_FINGERPRINT
    }

    fn airtime_profile(&self) -> LabRxProfile {
        E290_NA915_DEV_PROFILE
    }

    fn maximum_receive_operation_us(&self) -> NonZeroU64 {
        NonZeroU64::new(1_500_000).expect("scripted receive watchdog is nonzero")
    }

    fn receive_bounded<'a>(
        &'a mut self,
        _buffer: &'a mut [u8; SX1262_FRAME_MTU],
    ) -> impl Future<Output = Result<BoundedRxOutcome, Self::Fault>> + 'a {
        self.rx_calls += 1;
        async { Ok(BoundedRxOutcome::NoPreambleTimeout) }
    }

    fn receive_continuous_until<'a, SchedulerYield, ProgressDeadline>(
        &'a mut self,
        _buffer: &'a mut [u8; SX1262_FRAME_MTU],
        scheduler_yield: SchedulerYield,
        _progress_deadline: ProgressDeadline,
    ) -> impl Future<Output = Result<BoundedRxOutcome, Self::Fault>> + 'a
    where
        SchedulerYield: Future<Output = ()> + 'a,
        ProgressDeadline: Future<Output = ()> + 'a,
    {
        self.rx_calls += 1;
        async move {
            scheduler_yield.await;
            Ok(BoundedRxOutcome::SchedulerYield)
        }
    }

    fn invalidate_receive_session(&mut self) {}

    fn cad(&mut self) -> impl Future<Output = Result<CadObservation, Self::Fault>> + '_ {
        self.cad_calls += 1;
        let observed_at_us = 2_000_000 + (self.cad_calls as u64 - 1) * 500_000;
        async move { Ok(CadObservation::new(false, observed_at_us)) }
    }

    fn transmit<'a>(
        &'a mut self,
        frames: RnodeTxFrames<'a>,
    ) -> impl Future<Output = Result<PacketTxObservation, PacketTxFault<Self::Fault>>> + 'a {
        let second = frames.second().map(<[u8]>::to_vec);
        let two_frames = second.is_some();
        self.captured.push(CapturedPacket {
            first: frames.first().to_vec(),
            second,
        });
        self.tx_calls += 1;
        let completed_at_us = 2_100_000 + self.tx_calls as u64 * 500_000;
        async move {
            Ok(if two_frames {
                PacketTxObservation::two_frames(completed_at_us, completed_at_us + 100_000)
            } else {
                PacketTxObservation::one_frame(completed_at_us)
            })
        }
    }

    fn shutdown(&mut self) {
        self.active = false;
    }

    fn is_active(&self) -> bool {
        self.active
    }
}

pub struct LiveSubmissionService {
    runtime: SubmissionRuntime<
        { config::DURABLE_SUBMISSIONS },
        { config::DURABLE_PROJECTED_SUBMISSIONS },
    >,
    journal: BoundJournal<ScriptedNor>,
    state: DurabilityServiceState,
}

impl LiveSubmissionService {
    pub fn formatted(boot_sequence: u64) -> (Self, Vec<RecoveryStep>) {
        Self::mount(ScriptedNor::formatted(), boot_sequence)
            .expect("formatted product journal mounts")
    }

    pub fn mount(
        flash: ScriptedNor,
        boot_sequence: u64,
    ) -> Result<(Self, Vec<RecoveryStep>), MountError<FakeNorError>> {
        let binding = node_journal_binding(storage_device_id_from_eui48([
            0x02, 0x00, 0x00, 0x00, 0xe2, 0x90,
        ]));
        let mut journal = BoundJournal::new(flash, binding);
        let mut runtime = SubmissionRuntime::mount(&mut journal, FIRST_SUBMISSION, boot_sequence)?;
        let mut recovery = Vec::new();
        loop {
            match runtime.recover_boot_step(&mut journal) {
                Ok(step @ RecoveryStep::Submission { .. }) => recovery.push(step),
                Ok(step @ RecoveryStep::Complete) => {
                    recovery.push(step);
                    break;
                }
                Ok(RecoveryStep::AlreadyComplete) => break,
                Err(RuntimeError::Storage(DriveError::Backend(error))) => {
                    return Err(MountError::Backend(error));
                }
                Err(RuntimeError::Storage(DriveError::Binding(error))) => {
                    return Err(MountError::Binding(error));
                }
                Err(RuntimeError::Storage(DriveError::Faulted(error)))
                | Err(RuntimeError::Projection(ProjectorOperationError::Faulted(error))) => {
                    return Err(MountError::Fault(error));
                }
                Err(error) => panic!("unexpected recovery error: {error:?}"),
            }
        }
        Ok((
            Self {
                runtime,
                journal,
                state: DurabilityServiceState::Ready,
            },
            recovery,
        ))
    }

    pub fn service_state(&self) -> DurabilityServiceState {
        self.state
    }

    pub fn write_attempts(&self) -> usize {
        self.journal.backend().write_attempts()
    }

    pub fn state_for(&self, principal: PrincipalId, id: SubmissionId) -> Option<LifecycleState> {
        self.runtime.index().get_owned_state(principal, id)
    }

    pub fn pending_acknowledgements(&self) -> usize {
        self.runtime.storage().pending_acknowledgements().count()
    }

    pub fn offer_authorized_frame(
        &mut self,
        observation: AuthorizedFrameObservation,
    ) -> Result<FrameOfferProgress, RuntimeControlError> {
        self.state = self.state.observe_authorized_frame_request();
        if !self.state.can_offer_authorized_frame() {
            return Err(RuntimeControlError::Recovering);
        }
        self.runtime.offer_authorized_frame(observation)
    }

    pub fn drive_step(
        &mut self,
        node: &mut ProductSupervisor,
        rns_now: u64,
        owner_now_ms: u64,
        rng: &mut CounterRng,
    ) -> Result<RuntimeStep, RuntimeError<FakeNorError>> {
        let result = self.runtime.drive_step(
            &mut self.journal,
            node,
            MonotonicSeconds::new(rns_now),
            MonotonicMillis::new(owner_now_ms),
            TxLeaseDeadline::new(MonotonicMillis::new(
                owner_now_ms.saturating_add(config::SUBMISSION_OWNER_LEASE_MS),
            )),
            rng,
        );
        if result.is_ok() {
            self.state = self.state.runtime_progress();
        }
        result
    }

    pub fn drive_binding_failure(
        &mut self,
        node: &mut ProductSupervisor,
        rng: &mut CounterRng,
    ) -> Result<RuntimeStep, RuntimeError<FakeNorError>> {
        let wrong_binding = node_journal_binding(storage_device_id_from_eui48([
            0x02, 0x00, 0x00, 0x00, 0xba, 0xd0,
        ]));
        let mut wrong_journal = BoundJournal::new(ScriptedNor::formatted(), wrong_binding);
        let result = self.runtime.drive_step(
            &mut wrong_journal,
            node,
            MonotonicSeconds::new(2_000),
            MonotonicMillis::new(2_000),
            TxLeaseDeadline::new(MonotonicMillis::new(
                2_000 + config::SUBMISSION_OWNER_LEASE_MS,
            )),
            rng,
        );
        if result.is_err() {
            self.state = self
                .state
                .permanent_failure(AuthorizedFrameDurability::Unresolved);
        }
        result
    }

    pub fn into_flash(self) -> ScriptedNor {
        let Self {
            runtime,
            journal,
            state: _,
        } = self;
        let _storage = runtime.into_storage();
        journal.into_backend()
    }
}

impl SubmissionPort for LiveSubmissionService {
    fn availability(&mut self) -> CapabilityAvailability {
        if self.state == DurabilityServiceState::Ready && self.runtime.storage().fault().is_none() {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Disabled
        }
    }

    fn submission_state(
        &mut self,
        principal: PrincipalId,
        id: SubmissionId,
    ) -> Result<Option<LifecycleState>, SubmissionPortError> {
        if self.runtime.storage().fault().is_some() {
            return Err(SubmissionPortError::Faulted);
        }
        if self.runtime.storage().pending_kind().is_some() {
            return Err(SubmissionPortError::Busy);
        }
        Ok(self.runtime.index().get_owned_state(principal, id))
    }

    fn accept(
        &mut self,
        candidate: AcceptanceCandidate,
    ) -> Result<SubmissionAcceptance, SubmissionPortError> {
        if matches!(
            self.runtime.index().plan_accept(candidate),
            AcceptOutcome::Accepted(_)
        ) && self.runtime.storage().state().accepted_submissions()
            >= config::DURABLE_ACCEPTED_SUBMISSION_LIMIT
        {
            return Ok(SubmissionAcceptance::CapacityExhausted);
        }
        self.runtime
            .accept(&mut self.journal, candidate)
            .map(map_submission_acceptance)
            .map_err(map_runtime_error)
    }
}

fn map_submission_acceptance(progress: AcceptanceProgress) -> SubmissionAcceptance {
    match progress {
        AcceptanceProgress::Accepted(id) => SubmissionAcceptance::Accepted(id),
        AcceptanceProgress::Replay(id) => SubmissionAcceptance::Replay(id),
        AcceptanceProgress::IdempotencyConflict { .. } => SubmissionAcceptance::IdempotencyConflict,
        AcceptanceProgress::IndexExhausted | AcceptanceProgress::JournalCapacityExhausted => {
            SubmissionAcceptance::CapacityExhausted
        }
        AcceptanceProgress::IdentifierExhausted => SubmissionAcceptance::IdentifierExhausted,
    }
}

fn map_runtime_error(error: RuntimeError<FakeNorError>) -> SubmissionPortError {
    match error {
        RuntimeError::Recovering => SubmissionPortError::Unavailable,
        RuntimeError::Storage(DriveError::Backend(_)) => SubmissionPortError::Backend,
        RuntimeError::Storage(DriveError::Binding(_)) => SubmissionPortError::Binding,
        RuntimeError::Storage(DriveError::Busy { .. })
        | RuntimeError::Projection(ProjectorOperationError::Busy { .. }) => {
            SubmissionPortError::Busy
        }
        RuntimeError::Storage(DriveError::Faulted(_))
        | RuntimeError::Projection(ProjectorOperationError::Faulted(_))
        | RuntimeError::Projection(ProjectorOperationError::Rejected(_)) => {
            SubmissionPortError::Faulted
        }
    }
}

pub struct LiveNodeSystem {
    pub supervisor: ProductSupervisor,
    application_events: ApplicationEventOwner<'static>,
    pub dispatcher: ProductDispatcher,
    pub frame_node: AuthorizedFrameNodeHandoff<NoopRawMutex>,
    pub node_rng: CounterRng,
    pub destination: DestinationHash,
    lifecycle: InterfaceLifecycleActorHandoff<NoopRawMutex>,
    online_descriptor: InterfaceDescriptor,
    now_us: u64,
}

impl LiveNodeSystem {
    pub fn new() -> Self {
        let receiver_identity = identity(0x42);
        let receiver = NodeCore::<
            { config::PATHS },
            { config::ANNOUNCES },
            { config::DEDUPLICATION },
            { config::LINKS },
            0,
        >::new(
            identity(0x42),
            "reticulum",
            &["live-admission-receiver"],
            NodeInstanceId::new([0x42; 16]),
            NodeConfig::endpoint(),
        )
        .expect("receiver node constructs");
        let destination = receiver.destination_hash();

        let mut node = NodeCore::<
            { config::PATHS },
            { config::ANNOUNCES },
            { config::DEDUPLICATION },
            { config::LINKS },
            { config::DATA_BUFFERS },
        >::new(
            identity(0x24),
            config::RNS_APPLICATION_NAME,
            &config::RNS_PRIMARY_ASPECTS,
            NodeInstanceId::new([0x24; 16]),
            NodeConfig::transport(),
        )
        .expect("product node constructs");
        node.register_peer(
            &receiver_identity,
            "reticulum",
            &["live-admission-receiver"],
            MonotonicSeconds::new(0),
        )
        .expect("receiver identity caches");

        let mut data_buffers = Box::leak(Box::new(
            [const { TxPacketBuffer::new() }; config::DATA_BUFFERS],
        ))
        .each_mut();
        for buffer in &mut data_buffers {
            node.register_packet_buffer(buffer)
                .expect("product DATA buffer registers");
        }
        let data =
            DataRouterCoordinator::try_new(&node, data_buffers, config::data_router_config())
                .unwrap_or_else(|failure| panic!("DATA coordinator: {:?}", failure.reason()));

        let mut ordinary_owner = node
            .take_ordinary_action_owner::<{ config::ORDINARY_BUFFERS }>()
            .expect("ordinary owner is unique");
        let ordinary_buffers = Box::leak(Box::new(
            [const { OrdinaryPacketBuffer::new() }; config::ORDINARY_BUFFERS],
        ))
        .each_mut();
        let mut ordinary_pool = OrdinaryBufferPool::new();
        for buffer in ordinary_buffers {
            ordinary_owner
                .register_and_park(&mut ordinary_pool, buffer)
                .unwrap_or_else(|failure| panic!("ordinary buffer: {:?}", failure.reason()));
        }
        let ordinary = OrdinaryRouterCoordinator::try_new(
            ordinary_owner,
            ordinary_pool,
            config::ordinary_router_config(),
        )
        .unwrap_or_else(|failure| panic!("ordinary coordinator: {:?}", failure.reason()));

        let fabric = Box::leak(Box::new(InterfaceFabric::new()));
        let data_pair = Box::leak(Box::new(DataPermitHandoff::new())).split_paired();
        let ordinary_pair = Box::leak(Box::new(OrdinaryPermitHandoff::new())).split_paired();
        let (frame_node, frame_dispatcher) =
            Box::leak(Box::new(AuthorizedFrameHandoff::new())).split();
        let policy = ExactLoRaAirtimePolicy::new(
            LORA_INTERFACE,
            E290_NA915_DEV_PROFILE,
            E290_NA915_DEV_CONFIGURATION_FINGERPRINT,
        );
        let (mut supervisor, [actor]) = ProductSupervisor::try_new(
            node,
            fabric,
            data,
            ordinary,
            [data_pair],
            [ordinary_pair],
            policy,
        )
        .unwrap_or_else(|failure| panic!("supervisor: {:?}", failure.reason()))
        .into_parts();
        let offline = supervisor
            .register_interface(
                actor.queue_id(),
                LORA_INTERFACE,
                config::interface_properties(),
            )
            .expect("LoRa interface registers");
        let (interface, data_permit, ordinary_permit) = actor.into_parts();
        let (tx_interface, _ingress, mut lifecycle) = interface.into_parts();
        let ready = lifecycle
            .try_request_state(offline.lease(), InterfaceLifecycleState::Ready)
            .expect("LoRa Ready request fits");
        assert!(matches!(
            supervisor.step(MonotonicMillis::new(1_000)).transition(),
            NodeInterfaceSupervisorTransition::Lifecycle(transition)
                if transition.acknowledgement().request() == ready
        ));
        let online_descriptor = lifecycle
            .try_finish_request()
            .expect("LoRa Ready acknowledgement correlates")
            .expect("LoRa Ready acknowledgement is retained");
        let dispatcher = SoleRadioTxDispatcher::new(
            ScriptedRadio::new(),
            CounterRng::default(),
            tx_interface,
            data_permit,
            ordinary_permit,
            frame_dispatcher,
            config::dispatcher_config(),
        );
        let application_event_slots = Box::leak(Box::new(
            [const { ApplicationEventSlot::new() }; config::APPLICATION_EVENT_SLOTS],
        ));

        Self {
            supervisor,
            application_events: ApplicationEventOwner::new(application_event_slots),
            dispatcher,
            frame_node,
            node_rng: CounterRng::default(),
            destination,
            lifecycle,
            online_descriptor,
            now_us: 1_000_000,
        }
    }

    pub fn transmit_data_to_durability_gate(&mut self) -> DispatchReport {
        for _ in 0..256 {
            let _ = self
                .supervisor
                .step(MonotonicMillis::new(self.now_us / 1_000));
            match self.dispatcher.step(self.now_us) {
                RadioTxDispatcherStep::Advanced | RadioTxDispatcherStep::NeedJob => {}
                RadioTxDispatcherStep::WaitUntil { retry_at_us, .. } => {
                    self.now_us = retry_at_us;
                }
                RadioTxDispatcherStep::NeedCad(DispatchFamily::Data) => {
                    match block_on(self.dispatcher.perform_radio_operation(self.now_us)) {
                        RadioOperationStep::CadObserved {
                            family: DispatchFamily::Data,
                            activity_detected: false,
                            observed_at_us,
                        } => self.now_us = observed_at_us + 100,
                        other => panic!("unexpected DATA CAD result: {other:?}"),
                    }
                }
                RadioTxDispatcherStep::NeedTransmit(DispatchFamily::Data) => {
                    let result = block_on(self.dispatcher.perform_radio_operation(self.now_us));
                    let RadioOperationStep::Terminal(report) = result else {
                        panic!("DATA transmit did not terminate: {result:?}")
                    };
                    assert_eq!(report.family(), DispatchFamily::Data);
                    assert_eq!(report.outcome(), DispatchOutcome::Transmitted);
                    assert_eq!(
                        self.dispatcher.phase(),
                        RadioTxDispatcherPhase::AuthorizedFrameRequest
                    );
                    return report;
                }
                RadioTxDispatcherStep::NeedPermitReply { .. } => {}
                other => panic!("unexpected DATA dispatcher step: {other:?}"),
            }
            self.now_us = self.now_us.saturating_add(100);
        }
        panic!("DATA dispatch did not reach the durability gate")
    }

    pub fn take_authorized_frame_request(&mut self) -> AuthorizedFrameObservation {
        assert_eq!(
            self.dispatcher.step(self.now_us),
            RadioTxDispatcherStep::Advanced
        );
        assert_eq!(
            self.dispatcher.phase(),
            RadioTxDispatcherPhase::AuthorizedFrameAcknowledgementWait
        );
        self.frame_node
            .requests()
            .try_receive()
            .expect("dispatcher sends the exact authorized frame")
    }

    pub fn acknowledge_authorized_frame(&mut self, frame: AuthorizedFrameObservation) {
        self.frame_node
            .acknowledgements()
            .try_send(frame)
            .unwrap_or_else(|_| panic!("acknowledgement channel is empty"));
        for _ in 0..8 {
            let _ = self.dispatcher.step(self.now_us);
            if self.dispatcher.phase() == RadioTxDispatcherPhase::Idle {
                return;
            }
            self.now_us += 1;
        }
        panic!("durable frame acknowledgement did not release the completion")
    }

    pub fn drain_completion(&mut self) -> Vec<NodeInterfaceSupervisorTransition> {
        let mut transitions = Vec::new();
        for _ in 0..32 {
            let transition = self
                .supervisor
                .step(MonotonicMillis::new(self.now_us / 1_000))
                .transition();
            if transition != NodeInterfaceSupervisorTransition::Idle {
                transitions.push(transition);
            }
            if self.supervisor.data_parked_counts().available() == config::DATA_BUFFERS {
                break;
            }
        }
        transitions
    }

    pub fn queue_ordinary_announce_behind_frame(&mut self) -> bool {
        self.supervisor
            .queue_announce(
                Some(b"queued behind durability gate"),
                AnnounceEmissionTime::new(2_000).expect("announce time fits"),
                &mut self.node_rng,
            )
            .expect("ordinary announce queues");
        assert!(matches!(
            self.supervisor.flush_announces(
                MonotonicSeconds::new(2_000),
                config::ordinary_admission(2_000_000),
                &mut self.node_rng,
            ),
            NodeInterfaceAnnounceFlushResult::Accepted
        ));
        for _ in 0..64 {
            match self
                .supervisor
                .step(MonotonicMillis::new(2_000_000))
                .transition()
            {
                NodeInterfaceSupervisorTransition::Ordinary(
                    OrdinaryRouterStep::NonPacketActionsReady,
                ) => {
                    assert!(matches!(
                        self.supervisor
                            .drain_application_events(&mut self.application_events),
                        NodeInterfaceApplicationEventDrain::Drained(_)
                    ));
                    while let Some(lease) = self.application_events.lease_next() {
                        lease.discard(ApplicationEventDiscardReason::ConsumerUnavailable);
                    }
                }
                NodeInterfaceSupervisorTransition::Ordinary(OrdinaryRouterStep::Routed {
                    ..
                }) => return true,
                _ => {}
            }
        }
        false
    }

    pub fn ordinary_router_is_idle(&self) -> bool {
        let capacities = self.supervisor.ordinary_capacities();
        capacities.active == 0 && self.supervisor.ordinary_parked_count() == capacities.registered
    }

    pub fn actor_reports_lora_offline(&mut self) -> InterfaceDescriptor {
        let offline = self
            .lifecycle
            .try_request_state(
                self.online_descriptor.lease(),
                InterfaceLifecycleState::Offline,
            )
            .expect("LoRa Offline request fits");
        assert!(matches!(
            self.supervisor
                .step(MonotonicMillis::new(self.now_us / 1_000))
                .transition(),
            NodeInterfaceSupervisorTransition::Lifecycle(transition)
                if transition.acknowledgement().request() == offline
        ));
        self.online_descriptor = self
            .lifecycle
            .try_finish_request()
            .expect("LoRa Offline acknowledgement correlates")
            .expect("LoRa Offline acknowledgement is retained");
        self.online_descriptor
    }

    pub fn disable_lora_by_node_policy(&mut self) -> InterfaceDescriptor {
        self.online_descriptor = self
            .supervisor
            .disable_interface(self.online_descriptor.lease())
            .expect("node policy disables LoRa");
        self.online_descriptor
    }

    pub fn force_delivery_timeout(&mut self) {
        let _ = self.supervisor.tick(
            MonotonicSeconds::new(1_000_000),
            config::ordinary_admission(1_000_000_000),
            &mut self.node_rng,
        );
    }
}

fn identity(tag: u8) -> NodeIdentity {
    NodeIdentity::from_private_key(&[tag; 64]).expect("test private key is accepted")
}
