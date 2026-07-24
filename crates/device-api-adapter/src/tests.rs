extern crate std;

use std::{
    ops::{Deref, DerefMut},
    vec,
    vec::Vec,
};

use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
    check_erase, check_read, check_write,
};
#[cfg(any(feature = "experimental-rns-data", feature = "experimental-lxmf"))]
use reticulum_device_api::DestinationHash as ApiDestinationHash;
#[cfg(any(
    feature = "experimental-rns-data",
    all(feature = "experimental-rns-inbox", feature = "experimental-lxmf")
))]
use reticulum_device_api::IdempotencyKey as ApiIdempotencyKey;
use reticulum_device_api::{
    ApiErrorCode, ApiErrorResponse, ApiVersion, CapabilitySnapshot, DeviceRequest, DeviceResponse,
    DispatchContext, DispatchProvenance, OP_SUBMISSION_STATUS, Permissions,
    PrincipalId as ApiPrincipalId, RequestEnvelope, RequestId, SubmissionId as ApiSubmissionId,
};
use reticulum_storage_actor::{
    AcceptanceProgress, BoundJournal, DriveError, JournalBinding, StorageActor, StorageDeviceId,
};
#[cfg(feature = "experimental-rns-data")]
use reticulum_storage_actor::{PendingKind, PendingProgress};
use reticulum_storage_journal::{PHYSICAL_FORMAT_VERSION, format_erased};
use reticulum_storage_model::{
    AUTHORIZATION_PERMISSION_EXPERIMENTAL_SUBMIT_RNS_DATA, AcceptanceCandidate,
    AuthorizationSnapshot, BootRecoveryMarker, DestinationHash, EncodedPacketSha256,
    ExperimentalRnsDataIntent, FinalDisposition, IdempotencyKey, InternalFailure, InterruptedState,
    LifecycleState, PreparedPacketDetails as DurablePreparedPacketDetails, PrincipalId,
    RnsAttemptToken, SubmissionFailure as DurableSubmissionFailure, SubmissionId,
};

use super::*;

const PARTITION_SIZE: usize = 0x10_0000;
const ERASE_SIZE: usize = 0x1000;
#[cfg(feature = "experimental-rns-data")]
static OVERSIZED_PAYLOAD: [u8; api::MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES + 1] =
    [0x44; api::MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES + 1];
#[cfg(feature = "experimental-rns-data")]
static MAXIMUM_PAYLOAD: [u8; api::MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES] =
    [0x5a; api::MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FakeError {
    Bounds,
    Alignment,
    LostWriteReply,
}

impl NorFlashError for FakeError {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::Bounds => NorFlashErrorKind::OutOfBounds,
            Self::Alignment => NorFlashErrorKind::NotAligned,
            Self::LostWriteReply => NorFlashErrorKind::Other,
        }
    }
}

struct FakeNor {
    bytes: Vec<u8>,
    lost_write_reply_after: Option<usize>,
    dropped_write_after: Option<usize>,
    writes: usize,
    erases: usize,
}

impl FakeNor {
    fn formatted() -> Self {
        Self::formatted_with_write_faults(None, None)
    }

    #[cfg(feature = "experimental-rns-data")]
    fn formatted_with_lost_write_reply(lost_write_reply_after: Option<usize>) -> Self {
        Self::formatted_with_write_faults(lost_write_reply_after, None)
    }

    fn formatted_with_dropped_write(dropped_write_after: Option<usize>) -> Self {
        Self::formatted_with_write_faults(None, dropped_write_after)
    }

    fn formatted_with_write_faults(
        lost_write_reply_after: Option<usize>,
        dropped_write_after: Option<usize>,
    ) -> Self {
        let mut flash = Self {
            bytes: vec![0xff; PARTITION_SIZE],
            lost_write_reply_after: None,
            dropped_write_after: None,
            writes: 0,
            erases: 0,
        };
        format_erased(&mut flash).expect("the erased test journal formats");
        flash.lost_write_reply_after = lost_write_reply_after;
        flash.dropped_write_after = dropped_write_after;
        flash.writes = 0;
        flash.erases = 0;
        flash
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
        self.erases += 1;
        self.bytes[from as usize..to as usize].fill(0xff);
        Ok(())
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        check_write(self, offset, bytes.len()).map_err(map_check_error)?;
        self.writes += 1;
        let lose_reply = self.lost_write_reply_after == Some(0);
        if let Some(remaining) = &mut self.lost_write_reply_after
            && *remaining != 0
        {
            *remaining -= 1;
        }
        let drop_write = self.dropped_write_after == Some(0);
        if let Some(remaining) = &mut self.dropped_write_after
            && *remaining != 0
        {
            *remaining -= 1;
        }
        if drop_write {
            self.dropped_write_after = None;
        } else {
            self.program(offset as usize, bytes);
        }
        if lose_reply {
            self.lost_write_reply_after = None;
            Err(FakeError::LostWriteReply)
        } else {
            Ok(())
        }
    }
}

impl MultiwriteNorFlash for FakeNor {}

fn map_check_error(error: NorFlashErrorKind) -> FakeError {
    match error {
        NorFlashErrorKind::OutOfBounds => FakeError::Bounds,
        NorFlashErrorKind::NotAligned => FakeError::Alignment,
        _ => FakeError::LostWriteReply,
    }
}

const TEST_DEVICE: StorageDeviceId = StorageDeviceId::new([0x7a; 16]);

const fn test_binding() -> JournalBinding {
    JournalBinding::new(
        TEST_DEVICE,
        0x0063_0000,
        PARTITION_SIZE,
        PHYSICAL_FORMAT_VERSION,
    )
}

struct TestActor<const SUBMISSIONS: usize> {
    actor: StorageActor<SUBMISSIONS, 1>,
    journal: BoundJournal<FakeNor>,
    port_calls: usize,
}

impl<const SUBMISSIONS: usize> TestActor<SUBMISSIONS> {
    fn mount(flash: FakeNor, first_submission_id: SubmissionId) -> Self {
        let mut journal = BoundJournal::new(flash, test_binding());
        let actor = StorageActor::mount(&mut journal, first_submission_id).unwrap();
        Self {
            actor,
            journal,
            port_calls: 0,
        }
    }

    fn accept(
        &mut self,
        candidate: AcceptanceCandidate,
    ) -> Result<AcceptanceProgress, DriveError<FakeError>> {
        self.actor.accept(&mut self.journal, candidate)
    }

    #[cfg(feature = "experimental-rns-data")]
    fn drive_pending(&mut self) -> Result<PendingProgress, DriveError<FakeError>> {
        self.actor.drive_pending(&mut self.journal)
    }

    #[cfg(feature = "experimental-rns-data")]
    fn into_flash(self) -> FakeNor {
        self.journal.into_backend()
    }

    #[cfg(feature = "experimental-rns-data")]
    fn port_calls(&self) -> usize {
        self.port_calls
    }
}

impl<const SUBMISSIONS: usize> SubmissionPort for TestActor<SUBMISSIONS> {
    fn availability(&mut self) -> CapabilityAvailability {
        self.port_calls += 1;
        CapabilityAvailability::Available
    }

    fn submission_state(
        &mut self,
        principal: PrincipalId,
        id: SubmissionId,
    ) -> Result<Option<LifecycleState>, SubmissionPortError> {
        self.port_calls += 1;
        if self.actor.fault().is_some() {
            return Err(SubmissionPortError::Faulted);
        }
        if self.actor.pending_kind().is_some() {
            return Err(SubmissionPortError::Busy);
        }
        Ok(self.actor.index().get_owned_state(principal, id))
    }

    fn accept(
        &mut self,
        candidate: AcceptanceCandidate,
    ) -> Result<SubmissionAcceptance, SubmissionPortError> {
        self.port_calls += 1;
        self.actor
            .accept(&mut self.journal, candidate)
            .map(map_acceptance_progress)
            .map_err(map_drive_error)
    }
}

fn map_acceptance_progress(progress: AcceptanceProgress) -> SubmissionAcceptance {
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

fn map_drive_error(error: DriveError<FakeError>) -> SubmissionPortError {
    match error {
        DriveError::Backend(_) => SubmissionPortError::Backend,
        DriveError::Binding(_) => SubmissionPortError::Binding,
        DriveError::Busy { .. } => SubmissionPortError::Busy,
        DriveError::Faulted(_) => SubmissionPortError::Faulted,
    }
}

impl<const SUBMISSIONS: usize> Deref for TestActor<SUBMISSIONS> {
    type Target = StorageActor<SUBMISSIONS, 1>;

    fn deref(&self) -> &Self::Target {
        &self.actor
    }
}

impl<const SUBMISSIONS: usize> DerefMut for TestActor<SUBMISSIONS> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.actor
    }
}

fn mounted<const SUBMISSIONS: usize>() -> TestActor<SUBMISSIONS> {
    TestActor::mount(FakeNor::formatted(), SubmissionId::new(10))
}

fn identity_summary() -> api::IdentitySummary {
    api::IdentitySummary::new(api::DestinationHash([0xa5; 16]))
}

#[derive(Default)]
struct UnavailablePort {
    availability_calls: usize,
    status_calls: usize,
    acceptance_calls: usize,
}

impl SubmissionPort for UnavailablePort {
    fn availability(&mut self) -> CapabilityAvailability {
        self.availability_calls += 1;
        CapabilityAvailability::Disabled
    }

    fn submission_state(
        &mut self,
        _principal: PrincipalId,
        _id: SubmissionId,
    ) -> Result<Option<LifecycleState>, SubmissionPortError> {
        self.status_calls += 1;
        Err(SubmissionPortError::Unavailable)
    }

