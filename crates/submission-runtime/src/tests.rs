extern crate std;

use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
    check_erase, check_read, check_write,
};
use reticulum_node_core::{
    AcknowledgeError, AttemptOutcome, AttemptUnsentReason, AuthorizedFrameObservation,
    InboundProofPolicy, InterfaceSet, MAX_DIRECT_LXMF_WIRE, NodeConfig, NodeCore, NodeIdentity,
    NodeInstanceId, PacketInterfaceId, PermitResolution, PrepareDataRequest, RoutedTxJob,
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
    peer: TestNodeCore,
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
    integrated_direct_preparation: bool,
    has_usable_path: bool,
    removed_paths: Vec<DestinationHash>,
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
            peer: receiver_node,
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
            integrated_direct_preparation: false,
            has_usable_path: true,
            removed_paths: Vec::new(),
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

    fn rollback_queued_attempt(&mut self, now_ms: u64) {
        let job = self
            .job
            .take()
            .expect("the test must own one queued attempt");
        match self
            .core
            .rollback_queued(job, MonotonicMillis::new(now_ms))
            .unwrap_or_else(|failure| panic!("rollback failed: {:?}", failure.reason()))
        {
            TxCompletionDisposition::Available(buffer) => self.buffer = Some(buffer),
            TxCompletionDisposition::Next(_) => {
                panic!("single-interface job unexpectedly fanned out")
            }
            TxCompletionDisposition::Recovered { .. } => {
                panic!("pre-deadline queued job unexpectedly required recovery")
            }
            TxCompletionDisposition::Quarantined(_) => {
                panic!("ordinary queued rollback unexpectedly quarantined")
            }
        }
    }

    fn recover_expired_queued_attempt(
        &mut self,
        deadline_ms: u64,
        completed_at_ms: u64,
    ) -> TxRecoveryObservation {
        let job = self
            .job
            .take()
            .expect("the test must own one queued attempt");
        assert_eq!(
            self.core
                .maintain_tx(MonotonicMillis::new(deadline_ms))
                .newly_recovery_required,
            1
        );
        match self
            .core
            .rollback_queued(job, MonotonicMillis::new(completed_at_ms))
            .unwrap_or_else(|failure| panic!("recovery rollback failed: {:?}", failure.reason()))
        {
            TxCompletionDisposition::Recovered {
                buffer,
                observation,
            } => {
                self.buffer = Some(buffer);
                self.recovered = Some(observation);
                observation
            }
            TxCompletionDisposition::Available(_) => {
                panic!("expired queued job unexpectedly bypassed recovery")
            }
            TxCompletionDisposition::Next(_) => {
                panic!("single-interface job unexpectedly fanned out")
            }
            TxCompletionDisposition::Quarantined(_) => {
                panic!("ordinary queued recovery unexpectedly quarantined")
            }
        }
    }

    fn enable_integrated_direct_preparation(&mut self) -> (LinkHandle, Vec<u8>) {
        self.core
            .register_inbound_single_destination("lxmf", &["delivery"])
            .unwrap();
        let destination = self
            .peer
            .register_inbound_single_destination("lxmf", &["delivery"])
            .unwrap();
        self.core
            .register_peer(
                &identity(0x42),
                "lxmf",
                &["delivery"],
                MonotonicSeconds::new(89),
            )
            .unwrap();
        self.peer
            .set_destination_accepts_links(&destination, true)
            .unwrap();
        self.peer
            .set_destination_inbound_proof_policy(&destination, InboundProofPolicy::Always)
            .unwrap();

        let mut rng = CounterRng::default();
        let (request, link) = self
            .core
            .initiate_link(&destination, MonotonicSeconds::new(90), &mut rng)
            .unwrap();
        let response = self
            .peer
            .ingest(
                request.packets[0].bytes(),
                MonotonicSeconds::new(90),
                PacketInterfaceId::new(2),
                &mut rng,
            )
            .unwrap();
        let established = self
            .core
            .ingest(
                response.actions.packets[0].bytes(),
                MonotonicSeconds::new(91),
                PacketInterfaceId::new(1),
                &mut rng,
            )
            .unwrap();
        self.peer
            .ingest(
                established.actions.packets[0].bytes(),
                MonotonicSeconds::new(92),
                PacketInterfaceId::new(2),
                &mut rng,
            )
            .unwrap();
        assert_eq!(self.core.link_state(link), Some(LinkState::Active));

        let mut storage = [0_u8; MAX_DIRECT_LXMF_WIRE];
        let content = [b'P'; 295];
        let prepared = self
            .core
            .prepare_basic_direct_lxmf_into(
                &destination,
                1_700_000_000_000,
                b"link refresh",
                &content,
                &mut storage,
            )
            .unwrap();
        let wire = storage[..usize::from(prepared.wire_len())].to_vec();
        assert!(
            wire.len() - 16 > MAX_OPPORTUNISTIC_LXMF_CARRIER,
            "the regression requires a direct-only LXMF carrier"
        );

        self.destination = destination;
        self.retain_link(link, LinkState::Active);
        self.integrated_direct_preparation = true;
        (link, wire)
    }

    fn establish_additional_direct_link(&mut self) -> LinkHandle {
        let mut rng = CounterRng(0xa0);
        let (request, link) = self
            .core
            .initiate_link(&self.destination, MonotonicSeconds::new(190), &mut rng)
            .unwrap();
        let response = self
            .peer
            .ingest(
                request.packets[0].bytes(),
                MonotonicSeconds::new(190),
                PacketInterfaceId::new(2),
                &mut rng,
            )
            .unwrap();
        let established = self
            .core
            .ingest(
                response.actions.packets[0].bytes(),
                MonotonicSeconds::new(191),
                PacketInterfaceId::new(1),
                &mut rng,
            )
            .unwrap();
        self.peer
            .ingest(
                established.actions.packets[0].bytes(),
                MonotonicSeconds::new(192),
                PacketInterfaceId::new(2),
                &mut rng,
            )
            .unwrap();
        assert_eq!(self.core.link_state(link), Some(LinkState::Active));
        self.retain_link(link, LinkState::Active);
        link
    }

    fn direct_lxmf_wire(&self, timestamp_ms: u64, title: &[u8], content: u8) -> Vec<u8> {
        let mut storage = [0_u8; MAX_DIRECT_LXMF_WIRE];
        let content = [content; 295];
        let prepared = self
            .core
            .prepare_basic_direct_lxmf_into(
                &self.destination,
                timestamp_ms,
                title,
                &content,
                &mut storage,
            )
            .unwrap();
        let wire = storage[..usize::from(prepared.wire_len())].to_vec();
        assert!(
            wire.len() - 16 > MAX_OPPORTUNISTIC_LXMF_CARRIER,
            "the regression requires a direct-only LXMF carrier"
        );
        wire
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

    fn expose_frame_and_deliver(&mut self) -> AuthorizedFrameObservation {
        let job = self.job.take().expect("one prepared job must be retained");
        let round = self.direct_lxmf_prepare_calls as u64;
        let owner_base = 100_000 + round * 100;
        let protocol_now = MonotonicSeconds::new(100 + round);
        let requirements = TxPermitRequirements::try_new(TEST_PERMIT_RESOURCE, 1).unwrap();
        let (pending, request) = job.begin_permit(requirements);
        let reply = self
            .core
            .authorize_tx(
                request,
                MonotonicMillis::new(owner_base + 10),
                &mut AllowPolicy,
            )
            .unwrap_or_else(|_| panic!("fresh permit request must authorize"));
        let resolution = pending
            .resolve(reply, MonotonicMillis::new(owner_base + 11))
            .unwrap_or_else(|_| panic!("matching permit reply must resolve"));
        let PermitResolution::Authorized(mut authorized) = resolution else {
            panic!("allow policy must authorize the fresh packet")
        };
        let (observation, packet) = {
            let frame = authorized
                .frame(MonotonicMillis::new(owner_base + 12))
                .unwrap();
            (frame.observation(), frame.bytes().to_vec())
        };
        let disposition = self
            .core
            .complete_tx(
                authorized.complete(TxCompletionCode::new(0)),
                MonotonicMillis::new(owner_base + 13),
            )
            .unwrap_or_else(|_| panic!("matching completion must return"));
        let TxCompletionDisposition::Available(buffer) = disposition else {
            panic!("successful direct completion must return the buffer")
        };
        self.buffer = Some(buffer);

        let received = self
            .peer
            .ingest(
                &packet,
                protocol_now,
                PacketInterfaceId::new(2),
                &mut CounterRng::default(),
            )
            .unwrap_or_else(|failure| panic!("peer must accept direct Link DATA: {failure:?}"));
        assert_eq!(received.metadata.generated_proof_actions(), 1);
        let proof = received
            .actions
            .packets
            .first()
            .expect("direct Link DATA must generate one proof")
            .bytes()
            .to_vec();
        let delivered = self
            .core
            .ingest(
                &proof,
                protocol_now,
                PacketInterfaceId::new(1),
                &mut CounterRng::default(),
            )
            .unwrap_or_else(|failure| panic!("sender must correlate direct proof: {failure:?}"));
        assert_eq!(delivered.metadata.delivered_receipt_terminals(), 1);
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
        rng: &mut R,
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
        if let Some(forced) = self.forced_direct_lxmf_preparation.take() {
            return forced;
        }
        if !self.integrated_direct_preparation {
            return SubmissionPreparationObservation::RetrySameBoot;
        }

        let buffer = self
            .buffer
            .take()
            .expect("test node has one available owner");
        match self.core.prepare_rehydrated_direct_lxmf_into_slot(
            buffer,
            link,
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
                        .unwrap_or_else(|_| panic!("direct preparation rejection must recycle")),
                );
                SubmissionPreparationObservation::Rejected(reason)
            }
        }
    }

    fn has_usable_path(&self, _destination: &DestinationHash) -> bool {
        self.has_usable_path
    }

    fn remove_retained_path(&mut self, destination: &DestinationHash) -> bool {
        self.has_usable_path = false;
        self.removed_paths.push(*destination);
        self.core.remove_retained_path(destination)
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

    fn link_has_unacknowledged_attempt(&self, link: LinkHandle) -> bool {
        self.core.link_has_unacknowledged_attempt(link)
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

fn exact_lxmf_message_candidate(wire: &[u8], idempotency_tag: u8) -> AcceptanceCandidate {
    AcceptanceCandidate::new(
        PrincipalId::new([0x21; 16]),
        IdempotencyKey::new([idempotency_tag; 16]),
        LxmfMessageIntent::new(wire).unwrap(),
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
    assert!(
        node.core.has_path(&node.destination),
        "the timed-out attempt starts with one retained route"
    );

    let RuntimeStep::Terminal { terminal, progress } = drive(&mut runtime, &mut access, &mut node)
    else {
        panic!("terminal must be observed after the frame is durable")
    };
    assert_eq!(terminal.outcome(), AttemptOutcome::DeliveryTimeout);
    assert!(matches!(progress, ProjectionProgress::Persist(_)));
    assert_eq!(node.removed_paths, [node.destination]);
    assert!(
        !node.core.has_path(&node.destination),
        "a non-Link timeout must invalidate its stale route before retry"
    );
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
fn direct_link_single_flight_waits_through_terminal_ack_then_reuses_same_link() {
    let mut node = TestNode::new();
    let (link, first_wire) = node.enable_integrated_direct_preparation();
    let second_wire = node.direct_lxmf_wire(1_700_000_000_001, b"second delivery", b'Q');
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 4, 2>::mount(&mut access, SubmissionId::new(63), 9).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );

    let first_id = match runtime
        .accept(&mut access, exact_lxmf_message_candidate(&first_wire, 0x71))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("first direct candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::PreparationBarrier { id, .. } if id == first_id
    ));
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    let RuntimeStep::LinkEstablishment { offer, .. } = drive(&mut runtime, &mut access, &mut node)
    else {
        panic!("the first direct-only message must establish one Link")
    };
    runtime.attach_created_link(offer, link).unwrap();
    runtime
        .acknowledge_link_request_dispatched(offer, link, MonotonicMillis::new(100_000))
        .unwrap();
    assert!(matches!(
        try_drive_at(&mut runtime, &mut access, &mut node, 100_001).unwrap(),
        RuntimeStep::Preparation {
            id,
            progress: ProjectionProgress::AttemptBound,
        } if id == first_id
    ));
    assert!(node.core.link_has_unacknowledged_attempt(link));

    let second_id = match runtime
        .accept(
            &mut access,
            exact_lxmf_message_candidate(&second_wire, 0x72),
        )
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("second direct candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::PreparationBarrier { id, .. } if id == second_id
    ));
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::DirectLinkAttemptBackpressured {
            id: second_id,
            link,
        },
        "a free registry slot must not create a second same-destination Link"
    );
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Idle,
        "the busy submission must remain parked without spinning"
    );
    assert_eq!(node.direct_lxmf_prepare_calls, 1);
    assert_eq!(runtime.direct_link, None);

    let unrelated_destination = DestinationHash::new([0xc7; 16]);
    let unrelated_link = LinkHandle::new([0xc8; 16]);
    node.retain_link(unrelated_link, LinkState::Active);
    runtime.reusable_direct_links[1] = Some(ReusableDirectLink {
        destination: unrelated_destination,
        link: unrelated_link,
    });
    let unrelated_id = match runtime
        .accept(
            &mut access,
            lxmf_message_candidate(unrelated_destination, 64),
        )
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("unrelated Link candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::PreparationBarrier { id, .. } if id == unrelated_id
    ));
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    node.force_direct_lxmf_preparation(SubmissionPreparationObservation::Rejected(
        SubmitError::PayloadTooLarge {
            actual: 16 + 64,
            maximum: reticulum_node_core::MAX_DATA_PAYLOAD,
        },
    ));
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Preparation {
            id,
            progress: ProjectionProgress::NoAction,
        } if id == unrelated_id
    ));
    assert_eq!(
        node.last_direct_link,
        Some(unrelated_link),
        "a busy destination must not head-of-line block another reusable Link"
    );

    let opportunistic_id = match runtime
        .accept(&mut access, lxmf_message_candidate(node.destination, 63))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("opportunistic candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::PreparationBarrier { id, .. } if id == opportunistic_id
    ));
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Preparation {
            id,
            progress: ProjectionProgress::NoAction,
        } if id == opportunistic_id
    ));
    assert_eq!(
        node.opportunistic_lxmf_prepare_calls, 1,
        "an eligible opportunistic message must bypass a busy cached Link"
    );
    assert_eq!(
        node.direct_lxmf_prepare_calls, 2,
        "opportunistic fallback must not add another direct attempt"
    );

    let first_frame = node.expose_frame_and_deliver();
    assert_eq!(
        runtime.offer_authorized_frame(first_frame),
        Ok(FrameOfferProgress::Durable)
    );
    let RuntimeStep::Terminal {
        terminal: first_terminal,
        progress: first_terminal_progress,
    } = drive(&mut runtime, &mut access, &mut node)
    else {
        panic!("the first direct proof must produce a terminal")
    };
    assert_eq!(first_terminal.outcome(), AttemptOutcome::Delivered);
    assert_eq!(first_terminal.link(), Some(link));
    assert!(matches!(
        first_terminal_progress,
        ProjectionProgress::Persist(_)
    ));
    assert!(
        node.core.link_has_unacknowledged_attempt(link),
        "a durable terminal must keep the Link busy until exact acknowledgement"
    );
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Acknowledgement {
            reply: AcknowledgementReply::Completed,
            ..
        }
    ));
    assert!(!node.core.link_has_unacknowledged_attempt(link));

    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Preparation {
            id,
            progress: ProjectionProgress::AttemptBound,
        } if id == second_id
    ));
    assert_eq!(node.last_direct_link, Some(link));
    assert_eq!(node.direct_lxmf_prepare_calls, 3);

    let second_frame = node.expose_frame_and_deliver();
    assert_eq!(
        runtime.offer_authorized_frame(second_frame),
        Ok(FrameOfferProgress::Durable)
    );
    let RuntimeStep::Terminal {
        terminal: second_terminal,
        progress: second_terminal_progress,
    } = drive(&mut runtime, &mut access, &mut node)
    else {
        panic!("the second direct proof must produce a terminal")
    };
    assert_eq!(second_terminal.outcome(), AttemptOutcome::Delivered);
    assert_eq!(second_terminal.link(), Some(link));
    assert!(matches!(
        second_terminal_progress,
        ProjectionProgress::Persist(_)
    ));
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Acknowledgement {
            reply: AcknowledgementReply::Completed,
            ..
        }
    ));
    assert!(matches!(
        runtime.index().get(first_id).unwrap().state(),
        LifecycleState::Final(FinalDisposition::Delivered(_))
    ));
    assert!(matches!(
        runtime.index().get(second_id).unwrap().state(),
        LifecycleState::Final(FinalDisposition::Delivered(_))
    ));
    assert_eq!(node.link_state(link), Some(LinkState::Active));
}

