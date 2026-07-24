extern crate std;

use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
    check_erase, check_read, check_write,
};
use reticulum_node_core::{
    AcknowledgeError, AttemptOutcome, AuthorizedFrameObservation, InterfaceSet, NodeConfig,
    NodeCore, NodeIdentity, NodeInstanceId, PermitResolution, PrepareDataRequest, RoutedTxJob,
    TxAuthorizationCandidate, TxAuthorizationPolicy, TxCompletionCode, TxCompletionDisposition,
    TxPacketBuffer, TxPermitRequirements, TxPermitReservation, TxPermitResourceId,
    TxPolicyDecision,
};
use reticulum_storage_actor::{BoundJournal, DriveError, JournalBinding, StorageDeviceId};
use reticulum_storage_journal::{
    ERASE_SIZE, PARTITION_SIZE, PHYSICAL_FORMAT_VERSION, format_erased,
};
use reticulum_storage_model::{
    AUTHORIZATION_PERMISSION_EXPERIMENTAL_SUBMIT_RNS_DATA,
    AUTHORIZATION_PERMISSION_READ_SUBMISSION_STATUS, AuthorizationSnapshot,
    DestinationHash as StoredDestinationHash, ExperimentalRnsDataIntent, FinalDisposition,
    IdempotencyKey, LxmfMessageIntent, PrincipalId, SubmissionFailure,
};
use std::{boxed::Box, vec, vec::Vec};

use super::*;

type TestNodeCore = NodeCore<4, 2, 8, 2, 1>;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FakeError {
    Bounds,
    Alignment,
    Injected,
}

impl NorFlashError for FakeError {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::Bounds => NorFlashErrorKind::OutOfBounds,
            Self::Alignment => NorFlashErrorKind::NotAligned,
            Self::Injected => NorFlashErrorKind::Other,
        }
    }
}

#[derive(Clone)]
struct FakeNor {
    bytes: Vec<u8>,
    lost_write_reply_after: Option<usize>,
    reject_writes: bool,
}

impl FakeNor {
    fn formatted() -> Self {
        let mut flash = Self {
            bytes: vec![0xff; PARTITION_SIZE],
            lost_write_reply_after: None,
            reject_writes: false,
        };
        format_erased(&mut flash).unwrap();
        flash
    }

    fn lose_write_reply_after(&mut self, successful_writes: usize) {
        self.lost_write_reply_after = Some(successful_writes);
    }

    fn reject_writes_permanently(&mut self) {
        self.reject_writes = true;
    }

    fn program(&mut self, offset: usize, bytes: &[u8]) {
        for (stored, supplied) in self.bytes[offset..offset + bytes.len()]
            .iter_mut()
            .zip(bytes)
        {
            *stored &= *supplied;
        }
    }
}

impl ErrorType for FakeNor {
    type Error = FakeError;
}

impl ReadNorFlash for FakeNor {
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

impl NorFlash for FakeNor {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = ERASE_SIZE;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        check_erase(self, from, to).map_err(map_check_error)?;
        self.bytes[from as usize..to as usize].fill(0xff);
        Ok(())
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        check_write(self, offset, bytes.len()).map_err(map_check_error)?;
        if self.reject_writes {
            return Err(FakeError::Injected);
        }
        let should_lose_reply = self.lost_write_reply_after == Some(0);
        if let Some(remaining) = &mut self.lost_write_reply_after
            && *remaining != 0
        {
            *remaining -= 1;
        }
        self.program(offset as usize, bytes);
        if should_lose_reply {
            self.lost_write_reply_after = None;
            Err(FakeError::Injected)
        } else {
            Ok(())
        }
    }
}

impl MultiwriteNorFlash for FakeNor {}

fn map_check_error(error: NorFlashErrorKind) -> FakeError {
    match error {
        NorFlashErrorKind::OutOfBounds => FakeError::Bounds,
        _ => FakeError::Alignment,
    }
}

fn identity(tag: u8) -> NodeIdentity {
    NodeIdentity::from_private_key(&[tag; 64]).unwrap()
}

const TEST_PERMIT_RESOURCE: TxPermitResourceId = TxPermitResourceId::new([0x51; 16]);

struct AllowPolicy;

impl TxAuthorizationPolicy for AllowPolicy {
    fn authorize(&mut self, candidate: TxAuthorizationCandidate) -> TxPolicyDecision {
        TxPolicyDecision::Authorize(
            TxPermitReservation::try_new(
                candidate.requirements.resource(),
                candidate.requirements.required_units(),
            )
            .unwrap(),
        )
    }
}

struct TestNode {
    core: TestNodeCore,
    buffer: Option<&'static mut TxPacketBuffer>,
    job: Option<RoutedTxJob<'static>>,
    recovered: Option<TxRecoveryObservation>,
    hide_terminal: bool,
    destination: DestinationHash,
    prepare_calls: usize,
    opportunistic_lxmf_prepare_calls: usize,
    last_opportunistic_lxmf_wire_len: Option<usize>,
    forced_opportunistic_lxmf_preparation: Option<SubmissionPreparationObservation>,
    direct_lxmf_prepare_calls: usize,
    last_direct_lxmf_wire: Option<Vec<u8>>,
    forced_direct_lxmf_preparation: Option<SubmissionPreparationObservation>,
    has_usable_path: bool,
    retained_path_hops: Option<u8>,
    retained_path_first_hop_serialization_ms: Option<u64>,
    can_initiate_link: bool,
    retained_links: Vec<(LinkHandle, LinkState)>,
    unusable_links: Vec<LinkHandle>,
    last_direct_link: Option<LinkHandle>,
    abort_calls: usize,
    last_aborted_link: Option<LinkHandle>,
    acknowledge_calls: usize,
    forced_unknown_preparations: usize,
}

impl TestNode {
    fn new() -> Self {
        let receiver_node = TestNodeCore::new(
            identity(0x42),
            "reticulum",
            &["submission-runtime-receiver"],
            NodeInstanceId::new([0x82; 16]),
            NodeConfig::endpoint(),
        )
        .unwrap();
        let destination = DestinationHash::new(*receiver_node.destination_hash().as_bytes());
        let mut core = TestNodeCore::new(
            identity(0x41),
            "reticulum",
            &["submission-runtime-sender"],
            NodeInstanceId::new([0x81; 16]),
            NodeConfig::endpoint(),
        )
        .unwrap();
        core.register_peer(
            &identity(0x42),
            "reticulum",
            &["submission-runtime-receiver"],
            MonotonicSeconds::new(0),
        )
        .unwrap();
        let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        core.register_packet_buffer(buffer).unwrap();
        Self {
            core,
            buffer: Some(buffer),
            job: None,
            recovered: None,
            hide_terminal: false,
            destination,
            prepare_calls: 0,
            opportunistic_lxmf_prepare_calls: 0,
            last_opportunistic_lxmf_wire_len: None,
            forced_opportunistic_lxmf_preparation: None,
            direct_lxmf_prepare_calls: 0,
            last_direct_lxmf_wire: None,
            forced_direct_lxmf_preparation: None,
            has_usable_path: true,
            retained_path_hops: Some(1),
            retained_path_first_hop_serialization_ms: Some(0),
            can_initiate_link: true,
            retained_links: Vec::new(),
            unusable_links: Vec::new(),
            last_direct_link: None,
            abort_calls: 0,
            last_aborted_link: None,
            acknowledge_calls: 0,
            forced_unknown_preparations: 0,
        }
    }

    fn force_unknown_preparations(&mut self, count: usize) {
        self.forced_unknown_preparations = count;
    }

    fn force_opportunistic_lxmf_preparation(
        &mut self,
        observation: SubmissionPreparationObservation,
    ) {
        self.forced_opportunistic_lxmf_preparation = Some(observation);
    }

    fn force_direct_lxmf_preparation(&mut self, observation: SubmissionPreparationObservation) {
        self.forced_direct_lxmf_preparation = Some(observation);
    }

    fn retain_link(&mut self, link: LinkHandle, state: LinkState) {
        if let Some((_, retained_state)) = self
            .retained_links
            .iter_mut()
            .find(|(retained, _)| *retained == link)
        {
            *retained_state = state;
        } else {
            self.retained_links.push((link, state));
        }
    }

    fn set_has_usable_path(&mut self, has_usable_path: bool) {
        self.has_usable_path = has_usable_path;
    }

    fn set_retained_path_hops(&mut self, retained_path_hops: Option<u8>) {
        self.retained_path_hops = retained_path_hops;
    }

    fn set_retained_path_first_hop_serialization_ms(&mut self, milliseconds: Option<u64>) {
        self.retained_path_first_hop_serialization_ms = milliseconds;
    }

    fn set_can_initiate_link(&mut self, can_initiate_link: bool) {
        self.can_initiate_link = can_initiate_link;
    }