    fn accept(
        &mut self,
        _candidate: AcceptanceCandidate,
    ) -> Result<SubmissionAcceptance, SubmissionPortError> {
        self.acceptance_calls += 1;
        Err(SubmissionPortError::Unavailable)
    }
}

#[cfg(feature = "experimental-rns-inbox")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FakeInboxCalls {
    availability: usize,
    status: usize,
    peek: usize,
}

#[cfg(feature = "experimental-rns-inbox")]
struct FakeInbox {
    availability: CapabilityAvailability,
    status: Result<api::RnsInboxStatus, InboundMailboxPortError>,
    peek: Result<Option<InboundMailboxItem>, InboundMailboxPortError>,
    calls: FakeInboxCalls,
}

#[cfg(feature = "experimental-rns-inbox")]
impl FakeInbox {
    fn available(peek: Option<InboundMailboxItem>) -> Self {
        Self {
            availability: CapabilityAvailability::Available,
            status: Ok(api::RnsInboxStatus {
                depth: u16::from(peek.is_some()),
                capacity: 8,
                dropped_since_boot: 3,
                max_payload_bytes: api::MAX_RNS_INBOX_PAYLOAD_BYTES as u16,
                durable: true,
            }),
            peek: Ok(peek),
            calls: FakeInboxCalls {
                availability: 0,
                status: 0,
                peek: 0,
            },
        }
    }

    fn calls(&self) -> FakeInboxCalls {
        self.calls
    }
}

#[cfg(feature = "experimental-rns-inbox")]
impl InboundMailboxPort for FakeInbox {
    fn availability(&mut self) -> CapabilityAvailability {
        self.calls.availability += 1;
        self.availability
    }

    fn status(&mut self) -> Result<api::RnsInboxStatus, InboundMailboxPortError> {
        self.calls.status += 1;
        self.status
    }

    fn peek(&mut self) -> Result<Option<InboundMailboxItem>, InboundMailboxPortError> {
        self.calls.peek += 1;
        self.peek
    }
}

#[cfg(feature = "experimental-lxmf")]
#[derive(Default)]
struct FakeLxmfOnlyPort {
    submission_availability: usize,
    lxmf_availability: usize,
    lxmf_next: usize,
    compose_availability: usize,
    peer_availability: usize,
    peer_max_app_data: usize,
    peer_next: usize,
    observed_peer_cursor: Option<Option<api::LxmfPeerDiscoveryCursor>>,
}

#[cfg(feature = "experimental-lxmf")]
impl SubmissionPort for FakeLxmfOnlyPort {
    fn availability(&mut self) -> CapabilityAvailability {
        self.submission_availability += 1;
        CapabilityAvailability::Available
    }

    fn submission_state(
        &mut self,
        _principal: PrincipalId,
        _id: SubmissionId,
    ) -> Result<Option<LifecycleState>, SubmissionPortError> {
        Ok(None)
    }

    fn accept(
        &mut self,
        _candidate: AcceptanceCandidate,
    ) -> Result<SubmissionAcceptance, SubmissionPortError> {
        Err(SubmissionPortError::Unavailable)
    }
}

#[cfg(feature = "experimental-lxmf")]
impl LxmfInboxPort for FakeLxmfOnlyPort {
    fn availability(&mut self) -> CapabilityAvailability {
        self.lxmf_availability += 1;
        CapabilityAvailability::Available
    }

    fn next(
        &mut self,
        _after: Option<api::LxmfMessageHandle>,
    ) -> Result<Option<api::LxmfMessageSummary>, LxmfInboxPortError> {
        self.lxmf_next += 1;
        Ok(Some(
            api::LxmfMessageSummary::new(
                api::LxmfMessageHandle::new(7).unwrap(),
                [0x17; 32],
                ApiDestinationHash([0x27; 16]),
                ApiDestinationHash([0x37; 16]),
                1.25_f64.to_bits(),
                114,
                1,
                2,
                1,
                [0x47; 32],
            )
            .unwrap(),
        ))
    }

    fn read(
        &mut self,
        _handle: api::LxmfMessageHandle,
        _offset: u32,
        _max_bytes: api::LxmfReadLength,
    ) -> Result<Option<api::LxmfReadChunk>, LxmfInboxPortError> {
        Ok(None)
    }
}

#[cfg(feature = "experimental-lxmf")]
impl LxmfComposePort for FakeLxmfOnlyPort {
    fn availability(&mut self) -> CapabilityAvailability {
        self.compose_availability += 1;
        CapabilityAvailability::Available
    }

    fn compose_and_accept(
        &mut self,
        _request: LxmfComposeRequest<'_>,
    ) -> Result<LxmfComposeAcceptance, LxmfComposePortError> {
        Ok(LxmfComposeAcceptance::new(
            SubmissionAcceptance::Accepted(SubmissionId::new(41)),
            [0x67; 32],
        ))
    }
}

#[cfg(feature = "experimental-lxmf")]
impl PeerDiscoveryPort for FakeLxmfOnlyPort {
    fn availability(&mut self) -> CapabilityAvailability {
        self.peer_availability += 1;
        CapabilityAvailability::Available
    }

    fn max_app_data_bytes(&mut self) -> u16 {
        self.peer_max_app_data += 1;
        64
    }

    fn next(
        &mut self,
        after: Option<api::LxmfPeerDiscoveryCursor>,
    ) -> Result<api::LxmfPeerDiscoveryPage, PeerDiscoveryPortError> {
        self.peer_next += 1;
        self.observed_peer_cursor = Some(after);
        let incarnation = api::LxmfPeerDiscoveryIncarnation::new([0x88; 8]);
        let generation = api::LxmfPeerGeneration::new(3).unwrap();
        let peer = api::LxmfDiscoveredPeer::new(
            ApiDestinationHash([0x31; 16]),
            api::IdentityHash::new([0x41; 16]),
            b"nearby",
            2,
            7,
            Some(-95),
            Some(4),
            250,
            generation,
        )
        .unwrap();
        Ok(api::LxmfPeerDiscoveryPage::new(
            api::LxmfPeerDiscoveryCursor::new(incarnation, generation.get()),
            Some(generation),
            Some(generation),
            after.is_some_and(|cursor| cursor.incarnation() != incarnation),
            Some(peer),
        ))
    }
}

#[cfg(all(feature = "experimental-rns-inbox", feature = "experimental-lxmf"))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct CombinedPortCalls {
    submission_availability: usize,
    submission_status: usize,
    submission_accept: usize,
    inbox_availability: usize,
    inbox_status: usize,
    inbox_peek: usize,
    lxmf_availability: usize,
    lxmf_next: usize,
    lxmf_read: usize,
    compose_availability: usize,
    compose_and_accept: usize,
}

#[cfg(all(feature = "experimental-rns-inbox", feature = "experimental-lxmf"))]
#[derive(Clone, Debug, Eq, PartialEq)]
struct ObservedLxmfComposeRequest {
    principal: PrincipalId,
    destination: DestinationHash,
    timestamp_unix_ms: u64,
    title: Vec<u8>,
    content: Vec<u8>,
    idempotency_key: IdempotencyKey,
    authorization: AuthorizationSnapshot,
}

#[cfg(all(feature = "experimental-rns-inbox", feature = "experimental-lxmf"))]
struct FakeCombinedPort {
    calls: CombinedPortCalls,
    lxmf_availability: CapabilityAvailability,
    lxmf_next: Result<Option<api::LxmfMessageSummary>, LxmfInboxPortError>,
    lxmf_read: Result<Option<api::LxmfReadChunk>, LxmfInboxPortError>,
    compose_availability: CapabilityAvailability,
    compose_result: Result<LxmfComposeAcceptance, LxmfComposePortError>,
    observed_compose: Option<ObservedLxmfComposeRequest>,
}

#[cfg(all(feature = "experimental-rns-inbox", feature = "experimental-lxmf"))]
impl FakeCombinedPort {
    fn with_lxmf(
        summary: Option<api::LxmfMessageSummary>,
        chunk: Option<api::LxmfReadChunk>,
    ) -> Self {
        Self {
            calls: CombinedPortCalls::default(),
            lxmf_availability: CapabilityAvailability::Available,
            lxmf_next: Ok(summary),
            lxmf_read: Ok(chunk),
            compose_availability: CapabilityAvailability::Available,
            compose_result: Ok(LxmfComposeAcceptance::new(
                SubmissionAcceptance::Accepted(SubmissionId::new(41)),
                [0x67; 32],
            )),
            observed_compose: None,
        }
    }
}

#[cfg(all(feature = "experimental-rns-inbox", feature = "experimental-lxmf"))]
impl SubmissionPort for FakeCombinedPort {
    fn availability(&mut self) -> CapabilityAvailability {
        self.calls.submission_availability += 1;
        CapabilityAvailability::Available
    }

    fn submission_state(
        &mut self,
        _principal: PrincipalId,
        _id: SubmissionId,
    ) -> Result<Option<LifecycleState>, SubmissionPortError> {
        self.calls.submission_status += 1;
        Ok(None)
    }

    fn accept(
        &mut self,
        _candidate: AcceptanceCandidate,
    ) -> Result<SubmissionAcceptance, SubmissionPortError> {
        self.calls.submission_accept += 1;
        Err(SubmissionPortError::Unavailable)
    }
}

#[cfg(all(feature = "experimental-rns-inbox", feature = "experimental-lxmf"))]
impl InboundMailboxPort for FakeCombinedPort {
    fn availability(&mut self) -> CapabilityAvailability {
        self.calls.inbox_availability += 1;
        CapabilityAvailability::Available
    }