#[test]
fn direct_delivery_timeout_is_durable_before_reusable_link_retirement() {
    exercise_direct_delivery_timeout_retirement();
}

#[test]
fn timed_out_lxmf_retries_the_same_durable_wire_after_backoff_without_a_client() {
    let mut node = TestNode::new();
    let (link, wire) = node.enable_integrated_direct_preparation();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2, 2>::mount(&mut access, SubmissionId::new(49), 7).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let id = match runtime
        .accept(&mut access, exact_lxmf_message_candidate(&wire, 0x60))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("fresh direct candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::PreparationBarrier { id: observed, .. } if observed == id
    ));
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    let RuntimeStep::LinkEstablishment { offer, .. } = drive(&mut runtime, &mut access, &mut node)
    else {
        panic!("direct-only candidate must establish its first Link")
    };
    runtime.attach_created_link(offer, link).unwrap();
    runtime
        .acknowledge_link_request_dispatched(offer, link, MonotonicMillis::new(100_000))
        .unwrap();
    assert!(matches!(
        try_drive_at(&mut runtime, &mut access, &mut node, 100_001).unwrap(),
        RuntimeStep::Preparation {
            id: observed,
            progress: ProjectionProgress::AttemptBound,
        } if observed == id
    ));
    let first_attempt = node.job.as_ref().unwrap().prepared().attempt();
    assert_eq!(node.last_direct_lxmf_wire.as_deref(), Some(wire.as_slice()));

    let frame = node.expose_frame_and_timeout();
    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Durable)
    );
    assert!(matches!(
        try_drive_at(&mut runtime, &mut access, &mut node, 100_100).unwrap(),
        RuntimeStep::Terminal {
            progress: ProjectionProgress::RetryableAttemptTerminal,
            ..
        }
    ));
    assert_eq!(
        runtime.index().get(id).unwrap().state(),
        LifecycleState::Preparing
    );
    assert!(matches!(
        try_drive_at(&mut runtime, &mut access, &mut node, 100_101).unwrap(),
        RuntimeStep::DirectLinkDeliveryTimedOut { link: retired, .. } if retired == link
    ));
    let close = node.core.close_link(link, &mut CounterRng::default());
    assert_eq!(close.packets.len(), 1);
    node.set_link_state(link, LinkState::Closed);
    assert!(matches!(
        try_drive_at(&mut runtime, &mut access, &mut node, 100_102).unwrap(),
        RuntimeStep::Acknowledgement {
            reply: AcknowledgementReply::Completed,
            ..
        }
    ));

    let slot = runtime.submission_slot(id).unwrap();
    let retry_at = runtime.lxmf_delivery_loops[slot]
        .retry_not_before_ms
        .expect("receipt timeout must arm board-owned retry");
    assert_eq!(
        try_drive_at(&mut runtime, &mut access, &mut node, retry_at - 1).unwrap(),
        RuntimeStep::Idle
    );
    let RuntimeStep::LinkEstablishment {
        offer: retry_offer, ..
    } = try_drive_at(&mut runtime, &mut access, &mut node, retry_at).unwrap()
    else {
        panic!("the board must retry its durable LXMF obligation when due")
    };
    assert_eq!(retry_offer.id(), id);
    let retry_link = node.establish_additional_direct_link();
    assert_ne!(retry_link, link);
    runtime
        .attach_created_link(retry_offer, retry_link)
        .unwrap();
    runtime
        .acknowledge_link_request_dispatched(
            retry_offer,
            retry_link,
            MonotonicMillis::new(retry_at + 1),
        )
        .unwrap();
    assert!(matches!(
        try_drive_at(&mut runtime, &mut access, &mut node, retry_at + 2).unwrap(),
        RuntimeStep::Preparation {
            id: observed,
            progress: ProjectionProgress::AttemptBound,
        } if observed == id
    ));
    let second_attempt = node.job.as_ref().unwrap().prepared().attempt();
    assert_ne!(second_attempt, first_attempt);
    assert_eq!(node.last_direct_lxmf_wire.as_deref(), Some(wire.as_slice()));
    assert_eq!(runtime.active_lxmf_retry_submission, Some(id));
    assert_eq!(runtime.lxmf_delivery_loops[slot].attempts_started, 2);
}