    fn set_link_state(&mut self, link: LinkHandle, state: LinkState) {
        let (_, retained_state) = self
            .retained_links
            .iter_mut()
            .find(|(retained, _)| *retained == link)
            .expect("the test must update the exact retained Link");
        *retained_state = state;
    }

    fn set_link_usable(&mut self, link: LinkHandle, usable: bool) {
        if usable {
            self.unusable_links.retain(|retained| *retained != link);
        } else if !self.unusable_links.contains(&link) {
            self.unusable_links.push(link);
        }
    }

    fn expose_frame_and_timeout(&mut self) -> AuthorizedFrameObservation {
        let job = self.job.take().expect("one prepared job must be retained");
        let requirements = TxPermitRequirements::try_new(TEST_PERMIT_RESOURCE, 1).unwrap();
        let (pending, request) = job.begin_permit(requirements);
        let reply = self
            .core
            .authorize_tx(request, MonotonicMillis::new(100_010), &mut AllowPolicy)
            .unwrap_or_else(|_| panic!("fresh permit request must authorize"));
        let resolution = pending
            .resolve(reply, MonotonicMillis::new(100_011))
            .unwrap_or_else(|_| panic!("matching permit reply must resolve"));
        let PermitResolution::Authorized(mut authorized) = resolution else {
            panic!("allow policy must authorize the fresh packet")
        };
        let observation = authorized
            .frame(MonotonicMillis::new(100_012))
            .unwrap()
            .observation();
        let completion = authorized.complete(TxCompletionCode::new(1));
        let disposition = self
            .core
            .complete_tx(completion, MonotonicMillis::new(100_013))
            .unwrap_or_else(|_| panic!("matching completion must return"));
        let TxCompletionDisposition::Available(buffer) = disposition else {
            panic!("one-hop successful completion must return the buffer")
        };
        self.buffer = Some(buffer);
        assert_eq!(
            self.core
                .tick(MonotonicSeconds::new(132), &mut CounterRng::default())
                .timed_out_attempts,
            1
        );
        observation
    }

    fn expose_frame_and_recover(&mut self) -> (AuthorizedFrameObservation, TxRecoveryObservation) {
        let job = self.job.take().expect("one prepared job must be retained");
        let requirements = TxPermitRequirements::try_new(TEST_PERMIT_RESOURCE, 1).unwrap();
        let (pending, request) = job.begin_permit(requirements);
        let reply = self
            .core
            .authorize_tx(request, MonotonicMillis::new(100_010), &mut AllowPolicy)
            .unwrap_or_else(|_| panic!("fresh permit request must authorize"));
        let resolution = pending
            .resolve(reply, MonotonicMillis::new(100_011))
            .unwrap_or_else(|_| panic!("matching permit reply must resolve"));
        let PermitResolution::Authorized(mut authorized) = resolution else {
            panic!("allow policy must authorize the fresh packet")
        };
        let frame = authorized
            .frame(MonotonicMillis::new(100_012))
            .unwrap()
            .observation();
        assert_eq!(
            self.core
                .maintain_tx(MonotonicMillis::new(200_000))
                .newly_recovery_required,
            1
        );
        let completion = authorized.complete(TxCompletionCode::new(1));
        let disposition = self
            .core
            .complete_tx(completion, MonotonicMillis::new(200_001))
            .unwrap_or_else(|_| panic!("expired matching completion must return for recovery"));
        let TxCompletionDisposition::Recovered {
            buffer,
            observation: recovered,
        } = disposition
        else {
            panic!("expired authorized owner must return a recovery observation")
        };
        self.buffer = Some(buffer);
        self.recovered = Some(recovered);
        self.hide_terminal = true;
        (frame, recovered)
    }
}

impl SubmissionNodePort for TestNode {
    fn prepare_submission<R>(
        &mut self,
        request: SubmissionPrepareRequest<'_>,
        rng: &mut R,
    ) -> SubmissionPreparationObservation
    where
        R: RngCore + CryptoRng,
    {
        self.prepare_calls += 1;
        assert_eq!(request.destination, self.destination);
        if self.forced_unknown_preparations > 0 {
            self.forced_unknown_preparations -= 1;
            return SubmissionPreparationObservation::Rejected(SubmitError::UnknownDestination);
        }
        let buffer = self
            .buffer
            .take()
            .expect("test node has one available owner");
        match self.core.prepare_data_into_slot(
            buffer,
            PrepareDataRequest {
                destination: request.destination,
                plaintext: request.plaintext,
                rns_now: request.rns_now,
                owner_now: request.owner_now,
                deadline: request.deadline,
                enabled_interfaces: InterfaceSet::from_bits(1 << 1),
            },
            rng,
        ) {
            Ok(job) => {
                let prepared = job.prepared();
                self.job = Some(job);
                SubmissionPreparationObservation::Prepared(prepared)
            }
            Err(failure) => {
                let reason = failure.reason();
                self.buffer = Some(
                    failure
                        .into_buffer()
                        .unwrap_or_else(|_| panic!("ordinary preparation rejection must recycle")),
                );
                SubmissionPreparationObservation::Rejected(reason)
            }
        }
    }

    fn prepare_rehydrated_opportunistic_lxmf_submission<R>(
        &mut self,
        request: SubmissionPrepareRequest<'_>,
        _rng: &mut R,
    ) -> SubmissionPreparationObservation
    where
        R: RngCore + CryptoRng,
    {
        self.opportunistic_lxmf_prepare_calls += 1;
        assert_eq!(request.destination, self.destination);
        assert_eq!(
            request.plaintext.get(..16),
            Some(request.destination.as_bytes().as_slice())
        );
        self.last_opportunistic_lxmf_wire_len = Some(request.plaintext.len());
        self.forced_opportunistic_lxmf_preparation
            .take()
            .unwrap_or(SubmissionPreparationObservation::RetrySameBoot)
    }

    fn prepare_rehydrated_direct_lxmf_submission<R>(
        &mut self,
        link: LinkHandle,
        request: SubmissionPrepareRequest<'_>,
        _rng: &mut R,
    ) -> SubmissionPreparationObservation
    where
        R: RngCore + CryptoRng,
    {
        self.direct_lxmf_prepare_calls += 1;
        assert_eq!(
            request.plaintext.get(..16),
            Some(request.destination.as_bytes().as_slice())
        );
        assert_eq!(
            self.link_state(link),
            Some(LinkState::Active),
            "direct preparation requires the exact active test Link"
        );
        self.last_direct_link = Some(link);
        self.last_direct_lxmf_wire = Some(request.plaintext.to_vec());
        self.forced_direct_lxmf_preparation
            .take()
            .unwrap_or(SubmissionPreparationObservation::RetrySameBoot)
    }

    fn has_usable_path(&self, _destination: &DestinationHash) -> bool {
        self.has_usable_path
    }

    fn retained_path_hops(&self, _destination: &DestinationHash) -> Option<u8> {
        self.retained_path_hops
    }

    fn retained_path_first_hop_serialization_ms(
        &self,
        _destination: &DestinationHash,
    ) -> Option<u64> {
        self.retained_path_first_hop_serialization_ms
    }

    fn can_initiate_link(&self) -> bool {
        self.can_initiate_link
    }

    fn link_state(&self, link: LinkHandle) -> Option<LinkState> {
        self.retained_links
            .iter()
            .find(|(retained, _)| *retained == link)
            .map(|(_, state)| state)
            .copied()
    }

    fn link_is_usable(&self, link: LinkHandle) -> bool {
        self.link_state(link) == Some(LinkState::Active) && !self.unusable_links.contains(&link)
    }

    fn abort_unestablished_link(&mut self, link: LinkHandle) -> bool {
        self.abort_calls += 1;
        self.last_aborted_link = Some(link);
        let Some(index) = self.retained_links.iter().position(|(retained, state)| {
            *retained == link && matches!(state, LinkState::Pending | LinkState::Handshake)
        }) else {
            return false;
        };
        self.retained_links.swap_remove(index);
        true
    }

    fn terminal_attempts(&self) -> impl Iterator<Item = TerminalAttempt> + '_ {
        self.core
            .terminal_attempts()
            .filter(|_| !self.hide_terminal)
    }

    fn recovered_observations(&self) -> impl Iterator<Item = TxRecoveryObservation> + '_ {
        self.recovered.iter().copied()
    }

    fn quarantined_observations(&self) -> impl Iterator<Item = TxRecoveryObservation> + '_ {
        core::iter::empty()
    }

    fn acknowledge(&mut self, action: AcknowledgementAction) -> AcknowledgementReply {
        self.acknowledge_calls += 1;
        match action.kind() {
            AcknowledgementKind::Terminal(expected) => {
                match self.core.acknowledge_terminal(expected.handle()) {
                    Ok(actual) if actual == expected => AcknowledgementReply::Completed,
                    Err(AcknowledgeError::PacketStillBound) => AcknowledgementReply::Retryable,
                    _ => AcknowledgementReply::Error(1),
                }
            }
            AcknowledgementKind::Recovered(expected) => {
                if self.recovered == Some(expected) {
                    self.recovered = None;
                    AcknowledgementReply::Completed
                } else {
                    AcknowledgementReply::Error(2)
                }
            }
        }
    }
}