    fn status(&mut self) -> Result<api::RnsInboxStatus, InboundMailboxPortError> {
        self.calls.inbox_status += 1;
        Err(InboundMailboxPortError::Unavailable)
    }

    fn peek(&mut self) -> Result<Option<InboundMailboxItem>, InboundMailboxPortError> {
        self.calls.inbox_peek += 1;
        Err(InboundMailboxPortError::Unavailable)
    }
}

#[cfg(all(feature = "experimental-rns-inbox", feature = "experimental-lxmf"))]
impl LxmfInboxPort for FakeCombinedPort {
    fn availability(&mut self) -> CapabilityAvailability {
        self.calls.lxmf_availability += 1;
        self.lxmf_availability
    }

    fn next(
        &mut self,
        _after: Option<api::LxmfMessageHandle>,
    ) -> Result<Option<api::LxmfMessageSummary>, LxmfInboxPortError> {
        self.calls.lxmf_next += 1;
        self.lxmf_next
    }

    fn read(
        &mut self,
        _handle: api::LxmfMessageHandle,
        _offset: u32,
        _max_bytes: api::LxmfReadLength,
    ) -> Result<Option<api::LxmfReadChunk>, LxmfInboxPortError> {
        self.calls.lxmf_read += 1;
        self.lxmf_read
    }
}

#[cfg(all(feature = "experimental-rns-inbox", feature = "experimental-lxmf"))]
impl LxmfComposePort for FakeCombinedPort {
    fn availability(&mut self) -> CapabilityAvailability {
        self.calls.compose_availability += 1;
        self.compose_availability
    }

    fn compose_and_accept(
        &mut self,
        request: LxmfComposeRequest<'_>,
    ) -> Result<LxmfComposeAcceptance, LxmfComposePortError> {
        self.calls.compose_and_accept += 1;
        self.observed_compose = Some(ObservedLxmfComposeRequest {
            principal: request.principal(),
            destination: request.destination(),
            timestamp_unix_ms: request.timestamp_unix_ms(),
            title: request.title().to_vec(),
            content: request.content().to_vec(),
            idempotency_key: request.idempotency_key(),
            authorization: request.authorization(),
        });
        self.compose_result
    }
}

#[cfg(all(feature = "experimental-rns-inbox", feature = "experimental-lxmf"))]
fn lxmf_summary() -> api::LxmfMessageSummary {
    api::LxmfMessageSummary::new(
        api::LxmfMessageHandle::new(7).unwrap(),
        [0x17; 32],
        api::DestinationHash([0x27; 16]),
        api::DestinationHash([0x37; 16]),
        1.25_f64.to_bits(),
        114,
        1,
        2,
        1,
        [0x47; 32],
    )
    .unwrap()
}

#[cfg(all(feature = "experimental-rns-inbox", feature = "experimental-lxmf"))]
fn lxmf_chunk() -> api::LxmfReadChunk {
    api::LxmfReadChunk::new(api::LxmfMessageHandle::new(7).unwrap(), 0, 5, b"hello").unwrap()
}

fn dispatch<const SUBMISSIONS: usize>(
    actor: &mut TestActor<SUBMISSIONS>,
    context: DispatchContext,
    envelope: RequestEnvelope<'_>,
) -> ResponseEnvelope {
    super::dispatch(actor, identity_summary(), &context, envelope)
}

fn envelope<'a>(request_id: u64, request: DeviceRequest<'a>) -> RequestEnvelope<'a> {
    RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(request_id),
        request,
    }
}

fn authenticated(principal: u8, permissions: Permissions) -> DispatchContext {
    DispatchContext::authenticated(
        ApiPrincipalId([principal; 16]),
        permissions,
        dispatch_provenance(principal),
    )
}

fn dispatch_provenance(tag: u8) -> DispatchProvenance {
    DispatchProvenance::new(
        [tag.wrapping_add(0x40); 16],
        u64::from(tag) + 1,
        u64::from(tag) + 10,
        u32::from(tag) + 1,
    )
    .unwrap()
}

fn durable_authorization(tag: u8) -> AuthorizationSnapshot {
    durable_authorization_with_permissions(
        tag,
        AUTHORIZATION_PERMISSION_EXPERIMENTAL_SUBMIT_RNS_DATA,
    )
}

fn durable_authorization_with_permissions(tag: u8, permissions: u32) -> AuthorizationSnapshot {
    AuthorizationSnapshot::new(
        [tag.wrapping_add(0x40); 16],
        u64::from(tag) + 1,
        u64::from(tag) + 10,
        u32::from(tag) + 1,
        permissions,
    )
    .unwrap()
}

fn error_code(response: DeviceResponse) -> ApiErrorCode {
    match response {
        DeviceResponse::Error(error) => error.code,
        other => panic!("expected API error, received {other:?}"),
    }
}

fn durable_candidate(principal: u8, key: u8, payload: &[u8]) -> AcceptanceCandidate {
    AcceptanceCandidate::new(
        PrincipalId::new([principal; 16]),
        IdempotencyKey::new([key; 16]),
        ExperimentalRnsDataIntent::new(DestinationHash::new([0x33; 16]), payload).unwrap(),
        durable_authorization(principal),
    )
}

#[test]
fn authorization_errors_are_precise_and_echo_request_context() {
    let mut actor = mounted::<2>();
    let request = envelope(
        7,
        DeviceRequest::SubmissionStatus {
            id: ApiSubmissionId(10),
        },
    );
    let unauthenticated = dispatch(&mut actor, DispatchContext::UNAUTHENTICATED, request);
    assert_eq!(unauthenticated.version, ApiVersion::CURRENT);
    assert_eq!(unauthenticated.request_id, RequestId(7));
    assert_eq!(
        unauthenticated.response,
        DeviceResponse::Error(ApiErrorResponse {
            code: ApiErrorCode::AuthenticationRequired,
            operation: Some(OP_SUBMISSION_STATUS),
        })
    );

    let denied = dispatch(&mut actor, authenticated(1, Permissions::NONE), request);
    assert_eq!(
        denied.response,
        DeviceResponse::Error(ApiErrorResponse {
            code: ApiErrorCode::PermissionDenied,
            operation: Some(OP_SUBMISSION_STATUS),
        })
    );
}

#[test]
fn dispatch_rejects_a_manually_constructed_incompatible_major_version() {
    let mut actor = mounted::<2>();
    let response = dispatch(
        &mut actor,
        DispatchContext::UNAUTHENTICATED,
        RequestEnvelope {
            version: ApiVersion {
                major: ApiVersion::CURRENT.major + 1,
                minor: 0,
            },
            request_id: RequestId(8),
            request: DeviceRequest::SystemCapabilities,
        },
    );
    assert_eq!(response.version, ApiVersion::CURRENT);
    assert_eq!(response.request_id, RequestId(8));
    assert_eq!(
        error_code(response.response),
        ApiErrorCode::UnsupportedVersion
    );
}

#[test]
fn capabilities_are_public_current_and_side_effect_free() {
    let mut actor = mounted::<2>();
    let state_before = actor.state();
    let response = dispatch(
        &mut actor,
        DispatchContext::UNAUTHENTICATED,
        RequestEnvelope {
            version: ApiVersion {
                major: ApiVersion::CURRENT.major,
                minor: ApiVersion::CURRENT.minor + 1,
            },
            request_id: RequestId(11),
            request: DeviceRequest::SystemCapabilities,
        },
    );
    assert_eq!(response.version, ApiVersion::CURRENT);
    assert_eq!(response.request_id, RequestId(11));
    assert_eq!(
        response.response,
        DeviceResponse::SystemCapabilities(CapabilitySnapshot::for_dispatch(cfg!(
            feature = "experimental-rns-data"
        )))
    );
    let DeviceResponse::SystemCapabilities(capabilities) = response.response else {
        panic!("expected capabilities response")
    };
    assert_eq!(
        capabilities.experimental_submit_rns_data(),
        cfg!(feature = "experimental-rns-data"),
        "dependency feature unification must not advertise adapter-local code"
    );
    assert_eq!(actor.state(), state_before);
    assert_eq!(actor.pending_kind(), None);
}

#[test]
fn identity_summary_is_public_read_only_and_never_calls_submission_port() {
    let expected = identity_summary();
    for context in [
        DispatchContext::UNAUTHENTICATED,
        authenticated(1, Permissions::NONE),
    ] {
        let mut port = UnavailablePort::default();
        let response = super::dispatch(
            &mut port,
            expected,
            &context,
            envelope(110, DeviceRequest::IdentitySummary),
        );
        assert_eq!(response.version, ApiVersion::CURRENT);
        assert_eq!(response.request_id, RequestId(110));
        assert_eq!(response.response, DeviceResponse::IdentitySummary(expected));
        assert_eq!(port.availability_calls, 0);
        assert_eq!(port.status_calls, 0);
        assert_eq!(port.acceptance_calls, 0);
    }
}