#[test]
fn reboot_restores_pending_lxmf_and_arms_board_owned_retry() {
    let mut node = TestNode::new();
    let (_link, wire) = node.enable_integrated_direct_preparation();
    let mut access = formatted_access();
    let mut initial =
        SubmissionRuntime::<4, 2, 2>::mount(&mut access, SubmissionId::new(48), 7).unwrap();
    assert_eq!(
        initial.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let id = match initial
        .accept(&mut access, exact_lxmf_message_candidate(&wire, 0x5f))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("fresh direct candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        drive(&mut initial, &mut access, &mut node),
        RuntimeStep::PreparationBarrier { id: observed, .. } if observed == id
    ));
    assert_eq!(
        drive(&mut initial, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert_eq!(
        initial.index().get(id).unwrap().state(),
        LifecycleState::Preparing
    );
    let mut recovered =
        SubmissionRuntime::<4, 2, 2>::mount(&mut access, SubmissionId::new(48), 8).unwrap();
    assert_eq!(
        recovered.recover_boot_step(&mut access),
        Ok(RecoveryStep::Submission {
            id,
            progress: BootRecoveryProgress::ReplayPendingLxmf,
        })
    );
    assert_eq!(
        recovered.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    assert_eq!(
        try_drive_at(&mut recovered, &mut access, &mut node, 200_000).unwrap(),
        RuntimeStep::Idle,
        "the first live turn establishes a conservative boot-relative delay"
    );
    let slot = recovered.submission_slot(id).unwrap();
    let retry_at = recovered.lxmf_delivery_loops[slot]
        .retry_not_before_ms
        .expect("boot recovery must arm a retry deadline");
    let RuntimeStep::LinkEstablishment { offer, .. } =
        try_drive_at(&mut recovered, &mut access, &mut node, retry_at).unwrap()
    else {
        panic!("replayed LXMF must resume without any client connection")
    };
    assert_eq!(offer.id(), id);
}

#[test]
fn automatic_lxmf_retries_are_single_flight_and_path_learning_wakes_backoff() {
    let node = TestNode::new();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(47), 7).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let first = match runtime
        .accept(&mut access, lxmf_message_candidate(node.destination, 40))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("first candidate did not accept: {other:?}"),
    };
    let second = match runtime
        .accept(&mut access, lxmf_message_candidate(node.destination, 41))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("second candidate did not accept: {other:?}"),
    };
    for id in [first, second] {
        assert!(matches!(
            runtime.storage.begin_preparation(id).unwrap(),
            ProjectionProgress::Persist(_)
        ));
        let request = runtime
            .storage
            .projector()
            .pending_persistence()
            .next()
            .unwrap();
        assert_eq!(
            runtime.storage.persist_projector(&mut access, request),
            Ok(PersistenceProgress::Committed)
        );
    }
    let first_slot = runtime.submission_slot(first).unwrap();
    let second_slot = runtime.submission_slot(second).unwrap();
    for slot in [first_slot, second_slot] {
        runtime.lxmf_delivery_loops[slot].attempts_started = 1;
        runtime.lxmf_delivery_loops[slot].retry_pending = true;
        runtime.lxmf_delivery_loops[slot].retry_not_before_ms = Some(0);
    }

    let (selected_slot, selected, _, _) = runtime
        .select_ready_submission(&node, 100)
        .expect("one due retry must be selected");
    assert_eq!((selected_slot, selected), (first_slot, first));
    runtime.note_lxmf_attempt_started(selected_slot, selected);
    // The helper call above models the scheduler edge without allocating a
    // node-core packet; park that synthetic active row as a real binding would.
    runtime.direct_link_waiting[first_slot] = DirectLinkWaitReason::MatchingLinkBusy;
    assert_eq!(runtime.active_lxmf_retry_submission, Some(first));
    assert_eq!(
        runtime.select_ready_submission(&node, 101),
        None,
        "a second automatic retry must wait for the first terminal acknowledgement"
    );
    runtime.note_lxmf_terminal_acknowledged(first, AttemptOutcome::DeliveryTimeout);
    assert_eq!(runtime.active_lxmf_retry_submission, None);
    assert!(matches!(
        runtime.select_ready_submission(&node, 102),
        Some((slot, id, _, _)) if slot == second_slot && id == second
    ));

    let delivery = &mut runtime.lxmf_delivery_loops[second_slot];
    delivery.retry_pending = true;
    delivery.retry_not_before_ms = Some(1_000_000);
    delivery.arm_after_boot = false;
    delivery.last_path_usable = false;
    assert!(!runtime.lxmf_retry_is_eligible(second_slot, second, 200, false));
    assert!(
        runtime.lxmf_retry_is_eligible(second_slot, second, 201, true),
        "an exact destination becoming reachable must advance its retry"
    );
}