fn candidate(destination: DestinationHash) -> AcceptanceCandidate {
    AcceptanceCandidate::new(
        PrincipalId::new([0x11; 16]),
        IdempotencyKey::new([0x12; 16]),
        ExperimentalRnsDataIntent::new(
            StoredDestinationHash::new(*destination.as_bytes()),
            b"portable durable submission",
        )
        .unwrap(),
        AuthorizationSnapshot::new(
            [0x13; 16],
            7,
            9,
            1,
            AUTHORIZATION_PERMISSION_EXPERIMENTAL_SUBMIT_RNS_DATA
                | AUTHORIZATION_PERMISSION_READ_SUBMISSION_STATUS,
        )
        .unwrap(),
    )
}

fn lxmf_message_candidate(destination: DestinationHash, carrier_len: usize) -> AcceptanceCandidate {
    let mut wire = vec![0x5d; 16 + carrier_len];
    wire[..16].copy_from_slice(destination.as_bytes());
    AcceptanceCandidate::new(
        PrincipalId::new([0x21; 16]),
        IdempotencyKey::new([carrier_len as u8; 16]),
        LxmfMessageIntent::new(&wire).unwrap(),
        AuthorizationSnapshot::new(
            [0x23; 16],
            7,
            9,
            1,
            AUTHORIZATION_PERMISSION_EXPERIMENTAL_SUBMIT_RNS_DATA
                | AUTHORIZATION_PERMISSION_READ_SUBMISSION_STATUS,
        )
        .unwrap(),
    )
}

const TEST_DEVICE: StorageDeviceId = StorageDeviceId::new([0x51; 16]);

fn binding() -> JournalBinding {
    JournalBinding::new(
        TEST_DEVICE,
        0x0063_0000,
        PARTITION_SIZE,
        PHYSICAL_FORMAT_VERSION,
    )
}

fn formatted_access() -> BoundJournal<FakeNor> {
    BoundJournal::new(FakeNor::formatted(), binding())
}

#[test]
fn mount_into_initializes_the_supplied_storage_only_after_successful_replay() {
    let mut destination = MaybeUninit::<SubmissionRuntime<4, 2>>::uninit();
    let destination_address = destination.as_mut_ptr();
    let mut access = formatted_access();

    let runtime = SubmissionRuntime::<4, 2>::mount_into(
        &mut destination,
        &mut access,
        SubmissionId::new(38),
        6,
    )
    .unwrap();

    assert_eq!(core::ptr::from_mut(runtime), destination_address);
    assert_eq!(runtime.phase(), RuntimePhase::Recovering);
    assert_eq!(runtime.index().next_id(), Some(SubmissionId::new(38)));
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    assert_eq!(runtime.phase(), RuntimePhase::Ready);
}

#[test]
fn mount_into_failure_leaves_the_destination_available_for_a_later_mount() {
    assert!(!core::mem::needs_drop::<SubmissionRuntime<4, 2>>());
    let mut destination = MaybeUninit::<SubmissionRuntime<4, 2>>::uninit();
    let mut unformatted = BoundJournal::new(
        FakeNor {
            bytes: vec![0xff; PARTITION_SIZE],
            lost_write_reply_after: None,
            reject_writes: false,
        },
        binding(),
    );

    assert!(
        SubmissionRuntime::<4, 2>::mount_into(
            &mut destination,
            &mut unformatted,
            SubmissionId::new(39),
            7,
        )
        .is_err()
    );

    let mut formatted = formatted_access();
    let runtime = SubmissionRuntime::<4, 2>::mount_into(
        &mut destination,
        &mut formatted,
        SubmissionId::new(39),
        7,
    )
    .unwrap();
    assert_eq!(runtime.index().next_id(), Some(SubmissionId::new(39)));
}

fn try_drive<const SUBMISSIONS: usize, const PROJECTED: usize, const DIRECT_LINKS: usize>(
    runtime: &mut SubmissionRuntime<SUBMISSIONS, PROJECTED, DIRECT_LINKS>,
    access: &mut BoundJournal<FakeNor>,
    node: &mut TestNode,
) -> Result<RuntimeStep, RuntimeError<FakeError>> {
    try_drive_at(runtime, access, node, 100_000)
}

fn try_drive_at<const SUBMISSIONS: usize, const PROJECTED: usize, const DIRECT_LINKS: usize>(
    runtime: &mut SubmissionRuntime<SUBMISSIONS, PROJECTED, DIRECT_LINKS>,
    access: &mut BoundJournal<FakeNor>,
    node: &mut TestNode,
    now_ms: u64,
) -> Result<RuntimeStep, RuntimeError<FakeError>> {
    runtime.drive_step(
        access,
        node,
        MonotonicSeconds::new(now_ms / 1_000),
        MonotonicMillis::new(now_ms),
        TxLeaseDeadline::new(MonotonicMillis::new(now_ms.saturating_add(100_000))),
        &mut CounterRng::default(),
    )
}

fn drive<const SUBMISSIONS: usize, const PROJECTED: usize, const DIRECT_LINKS: usize>(
    runtime: &mut SubmissionRuntime<SUBMISSIONS, PROJECTED, DIRECT_LINKS>,
    access: &mut BoundJournal<FakeNor>,
    node: &mut TestNode,
) -> RuntimeStep {
    try_drive(runtime, access, node).unwrap()
}

fn accept_lxmf_and_emit_link<
    const SUBMISSIONS: usize,
    const PROJECTED: usize,
    const DIRECT_LINKS: usize,
>(
    runtime: &mut SubmissionRuntime<SUBMISSIONS, PROJECTED, DIRECT_LINKS>,
    access: &mut BoundJournal<FakeNor>,
    destination: DestinationHash,
    node: &mut TestNode,
    carrier_len: usize,
) -> (SubmissionId, LinkEstablishmentOffer) {
    let id = match runtime
        .accept(access, lxmf_message_candidate(destination, carrier_len))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("fresh LXMF candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        drive(runtime, access, node),
        RuntimeStep::PreparationBarrier { id: observed, .. } if observed == id
    ));
    assert_eq!(
        drive(runtime, access, node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    let RuntimeStep::LinkEstablishment {
        offer,
        progress: ProjectionProgress::NoAction,
    } = drive(runtime, access, node)
    else {
        panic!("direct-required LXMF must emit one Link establishment offer")
    };
    assert_eq!(offer.id(), id);
    (id, offer)
}

fn expected_lxmf_wire(destination: DestinationHash, carrier_len: usize) -> Vec<u8> {
    let mut wire = vec![0x5d; 16 + carrier_len];
    wire[..16].copy_from_slice(destination.as_bytes());
    wire
}

#[test]
fn durable_runtime_enforces_barrier_frame_terminal_and_ack_ordering() {
    let mut node = TestNode::new();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(40), 7).unwrap();
    assert_eq!(runtime.phase(), RuntimePhase::Recovering);
    assert!(matches!(
        runtime.accept(&mut access, candidate(node.destination)),
        Err(RuntimeError::Recovering)
    ));
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );

    let id = match runtime
        .accept(&mut access, candidate(node.destination))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("fresh candidate did not accept: {other:?}"),
    };
    assert_eq!(node.prepare_calls, 0);

    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::PreparationBarrier {
            id: observed,
            progress: ProjectionProgress::Persist(_),
        } if observed == id
    ));
    assert_eq!(node.prepare_calls, 0);
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert_eq!(node.prepare_calls, 0);
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Preparation {
            id,
            progress: ProjectionProgress::AttemptBound,
        }
    );
    assert_eq!(node.prepare_calls, 1);

    let frame = node.expose_frame_and_timeout();
    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Retain)
    );
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Durable)
    );

    let RuntimeStep::Terminal { terminal, progress } = drive(&mut runtime, &mut access, &mut node)
    else {
        panic!("terminal must be observed after the frame is durable")
    };
    assert_eq!(terminal.outcome(), AttemptOutcome::DeliveryTimeout);
    assert!(matches!(progress, ProjectionProgress::Persist(_)));
    assert_eq!(runtime.storage().pending_acknowledgements().count(), 0);
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert_eq!(runtime.storage().pending_acknowledgements().count(), 1);
    let expected_action = runtime.storage().pending_acknowledgements().next().unwrap();
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Acknowledgement {
            action: expected_action,
            reply: AcknowledgementReply::Completed,
        }
    );

    assert_eq!(runtime.storage().pending_acknowledgements().count(), 0);
    assert!(matches!(
        runtime.index().get(id).unwrap().state(),
        LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::DeliveryTimeout))
    ));
}