#[cfg(feature = "experimental-lxmf")]
#[test]
fn lxmf_dispatcher_does_not_require_the_raw_inbox_feature_or_port() {
    let mut port = FakeLxmfOnlyPort::default();
    let capabilities = super::dispatch_with_lxmf(
        &mut port,
        identity_summary(),
        &DispatchContext::UNAUTHENTICATED,
        envelope(111, DeviceRequest::SystemCapabilities),
    );
    assert_eq!(
        capabilities.response,
        DeviceResponse::SystemCapabilities(
            CapabilitySnapshot::for_dispatch_with_inbox_lxmf_and_basic_send(
                cfg!(feature = "experimental-rns-data"),
                CapabilityAvailability::Unavailable,
                CapabilityAvailability::Available,
                CapabilityAvailability::Available,
            )
        )
    );

    let next = super::dispatch_with_lxmf(
        &mut port,
        identity_summary(),
        &authenticated(1, Permissions::NONE),
        envelope(112, DeviceRequest::LxmfNext { after: None }),
    );
    let DeviceResponse::LxmfNext(summary) = next.response else {
        panic!("expected standalone LXMF summary")
    };
    assert_eq!(summary.handle(), api::LxmfMessageHandle::new(7).unwrap());
    assert_eq!(port.lxmf_availability, 2);
    assert_eq!(port.lxmf_next, 1);
    assert_eq!(port.compose_availability, 1);
    assert_eq!(
        port.submission_availability,
        usize::from(cfg!(feature = "experimental-rns-data"))
    );
}

#[cfg(feature = "experimental-lxmf")]
#[test]
fn nearby_peer_dispatch_is_authenticated_and_only_new_entry_point_advertises_it() {
    let request = envelope(113, DeviceRequest::LxmfPeerNext { after: None });

    let mut legacy_port = FakeLxmfOnlyPort::default();
    let legacy = super::dispatch_with_lxmf(
        &mut legacy_port,
        identity_summary(),
        &authenticated(1, Permissions::NONE),
        request,
    );
    assert_eq!(
        legacy.response,
        DeviceResponse::Error(ApiErrorResponse {
            code: ApiErrorCode::UnsupportedOperation,
            operation: Some(api::OP_EXPERIMENTAL_LXMF_PEER_NEXT),
        })
    );
    assert_eq!(legacy_port.peer_availability, 0);
    assert_eq!(legacy_port.peer_next, 0);

    let mut unauthenticated_port = FakeLxmfOnlyPort::default();
    let unauthenticated = super::dispatch_with_lxmf_and_peer_discovery(
        &mut unauthenticated_port,
        identity_summary(),
        &DispatchContext::UNAUTHENTICATED,
        request,
    );
    assert_eq!(
        unauthenticated.response,
        DeviceResponse::Error(ApiErrorResponse {
            code: ApiErrorCode::AuthenticationRequired,
            operation: Some(api::OP_EXPERIMENTAL_LXMF_PEER_NEXT),
        })
    );
    assert_eq!(unauthenticated_port.peer_availability, 0);
    assert_eq!(unauthenticated_port.peer_next, 0);
}

#[cfg(feature = "experimental-lxmf")]
#[test]
fn nearby_peer_dispatch_reports_port_limit_and_returns_one_reset_page() {
    let mut capability_port = FakeLxmfOnlyPort::default();
    let capabilities = super::dispatch_with_lxmf_and_peer_discovery(
        &mut capability_port,
        identity_summary(),
        &DispatchContext::UNAUTHENTICATED,
        envelope(114, DeviceRequest::SystemCapabilities),
    );
    assert_eq!(
        capabilities.response,
        DeviceResponse::SystemCapabilities(
            CapabilitySnapshot::for_dispatch_with_inbox_lxmf_basic_send_and_peer_discovery(
                cfg!(feature = "experimental-rns-data"),
                CapabilityAvailability::Unavailable,
                CapabilityAvailability::Available,
                CapabilityAvailability::Available,
                CapabilityAvailability::Available,
                64,
            )
        )
    );
    assert_eq!(capability_port.peer_availability, 1);
    assert_eq!(capability_port.peer_max_app_data, 1);
    assert_eq!(capability_port.peer_next, 0);

    let stale =
        api::LxmfPeerDiscoveryCursor::new(api::LxmfPeerDiscoveryIncarnation::new([0x99; 8]), 77);
    let mut read_port = FakeLxmfOnlyPort::default();
    let response = super::dispatch_with_lxmf_and_peer_discovery(
        &mut read_port,
        identity_summary(),
        &authenticated(1, Permissions::NONE),
        envelope(115, DeviceRequest::LxmfPeerNext { after: Some(stale) }),
    );
    let DeviceResponse::LxmfPeerNext(page) = response.response else {
        panic!("expected nearby peer page")
    };
    assert_eq!(read_port.observed_peer_cursor, Some(Some(stale)));
    assert_eq!(read_port.peer_availability, 1);
    assert_eq!(read_port.peer_next, 1);
    assert!(page.history_gap());
    assert_eq!(
        page.next_cursor().incarnation(),
        api::LxmfPeerDiscoveryIncarnation::new([0x88; 8])
    );
    let peer = page.peer().unwrap();
    assert_eq!(peer.destination(), ApiDestinationHash([0x31; 16]));
    assert_eq!(peer.identity_hash(), api::IdentityHash::new([0x41; 16]));
    assert_eq!(peer.app_data(), b"nearby");
    assert_eq!(peer.observed_age_ms(), 250);
}

#[cfg(feature = "experimental-rns-inbox")]
#[test]
fn composite_identity_read_acquires_neither_port() {
    let expected = identity_summary();
    for context in [
        DispatchContext::UNAUTHENTICATED,
        authenticated(1, Permissions::NONE),
    ] {
        let mut submission = UnavailablePort::default();
        let mut inbox = FakeInbox::available(None);
        let response = super::dispatch_with_inbox(
            &mut submission,
            &mut inbox,
            expected,
            &context,
            envelope(210, DeviceRequest::IdentitySummary),
        );
        assert_eq!(response.response, DeviceResponse::IdentitySummary(expected));
        assert_eq!(submission.availability_calls, 0);
        assert_eq!(submission.status_calls, 0);
        assert_eq!(submission.acceptance_calls, 0);
        assert_eq!(
            inbox.calls(),
            FakeInboxCalls {
                availability: 0,
                status: 0,
                peek: 0,
            }
        );
    }
}

#[cfg(all(feature = "experimental-rns-inbox", feature = "experimental-lxmf"))]
#[test]
fn combined_identity_and_authentication_preflight_acquire_no_ports() {
    let expected = api::IdentitySummary::with_lxmf_delivery_destination(
        api::DestinationHash([0xa5; 16]),
        api::DestinationHash([0xb6; 16]),
    );
    let mut port = FakeCombinedPort::with_lxmf(Some(lxmf_summary()), Some(lxmf_chunk()));
    let identity = super::dispatch_with_inbox_and_lxmf(
        &mut port,
        expected,
        &authenticated(1, Permissions::NONE),
        envelope(220, DeviceRequest::IdentitySummary),
    );
    assert_eq!(identity.response, DeviceResponse::IdentitySummary(expected));
    assert_eq!(port.calls, CombinedPortCalls::default());

    let unauthenticated = super::dispatch_with_inbox_and_lxmf(
        &mut port,
        expected,
        &DispatchContext::UNAUTHENTICATED,
        envelope(221, DeviceRequest::LxmfNext { after: None }),
    );
    assert_eq!(
        unauthenticated.response,
        DeviceResponse::Error(ApiErrorResponse {
            code: ApiErrorCode::AuthenticationRequired,
            operation: Some(api::OP_EXPERIMENTAL_LXMF_NEXT),
        })
    );
    assert_eq!(port.calls, CombinedPortCalls::default());
}

#[cfg(all(feature = "experimental-rns-inbox", feature = "experimental-lxmf"))]
#[test]
fn combined_capabilities_query_each_availability_once() {
    let mut port = FakeCombinedPort::with_lxmf(None, None);
    let response = super::dispatch_with_inbox_and_lxmf(
        &mut port,
        identity_summary(),
        &DispatchContext::UNAUTHENTICATED,
        envelope(222, DeviceRequest::SystemCapabilities),
    );
    assert_eq!(
        response.response,
        DeviceResponse::SystemCapabilities(
            CapabilitySnapshot::for_dispatch_with_inbox_lxmf_and_basic_send(
                cfg!(feature = "experimental-rns-data"),
                CapabilityAvailability::Available,
                CapabilityAvailability::Available,
                CapabilityAvailability::Available,
            ),
        )
    );
    assert_eq!(
        port.calls,
        CombinedPortCalls {
            submission_availability: usize::from(cfg!(feature = "experimental-rns-data")),
            inbox_availability: 1,
            lxmf_availability: 1,
            compose_availability: 1,
            ..CombinedPortCalls::default()
        }
    );
}

#[cfg(all(feature = "experimental-rns-inbox", feature = "experimental-lxmf"))]
#[test]
fn combined_lxmf_next_and_read_enter_only_the_selected_port_method() {
    let summary = lxmf_summary();
    let chunk = lxmf_chunk();
    let mut next_port = FakeCombinedPort::with_lxmf(Some(summary), Some(chunk));
    let next = super::dispatch_with_inbox_and_lxmf(
        &mut next_port,
        identity_summary(),
        &authenticated(1, Permissions::NONE),
        envelope(223, DeviceRequest::LxmfNext { after: None }),
    );
    assert_eq!(next.response, DeviceResponse::LxmfNext(summary));
    assert_eq!(
        next_port.calls,
        CombinedPortCalls {
            lxmf_availability: 1,
            lxmf_next: 1,
            ..CombinedPortCalls::default()
        }
    );

    let mut read_port = FakeCombinedPort::with_lxmf(Some(summary), Some(chunk));
    let read = super::dispatch_with_inbox_and_lxmf(
        &mut read_port,
        identity_summary(),
        &authenticated(1, Permissions::NONE),
        envelope(
            224,
            DeviceRequest::LxmfRead {
                handle: api::LxmfMessageHandle::new(7).unwrap(),
                offset: 0,
                max_bytes: api::LxmfReadLength::new(5).unwrap(),
            },
        ),
    );
    assert_eq!(read.response, DeviceResponse::LxmfRead(chunk));
    assert_eq!(
        read_port.calls,
        CombinedPortCalls {
            lxmf_availability: 1,
            lxmf_read: 1,
            ..CombinedPortCalls::default()
        }
    );
}