#[test]
fn transient_interface_loss_keeps_lxmf_pending_and_retries_on_the_board() {
    let mut node = TestNode::new();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(50), 7).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );

    let id = match runtime
        .accept(&mut access, lxmf_message_candidate(node.destination, 46))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        try_drive_at(&mut runtime, &mut access, &mut node, 100).unwrap(),
        RuntimeStep::PreparationBarrier { id: observed, .. } if observed == id
    ));
    assert_eq!(
        try_drive_at(&mut runtime, &mut access, &mut node, 100).unwrap(),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );

    node.force_opportunistic_lxmf_preparation(SubmissionPreparationObservation::Rejected(
        SubmitError::NoEligibleInterface {
            target: reticulum_node_core::TxTarget::All,
        },
    ));
    assert_eq!(
        try_drive_at(&mut runtime, &mut access, &mut node, 100).unwrap(),
        RuntimeStep::Preparation {
            id,
            progress: ProjectionProgress::NoAction,
        }
    );
    assert_eq!(
        runtime.index().get(id).unwrap().state(),
        LifecycleState::Preparing,
        "temporary interface loss must not permanently fail a durable message"
    );
    let slot = runtime.submission_slot(id).unwrap();
    let retry_at = runtime.lxmf_delivery_loops[slot]
        .retry_not_before_ms
        .expect("interface loss must arm the autonomous retry scheduler");
    assert_eq!(
        try_drive_at(&mut runtime, &mut access, &mut node, retry_at - 1).unwrap(),
        RuntimeStep::Idle
    );

    node.force_opportunistic_lxmf_preparation(SubmissionPreparationObservation::Rejected(
        SubmitError::NoEligibleInterface {
            target: reticulum_node_core::TxTarget::All,
        },
    ));
    assert_eq!(
        try_drive_at(&mut runtime, &mut access, &mut node, retry_at).unwrap(),
        RuntimeStep::Preparation {
            id,
            progress: ProjectionProgress::NoAction,
        }
    );
    assert!(
        runtime.lxmf_delivery_loops[slot]
            .retry_not_before_ms
            .is_some_and(|next_retry| next_retry > retry_at),
        "a still-offline interface must rearm another bounded retry"
    );
}