#[test]
fn opportunistic_header_two_overflow_emits_one_exact_link_establishment_offer() {
    let mut node = TestNode::new();
    node.force_opportunistic_lxmf_preparation(SubmissionPreparationObservation::Rejected(
        SubmitError::PayloadTooLarge {
            actual: MAX_OPPORTUNISTIC_LXMF_CARRIER,
            maximum: reticulum_node_core::MAX_DATA_PAYLOAD,
        },
    ));
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(45), 7).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let id = match runtime
        .accept(
            &mut access,
            lxmf_message_candidate(node.destination, MAX_OPPORTUNISTIC_LXMF_CARRIER),
        )
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("fresh LXMF-message candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::PreparationBarrier { id: observed, .. } if observed == id
    ));
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    let RuntimeStep::LinkEstablishment {
        offer,
        progress: ProjectionProgress::NoAction,
    } = drive(&mut runtime, &mut access, &mut node)
    else {
        panic!("Header-2 overflow must escalate the exact durable wire to a Link")
    };
    assert_eq!(offer.id(), id);
    assert_eq!(offer.destination(), node.destination);
    assert_eq!(offer.generation(), 1);
    assert_eq!(node.prepare_calls, 0);
    assert_eq!(node.opportunistic_lxmf_prepare_calls, 1);
    assert_eq!(
        node.last_opportunistic_lxmf_wire_len,
        Some(16 + MAX_OPPORTUNISTIC_LXMF_CARRIER)
    );
    assert_eq!(
        runtime.index().get(id).unwrap().state(),
        LifecycleState::Preparing
    );
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::LinkEstablishment {
            offer,
            progress: ProjectionProgress::NoAction,
        },
        "a transient creation failure must retry the same exact generation"
    );
    let link = LinkHandle::new([0x90; 16]);
    node.retain_link(link, LinkState::Pending);
    runtime.attach_created_link(offer, link).unwrap();
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Idle,
        "attachment must stop creation retries while exact dispatch is pending"
    );
}

#[test]
fn lxmf_message_above_opportunistic_ceiling_emits_link_without_terminal_rejection() {
    let mut node = TestNode::new();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(46), 7).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let id = match runtime
        .accept(
            &mut access,
            lxmf_message_candidate(node.destination, MAX_OPPORTUNISTIC_LXMF_CARRIER + 1),
        )
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("fresh LXMF-message candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::PreparationBarrier { id: observed, .. } if observed == id
    ));
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    let RuntimeStep::LinkEstablishment {
        offer,
        progress: ProjectionProgress::NoAction,
    } = drive(&mut runtime, &mut access, &mut node)
    else {
        panic!("oversize opportunistic carrier must emit a direct-Link offer")
    };
    assert_eq!(offer.id(), id);
    assert_eq!(offer.destination(), node.destination);
    assert_eq!(node.prepare_calls, 0);
    assert_eq!(node.opportunistic_lxmf_prepare_calls, 0);
    assert_eq!(
        runtime.index().get(id).unwrap().state(),
        LifecycleState::Preparing
    );
}

#[test]
fn direct_link_control_is_exact_and_deadline_starts_at_first_dispatch() {
    let mut node = TestNode::new();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(47), 7).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let (_id, offer) = accept_lxmf_and_emit_link(
        &mut runtime,
        &mut access,
        node.destination,
        &mut node,
        MAX_OPPORTUNISTIC_LXMF_CARRIER + 1,
    );
    let link = LinkHandle::new([0x91; 16]);
    let wrong_link = LinkHandle::new([0x92; 16]);
    let wrong_offer = LinkEstablishmentOffer {
        generation: offer.generation() + 1,
        ..offer
    };
    assert_eq!(
        offer.establishment_timeout_ms(),
        DIRECT_LINK_ESTABLISHMENT_TIMEOUT_MS,
        "one-hop routes retain the product minimum establishment window"
    );

    assert_eq!(
        runtime.acknowledge_link_request_dispatched(offer, link, MonotonicMillis::new(500_000),),
        Err(LinkEstablishmentControlError::WrongPhase)
    );
    assert_eq!(
        runtime.attach_created_link(wrong_offer, link),
        Err(LinkEstablishmentControlError::OfferMismatch)
    );

    node.retain_link(link, LinkState::Pending);
    assert_eq!(runtime.attach_created_link(offer, link), Ok(()));
    assert_eq!(
        runtime.attach_created_link(offer, link),
        Ok(()),
        "exact duplicate attachment must be idempotent"
    );
    assert_eq!(
        runtime.attach_created_link(offer, wrong_link),
        Err(LinkEstablishmentControlError::LinkMismatch)
    );
    assert_eq!(
        try_drive_at(&mut runtime, &mut access, &mut node, 900_000).unwrap(),
        RuntimeStep::Idle,
        "an attached but undispatched Link must have no establishment deadline"
    );
    assert_eq!(node.abort_calls, 0);

    assert_eq!(
        runtime.acknowledge_link_request_dispatched(
            wrong_offer,
            link,
            MonotonicMillis::new(500_000),
        ),
        Err(LinkEstablishmentControlError::OfferMismatch)
    );
    assert_eq!(
        runtime.acknowledge_link_request_dispatched(
            offer,
            wrong_link,
            MonotonicMillis::new(500_000),
        ),
        Err(LinkEstablishmentControlError::LinkMismatch)
    );
    assert_eq!(
        runtime.acknowledge_link_request_dispatched(offer, link, MonotonicMillis::new(500_000),),
        Ok(())
    );
    assert_eq!(
        runtime.acknowledge_link_request_dispatched(offer, link, MonotonicMillis::new(500_000),),
        Ok(()),
        "exact duplicate first-dispatch acknowledgement must be idempotent"
    );
    assert_eq!(
        runtime.acknowledge_link_request_dispatched(offer, link, MonotonicMillis::new(500_001),),
        Err(LinkEstablishmentControlError::DispatchTimeMismatch)
    );
    assert_eq!(
        try_drive_at(
            &mut runtime,
            &mut access,
            &mut node,
            500_000 + DIRECT_LINK_ESTABLISHMENT_TIMEOUT_MS - 1,
        )
        .unwrap(),
        RuntimeStep::Idle
    );
    assert_eq!(node.abort_calls, 0);
    assert_eq!(
        try_drive_at(
            &mut runtime,
            &mut access,
            &mut node,
            500_000 + DIRECT_LINK_ESTABLISHMENT_TIMEOUT_MS,
        )
        .unwrap(),
        RuntimeStep::LinkEstablishmentExpired {
            offer,
            link,
            aborted: true,
        }
    );
    assert_eq!(node.abort_calls, 1);
    assert_eq!(node.last_aborted_link, Some(link));

    let RuntimeStep::LinkEstablishment {
        offer: retry,
        progress: ProjectionProgress::NoAction,
    } = try_drive_at(&mut runtime, &mut access, &mut node, 530_001).unwrap()
    else {
        panic!("the still-durable message must receive a fresh exact retry offer")
    };
    assert_eq!(retry.id(), offer.id());
    assert_eq!(retry.destination(), offer.destination());
    assert_ne!(retry.generation(), offer.generation());
    let retry_link = LinkHandle::new([0x93; 16]);
    node.retain_link(retry_link, LinkState::Pending);
    assert_eq!(
        runtime.attach_created_link(offer, link),
        Err(LinkEstablishmentControlError::OfferMismatch),
        "a stale callback must not consume the newer pending offer"
    );
    assert_eq!(runtime.attach_created_link(retry, retry_link), Ok(()));
}

#[test]
fn link_establishment_deadline_snapshots_the_retained_route_hops() {
    assert_eq!(
        direct_link_establishment_timeout(Some(3), Some(0)),
        DIRECT_LINK_ESTABLISHMENT_TIMEOUT_MS,
        "three hops with unknown bitrate remain covered by the 30-second product minimum"
    );
    let mut node = TestNode::new();
    node.set_retained_path_hops(Some(8));
    node.set_retained_path_first_hop_serialization_ms(Some(732));
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(60), 7).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let (_id, offer) = accept_lxmf_and_emit_link(
        &mut runtime,
        &mut access,
        node.destination,
        &mut node,
        MAX_OPPORTUNISTIC_LXMF_CARRIER + 5,
    );
    assert_eq!(offer.establishment_timeout_ms(), 56_732);

    let link = LinkHandle::new([0x95; 16]);
    node.retain_link(link, LinkState::Pending);
    runtime.attach_created_link(offer, link).unwrap();
    node.set_retained_path_hops(Some(1));
    runtime
        .acknowledge_link_request_dispatched(offer, link, MonotonicMillis::new(600_000))
        .unwrap();
    assert_eq!(
        try_drive_at(
            &mut runtime,
            &mut access,
            &mut node,
            600_000
                + 9 * DIRECT_LINK_ESTABLISHMENT_PER_HOP_MS
                + 732
                + DIRECT_LINK_ESTABLISHMENT_DISPATCH_GUARD_MS
                - 1,
        )
        .unwrap(),
        RuntimeStep::Idle
    );
    assert_eq!(
        try_drive_at(
            &mut runtime,
            &mut access,
            &mut node,
            600_000
                + 9 * DIRECT_LINK_ESTABLISHMENT_PER_HOP_MS
                + 732
                + DIRECT_LINK_ESTABLISHMENT_DISPATCH_GUARD_MS,
        )
        .unwrap(),
        RuntimeStep::LinkEstablishmentExpired {
            offer,
            link,
            aborted: true,
        },
        "later route changes must not shorten the exact offer's deadline"
    );
}