#[cfg(all(feature = "experimental-rns-inbox", feature = "experimental-lxmf"))]
#[test]
fn combined_lxmf_empty_unavailable_and_port_failures_map_closed() {
    let handle = api::LxmfMessageHandle::new(7).unwrap();
    let request = DeviceRequest::LxmfNext {
        after: Some(handle),
    };
    let mut empty = FakeCombinedPort::with_lxmf(None, None);
    let response = super::dispatch_with_inbox_and_lxmf(
        &mut empty,
        identity_summary(),
        &authenticated(1, Permissions::NONE),
        envelope(225, request),
    );
    assert_eq!(error_code(response.response), ApiErrorCode::NotFound);

    let mut disabled = FakeCombinedPort::with_lxmf(None, None);
    disabled.lxmf_availability = CapabilityAvailability::Disabled;
    let response = super::dispatch_with_inbox_and_lxmf(
        &mut disabled,
        identity_summary(),
        &authenticated(1, Permissions::NONE),
        envelope(226, request),
    );
    assert_eq!(
        error_code(response.response),
        ApiErrorCode::CapabilityUnavailable
    );
    assert_eq!(disabled.calls.lxmf_next, 0);

    for (port_error, api_error) in [
        (
            LxmfInboxPortError::Unavailable,
            ApiErrorCode::CapabilityUnavailable,
        ),
        (
            LxmfInboxPortError::InvalidRequest,
            ApiErrorCode::InvalidRequest,
        ),
        (LxmfInboxPortError::Busy, ApiErrorCode::Internal),
        (LxmfInboxPortError::Backend, ApiErrorCode::Internal),
        (LxmfInboxPortError::Binding, ApiErrorCode::Internal),
        (LxmfInboxPortError::Faulted, ApiErrorCode::Internal),
    ] {
        let mut port = FakeCombinedPort::with_lxmf(None, None);
        port.lxmf_next = Err(port_error);
        let response = super::dispatch_with_inbox_and_lxmf(
            &mut port,
            identity_summary(),
            &authenticated(1, Permissions::NONE),
            envelope(227, request),
        );
        assert_eq!(error_code(response.response), api_error);
    }
}

#[cfg(all(feature = "experimental-rns-inbox", feature = "experimental-lxmf"))]
fn basic_lxmf_send_request() -> DeviceRequest<'static> {
    DeviceRequest::LxmfBasicSend {
        destination: ApiDestinationHash([0x28; 16]),
        timestamp_unix_ms: 1_753_141_234_567,
        title: b"title",
        content: b"content",
        idempotency_key: ApiIdempotencyKey([0x38; 16]),
    }
}

#[cfg(all(feature = "experimental-rns-inbox", feature = "experimental-lxmf"))]
#[test]
fn combined_basic_lxmf_send_derives_provenance_and_enters_only_compose_port() {
    let permissions =
        Permissions::EXPERIMENTAL_SUBMIT_RNS_DATA | Permissions::READ_SUBMISSION_STATUS;
    for acceptance in [
        SubmissionAcceptance::Accepted(SubmissionId::new(41)),
        SubmissionAcceptance::Replay(SubmissionId::new(41)),
    ] {
        let mut port = FakeCombinedPort::with_lxmf(None, None);
        port.compose_result = Ok(LxmfComposeAcceptance::new(acceptance, [0x67; 32]));
        let response = super::dispatch_with_inbox_and_lxmf(
            &mut port,
            identity_summary(),
            &authenticated(5, permissions),
            envelope(228, basic_lxmf_send_request()),
        );
        assert_eq!(
            response.response,
            DeviceResponse::LxmfBasicSendAccepted(api::LxmfBasicSendAccepted::new(
                ApiSubmissionId(41),
                [0x67; 32],
            ))
        );
        assert_eq!(
            port.calls,
            CombinedPortCalls {
                compose_availability: 1,
                compose_and_accept: 1,
                ..CombinedPortCalls::default()
            }
        );
        assert_eq!(
            port.observed_compose,
            Some(ObservedLxmfComposeRequest {
                principal: PrincipalId::new([5; 16]),
                destination: DestinationHash::new([0x28; 16]),
                timestamp_unix_ms: 1_753_141_234_567,
                title: b"title".to_vec(),
                content: b"content".to_vec(),
                idempotency_key: IdempotencyKey::new([0x38; 16]),
                authorization: durable_authorization_with_permissions(5, permissions.bits()),
            })
        );
    }
}

#[cfg(all(feature = "experimental-rns-inbox", feature = "experimental-lxmf"))]
#[test]
fn combined_basic_lxmf_send_rejects_auth_and_version_before_every_port_call() {
    let cases = [
        (
            DispatchContext::UNAUTHENTICATED,
            ApiVersion::CURRENT,
            ApiErrorCode::AuthenticationRequired,
        ),
        (
            authenticated(6, Permissions::NONE),
            ApiVersion::CURRENT,
            ApiErrorCode::PermissionDenied,
        ),
        (
            authenticated(6, Permissions::EXPERIMENTAL_SUBMIT_RNS_DATA),
            ApiVersion {
                major: ApiVersion::CURRENT.major + 1,
                minor: 0,
            },
            ApiErrorCode::UnsupportedVersion,
        ),
    ];
    for (context, version, expected) in cases {
        let mut port = FakeCombinedPort::with_lxmf(None, None);
        let response = super::dispatch_with_inbox_and_lxmf(
            &mut port,
            identity_summary(),
            &context,
            RequestEnvelope {
                version,
                request_id: RequestId(229),
                request: basic_lxmf_send_request(),
            },
        );
        assert_eq!(error_code(response.response), expected);
        assert_eq!(port.calls, CombinedPortCalls::default());
        assert_eq!(port.observed_compose, None);
    }
}

#[cfg(all(feature = "experimental-rns-inbox", feature = "experimental-lxmf"))]
#[test]
fn combined_basic_lxmf_send_maps_unavailability_and_closed_port_errors() {
    let context = authenticated(7, Permissions::EXPERIMENTAL_SUBMIT_RNS_DATA);
    let mut disabled = FakeCombinedPort::with_lxmf(None, None);
    disabled.compose_availability = CapabilityAvailability::Disabled;
    let response = super::dispatch_with_inbox_and_lxmf(
        &mut disabled,
        identity_summary(),
        &context,
        envelope(230, basic_lxmf_send_request()),
    );
    assert_eq!(
        error_code(response.response),
        ApiErrorCode::CapabilityUnavailable
    );
    assert_eq!(disabled.calls.compose_availability, 1);
    assert_eq!(disabled.calls.compose_and_accept, 0);
    assert_eq!(disabled.calls.submission_accept, 0);

    for (port_error, api_error) in [
        (
            LxmfComposePortError::Unavailable,
            ApiErrorCode::CapabilityUnavailable,
        ),
        (
            LxmfComposePortError::InvalidRequest,
            ApiErrorCode::InvalidRequest,
        ),
        (LxmfComposePortError::Busy, ApiErrorCode::Internal),
        (LxmfComposePortError::Backend, ApiErrorCode::Internal),
        (LxmfComposePortError::Binding, ApiErrorCode::Internal),
        (LxmfComposePortError::Faulted, ApiErrorCode::Internal),
        (LxmfComposePortError::Invariant, ApiErrorCode::Internal),
    ] {
        let mut port = FakeCombinedPort::with_lxmf(None, None);
        port.compose_result = Err(port_error);
        let response = super::dispatch_with_inbox_and_lxmf(
            &mut port,
            identity_summary(),
            &context,
            envelope(231, basic_lxmf_send_request()),
        );
        assert_eq!(error_code(response.response), api_error);
        assert_eq!(port.calls.compose_and_accept, 1);
        assert_eq!(port.calls.submission_accept, 0);
    }
}

#[cfg(all(feature = "experimental-rns-inbox", feature = "experimental-lxmf"))]
#[test]
fn combined_basic_lxmf_send_maps_durable_acceptance_outcomes() {
    for (acceptance, expected) in [
        (
            SubmissionAcceptance::IdempotencyConflict,
            ApiErrorCode::IdempotencyConflict,
        ),
        (
            SubmissionAcceptance::CapacityExhausted,
            ApiErrorCode::CapacityExhausted,
        ),
        (
            SubmissionAcceptance::IdentifierExhausted,
            ApiErrorCode::Internal,
        ),
    ] {
        let mut port = FakeCombinedPort::with_lxmf(None, None);
        port.compose_result = Ok(LxmfComposeAcceptance::new(acceptance, [0x68; 32]));
        let response = super::dispatch_with_inbox_and_lxmf(
            &mut port,
            identity_summary(),
            &authenticated(8, Permissions::EXPERIMENTAL_SUBMIT_RNS_DATA),
            envelope(232, basic_lxmf_send_request()),
        );
        assert_eq!(error_code(response.response), expected);
        assert_eq!(port.calls.compose_and_accept, 1);
        assert_eq!(port.calls.submission_accept, 0);
    }
}