#[test]
fn definitely_unsent_lxmf_attempt_recycles_into_the_board_retry_loop() {
    let mut node = TestNode::new();
    let mut wire = vec![0x6d; 56];
    wire[..16].copy_from_slice(node.destination.as_bytes());
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(53), 7).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );

    let id = match runtime
        .accept(&mut access, exact_lxmf_message_candidate(&wire, 0x53))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        try_drive_at(&mut runtime, &mut access, &mut node, 100_000).unwrap(),
        RuntimeStep::PreparationBarrier { id: observed, .. } if observed == id
    ));
    assert_eq!(
        try_drive_at(&mut runtime, &mut access, &mut node, 100_000).unwrap(),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );

    let first_observation = node.prepare_submission(
        SubmissionPrepareRequest {
            destination: node.destination,
            plaintext: &wire,
            rns_now: MonotonicSeconds::new(100),
            owner_now: MonotonicMillis::new(100_000),
            deadline: TxLeaseDeadline::new(MonotonicMillis::new(200_000)),
        },
        &mut CounterRng(0x20),
    );
    let SubmissionPreparationObservation::Prepared(first_prepared) = first_observation else {
        panic!("test node must prepare the first exact attempt")
    };
    node.force_opportunistic_lxmf_preparation(first_observation);
    assert_eq!(
        try_drive_at(&mut runtime, &mut access, &mut node, 100_000).unwrap(),
        RuntimeStep::Preparation {
            id,
            progress: ProjectionProgress::AttemptBound,
        }
    );

    node.rollback_queued_attempt(100_100);
    let first_terminal = node.core.terminal_attempts().next().unwrap();
    assert_eq!(
        first_terminal.outcome(),
        AttemptOutcome::Unsent(AttemptUnsentReason::QueueRollback)
    );
    assert_eq!(
        try_drive_at(&mut runtime, &mut access, &mut node, 100_100).unwrap(),
        RuntimeStep::Terminal {
            terminal: first_terminal,
            progress: ProjectionProgress::RetryableAttemptTerminal,
        }
    );
    assert_eq!(
        runtime.index().get(id).unwrap().state(),
        LifecycleState::Preparing
    );
    let slot = runtime.submission_slot(id).unwrap();
    let retry_at = runtime.lxmf_delivery_loops[slot]
        .retry_not_before_ms
        .expect("definitely-unsent terminal must schedule a board retry");
    assert!(matches!(
        try_drive_at(&mut runtime, &mut access, &mut node, 100_101).unwrap(),
        RuntimeStep::Acknowledgement {
            reply: AcknowledgementReply::Completed,
            ..
        }
    ));
    assert_eq!(
        try_drive_at(&mut runtime, &mut access, &mut node, retry_at - 1).unwrap(),
        RuntimeStep::Idle
    );

    let second_observation = node.prepare_submission(
        SubmissionPrepareRequest {
            destination: node.destination,
            plaintext: &wire,
            rns_now: MonotonicSeconds::new(retry_at / 1_000),
            owner_now: MonotonicMillis::new(retry_at),
            deadline: TxLeaseDeadline::new(MonotonicMillis::new(retry_at.saturating_add(100_000))),
        },
        &mut CounterRng(0x80),
    );
    let SubmissionPreparationObservation::Prepared(second_prepared) = second_observation else {
        panic!("test node must prepare the replacement carrier attempt")
    };
    node.force_opportunistic_lxmf_preparation(second_observation);
    assert_eq!(
        try_drive_at(&mut runtime, &mut access, &mut node, retry_at).unwrap(),
        RuntimeStep::Preparation {
            id,
            progress: ProjectionProgress::AttemptBound,
        }
    );
    assert_ne!(first_prepared.attempt(), second_prepared.attempt());
    assert_eq!(runtime.active_lxmf_retry_submission, Some(id));
}

#[test]
fn recovery_backed_unsent_lxmf_releases_both_owners_before_recycling() {
    let mut node = TestNode::new();
    let mut wire = vec![0x6e; 57];
    wire[..16].copy_from_slice(node.destination.as_bytes());
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(54), 7).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );

    let id = match runtime
        .accept(&mut access, exact_lxmf_message_candidate(&wire, 0x54))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        try_drive_at(&mut runtime, &mut access, &mut node, 100_000).unwrap(),
        RuntimeStep::PreparationBarrier { id: observed, .. } if observed == id
    ));
    assert_eq!(
        try_drive_at(&mut runtime, &mut access, &mut node, 100_000).unwrap(),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    let prepared = node.prepare_submission(
        SubmissionPrepareRequest {
            destination: node.destination,
            plaintext: &wire,
            rns_now: MonotonicSeconds::new(100),
            owner_now: MonotonicMillis::new(100_000),
            deadline: TxLeaseDeadline::new(MonotonicMillis::new(200_000)),
        },
        &mut CounterRng(0x30),
    );
    assert!(matches!(
        prepared,
        SubmissionPreparationObservation::Prepared(_)
    ));
    node.force_opportunistic_lxmf_preparation(prepared);
    assert!(matches!(
        try_drive_at(&mut runtime, &mut access, &mut node, 100_000).unwrap(),
        RuntimeStep::Preparation {
            id: observed,
            progress: ProjectionProgress::AttemptBound,
        } if observed == id
    ));

    let recovered = node.recover_expired_queued_attempt(200_000, 200_001);
    let terminal = node.core.terminal_attempts().next().unwrap();
    assert_eq!(
        terminal.outcome(),
        AttemptOutcome::Unsent(AttemptUnsentReason::PermitDeadlineExpired)
    );
    assert!(matches!(
        try_drive_at(&mut runtime, &mut access, &mut node, 200_001).unwrap(),
        RuntimeStep::Recovered {
            observation,
            progress: ProjectionProgress::RecoveryDurablyCovered,
        } if observation == recovered
    ));
    assert!(matches!(
        try_drive_at(&mut runtime, &mut access, &mut node, 200_001).unwrap(),
        RuntimeStep::Acknowledgement {
            action,
            reply: AcknowledgementReply::Completed,
        } if action.kind() == AcknowledgementKind::Recovered(recovered)
    ));
    assert!(matches!(
        try_drive_at(&mut runtime, &mut access, &mut node, 200_001).unwrap(),
        RuntimeStep::Terminal {
            terminal: observed,
            progress: ProjectionProgress::RetryableAttemptTerminal,
        } if observed == terminal
    ));
    let slot = runtime.submission_slot(id).unwrap();
    assert!(runtime.lxmf_delivery_loops[slot].retry_pending);
    assert!(matches!(
        try_drive_at(&mut runtime, &mut access, &mut node, 200_001).unwrap(),
        RuntimeStep::Acknowledgement {
            action,
            reply: AcknowledgementReply::Completed,
        } if action.kind() == AcknowledgementKind::Terminal(terminal)
    ));
    assert_eq!(
        runtime.index().get(id).unwrap().state(),
        LifecycleState::Preparing
    );
    assert!(
        runtime
            .storage()
            .projector()
            .preparation_allowed(runtime.index(), id)
    );
}