#[test]
fn oversize_lxmf_requires_a_usable_path_before_offering_link_establishment() {
    let mut node = TestNode::new();
    node.set_has_usable_path(false);
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(48), 7).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let id = match runtime
        .accept(
            &mut access,
            lxmf_message_candidate(node.destination, MAX_OPPORTUNISTIC_LXMF_CARRIER + 1),
        )
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("fresh LXMF candidate did not accept: {other:?}"),
    };
    let _ = drive(&mut runtime, &mut access, &mut node);
    let _ = drive(&mut runtime, &mut access, &mut node);
    let RuntimeStep::PathDiscoveryRequest {
        offer: path_offer,
        progress: ProjectionProgress::NoAction,
    } = try_drive_at(&mut runtime, &mut access, &mut node, 100_000).unwrap()
    else {
        panic!("direct Link creation must wait behind destination path discovery")
    };
    assert_eq!(path_offer.id(), id);
    assert_eq!(node.opportunistic_lxmf_prepare_calls, 0);
    assert_eq!(node.direct_lxmf_prepare_calls, 0);
    runtime
        .acknowledge_path_request_dispatched(path_offer, MonotonicMillis::new(100_000))
        .unwrap();

    node.set_has_usable_path(true);
    let RuntimeStep::LinkEstablishment {
        offer,
        progress: ProjectionProgress::NoAction,
    } = try_drive_at(&mut runtime, &mut access, &mut node, 107_000).unwrap()
    else {
        panic!("the learned path must unlock the exact direct-Link offer")
    };
    assert_eq!(offer.id(), id);
    assert_eq!(offer.destination(), node.destination);
}

#[test]
fn native_link_table_pressure_does_not_block_later_opportunistic_work() {
    let mut node = TestNode::new();
    node.set_can_initiate_link(false);
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 4, 1>::mount(&mut access, SubmissionId::new(55), 7).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );

    let direct_id = match runtime
        .accept(
            &mut access,
            lxmf_message_candidate(node.destination, MAX_OPPORTUNISTIC_LXMF_CARRIER + 1),
        )
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("direct-required candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::PreparationBarrier { id, .. } if id == direct_id
    ));
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::LinkCapacityBackpressured {
            id: direct_id,
            limit: 1,
        }
    );

    let short_id = match runtime
        .accept(&mut access, lxmf_message_candidate(node.destination, 63))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("later opportunistic candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::PreparationBarrier { id, .. } if id == short_id
    ));
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Preparation { id, .. } if id == short_id
    ));
    assert_eq!(
        node.opportunistic_lxmf_prepare_calls, 1,
        "native Link-table pressure must not head-of-line block short LXMF"
    );

    node.set_can_initiate_link(true);
    let RuntimeStep::LinkEstablishment { offer, .. } = drive(&mut runtime, &mut access, &mut node)
    else {
        panic!("native admission recovery must resume the waiting direct submission")
    };
    assert_eq!(offer.id(), direct_id);
}

#[test]
fn awaiting_link_offer_yields_when_native_admission_is_lost() {
    let mut node = TestNode::new();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 4, 1>::mount(&mut access, SubmissionId::new(57), 7).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let (direct_id, first_offer) = accept_lxmf_and_emit_link(
        &mut runtime,
        &mut access,
        node.destination,
        &mut node,
        MAX_OPPORTUNISTIC_LXMF_CARRIER + 2,
    );

    node.set_can_initiate_link(false);
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::LinkCapacityBackpressured {
            id: direct_id,
            limit: 1,
        }
    );
    assert_eq!(runtime.direct_link, None);

    let short_id = match runtime
        .accept(&mut access, lxmf_message_candidate(node.destination, 62))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("later opportunistic candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::PreparationBarrier { id, .. } if id == short_id
    ));
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Preparation { id, .. } if id == short_id
    ));

    node.set_can_initiate_link(true);
    let RuntimeStep::LinkEstablishment {
        offer: retry_offer, ..
    } = drive(&mut runtime, &mut access, &mut node)
    else {
        panic!("restored native admission must create a fresh exact offer")
    };
    assert_eq!(retry_offer.id(), direct_id);
    assert_ne!(retry_offer.generation(), first_offer.generation());
}

#[test]
fn active_link_preserves_wire_is_reused_and_resource_message_stays_preparing() {
    let mut node = TestNode::new();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(49), 7).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let first_carrier_len = MAX_OPPORTUNISTIC_LXMF_CARRIER + 1;
    let (first_id, offer) = accept_lxmf_and_emit_link(
        &mut runtime,
        &mut access,
        node.destination,
        &mut node,
        first_carrier_len,
    );
    let link = LinkHandle::new([0x94; 16]);
    node.retain_link(link, LinkState::Pending);
    runtime.attach_created_link(offer, link).unwrap();
    runtime
        .acknowledge_link_request_dispatched(offer, link, MonotonicMillis::new(100_000))
        .unwrap();
    node.set_link_state(link, LinkState::Active);
    node.force_direct_lxmf_preparation(SubmissionPreparationObservation::Rejected(
        SubmitError::PayloadTooLarge {
            actual: 16 + first_carrier_len,
            maximum: reticulum_node_core::MAX_DATA_PAYLOAD,
        },
    ));

    assert!(matches!(
        try_drive_at(&mut runtime, &mut access, &mut node, 100_001).unwrap(),
        RuntimeStep::Preparation {
            id,
            progress: ProjectionProgress::NoAction,
        } if id == first_id
    ));
    assert_eq!(node.direct_lxmf_prepare_calls, 1);
    assert_eq!(node.opportunistic_lxmf_prepare_calls, 0);
    assert_eq!(
        node.last_direct_lxmf_wire,
        Some(expected_lxmf_wire(node.destination, first_carrier_len))
    );
    assert_eq!(
        runtime.index().get(first_id).unwrap().state(),
        LifecycleState::Preparing,
        "Link MDU overflow must remain available for future Resource delivery"
    );
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Idle,
        "Resource-waiting work must not spin on direct preparation"
    );

    let second_carrier_len = 64;
    let second_id = match runtime
        .accept(
            &mut access,
            lxmf_message_candidate(node.destination, second_carrier_len),
        )
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("later LXMF candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::PreparationBarrier { id, .. } if id == second_id
    ));
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    node.force_direct_lxmf_preparation(SubmissionPreparationObservation::Rejected(
        SubmitError::LinkNotFound,
    ));
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Preparation {
            id,
            progress: ProjectionProgress::NoAction,
        } if id == second_id
    ));
    assert_eq!(
        node.last_direct_lxmf_wire,
        Some(expected_lxmf_wire(node.destination, second_carrier_len))
    );
    assert_eq!(node.direct_lxmf_prepare_calls, 2);
    assert_eq!(
        node.opportunistic_lxmf_prepare_calls, 0,
        "an active matching product Link must win before opportunistic delivery"
    );

    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Preparation {
            id,
            progress: ProjectionProgress::NoAction,
        } if id == second_id
    ));
    assert_eq!(
        node.opportunistic_lxmf_prepare_calls, 1,
        "a failed exact Link revalidation must evict it before the next Auto attempt"
    );
}