#[cfg(feature = "experimental-rns-inbox")]
#[test]
fn composite_capabilities_report_each_port_without_cross_dispatch() {
    let mut submission = UnavailablePort::default();
    let mut inbox = FakeInbox::available(None);
    let response = super::dispatch_with_inbox(
        &mut submission,
        &mut inbox,
        identity_summary(),
        &DispatchContext::UNAUTHENTICATED,
        envelope(211, DeviceRequest::SystemCapabilities),
    );
    assert_eq!(
        response.response,
        DeviceResponse::SystemCapabilities(CapabilitySnapshot::for_dispatch_with_inbox(
            false,
            CapabilityAvailability::Available,
        ))
    );
    assert_eq!(
        submission.availability_calls,
        usize::from(cfg!(feature = "experimental-rns-data"))
    );
    assert_eq!(submission.status_calls, 0);
    assert_eq!(submission.acceptance_calls, 0);
    assert_eq!(
        inbox.calls(),
        FakeInboxCalls {
            availability: 1,
            status: 0,
            peek: 0,
        }
    );
}

#[cfg(feature = "experimental-rns-inbox")]
#[test]
fn inbox_authentication_rejection_precedes_both_ports() {
    for request in [DeviceRequest::RnsInboxStatus, DeviceRequest::RnsInboxPeek] {
        let mut submission = UnavailablePort::default();
        let mut inbox = FakeInbox::available(None);
        let response = super::dispatch_with_inbox(
            &mut submission,
            &mut inbox,
            identity_summary(),
            &DispatchContext::UNAUTHENTICATED,
            envelope(212, request),
        );
        assert_eq!(
            error_code(response.response),
            ApiErrorCode::AuthenticationRequired
        );
        assert_eq!(submission.availability_calls, 0);
        assert_eq!(submission.status_calls, 0);
        assert_eq!(submission.acceptance_calls, 0);
        assert_eq!(
            inbox.calls(),
            FakeInboxCalls {
                availability: 0,
                status: 0,
                peek: 0,
            }
        );
    }
}

#[cfg(feature = "experimental-rns-inbox")]
#[test]
fn inbox_status_uses_only_the_inbox_port_and_needs_no_permission_bit() {
    let mut submission = UnavailablePort::default();
    let mut inbox = FakeInbox::available(None);
    let expected = inbox.status.unwrap();
    let response = super::dispatch_with_inbox(
        &mut submission,
        &mut inbox,
        identity_summary(),
        &authenticated(1, Permissions::NONE),
        envelope(213, DeviceRequest::RnsInboxStatus),
    );
    assert_eq!(response.response, DeviceResponse::RnsInboxStatus(expected));
    assert_eq!(submission.availability_calls, 0);
    assert_eq!(submission.status_calls, 0);
    assert_eq!(submission.acceptance_calls, 0);
    assert_eq!(
        inbox.calls(),
        FakeInboxCalls {
            availability: 1,
            status: 1,
            peek: 0,
        }
    );
}

#[cfg(feature = "experimental-rns-inbox")]
#[test]
fn inbox_peek_returns_an_owned_item_and_empty_is_not_found() {
    let semantic = InboundMailboxItem::new(
        core::num::NonZeroU64::new(17).unwrap(),
        api::DestinationHash([0x31; 16]),
        b"received",
    )
    .unwrap();
    let mut submission = UnavailablePort::default();
    let mut occupied = FakeInbox::available(Some(semantic));
    let response = super::dispatch_with_inbox(
        &mut submission,
        &mut occupied,
        identity_summary(),
        &authenticated(1, Permissions::NONE),
        envelope(214, DeviceRequest::RnsInboxPeek),
    );
    let DeviceResponse::RnsInboxPeek(item) = response.response else {
        panic!("expected occupied inbox response")
    };
    assert_eq!(item.id(), 17);
    assert_eq!(item.destination(), api::DestinationHash([0x31; 16]));
    assert_eq!(item.payload(), b"received");
    assert_eq!(submission.availability_calls, 0);
    assert_eq!(submission.status_calls, 0);
    assert_eq!(submission.acceptance_calls, 0);
    assert_eq!(
        occupied.calls(),
        FakeInboxCalls {
            availability: 1,
            status: 0,
            peek: 1,
        }
    );

    let mut empty = FakeInbox::available(None);
    let response = super::dispatch_with_inbox(
        &mut submission,
        &mut empty,
        identity_summary(),
        &authenticated(1, Permissions::NONE),
        envelope(215, DeviceRequest::RnsInboxPeek),
    );
    assert_eq!(
        response.response,
        DeviceResponse::Error(ApiErrorResponse {
            code: ApiErrorCode::NotFound,
            operation: Some(api::OP_EXPERIMENTAL_RNS_INBOX_PEEK),
        })
    );
    assert_eq!(
        empty.calls(),
        FakeInboxCalls {
            availability: 1,
            status: 0,
            peek: 1,
        }
    );
}

#[cfg(feature = "experimental-rns-inbox")]
#[test]
fn unavailable_inbox_is_not_entered_and_port_failures_are_closed() {
    for request in [DeviceRequest::RnsInboxStatus, DeviceRequest::RnsInboxPeek] {
        let mut submission = UnavailablePort::default();
        let mut inbox = FakeInbox::available(None);
        inbox.availability = CapabilityAvailability::Disabled;
        let response = super::dispatch_with_inbox(
            &mut submission,
            &mut inbox,
            identity_summary(),
            &authenticated(1, Permissions::NONE),
            envelope(216, request),
        );
        assert_eq!(
            error_code(response.response),
            ApiErrorCode::CapabilityUnavailable
        );
        assert_eq!(
            inbox.calls(),
            FakeInboxCalls {
                availability: 1,
                status: 0,
                peek: 0,
            }
        );
    }

    for (port_error, api_error) in [
        (
            InboundMailboxPortError::Unavailable,
            ApiErrorCode::CapabilityUnavailable,
        ),
        (InboundMailboxPortError::Busy, ApiErrorCode::Internal),
        (InboundMailboxPortError::Backend, ApiErrorCode::Internal),
        (InboundMailboxPortError::Faulted, ApiErrorCode::Internal),
    ] {
        for request in [DeviceRequest::RnsInboxStatus, DeviceRequest::RnsInboxPeek] {
            let mut submission = UnavailablePort::default();
            let mut inbox = FakeInbox::available(None);
            inbox.status = Err(port_error);
            inbox.peek = Err(port_error);
            let response = super::dispatch_with_inbox(
                &mut submission,
                &mut inbox,
                identity_summary(),
                &authenticated(1, Permissions::NONE),
                envelope(217, request),
            );
            assert_eq!(error_code(response.response), api_error);
        }
    }
}

#[cfg(feature = "experimental-rns-inbox")]
#[test]
fn composite_version_rejection_precedes_both_ports() {
    let mut submission = UnavailablePort::default();
    let mut inbox = FakeInbox::available(None);
    let response = super::dispatch_with_inbox(
        &mut submission,
        &mut inbox,
        identity_summary(),
        &authenticated(1, Permissions::NONE),
        RequestEnvelope {
            version: ApiVersion {
                major: ApiVersion::CURRENT.major + 1,
                minor: 0,
            },
            request_id: RequestId(219),
            request: DeviceRequest::RnsInboxPeek,
        },
    );
    assert_eq!(
        error_code(response.response),
        ApiErrorCode::UnsupportedVersion
    );
    assert_eq!(submission.availability_calls, 0);
    assert_eq!(submission.status_calls, 0);
    assert_eq!(submission.acceptance_calls, 0);
    assert_eq!(
        inbox.calls(),
        FakeInboxCalls {
            availability: 0,
            status: 0,
            peek: 0,
        }
    );
}

#[cfg(feature = "experimental-rns-inbox")]
#[test]
fn existing_submission_dispatch_never_enters_the_inbox_port() {
    let mut submission = mounted::<2>();
    let mut inbox = FakeInbox::available(None);
    let response = super::dispatch_with_inbox(
        &mut submission,
        &mut inbox,
        identity_summary(),
        &authenticated(1, Permissions::READ_SUBMISSION_STATUS),
        envelope(
            218,
            DeviceRequest::SubmissionStatus {
                id: ApiSubmissionId(99),
            },
        ),
    );
    assert_eq!(error_code(response.response), ApiErrorCode::NotFound);
    assert_eq!(submission.port_calls, 2);
    assert_eq!(
        inbox.calls(),
        FakeInboxCalls {
            availability: 0,
            status: 0,
            peek: 0,
        }
    );
}

#[cfg(feature = "experimental-rns-inbox")]
#[test]
fn semantic_inbox_item_owns_and_bounds_its_payload() {
    let mut payload = [0x42_u8; api::MAX_RNS_INBOX_PAYLOAD_BYTES];
    let item = InboundMailboxItem::new(
        core::num::NonZeroU64::new(9).unwrap(),
        api::DestinationHash([0x21; 16]),
        &payload,
    )
    .unwrap();
    payload.fill(0);
    assert_eq!(item.id(), 9);
    assert_eq!(item.destination(), api::DestinationHash([0x21; 16]));
    assert_eq!(item.payload(), &[0x42; api::MAX_RNS_INBOX_PAYLOAD_BYTES]);
    let debug = std::format!("{item:?}");
    assert!(debug.contains("payload_len: 383"));
    assert!(!debug.contains("66, 66"));

    let oversized = [0_u8; api::MAX_RNS_INBOX_PAYLOAD_BYTES + 1];
    let error = InboundMailboxItem::new(
        core::num::NonZeroU64::new(10).unwrap(),
        api::DestinationHash([0; 16]),
        &oversized,
    )
    .unwrap_err();
    assert_eq!(error.actual(), api::MAX_RNS_INBOX_PAYLOAD_BYTES + 1);
    assert_eq!(error.maximum(), api::MAX_RNS_INBOX_PAYLOAD_BYTES);
}