#[test]
fn a_shared_path_offer_cannot_consume_an_lxmf_path_wakeup() {
    let node = TestNode::new();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(52), 7).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );

    let id = match runtime
        .accept(&mut access, lxmf_message_candidate(node.destination, 47))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        runtime.storage.begin_preparation(id).unwrap(),
        ProjectionProgress::Persist(_)
    ));
    let request = runtime
        .storage
        .projector()
        .pending_persistence()
        .next()
        .unwrap();
    assert_eq!(
        runtime.storage.persist_projector(&mut access, request),
        Ok(PersistenceProgress::Committed)
    );
    let slot = runtime.submission_slot(id).unwrap();
    runtime.lxmf_delivery_loops[slot].retry_pending = true;
    runtime.lxmf_delivery_loops[slot].retry_not_before_ms = Some(1_000_000);
    runtime.lxmf_delivery_loops[slot].last_path_usable = false;

    let (_, offer) = runtime.classify_path_discovery(
        id,
        node.destination,
        SubmissionPreparationObservation::Rejected(SubmitError::UnknownDestination),
        100,
    );
    assert!(
        offer.is_some(),
        "the shared path request must remain undispatched"
    );
    assert_eq!(runtime.select_ready_submission(&node, 101), None);
    assert!(
        !runtime.lxmf_delivery_loops[slot].last_path_usable,
        "a later selection gate must not consume the false-to-true path edge"
    );

    runtime.clear_path_discovery(node.destination);
    assert!(matches!(
        runtime.select_ready_submission(&node, 102),
        Some((selected_slot, selected, _, _)) if selected_slot == slot && selected == id
    ));
}

#[test]
fn same_boot_preparation_pressure_cannot_consume_an_lxmf_path_wakeup() {
    let node = TestNode::new();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(55), 7).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );

    let id = match runtime
        .accept(&mut access, lxmf_message_candidate(node.destination, 49))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        runtime.storage.begin_preparation(id).unwrap(),
        ProjectionProgress::Persist(_)
    ));
    let request = runtime
        .storage
        .projector()
        .pending_persistence()
        .next()
        .unwrap();
    assert_eq!(
        runtime.storage.persist_projector(&mut access, request),
        Ok(PersistenceProgress::Committed)
    );
    let slot = runtime.submission_slot(id).unwrap();
    runtime.lxmf_delivery_loops[slot].retry_pending = true;
    runtime.lxmf_delivery_loops[slot].retry_not_before_ms = Some(1_000_000);
    runtime.lxmf_delivery_loops[slot].last_path_usable = false;

    assert!(matches!(
        runtime.select_ready_submission(&node, 101),
        Some((selected_slot, selected, _, _)) if selected_slot == slot && selected == id
    ));
    assert_eq!(
        runtime
            .project_preparation(id, SubmissionPreparationObservation::RetrySameBoot)
            .unwrap(),
        ProjectionProgress::NoAction
    );
    assert!(
        !runtime.lxmf_delivery_loops[slot].last_path_usable,
        "only a bound carrier attempt may consume the exact path edge"
    );
    assert!(matches!(
        runtime.select_ready_submission(&node, 102),
        Some((selected_slot, selected, _, _)) if selected_slot == slot && selected == id
    ));
}

#[test]
fn newly_accepted_work_crosses_its_barrier_before_a_due_background_retry() {
    let mut node = TestNode::new();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(49), 7).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );

    let retrying = match runtime
        .accept(&mut access, lxmf_message_candidate(node.destination, 42))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("retry candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        runtime.storage.begin_preparation(retrying).unwrap(),
        ProjectionProgress::Persist(_)
    ));
    let request = runtime
        .storage
        .projector()
        .pending_persistence()
        .next()
        .unwrap();
    assert_eq!(
        runtime.storage.persist_projector(&mut access, request),
        Ok(PersistenceProgress::Committed)
    );
    let retrying_slot = runtime.submission_slot(retrying).unwrap();
    runtime.lxmf_delivery_loops[retrying_slot].attempts_started = 1;
    runtime.lxmf_delivery_loops[retrying_slot].retry_pending = true;
    runtime.lxmf_delivery_loops[retrying_slot].retry_not_before_ms = Some(0);

    let fresh = match runtime
        .accept(&mut access, lxmf_message_candidate(node.destination, 43))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("fresh candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        try_drive_at(&mut runtime, &mut access, &mut node, 100).unwrap(),
        RuntimeStep::PreparationBarrier { id, .. } if id == fresh
    ));
    assert_eq!(runtime.active_lxmf_retry_submission, None);
}

#[test]
fn a_finalized_retry_owner_cannot_strand_the_global_retry_gate() {
    let mut node = TestNode::new();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(51), 7).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );

    let mut ids = [SubmissionId::new(0); 2];
    for (index, tag) in [44, 45].into_iter().enumerate() {
        let id = match runtime
            .accept(&mut access, lxmf_message_candidate(node.destination, tag))
            .unwrap()
        {
            AcceptanceProgress::Accepted(id) => id,
            other => panic!("candidate did not accept: {other:?}"),
        };
        ids[index] = id;
        assert!(matches!(
            runtime.storage.begin_preparation(id).unwrap(),
            ProjectionProgress::Persist(_)
        ));
        let request = runtime
            .storage
            .projector()
            .pending_persistence()
            .next()
            .unwrap();
        assert_eq!(
            runtime.storage.persist_projector(&mut access, request),
            Ok(PersistenceProgress::Committed)
        );
        let slot = runtime.submission_slot(id).unwrap();
        runtime.lxmf_delivery_loops[slot].attempts_started = 1;
        runtime.lxmf_delivery_loops[slot].retry_pending = true;
        runtime.lxmf_delivery_loops[slot].retry_not_before_ms = Some(0);
    }

    runtime.active_lxmf_retry_submission = Some(ids[0]);
    assert!(matches!(
        runtime
            .storage
            .observe_preparation(ids[0], SubmissionPreparationObservation::InternalFailure)
            .unwrap(),
        ProjectionProgress::Persist(_)
    ));
    assert_eq!(
        try_drive_at(&mut runtime, &mut access, &mut node, 100).unwrap(),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert!(runtime.index().get(ids[0]).unwrap().state().is_final());

    assert!(runtime.storage.ready_intent(ids[1]).is_some());
    let resumed = try_drive_at(&mut runtime, &mut access, &mut node, 101).unwrap();
    assert!(
        matches!(resumed, RuntimeStep::Preparation { id, .. } if id == ids[1]),
        "the next retry did not resume after finalization: {resumed:?}"
    );
    assert_eq!(runtime.active_lxmf_retry_submission, None);
}