#[test]
fn reusable_link_registry_avoids_head_of_line_blocking_and_retains_stale_entries() {
    let mut node = TestNode::new();
    let first_destination = node.destination;
    let second_destination = DestinationHash::new([0xa2; 16]);
    let third_destination = DestinationHash::new([0xa3; 16]);
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<8, 8, 2>::mount(&mut access, SubmissionId::new(52), 8).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );

    let first_carrier = MAX_OPPORTUNISTIC_LXMF_CARRIER + 1;
    let (_first_id, first_offer) = accept_lxmf_and_emit_link(
        &mut runtime,
        &mut access,
        first_destination,
        &mut node,
        first_carrier,
    );
    let first_link = LinkHandle::new([0xa1; 16]);
    node.retain_link(first_link, LinkState::Pending);
    runtime
        .attach_created_link(first_offer, first_link)
        .unwrap();
    runtime
        .acknowledge_link_request_dispatched(first_offer, first_link, MonotonicMillis::new(100_000))
        .unwrap();
    node.set_link_state(first_link, LinkState::Active);
    node.force_direct_lxmf_preparation(SubmissionPreparationObservation::Rejected(
        SubmitError::PayloadTooLarge {
            actual: 16 + first_carrier,
            maximum: reticulum_node_core::MAX_DATA_PAYLOAD,
        },
    ));
    let _ = drive(&mut runtime, &mut access, &mut node);

    let second_carrier = MAX_OPPORTUNISTIC_LXMF_CARRIER + 2;
    let (_second_id, second_offer) = accept_lxmf_and_emit_link(
        &mut runtime,
        &mut access,
        second_destination,
        &mut node,
        second_carrier,
    );
    assert_eq!(second_offer.destination(), second_destination);
    let second_link = LinkHandle::new([0xa2; 16]);
    node.retain_link(second_link, LinkState::Pending);
    runtime
        .attach_created_link(second_offer, second_link)
        .unwrap();
    runtime
        .acknowledge_link_request_dispatched(
            second_offer,
            second_link,
            MonotonicMillis::new(101_000),
        )
        .unwrap();
    node.set_link_state(second_link, LinkState::Active);
    node.force_direct_lxmf_preparation(SubmissionPreparationObservation::Rejected(
        SubmitError::PayloadTooLarge {
            actual: 16 + second_carrier,
            maximum: reticulum_node_core::MAX_DATA_PAYLOAD,
        },
    ));
    let _ = drive(&mut runtime, &mut access, &mut node);

    let reuse_carrier = 60;
    let reuse_id = match runtime
        .accept(
            &mut access,
            lxmf_message_candidate(first_destination, reuse_carrier),
        )
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("same-destination reuse candidate did not accept: {other:?}"),
    };
    let _ = drive(&mut runtime, &mut access, &mut node);
    let _ = drive(&mut runtime, &mut access, &mut node);
    node.force_direct_lxmf_preparation(SubmissionPreparationObservation::Rejected(
        SubmitError::PayloadTooLarge {
            actual: 16 + reuse_carrier,
            maximum: reticulum_node_core::MAX_DATA_PAYLOAD,
        },
    ));
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Preparation { id, .. } if id == reuse_id
    ));
    assert_eq!(
        node.last_direct_link,
        Some(first_link),
        "the second destination must not overwrite the first live reusable Link"
    );

    let third_carrier = MAX_OPPORTUNISTIC_LXMF_CARRIER + 3;
    let third_id = match runtime
        .accept(
            &mut access,
            lxmf_message_candidate(third_destination, third_carrier),
        )
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("capacity probe candidate did not accept: {other:?}"),
    };
    let _ = drive(&mut runtime, &mut access, &mut node);
    let _ = drive(&mut runtime, &mut access, &mut node);
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::LinkCapacityBackpressured {
            id: third_id,
            limit: 2,
        }
    );
    assert_eq!(
        runtime.index().get(third_id).unwrap().state(),
        LifecycleState::Preparing,
        "registry pressure must not terminally reject durable work"
    );

    let later_id = match runtime
        .accept(&mut access, lxmf_message_candidate(first_destination, 61))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("later reusable-Link candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::PreparationBarrier { id, .. } if id == later_id
    ));
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Preparation { id, .. } if id == later_id
    ));
    assert_eq!(
        node.last_direct_link,
        Some(first_link),
        "a capacity-blocked earlier destination must not starve reusable later work"
    );

    node.set_link_state(first_link, LinkState::Stale);
    assert!(
        runtime
            .reusable_direct_links
            .iter()
            .flatten()
            .any(|candidate| candidate.link == first_link),
        "a revivable stale Link must retain its destination correlation"
    );
    node.set_link_state(first_link, LinkState::Closed);
    let RuntimeStep::LinkEstablishment { offer, .. } = drive(&mut runtime, &mut access, &mut node)
    else {
        panic!("a closed registry entry must be pruned before the next direct offer")
    };
    assert_eq!(offer.id(), third_id);
    assert_eq!(offer.destination(), third_destination);
    assert_eq!(
        node.link_state(second_link),
        Some(LinkState::Active),
        "pruning one closed destination must retain the other live Link"
    );
}

#[test]
fn full_registry_wait_resumes_when_its_matching_link_becomes_usable() {
    let mut node = TestNode::new();
    let destination = node.destination;
    let link = LinkHandle::new([0xb2; 16]);
    node.retain_link(link, LinkState::Active);
    node.set_link_usable(link, false);
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 4, 1>::mount(&mut access, SubmissionId::new(58), 8).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    runtime.reusable_direct_links[0] = Some(ReusableDirectLink { destination, link });

    let id = match runtime
        .accept(
            &mut access,
            lxmf_message_candidate(destination, MAX_OPPORTUNISTIC_LXMF_CARRIER + 3),
        )
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("direct-required candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::PreparationBarrier { id: observed, .. } if observed == id
    ));
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::LinkCapacityBackpressured { id, limit: 1 }
    );

    node.set_link_usable(link, true);
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Preparation {
            id: observed,
            progress: ProjectionProgress::NoAction,
        } if observed == id
    ));
    assert_eq!(node.last_direct_link, Some(link));
    assert_eq!(
        runtime.reusable_direct_links[0],
        Some(ReusableDirectLink { destination, link }),
        "reactivation must reuse the retained destination correlation"
    );
}

#[test]
fn establishing_link_that_becomes_stale_is_cached_for_later_revival() {
    let mut node = TestNode::new();
    let destination = node.destination;
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 4, 1>::mount(&mut access, SubmissionId::new(59), 8).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let (id, offer) = accept_lxmf_and_emit_link(
        &mut runtime,
        &mut access,
        destination,
        &mut node,
        MAX_OPPORTUNISTIC_LXMF_CARRIER + 4,
    );
    let link = LinkHandle::new([0xb3; 16]);
    node.retain_link(link, LinkState::Pending);
    runtime.attach_created_link(offer, link).unwrap();
    runtime
        .acknowledge_link_request_dispatched(offer, link, MonotonicMillis::new(100_000))
        .unwrap();
    node.set_link_state(link, LinkState::Stale);

    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::LinkEstablishmentLost {
            offer,
            link,
            state: Some(LinkState::Stale),
        }
    );
    assert_eq!(runtime.direct_link, None);
    assert_eq!(
        runtime.reusable_direct_links[0],
        Some(ReusableDirectLink { destination, link }),
        "Stale must retain exact destination correlation"
    );
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::LinkCapacityBackpressured { id, limit: 1 },
        "the retained Stale Link must occupy its cache slot without blocking other work"
    );

    node.set_link_state(link, LinkState::Active);
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Preparation {
            id: observed,
            progress: ProjectionProgress::NoAction,
        } if observed == id
    ));
    assert_eq!(node.last_direct_link, Some(link));
}

#[test]
fn newly_active_link_on_an_ineligible_interface_is_cached_without_direct_preparation() {
    let mut node = TestNode::new();
    let destination = node.destination;
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 4, 2>::mount(&mut access, SubmissionId::new(61), 8).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let (id, offer) = accept_lxmf_and_emit_link(
        &mut runtime,
        &mut access,
        destination,
        &mut node,
        MAX_OPPORTUNISTIC_LXMF_CARRIER + 4,
    );
    let link = LinkHandle::new([0xb4; 16]);
    node.retain_link(link, LinkState::Pending);
    runtime.attach_created_link(offer, link).unwrap();
    runtime
        .acknowledge_link_request_dispatched(offer, link, MonotonicMillis::new(100_000))
        .unwrap();
    node.set_link_state(link, LinkState::Active);
    node.set_link_usable(link, false);

    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::LinkEstablishmentLost {
            offer,
            link,
            state: Some(LinkState::Active),
        }
    );
    assert_eq!(node.direct_lxmf_prepare_calls, 0);
    assert_eq!(
        runtime.reusable_direct_links[0],
        Some(ReusableDirectLink { destination, link }),
        "the live Link remains correlated for later interface recovery"
    );

    let RuntimeStep::LinkEstablishment {
        offer: replacement, ..
    } = drive(&mut runtime, &mut access, &mut node)
    else {
        panic!("a second registry/native slot must permit another routed Link attempt")
    };
    assert_eq!(replacement.id(), id);
    assert_ne!(replacement.generation(), offer.generation());
}