#[test]
fn unavailable_service_is_not_advertised_or_called_for_status() {
    let mut port = UnavailablePort::default();
    let capabilities = super::dispatch(
        &mut port,
        identity_summary(),
        &DispatchContext::UNAUTHENTICATED,
        envelope(111, DeviceRequest::SystemCapabilities),
    );
    assert_eq!(
        capabilities.response,
        DeviceResponse::SystemCapabilities(CapabilitySnapshot::for_dispatch(false))
    );

    let status = super::dispatch(
        &mut port,
        identity_summary(),
        &authenticated(1, Permissions::READ_SUBMISSION_STATUS),
        envelope(
            112,
            DeviceRequest::SubmissionStatus {
                id: ApiSubmissionId(10),
            },
        ),
    );
    assert_eq!(
        error_code(status.response),
        ApiErrorCode::CapabilityUnavailable
    );
    assert_eq!(port.status_calls, 0);
    assert_eq!(port.acceptance_calls, 0);
}

#[test]
fn status_fails_closed_while_actor_is_faulted_but_capabilities_remain_public() {
    let flash = FakeNor::formatted_with_dropped_write(Some(0));
    let mut actor = TestActor::<2>::mount(flash, SubmissionId::new(10));
    assert!(matches!(
        actor.accept(durable_candidate(1, 2, b"fault")),
        Err(DriveError::Faulted(_))
    ));
    assert!(actor.fault().is_some());

    let status = dispatch(
        &mut actor,
        authenticated(1, Permissions::READ_SUBMISSION_STATUS),
        envelope(
            12,
            DeviceRequest::SubmissionStatus {
                id: ApiSubmissionId(10),
            },
        ),
    );
    assert_eq!(error_code(status.response), ApiErrorCode::Internal);

    let capabilities = dispatch(
        &mut actor,
        DispatchContext::UNAUTHENTICATED,
        envelope(13, DeviceRequest::SystemCapabilities),
    );
    assert_eq!(
        capabilities.response,
        DeviceResponse::SystemCapabilities(CapabilitySnapshot::for_dispatch(cfg!(
            feature = "experimental-rns-data"
        )))
    );
}

#[test]
fn submission_status_is_principal_scoped_and_hides_missing_ids() {
    let mut actor = mounted::<2>();
    assert_eq!(
        actor.accept(durable_candidate(1, 2, b"private")),
        Ok(AcceptanceProgress::Accepted(SubmissionId::new(10)))
    );
    let request = envelope(
        13,
        DeviceRequest::SubmissionStatus {
            id: ApiSubmissionId(10),
        },
    );

    let foreign = dispatch(
        &mut actor,
        authenticated(2, Permissions::READ_SUBMISSION_STATUS),
        request,
    );
    assert_eq!(error_code(foreign.response), ApiErrorCode::NotFound);

    let owned = dispatch(
        &mut actor,
        authenticated(1, Permissions::READ_SUBMISSION_STATUS),
        request,
    );
    assert_eq!(
        owned.response,
        DeviceResponse::SubmissionStatus(SubmissionStatus {
            id: ApiSubmissionId(10),
            state: SubmissionState::Queued,
        })
    );

    let missing = dispatch(
        &mut actor,
        authenticated(1, Permissions::READ_SUBMISSION_STATUS),
        envelope(
            14,
            DeviceRequest::SubmissionStatus {
                id: ApiSubmissionId(99),
            },
        ),
    );
    assert_eq!(error_code(missing.response), ApiErrorCode::NotFound);
}

#[test]
fn every_durable_lifecycle_state_maps_to_its_closed_api_shape() {
    let durable_details = DurablePreparedPacketDetails::new(
        97,
        EncodedPacketSha256::new([0x5a; 32]),
        RnsAttemptToken::new([0x6b; 32]),
    )
    .unwrap();
    let api_details = PreparedPacketDetails {
        packet_len: 97,
        encoded_packet_sha256: api::EncodedPacketSha256::new([0x5a; 32]),
    };
    let reset = BootRecoveryMarker::new(4, InterruptedState::Preparing);
    let cases = [
        (LifecycleState::Queued, SubmissionState::Queued),
        (LifecycleState::Preparing, SubmissionState::Preparing),
        (
            LifecycleState::AwaitingDelivery(durable_details),
            SubmissionState::AwaitingDelivery(api_details),
        ),
        (
            LifecycleState::Final(FinalDisposition::Delivered(durable_details)),
            SubmissionState::Delivered(api_details),
        ),
        (
            LifecycleState::Final(FinalDisposition::Failed(DurableSubmissionFailure::NoPath)),
            SubmissionState::Failed(SubmissionFailure::NoPath),
        ),
        (
            LifecycleState::Final(FinalDisposition::Failed(
                DurableSubmissionFailure::DeliveryTimeout,
            )),
            SubmissionState::Failed(SubmissionFailure::DeliveryTimeout),
        ),
        (
            LifecycleState::Final(FinalDisposition::Failed(DurableSubmissionFailure::Rejected)),
            SubmissionState::Failed(SubmissionFailure::Rejected),
        ),
        (
            LifecycleState::Final(FinalDisposition::Failed(
                DurableSubmissionFailure::Internal(InternalFailure::Unspecified),
            )),
            SubmissionState::Failed(SubmissionFailure::Internal),
        ),
        (
            LifecycleState::Final(FinalDisposition::Failed(
                DurableSubmissionFailure::Internal(InternalFailure::InterruptedByReset(reset)),
            )),
            SubmissionState::Failed(SubmissionFailure::Internal),
        ),
        (
            LifecycleState::Final(FinalDisposition::Cancelled),
            SubmissionState::Cancelled,
        ),
    ];
    for (durable, expected) in cases {
        assert_eq!(api_submission_state(durable), expected);
    }
}

#[cfg(feature = "experimental-rns-data")]
fn submit_request(payload: &[u8], key: u8) -> DeviceRequest<'_> {
    DeviceRequest::SubmitRnsData {
        destination: ApiDestinationHash([0x33; 16]),
        payload,
        idempotency_key: ApiIdempotencyKey([key; 16]),
    }
}

#[cfg(feature = "experimental-rns-data")]
fn submit_context(principal: u8) -> DispatchContext {
    submit_context_with_provenance(principal, principal)
}

#[cfg(feature = "experimental-rns-data")]
fn submit_context_with_provenance(principal: u8, provenance_tag: u8) -> DispatchContext {
    DispatchContext::authenticated(
        ApiPrincipalId([principal; 16]),
        Permissions::EXPERIMENTAL_SUBMIT_RNS_DATA | Permissions::READ_SUBMISSION_STATUS,
        dispatch_provenance(provenance_tag),
    )
}

#[cfg(feature = "experimental-rns-data")]
#[test]
fn unavailable_service_rejects_mutation_before_acceptance() {
    let mut port = UnavailablePort::default();
    let response = super::dispatch(
        &mut port,
        identity_summary(),
        &submit_context(1),
        envelope(118, submit_request(b"unavailable", 2)),
    );
    assert_eq!(
        error_code(response.response),
        ApiErrorCode::CapabilityUnavailable
    );
    assert_eq!(port.availability_calls, 1);
    assert_eq!(port.status_calls, 0);
    assert_eq!(port.acceptance_calls, 0);
}

#[cfg(feature = "experimental-rns-data")]
fn accepted_id(response: DeviceResponse) -> ApiSubmissionId {
    match response {
        DeviceResponse::SubmitRnsDataAccepted(accepted) => accepted.id,
        other => panic!("expected accepted response, received {other:?}"),
    }
}

#[cfg(feature = "experimental-rns-data")]
fn assert_submit_rejected_without_mutation(
    context: DispatchContext,
    version: ApiVersion,
    expected: ApiErrorCode,
) {
    let mut actor = mounted::<2>();
    let state_before = actor.state();
    let next_id_before = actor.index().next_id();
    let response = dispatch(
        &mut actor,
        context,
        RequestEnvelope {
            version,
            request_id: RequestId(19),
            request: submit_request(&OVERSIZED_PAYLOAD, 2),
        },
    );
    assert_eq!(
        actor.port_calls(),
        0,
        "version and authorization rejection must precede every port call"
    );
    assert_eq!(error_code(response.response), expected);
    assert_eq!(actor.state(), state_before);
    assert_eq!(actor.index().next_id(), next_id_before);
    assert_eq!(actor.index().get(SubmissionId::new(10)), None);
    assert_eq!(actor.pending_kind(), None);
    assert_eq!(actor.fault(), None);
    let flash = actor.into_flash();
    assert_eq!(flash.writes, 0);
    assert_eq!(flash.erases, 0);
}

#[cfg(feature = "experimental-rns-data")]
#[test]
fn mutating_auth_and_version_rejections_precede_validation_and_storage() {
    assert_submit_rejected_without_mutation(
        DispatchContext::UNAUTHENTICATED,
        ApiVersion::CURRENT,
        ApiErrorCode::AuthenticationRequired,
    );
    assert_submit_rejected_without_mutation(
        authenticated(1, Permissions::NONE),
        ApiVersion::CURRENT,
        ApiErrorCode::PermissionDenied,
    );
    assert_submit_rejected_without_mutation(
        submit_context(1),
        ApiVersion {
            major: ApiVersion::CURRENT.major + 1,
            minor: 0,
        },
        ApiErrorCode::UnsupportedVersion,
    );
}