fn exercise_direct_delivery_timeout_retirement() {
    let mut node = TestNode::new();
    let (link, wire) = node.enable_integrated_direct_preparation();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2, 2>::mount(&mut access, SubmissionId::new(50), 7).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );

    let first_id = match runtime
        .accept(&mut access, exact_lxmf_message_candidate(&wire, 0x61))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("fresh direct candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::PreparationBarrier { id, .. } if id == first_id
    ));
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    let RuntimeStep::LinkEstablishment { offer, .. } = drive(&mut runtime, &mut access, &mut node)
    else {
        panic!("direct-only candidate must request one Link")
    };
    runtime.attach_created_link(offer, link).unwrap();
    runtime
        .acknowledge_link_request_dispatched(offer, link, MonotonicMillis::new(100_000))
        .unwrap();

    assert!(matches!(
        try_drive_at(&mut runtime, &mut access, &mut node, 100_001).unwrap(),
        RuntimeStep::Preparation {
            id,
            progress: ProjectionProgress::AttemptBound,
        } if id == first_id
    ));
    assert_eq!(node.last_direct_link, Some(link));

    let second_id = match runtime
        .accept(&mut access, exact_lxmf_message_candidate(&wire, 0x62))
        .unwrap()
    {
        AcceptanceProgress::Accepted(id) => id,
        other => panic!("follow-up direct candidate did not accept: {other:?}"),
    };
    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::PreparationBarrier { id, .. } if id == second_id
    ));
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Persistence(PersistenceProgress::Committed)
    );
    assert_eq!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::DirectLinkAttemptBackpressured {
            id: second_id,
            link,
        }
    );

    let frame = node.expose_frame_and_timeout();
    assert_eq!(
        runtime.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Durable)
    );

    let RuntimeStep::Terminal { terminal, progress } = drive(&mut runtime, &mut access, &mut node)
    else {
        panic!("a direct receipt timeout must preserve the durable delivery loop")
    };
    assert_eq!(terminal.outcome(), AttemptOutcome::DeliveryTimeout);
    assert_eq!(terminal.link(), Some(link));
    assert_eq!(progress, ProjectionProgress::RetryableAttemptTerminal);
    assert!(
        node.removed_paths.is_empty(),
        "direct Link timeout must not enter opportunistic route invalidation"
    );
    assert_eq!(
        node.link_state(link),
        Some(LinkState::Active),
        "receipt expiry is independent of native Link state"
    );
    assert!(
        runtime
            .reusable_direct_links
            .iter()
            .flatten()
            .any(|candidate| candidate.link == link),
        "retirement remains a separate product control edge"
    );
    assert!(matches!(
        runtime.direct_link_retirement,
        Some(DirectLinkRetirement::Ready {
            terminal: ready_terminal,
            link: ready_link,
        }) if ready_terminal == terminal && ready_link == link
    ));
    assert!(runtime.direct_link_retirement_is_next_step());
    assert!(
        runtime
            .reusable_direct_links
            .iter()
            .flatten()
            .any(|candidate| candidate.link == link),
        "the continuing durable loop and Link-retirement signal are separate"
    );

    let RuntimeStep::DirectLinkDeliveryTimedOut {
        terminal: durable_terminal,
        link: retired,
    } = drive(&mut runtime, &mut access, &mut node)
    else {
        panic!("durable timeout commit must release the exact Link retirement")
    };
    assert_eq!(durable_terminal, terminal);
    assert_eq!(retired, link);
    assert!(!runtime.direct_link_retirement_is_next_step());
    assert_eq!(
        runtime.index().get(first_id).unwrap().state(),
        LifecycleState::Preparing
    );
    assert!(
        runtime.reusable_direct_links.iter().all(Option::is_none),
        "the product registry must evict the timed-out session after durability"
    );

    let close = node.core.close_link(link, &mut CounterRng::default());
    assert_eq!(node.core.link_state(link), None);
    assert_eq!(close.packets.len(), 1);

    assert!(matches!(
        drive(&mut runtime, &mut access, &mut node),
        RuntimeStep::Acknowledgement {
            reply: AcknowledgementReply::Completed,
            ..
        }
    ));

    let RuntimeStep::LinkEstablishment {
        offer: replacement, ..
    } = drive(&mut runtime, &mut access, &mut node)
    else {
        panic!("follow-up work must establish a fresh Link instead of reusing the timed-out one")
    };
    assert_eq!(replacement.id(), second_id);
    assert_ne!(replacement.generation(), offer.generation());
    assert_eq!(
        node.direct_lxmf_prepare_calls, 1,
        "the still-native-Active stale session must not be selected again"
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
    node.set_has_usable_path(false);
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
    node.set_has_usable_path(true);
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
fn raw_path_discovery_exhaustion_remains_terminal_no_path() {
    let mut node = TestNode::new();
    node.set_has_usable_path(false);
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
        }
        if observed == id
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
fn shared_path_exhaustion_keeps_raw_and_lxmf_policies_per_submission() {
    for lxmf_reaches_exhaustion_first in [false, true] {
        let node = TestNode::new();
        let mut access = formatted_access();
        let mut runtime =
            SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(54), 9).unwrap();
        assert_eq!(
            runtime.recover_boot_step(&mut access),
            Ok(RecoveryStep::Complete)
        );
        let raw = match runtime
            .accept(&mut access, candidate(node.destination))
            .unwrap()
        {
            AcceptanceProgress::Accepted(id) => id,
            other => panic!("raw candidate did not accept: {other:?}"),
        };
        let lxmf = match runtime
            .accept(&mut access, lxmf_message_candidate(node.destination, 48))
            .unwrap()
        {
            AcceptanceProgress::Accepted(id) => id,
            other => panic!("LXMF candidate did not accept: {other:?}"),
        };
        for id in [raw, lxmf] {
            assert!(matches!(
                runtime.storage.begin_preparation(id).unwrap(),
                ProjectionProgress::Persist(_)
            ));
            let request = runtime
                .storage
                .projector()
                .pending_persistence()
                .next()
                .unwrap();
            assert_eq!(
                runtime.storage.persist_projector(&mut access, request),
                Ok(PersistenceProgress::Committed)
            );
        }
        runtime.path_discoveries[0] = Some(PathDiscovery {
            destination: node.destination,
            phase: PathDiscoveryPhase::Waiting {
                first_request_ms: 100_000,
                next_probe_ms: 128_000,
                requests_sent: PATH_DISCOVERY_MAX_REQUESTS,
            },
        });
        let unknown = SubmissionPreparationObservation::Rejected(SubmitError::UnknownDestination);
        let order = if lxmf_reaches_exhaustion_first {
            [lxmf, raw]
        } else {
            [raw, lxmf]
        };
        for id in order {
            let (observation, offer) =
                runtime.classify_path_discovery(id, node.destination, unknown, 128_000);
            assert_eq!(offer, None);
            assert_eq!(
                observation,
                if id == lxmf {
                    SubmissionPreparationObservation::RetrySameBoot
                } else {
                    unknown
                },
                "shared discovery must not transfer one submission's completion policy"
            );
        }
        assert!(matches!(
            runtime.path_discoveries[0],
            Some(PathDiscovery {
                phase: PathDiscoveryPhase::CycleBackoff { .. },
                ..
            })
        ));
        assert!(
            runtime.path_discovery_ready(node.destination, 128_001, false, false),
            "raw work must be allowed to observe terminal exhaustion immediately"
        );
        assert!(
            !runtime.path_discovery_ready(node.destination, 187_999, false, true),
            "LXMF must respect its longer cycle backoff"
        );
        let (observation, offer) =
            runtime.classify_path_discovery(lxmf, node.destination, unknown, 188_000);
        assert_eq!(observation, SubmissionPreparationObservation::RetrySameBoot);
        let offer = offer.expect("LXMF must begin a fresh shared discovery cycle when due");
        assert_eq!(offer.id(), lxmf);
        assert_eq!(offer.ordinal(), 1);
    }
}