#[test]
fn active_link_on_an_ineligible_interface_is_retained_but_not_selected() {
    let mut node = TestNode::new();
    let destination = node.destination;
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 4, 2>::mount(&mut access, SubmissionId::new(54), 8).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );

    let carrier = MAX_OPPORTUNISTIC_LXMF_CARRIER + 1;
    let (_first_id, offer) =
        accept_lxmf_and_emit_link(&mut runtime, &mut access, destination, &mut node, carrier);
    let link = LinkHandle::new([0xb1; 16]);
    node.retain_link(link, LinkState::Pending);
    runtime.attach_created_link(offer, link).unwrap();
    runtime
        .acknowledge_link_request_dispatched(offer, link, MonotonicMillis::new(100_000))
        .unwrap();
    node.set_link_state(link, LinkState::Active);
    node.force_direct_lxmf_preparation(SubmissionPreparationObservation::Rejected(
        SubmitError::PayloadTooLarge {
            actual: 16 + carrier,
            maximum: reticulum_node_core::MAX_DATA_PAYLOAD,
        },
    ));
    let _ = drive(&mut runtime, &mut access, &mut node);
    let direct_calls = node.direct_lxmf_prepare_calls;

    node.set_link_usable(link, false);
    let short_id = match runtime
        .accept(&mut access, lxmf_message_candidate(destination, 64))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("short fallback candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::PreparationBarrier { id, .. } if id == short_id
    ));
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Preparation { id, .. } if id == short_id
    ));
    assert_eq!(node.direct_lxmf_prepare_calls, direct_calls);
    assert_eq!(
        node.opportunistic_lxmf_prepare_calls, 1,
        "Auto must bypass an active Link whose bound interface is offline"
    );
    assert!(
        runtime
            .reusable_direct_links
            .iter()
            .flatten()
            .any(|candidate| candidate.link == link),
        "an offline-interface Link remains correlated for possible later reuse"
    );
}

#[test]
fn unknown_destination_requests_a_path_twice_then_prepares_after_announce_learning() {
    let mut node = TestNode::new();
    node.force_unknown_preparations(3);
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(50), 8).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let id = match runtime
        .accept(&mut access, candidate(node.destination))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("fresh candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        try_drive_at(&mut runtime, &mut access, &mut node, 100_000).unwrap(),
        RuntimeStep::PreparationBarrier { .. }
    ));
    assert_eq!(
        try_drive_at(&mut runtime, &mut access, &mut node, 100_000).unwrap(),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    let RuntimeStep::PathDiscoveryRequest {
        offer: first_offer,
        progress: ProjectionProgress::NoAction,
    } = try_drive_at(&mut runtime, &mut access, &mut node, 100_000).unwrap()
    else {
        panic!("unknown destination must produce its first exact path offer")
    };
    assert_eq!(first_offer.id(), id);
    assert_eq!(first_offer.destination(), node.destination);
    assert_eq!(first_offer.ordinal(), 1);
    assert_eq!(node.prepare_calls, 1);
    assert_eq!(
        try_drive_at(&mut runtime, &mut access, &mut node, 200_000).unwrap(),
        RuntimeStep::Idle,
        "an undispatched offer must not start or exhaust discovery clocks"
    );
    runtime
        .acknowledge_path_request_dispatched(first_offer, MonotonicMillis::new(100_000))
        .unwrap();
    assert_eq!(
        try_drive_at(&mut runtime, &mut access, &mut node, 106_999).unwrap(),
        RuntimeStep::Idle
    );
    assert!(matches!(
        try_drive_at(&mut runtime, &mut access, &mut node, 107_000).unwrap(),
        RuntimeStep::Preparation {
            id: observed,
            progress: ProjectionProgress::NoAction,
        } if observed == id
    ));
    assert_eq!(node.prepare_calls, 2);
    assert_eq!(
        try_drive_at(&mut runtime, &mut access, &mut node, 120_999).unwrap(),
        RuntimeStep::Idle
    );
    let RuntimeStep::PathDiscoveryRequest {
        offer: second_offer,
        progress: ProjectionProgress::NoAction,
    } = try_drive_at(&mut runtime, &mut access, &mut node, 121_000).unwrap()
    else {
        panic!("the shared discovery must produce its bounded retry offer")
    };
    assert_eq!(second_offer.id(), id);
    assert_eq!(second_offer.destination(), node.destination);
    assert_eq!(second_offer.ordinal(), 2);
    assert_eq!(node.prepare_calls, 3);
    runtime
        .acknowledge_path_request_dispatched(second_offer, MonotonicMillis::new(121_000))
        .unwrap();
    assert_eq!(
        try_drive_at(&mut runtime, &mut access, &mut node, 127_999).unwrap(),
        RuntimeStep::Idle
    );
    assert!(matches!(
        try_drive_at(&mut runtime, &mut access, &mut node, 128_000).unwrap(),
        RuntimeStep::Preparation {
            id: observed,
            progress: ProjectionProgress::AttemptBound,
        } if observed == id
    ));
    assert_eq!(node.prepare_calls, 4);
    assert_eq!(
        runtime.index().get(id).unwrap().state(),
        LifecycleState::Preparing
    );
}

#[test]
fn path_discovery_exhaustion_commits_terminal_no_path() {
    let mut node = TestNode::new();
    node.force_unknown_preparations(4);
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(55), 9).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let id = match runtime
        .accept(&mut access, candidate(node.destination))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("fresh candidate did not accept: {other:?}"),
    };
    let _ = try_drive_at(&mut runtime, &mut access, &mut node, 100_000).unwrap();
    let _ = try_drive_at(&mut runtime, &mut access, &mut node, 100_000).unwrap();
    let RuntimeStep::PathDiscoveryRequest {
        offer: first_offer, ..
    } = try_drive_at(&mut runtime, &mut access, &mut node, 100_000).unwrap()
    else {
        panic!("first path offer must be produced")
    };
    assert_eq!(first_offer.ordinal(), 1);
    runtime
        .acknowledge_path_request_dispatched(first_offer, MonotonicMillis::new(100_000))
        .unwrap();
    let _ = try_drive_at(&mut runtime, &mut access, &mut node, 107_000).unwrap();
    let RuntimeStep::PathDiscoveryRequest {
        offer: second_offer,
        ..
    } = try_drive_at(&mut runtime, &mut access, &mut node, 121_000).unwrap()
    else {
        panic!("second path offer must be produced")
    };
    assert_eq!(second_offer.ordinal(), 2);
    runtime
        .acknowledge_path_request_dispatched(second_offer, MonotonicMillis::new(121_000))
        .unwrap();
    assert!(matches!(
        try_drive_at(&mut runtime, &mut access, &mut node, 128_000).unwrap(),
        RuntimeStep::Preparation {
            id: observed,
            progress: ProjectionProgress::Persist(_),
        } if observed == id
    ));
    assert_eq!(
        try_drive_at(&mut runtime, &mut access, &mut node, 128_000).unwrap(),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert_eq!(
        runtime.index().get(id).unwrap().state(),
        LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::NoPath))
    );
}

#[test]
fn path_discovery_is_shared_per_destination_and_acknowledged_exactly() {
    let node = TestNode::new();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(58), 10).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let first_id = SubmissionId::new(58);
    let second_id = SubmissionId::new(59);
    let unknown = SubmissionPreparationObservation::Rejected(SubmitError::UnknownDestination);

    let (first_observation, first_offer) =
        runtime.classify_path_discovery(first_id, node.destination, unknown, 100_000);
    assert_eq!(
        first_observation,
        SubmissionPreparationObservation::RetrySameBoot
    );
    let first_offer = first_offer.expect("first destination miss must own one offer");
    assert_eq!(first_offer.ordinal(), 1);

    let (second_observation, duplicate_offer) =
        runtime.classify_path_discovery(second_id, node.destination, unknown, 100_000);
    assert_eq!(
        second_observation,
        SubmissionPreparationObservation::RetrySameBoot
    );
    assert_eq!(duplicate_offer, None);

    let mismatched = PathDiscoveryOffer {
        id: second_id,
        destination: node.destination,
        ordinal: 1,
    };
    assert_eq!(
        runtime.acknowledge_path_request_dispatched(mismatched, MonotonicMillis::new(100_000)),
        Err(PathDiscoveryAcknowledgeError::OfferMismatch)
    );
    runtime
        .acknowledge_path_request_dispatched(first_offer, MonotonicMillis::new(100_000))
        .unwrap();
    assert!(!runtime.path_discovery_due(node.destination, 106_999));
    assert!(runtime.path_discovery_due(node.destination, 107_000));

    let (_, early_retry) =
        runtime.classify_path_discovery(second_id, node.destination, unknown, 107_000);
    assert_eq!(early_retry, None);
    assert!(!runtime.path_discovery_due(node.destination, 120_999));
    assert!(runtime.path_discovery_due(node.destination, 121_000));
    let (_, shared_retry) =
        runtime.classify_path_discovery(second_id, node.destination, unknown, 121_000);
    let shared_retry = shared_retry.expect("one shared retry must become due");
    assert_eq!(shared_retry.id(), second_id);
    assert_eq!(shared_retry.ordinal(), 2);
}