#[cfg(feature = "experimental-rns-data")]
#[test]
fn durable_accept_owns_payload_and_replay_conflict_preserve_one_submission() {
    let mut actor = mounted::<2>();
    let mut borrowed_payload = *b"same";
    let first = dispatch(
        &mut actor,
        submit_context(1),
        envelope(20, submit_request(&borrowed_payload, 2)),
    );
    assert_eq!(accepted_id(first.response), ApiSubmissionId(10));
    assert_eq!(actor.state().committed_records(), 1);
    borrowed_payload.fill(b'x');
    let original = actor.index().get(SubmissionId::new(10)).unwrap().accepted();
    assert_eq!(
        original
            .intent()
            .experimental_rns_data()
            .expect("raw RNS submission retains its intent kind")
            .payload(),
        b"same"
    );
    let expected_provenance = dispatch_provenance(1);
    assert_eq!(
        original.authorization().credential_id(),
        &expected_provenance.credential_id()
    );
    assert_eq!(
        original.authorization().credential_generation(),
        expected_provenance.credential_generation()
    );
    assert_eq!(
        original.authorization().authority_revision(),
        expected_provenance.authority_revision()
    );
    assert_eq!(
        original.authorization().policy_version(),
        expected_provenance.policy_version()
    );
    assert_eq!(
        original.authorization().granted_permission_bits(),
        Permissions::EXPERIMENTAL_SUBMIT_RNS_DATA.bits()
            | Permissions::READ_SUBMISSION_STATUS.bits()
    );

    let replay = dispatch(
        &mut actor,
        submit_context_with_provenance(1, 9),
        envelope(21, submit_request(b"same", 2)),
    );
    assert_eq!(accepted_id(replay.response), ApiSubmissionId(10));
    assert_eq!(actor.state().committed_records(), 1);
    assert_eq!(
        actor
            .index()
            .get(SubmissionId::new(10))
            .unwrap()
            .accepted()
            .authorization(),
        original.authorization(),
        "a rotated retry must preserve the original durable evidence"
    );

    let conflict = dispatch(
        &mut actor,
        submit_context(1),
        envelope(22, submit_request(b"different", 2)),
    );
    assert_eq!(
        error_code(conflict.response),
        ApiErrorCode::IdempotencyConflict
    );
    assert_eq!(actor.state().committed_records(), 1);
}

#[cfg(feature = "experimental-rns-data")]
#[test]
fn identical_idempotency_keys_are_isolated_between_principals() {
    let mut actor = mounted::<2>();
    let first = dispatch(
        &mut actor,
        submit_context(1),
        envelope(23, submit_request(b"same", 2)),
    );
    let second = dispatch(
        &mut actor,
        submit_context(2),
        envelope(24, submit_request(b"same", 2)),
    );
    assert_eq!(accepted_id(first.response), ApiSubmissionId(10));
    assert_eq!(accepted_id(second.response), ApiSubmissionId(11));
    assert_eq!(actor.state().committed_records(), 2);
}

#[cfg(feature = "experimental-rns-data")]
#[test]
fn maximum_advertised_payload_is_durably_accepted() {
    let mut actor = mounted::<1>();
    let response = dispatch(
        &mut actor,
        submit_context(1),
        envelope(25, submit_request(&MAXIMUM_PAYLOAD, 2)),
    );
    assert_eq!(accepted_id(response.response), ApiSubmissionId(10));
    assert_eq!(actor.state().committed_records(), 1);
    assert_eq!(
        actor
            .index()
            .get(SubmissionId::new(10))
            .unwrap()
            .accepted()
            .intent()
            .experimental_rns_data()
            .expect("raw RNS submission retains its intent kind")
            .payload(),
        &MAXIMUM_PAYLOAD
    );
}

#[cfg(feature = "experimental-rns-data")]
#[test]
fn accepted_response_survives_remount_and_status_dispatch() {
    let mut actor = mounted::<2>();
    let accepted = dispatch(
        &mut actor,
        submit_context(1),
        envelope(26, submit_request(b"remount", 2)),
    );
    assert_eq!(accepted_id(accepted.response), ApiSubmissionId(10));

    let flash = actor.into_flash();
    let mut remounted = TestActor::<2>::mount(flash, SubmissionId::new(10));
    assert_eq!(
        remounted
            .index()
            .get(SubmissionId::new(10))
            .unwrap()
            .accepted()
            .authorization(),
        durable_authorization_with_permissions(
            1,
            Permissions::EXPERIMENTAL_SUBMIT_RNS_DATA.bits()
                | Permissions::READ_SUBMISSION_STATUS.bits(),
        )
    );
    let status = dispatch(
        &mut remounted,
        authenticated(1, Permissions::READ_SUBMISSION_STATUS),
        envelope(
            27,
            DeviceRequest::SubmissionStatus {
                id: ApiSubmissionId(10),
            },
        ),
    );
    assert_eq!(
        status.response,
        DeviceResponse::SubmissionStatus(SubmissionStatus {
            id: ApiSubmissionId(10),
            state: SubmissionState::Queued,
        })
    );
}

#[cfg(feature = "experimental-rns-data")]
#[test]
fn invalid_intent_and_actor_capacity_have_stable_api_errors() {
    let mut actor = mounted::<0>();
    let invalid = dispatch(
        &mut actor,
        submit_context(1),
        envelope(30, submit_request(&OVERSIZED_PAYLOAD, 2)),
    );
    assert_eq!(error_code(invalid.response), ApiErrorCode::InvalidRequest);

    let full = dispatch(
        &mut actor,
        submit_context(1),
        envelope(31, submit_request(b"fits", 2)),
    );
    assert_eq!(error_code(full.response), ApiErrorCode::CapacityExhausted);

    assert_eq!(
        error_code(acceptance_response(
            Ok(SubmissionAcceptance::CapacityExhausted),
            api::OP_EXPERIMENTAL_SUBMIT_RNS_DATA,
        )),
        ApiErrorCode::CapacityExhausted
    );
}

#[cfg(feature = "experimental-rns-data")]
#[test]
fn identifier_exhaustion_is_internal_not_retryable_capacity() {
    let flash = FakeNor::formatted();
    let mut actor = TestActor::<2>::mount(flash, SubmissionId::new(u64::MAX));
    let accepted = dispatch(
        &mut actor,
        submit_context(1),
        envelope(40, submit_request(b"last", 2)),
    );
    assert_eq!(accepted_id(accepted.response), ApiSubmissionId(u64::MAX));
    let exhausted = dispatch(
        &mut actor,
        submit_context(1),
        envelope(41, submit_request(b"later", 3)),
    );
    assert_eq!(error_code(exhausted.response), ApiErrorCode::Internal);
}

#[cfg(feature = "experimental-rns-data")]
#[test]
fn lost_write_reply_is_internal_until_autonomous_reconciliation() {
    // Pre-arm the backend before mount. Mount performs only reads, so the
    // second acceptance write commits and then loses its reply exactly once.
    let flash = FakeNor::formatted_with_lost_write_reply(Some(1));
    let mut actor = TestActor::<2>::mount(flash, SubmissionId::new(70));
    let request = envelope(50, submit_request(b"ambiguous", 2));
    let ambiguous = dispatch(&mut actor, submit_context(1), request);
    assert_eq!(error_code(ambiguous.response), ApiErrorCode::Internal);
    assert_eq!(actor.pending_kind(), Some(PendingKind::Acceptance));
    assert_eq!(actor.index().get(SubmissionId::new(70)), None);
    assert_eq!(actor.state().committed_records(), 0);

    let pending_status = dispatch(
        &mut actor,
        authenticated(1, Permissions::READ_SUBMISSION_STATUS),
        envelope(
            52,
            DeviceRequest::SubmissionStatus {
                id: ApiSubmissionId(70),
            },
        ),
    );
    assert_eq!(error_code(pending_status.response), ApiErrorCode::Internal);
    assert_eq!(actor.pending_kind(), Some(PendingKind::Acceptance));

    let busy = dispatch(
        &mut actor,
        submit_context(1),
        envelope(51, submit_request(b"different", 3)),
    );
    assert_eq!(error_code(busy.response), ApiErrorCode::Internal);
    assert_eq!(actor.pending_kind(), Some(PendingKind::Acceptance));

    assert_eq!(
        actor.drive_pending(),
        Ok(PendingProgress::AcceptanceCommitted(SubmissionId::new(70)))
    );
    assert_eq!(actor.pending_kind(), None);
    assert_eq!(actor.state().committed_records(), 1);

    let retry = dispatch(&mut actor, submit_context(1), request);
    assert_eq!(accepted_id(retry.response), ApiSubmissionId(70));
    assert_eq!(actor.state().committed_records(), 1);
}

#[cfg(feature = "experimental-rns-data")]
#[test]
fn wrong_journal_binding_is_internal_and_touches_no_storage() {
    let mut mounted_journal = BoundJournal::new(FakeNor::formatted(), test_binding());
    let actor = StorageActor::<2, 1>::mount(&mut mounted_journal, SubmissionId::new(80)).unwrap();
    let flash = mounted_journal.into_backend();
    let wrong_binding = JournalBinding::new(
        StorageDeviceId::new([0x33; 16]),
        test_binding().absolute_offset(),
        test_binding().length(),
        test_binding().layout_version(),
    );
    let wrong_journal = BoundJournal::new(flash, wrong_binding);

    let mut port = TestActor {
        actor,
        journal: wrong_journal,
        port_calls: 0,
    };
    let response = super::dispatch(
        &mut port,
        identity_summary(),
        &submit_context(1),
        envelope(60, submit_request(b"wrong-backend", 2)),
    );

    assert_eq!(error_code(response.response), ApiErrorCode::Internal);
    assert_eq!(port.index().get(SubmissionId::new(80)), None);
    assert_eq!(port.pending_kind(), None);
    assert_eq!(port.fault(), None);
    let flash = port.into_flash();
    assert_eq!(flash.writes, 0);
    assert_eq!(flash.erases, 0);
}