#[test]
fn lxmf_path_discovery_exhaustion_retains_message_and_starts_a_later_cycle() {
    let mut node = TestNode::new();
    node.set_has_usable_path(false);
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(56), 9).unwrap();
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
    runtime
        .acknowledge_path_request_dispatched(second_offer, MonotonicMillis::new(121_000))
        .unwrap();
    let _ = try_drive_at(&mut runtime, &mut access, &mut node, 128_000).unwrap();

    assert_eq!(
        runtime.index().get(id).unwrap().state(),
        LifecycleState::Preparing
    );
    assert_eq!(
        try_drive_at(&mut runtime, &mut access, &mut node, 187_999).unwrap(),
        RuntimeStep::Idle
    );
    let RuntimeStep::PathDiscoveryRequest {
        offer: third_offer,
        progress: ProjectionProgress::NoAction,
    } = try_drive_at(&mut runtime, &mut access, &mut node, 188_000).unwrap()
    else {
        panic!("the retained LXMF message must start another discovery cycle")
    };
    assert_eq!(third_offer.id(), id);
    assert_eq!(third_offer.ordinal(), 1);
}

#[test]
fn newly_learned_path_wakes_an_exhausted_lxmf_discovery_cycle_immediately() {
    let mut node = TestNode::new();
    node.set_has_usable_path(false);
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(56), 9).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let id = match runtime
        .accept(
            &mut access,
            lxmf_message_candidate(node.destination, MAX_OPPORTUNISTIC_LXMF_CARRIER + 2),
        )
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
    runtime
        .acknowledge_path_request_dispatched(second_offer, MonotonicMillis::new(121_000))
        .unwrap();
    let _ = try_drive_at(&mut runtime, &mut access, &mut node, 128_000).unwrap();

    node.set_has_usable_path(true);
    assert!(matches!(
        try_drive_at(&mut runtime, &mut access, &mut node, 128_001).unwrap(),
        RuntimeStep::LinkEstablishment {
            offer,
            progress: ProjectionProgress::NoAction,
        } if offer.id() == id
    ));
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
fn undispatched_path_offer_retries_the_exact_ordinal_after_its_deadline() {
    let node = TestNode::new();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(59), 10).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let id = SubmissionId::new(59);
    let unknown = SubmissionPreparationObservation::Rejected(SubmitError::UnknownDestination);
    let (_, first_offer) = runtime.classify_path_discovery(id, node.destination, unknown, 100_000);
    let first_offer = first_offer.expect("the first miss must offer one path request");

    runtime
        .retry_path_request_dispatch(first_offer, MonotonicMillis::new(105_000))
        .unwrap();
    assert!(!runtime.path_discovery_due(node.destination, 104_999));
    let (_, early_offer) = runtime.classify_path_discovery(id, node.destination, unknown, 104_999);
    assert_eq!(early_offer, None);
    assert!(runtime.path_discovery_due(node.destination, 105_000));
    let (_, retried_offer) =
        runtime.classify_path_discovery(id, node.destination, unknown, 105_000);
    assert_eq!(retried_offer, Some(first_offer));
    assert_eq!(retried_offer.unwrap().ordinal(), 1);
}

#[test]
fn path_dispatch_retry_rejects_mismatches_without_releasing_the_offer() {
    let node = TestNode::new();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(60), 10).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let id = SubmissionId::new(60);
    let unknown = SubmissionPreparationObservation::Rejected(SubmitError::UnknownDestination);
    let (_, first_offer) = runtime.classify_path_discovery(id, node.destination, unknown, 100_000);
    let first_offer = first_offer.expect("the first miss must offer one path request");
    let mismatched = PathDiscoveryOffer {
        id: SubmissionId::new(61),
        ..first_offer
    };
    assert_eq!(
        runtime.retry_path_request_dispatch(mismatched, MonotonicMillis::new(105_000)),
        Err(PathDiscoveryAcknowledgeError::OfferMismatch)
    );
    let absent_destination = PathDiscoveryOffer {
        destination: DestinationHash::new([0x7a; 16]),
        ..first_offer
    };
    assert_eq!(
        runtime.retry_path_request_dispatch(absent_destination, MonotonicMillis::new(105_000)),
        Err(PathDiscoveryAcknowledgeError::DestinationNotPending)
    );

    runtime
        .acknowledge_path_request_dispatched(first_offer, MonotonicMillis::new(100_000))
        .expect("mismatches must leave the original exact offer pending");
}

#[test]
fn untransmitted_retry_then_completed_transmission_consumes_one_path_attempt() {
    let node = TestNode::new();
    let mut access = formatted_access();
    let mut runtime =
        SubmissionRuntime::<4, 2>::mount(&mut access, SubmissionId::new(62), 10).unwrap();
    assert_eq!(
        runtime.recover_boot_step(&mut access),
        Ok(RecoveryStep::Complete)
    );
    let id = SubmissionId::new(62);
    let unknown = SubmissionPreparationObservation::Rejected(SubmitError::UnknownDestination);
    let (_, first_offer) = runtime.classify_path_discovery(id, node.destination, unknown, 100_000);
    let first_offer = first_offer.expect("the first miss must offer one path request");
    runtime
        .retry_path_request_dispatch(first_offer, MonotonicMillis::new(105_000))
        .unwrap();
    let (_, retried_offer) =
        runtime.classify_path_discovery(id, node.destination, unknown, 105_000);
    let retried_offer = retried_offer.expect("the exact undispatched request must return");
    assert_eq!(retried_offer, first_offer);
    runtime
        .acknowledge_path_request_dispatched(retried_offer, MonotonicMillis::new(105_000))
        .unwrap();

    assert!(!runtime.path_discovery_due(node.destination, 111_999));
    assert!(runtime.path_discovery_due(node.destination, 112_000));
    let (_, response_timeout_offer) =
        runtime.classify_path_discovery(id, node.destination, unknown, 112_000);
    assert_eq!(response_timeout_offer, None);
    assert!(!runtime.path_discovery_due(node.destination, 125_999));
    assert!(runtime.path_discovery_due(node.destination, 126_000));
    let (_, second_offer) = runtime.classify_path_discovery(id, node.destination, unknown, 126_000);
    let second_offer =
        second_offer.expect("one completed transmission must advance to ordinal two");
    assert_eq!(second_offer.ordinal(), 2);
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