#[test]
fn authorized_frame_is_retained_while_the_actor_reconciles_a_lost_write_reply() {
    let mut node = TestNode::new();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(60), 8).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let _id = match runtime
        .accept(&mut access, candidate(node.destination))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("fresh candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::PreparationBarrier { .. }
    ));
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Preparation { .. }
    ));

    let frame = node.expose_frame_and_timeout();
    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Retain)
    );
    access.backend_mut().lose_write_reply_after(1);
    assert_eq!(
        try_drive(&mut runtime, &mut access, &mut node),
        Err(RuntimeError::Storage(DriveError::Backend(
            FakeError::Injected
        )))
    );
    assert_eq!(
        runtime.storage().pending_kind(),
        Some(reticulum_storage_actor::PendingKind::Projector)
    );

    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Retain)
    );
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Pending(PendingProgress::ProjectorCommitted)
    );
    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Durable)
    );
}

#[test]
fn permanent_frame_persistence_failure_never_unlocks_durability_or_acknowledgement() {
    let mut node = TestNode::new();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(65), 8).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let id = match runtime
        .accept(&mut access, candidate(node.destination))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("fresh candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::PreparationBarrier { .. }
    ));
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Preparation { .. }
    ));

    let frame = node.expose_frame_and_timeout();
    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Retain)
    );
    access.backend_mut().reject_writes_permanently();

    for _ in 0..3 {
        assert_eq!(
            try_drive(&mut runtime, &mut access, &mut node),
            Err(RuntimeError::Storage(DriveError::Backend(
                FakeError::Injected
            )))
        );
        assert_eq!(
            runtime.offer_authorized_frame(frame),
            Ok(FrameOfferProgress::Retain),
            "a permanently uncommitted frame must never become durable"
        );
        assert_eq!(node.acknowledge_calls, 0);
        assert_eq!(runtime.storage().pending_acknowledgements().count(), 0);
    }

    assert_eq!(
        runtime.storage().pending_kind(),
        Some(reticulum_storage_actor::PendingKind::Projector)
    );
    assert_eq!(
        runtime.index().get(id).unwrap().state(),
        LifecycleState::Preparing
    );
}

#[test]
fn authorized_frame_is_retained_behind_a_pre_frame_terminal_write() {
    let mut node = TestNode::new();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(70), 9).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let _id = match runtime
        .accept(&mut access, candidate(node.destination))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("fresh candidate did not accept: {other:?}"),
    };
    let _ = drive(&mut runtime, &mut access, &mut node);
    let _ = drive(&mut runtime, &mut access, &mut node);
    let _ = drive(&mut runtime, &mut access, &mut node);

    let frame = node.expose_frame_and_timeout();
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Terminal {
            progress: ProjectionProgress::Persist(_),
            ..
        }
    ));
    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Retain)
    );
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Durable)
    );
}

#[test]
fn authorized_frame_is_retained_until_a_recovery_acknowledgement_clears() {
    let mut node = TestNode::new();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(75), 10).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let id = match runtime
        .accept(&mut access, candidate(node.destination))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("fresh candidate did not accept: {other:?}"),
    };
    let _ = drive(&mut runtime, &mut access, &mut node);
    let _ = drive(&mut runtime, &mut access, &mut node);
    let _ = drive(&mut runtime, &mut access, &mut node);

    let (frame, recovered) = node.expose_frame_and_recover();
    let RuntimeStep::Recovered {
        observation,
        progress: ProjectionProgress::Persist(_),
    } = drive(&mut runtime, &mut access, &mut node)
    else {
        panic!("recovery observation must plan its durable audit")
    };
    assert_eq!(observation, recovered);
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert_eq!(
        runtime.index().get(id).unwrap().state(),
        LifecycleState::Preparing
    );
    assert_eq!(runtime.storage().pending_kind(), None);
    assert_eq!(
        runtime.storage().projector().pending_persistence().count(),
        0
    );
    let recovery_action = runtime
        .storage()
        .pending_acknowledgements()
        .next()
        .expect("durable recovery audit must unlock its exact acknowledgement");
    assert_eq!(
        recovery_action.kind(),
        AcknowledgementKind::Recovered(recovered)
    );

    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Retain)
    );
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Acknowledgement {
            action: recovery_action,
            reply: AcknowledgementReply::Completed,
        }
    );
    assert_eq!(runtime.storage().pending_acknowledgements().count(), 0);

    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Retain)
    );
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Durable)
    );
}

#[test]
fn frame_offer_retry_classification_excludes_permanent_projector_errors() {
    assert!(frame_offer_must_retry(ProjectorOperationError::Busy {
        pending: reticulum_storage_actor::PendingKind::Acceptance,
    }));
    assert!(frame_offer_must_retry(ProjectorOperationError::Rejected(
        ProjectorError::WritePending,
    )));
    assert!(frame_offer_must_retry(ProjectorOperationError::Rejected(
        ProjectorError::AcknowledgementPending,
    )));
    assert!(!frame_offer_must_retry(ProjectorOperationError::Rejected(
        ProjectorError::UnknownSubmission
    )));
    assert!(!frame_offer_must_retry(ProjectorOperationError::Rejected(
        ProjectorError::PreparationBarrierNotDurable
    )));
    assert!(!frame_offer_must_retry(ProjectorOperationError::Faulted(
        reticulum_storage_actor::StorageFault::ReservationInvariant,
    )));
}

#[test]
fn remount_boot_recovery_preserves_a_durable_final_submission() {
    let mut node = TestNode::new();
    let mut access = formatted_access();
    let expected_authorization = candidate(node.destination).authorization();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(80), 11).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let id = match runtime
        .accept(&mut access, candidate(node.destination))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("fresh candidate did not accept: {other:?}"),
    };
    let _ = drive(&mut runtime, &mut access, &mut node);
    let _ = drive(&mut runtime, &mut access, &mut node);
    let _ = drive(&mut runtime, &mut access, &mut node);
    let frame = node.expose_frame_and_timeout();
    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Retain)
    );
    let _ = drive(&mut runtime, &mut access, &mut node);
    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Durable)
    );
    let _ = drive(&mut runtime, &mut access, &mut node);
    let _ = drive(&mut runtime, &mut access, &mut node);
    let _ = drive(&mut runtime, &mut access, &mut node);

    {
        let storage = runtime.into_storage();
        assert_eq!(storage.binding(), binding());
    }
    let mut remounted =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(80), 12).unwrap();
    assert_eq!(
        remounted.recover_boot_step(&mut access),
        Ok(RecoveryStep::Submission {
            id,
            progress: BootRecoveryProgress::AlreadyFinal,
        })
    );
    assert_eq!(
        remounted.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    assert!(matches!(
        remounted.index().get(id).unwrap().state(),
        LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::DeliveryTimeout))
    ));
    assert_eq!(
        remounted
            .index()
            .get(id)
            .unwrap()
            .accepted()
            .authorization(),
        expected_authorization
    );
}

#[test]
fn wrong_binding_fails_before_node_preparation_or_acknowledgement() {
    let mut node = TestNode::new();
    let mut access = formatted_access();
    let mut wrong_access = BoundJournal::new(
        FakeNor::formatted(),
        JournalBinding::new(
            StorageDeviceId::new([0x52; 16]),
            binding().absolute_offset(),
            binding().length(),
            binding().layout_version(),
        ),
    );
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(120), 17).unwrap();
    assert!(matches!(
        runtime.recover_boot_step(&mut wrong_access),
        Err(RuntimeError::Storage(DriveError::Binding(_)))
    ));
    assert_eq!(runtime.phase(), RuntimePhase::Recovering);
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let id = match runtime
        .accept(&mut access, candidate(node.destination))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("fresh candidate did not accept: {other:?}"),
    };

    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::PreparationBarrier { id: observed, .. } if observed == id
    ));
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );

    assert!(matches!(
        try_drive(&mut runtime, &mut wrong_access, &mut node),
        Err(RuntimeError::Storage(DriveError::Binding(_)))
    ));
    assert_eq!(node.prepare_calls, 0);
    assert_eq!(node.acknowledge_calls, 0);

    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Preparation { id: observed, .. } if observed == id
    ));
    assert_eq!(node.prepare_calls, 1);
    let frame = node.expose_frame_and_timeout();
    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Retain)
    );
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Terminal { .. }
    ));
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert_eq!(runtime.storage().pending_acknowledgements().count(), 1);
    assert_eq!(node.acknowledge_calls, 0);

    assert!(matches!(
        try_drive(&mut runtime, &mut wrong_access, &mut node),
        Err(RuntimeError::Storage(DriveError::Binding(_)))
    ));
    assert_eq!(runtime.storage().pending_acknowledgements().count(), 1);
    assert_eq!(node.acknowledge_calls, 0);

    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Acknowledgement {
            reply: AcknowledgementReply::Completed,
            ..
        }
    ));
    assert_eq!(node.acknowledge_calls, 1);
}
