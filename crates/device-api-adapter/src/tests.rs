extern crate std;

#[cfg(feature = "nomad")]
use std::string::String;
use std::{
    ops::{Deref, DerefMut},
    vec,
    vec::Vec,
};

use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
    check_erase, check_read, check_write,
};
#[cfg(any(feature = "rns-data", feature = "lxmf"))]
use reticulum_device_api::DestinationHash as ApiDestinationHash;
#[cfg(any(feature = "rns-data", feature = "lxmf"))]
use reticulum_device_api::IdempotencyKey as ApiIdempotencyKey;
use reticulum_device_api::{
    ApiErrorCode, ApiErrorResponse, ApiVersion, CapabilitySnapshot, DeviceRequest, DeviceResponse,
    DispatchContext, DispatchProvenance, OP_SUBMISSION_STATUS, Permissions,
    PrincipalId as ApiPrincipalId, RequestEnvelope, RequestId, SubmissionId as ApiSubmissionId,
};
use reticulum_storage_actor::{
    AcceptanceProgress, BoundJournal, DriveError, JournalBinding, StorageActor, StorageDeviceId,
};
#[cfg(feature = "rns-data")]
use reticulum_storage_actor::{PendingKind, PendingProgress};
use reticulum_storage_journal::{PHYSICAL_FORMAT_VERSION, format_erased};
use reticulum_storage_model::{
    AUTHORIZATION_PERMISSION_SUBMIT_RNS_DATA, AcceptanceCandidate, AuthorizationSnapshot,
    BootRecoveryMarker, DestinationHash, EncodedPacketSha256, FinalDisposition, IdempotencyKey,
    InternalFailure, InterruptedState, LifecycleState,
    PreparedPacketDetails as DurablePreparedPacketDetails, PrincipalId, RnsAttemptToken,
    RnsDataIntent, SubmissionFailure as DurableSubmissionFailure, SubmissionId,
};

use super::*;

const PARTITION_SIZE: usize = 0x10_0000;
const ERASE_SIZE: usize = 0x1000;
#[cfg(feature = "rns-data")]
static OVERSIZED_PAYLOAD: [u8; api::MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES + 1] =
    [0x44; api::MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES + 1];
#[cfg(feature = "rns-data")]
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

    #[cfg(feature = "rns-data")]
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

    #[cfg(feature = "rns-data")]
    fn drive_pending(&mut self) -> Result<PendingProgress, DriveError<FakeError>> {
        self.actor.drive_pending(&mut self.journal)
    }

    #[cfg(feature = "rns-data")]
    fn into_flash(self) -> FakeNor {
        self.journal.into_backend()
    }

    #[cfg(feature = "rns-data")]
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

#[cfg(feature = "nomad")]
#[derive(Clone, Debug, Eq, PartialEq)]
struct ObservedNomadStart {
    principal: api::PrincipalId,
    destination: api::DestinationHash,
    path: String,
    timestamp_unix_ms: u64,
    idempotency_key: api::IdempotencyKey,
}

#[cfg(feature = "nomad")]
struct FakeNomadPort {
    availability: CapabilityAvailability,
    start_result: Result<NomadFetchStartDisposition, NomadFetchPortError>,
    poll_result: Result<Option<api::NomadFetchPollResponse>, NomadFetchPortError>,
    availability_calls: usize,
    start_calls: usize,
    poll_calls: usize,
    observed_start: Option<ObservedNomadStart>,
    observed_poll: Option<(api::PrincipalId, api::NomadFetchId)>,
}

#[cfg(feature = "nomad")]
impl FakeNomadPort {
    fn available(id: api::NomadFetchId) -> Self {
        Self {
            availability: CapabilityAvailability::Available,
            start_result: Ok(NomadFetchStartDisposition::Accepted(id)),
            poll_result: Ok(Some(api::NomadFetchPollResponse::Pending(
                api::NomadFetchPhase::AwaitingResponse,
            ))),
            availability_calls: 0,
            start_calls: 0,
            poll_calls: 0,
            observed_start: None,
            observed_poll: None,
        }
    }
}

#[cfg(feature = "nomad")]
impl NomadFetchPort for FakeNomadPort {
    fn availability(&mut self) -> CapabilityAvailability {
        self.availability_calls += 1;
        self.availability
    }

    fn start(
        &mut self,
        principal: api::PrincipalId,
        request: api::NomadFetchStartRequest<'_>,
    ) -> Result<NomadFetchStartDisposition, NomadFetchPortError> {
        self.start_calls += 1;
        self.observed_start = Some(ObservedNomadStart {
            principal,
            destination: request.destination(),
            path: request.path().as_str().into(),
            timestamp_unix_ms: request.timestamp_unix_ms().get(),
            idempotency_key: request.idempotency_key(),
        });
        self.start_result
    }

    fn poll(
        &mut self,
        principal: api::PrincipalId,
        id: api::NomadFetchId,
    ) -> Result<Option<api::NomadFetchPollResponse>, NomadFetchPortError> {
        self.poll_calls += 1;
        self.observed_poll = Some((principal, id));
        self.poll_result
    }
}

#[cfg(feature = "lxmf")]
#[derive(Debug, Default, Eq, PartialEq)]
struct FakeLxmfOnlyPort {
    submission_availability: usize,
    lxmf_availability: usize,
    lxmf_next: usize,
    lxmf_mailbox_status: usize,
    lxmf_mailbox_acknowledge: usize,
    observed_mailbox_acknowledgement: Option<api::LxmfMessageHandle>,
    compose_availability: usize,
    peer_availability: usize,
    peer_max_app_data: usize,
    peer_next: usize,
    observed_peer_cursor: Option<Option<api::LxmfPeerDiscoveryCursor>>,
    #[cfg(all(feature = "nomad", feature = "network-config"))]
    network_availability: usize,
    #[cfg(all(feature = "nomad", feature = "network-config"))]
    network_configuration: usize,
    #[cfg(all(feature = "nomad", feature = "network-config"))]
    network_mutate: usize,
    #[cfg(all(feature = "nomad", feature = "network-config"))]
    network_status: usize,
    #[cfg(all(feature = "nomad", feature = "network-config"))]
    manual_announce_availability: usize,
    #[cfg(all(feature = "nomad", feature = "network-config"))]
    manual_announce_queue: usize,
    #[cfg(all(feature = "nomad", feature = "network-config"))]
    node_diagnostics: usize,
    #[cfg(all(feature = "nomad", feature = "network-config"))]
    route_diagnostics: usize,
    #[cfg(all(feature = "nomad", feature = "network-config"))]
    observed_route_cursor: Option<Option<api::DestinationHash>>,
    #[cfg(all(feature = "nomad", feature = "network-config"))]
    radio_trace: usize,
    #[cfg(all(feature = "nomad", feature = "network-config"))]
    observed_radio_trace_cursor: Option<Option<api::RadioTraceCursor>>,
}

#[cfg(feature = "lxmf")]
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

#[cfg(feature = "lxmf")]
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

    fn mailbox_status(&mut self) -> Result<api::LxmfMailboxStatus, LxmfInboxPortError> {
        self.lxmf_mailbox_status += 1;
        api::LxmfMailboxStatus::new(
            Some(api::LxmfMessageHandle::new(9).unwrap()),
            Some(api::LxmfMessageHandle::new(7).unwrap()),
        )
        .map_err(|_| LxmfInboxPortError::Faulted)
    }

    fn acknowledge_mailbox_through(
        &mut self,
        through: api::LxmfMessageHandle,
    ) -> Result<api::LxmfMailboxStatus, LxmfInboxPortError> {
        self.lxmf_mailbox_acknowledge += 1;
        self.observed_mailbox_acknowledgement = Some(through);
        api::LxmfMailboxStatus::new(Some(api::LxmfMessageHandle::new(9).unwrap()), Some(through))
            .map_err(|_| LxmfInboxPortError::InvalidRequest)
    }
}

#[cfg(feature = "lxmf")]
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

#[cfg(feature = "lxmf")]
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

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
impl NetworkConfigPort for FakeLxmfOnlyPort {
    fn availability(&mut self) -> CapabilityAvailability {
        self.network_availability += 1;
        CapabilityAvailability::Available
    }

    fn configuration(&mut self) -> Result<api::NetworkConfigSnapshot, NetworkConfigPortError> {
        self.network_configuration += 1;
        Ok(network_config_snapshot())
    }

    fn mutate(
        &mut self,
        _principal: api::PrincipalId,
        _request: api::NetworkConfigMutationRequest<'_>,
    ) -> Result<api::NetworkConfigMutationOutcome, NetworkConfigPortError> {
        self.network_mutate += 1;
        Ok(api::NetworkConfigMutationOutcome::Applied {
            revision: 8,
            reboot_required: true,
        })
    }

    fn status(&mut self) -> Result<api::NetworkRuntimeStatus, NetworkConfigPortError> {
        self.network_status += 1;
        Ok(network_runtime_status())
    }
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
impl ManualServiceAnnouncePort for FakeLxmfOnlyPort {
    fn availability(&mut self) -> CapabilityAvailability {
        self.manual_announce_availability += 1;
        CapabilityAvailability::Available
    }

    fn queue_service_announce(&mut self) -> api::ManualServiceAnnounceDisposition {
        self.manual_announce_queue += 1;
        api::ManualServiceAnnounceDisposition::Queued
    }
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
fn node_diagnostics_snapshot() -> api::NodeDiagnosticsSnapshot {
    api::NodeDiagnosticsSnapshot::new(
        1_000,
        [
            Some(api::DiagnosticInterfaceRecord::new(
                1,
                api::DiagnosticInterfaceKind::LoRa,
                api::DiagnosticInterfaceState::Online,
                3,
                500,
                Some(125_000),
            )),
            None,
            None,
            None,
        ],
        None,
        api::RnsDiagnostics::new(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11),
        2,
        4,
        3,
    )
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
fn route_diagnostics_page() -> api::RouteDiagnosticsPage {
    let entry = api::RouteDiagnosticEntry::new(
        api::DestinationHash([0x31; 16]),
        Some(api::IdentityHash::new([0x41; 16])),
        2,
        Some(1),
        api::RouteDiagnosticResolution::ExactReady,
        Some(100),
        Some(50),
        Some(30_000),
    );
    api::RouteDiagnosticsPage::new(13, 1, [Some(entry), None, None, None], None).unwrap()
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
fn radio_trace_page() -> api::RadioTracePage {
    api::RadioTracePage::new(
        99,
        api::RadioTraceAppliedLoraProfile::new(
            [0x91; 16],
            915_000_000,
            125_000,
            12,
            22,
            10,
            5,
            true,
            true,
            false,
        ),
        7,
        7,
        false,
        [None; api::MAX_RADIO_TRACE_PAGE_ENTRIES],
        None,
    )
    .unwrap()
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
impl NodeDiagnosticsPort for FakeLxmfOnlyPort {
    fn node_diagnostics(
        &mut self,
    ) -> Result<api::NodeDiagnosticsSnapshot, NodeDiagnosticsPortError> {
        self.node_diagnostics += 1;
        Ok(node_diagnostics_snapshot())
    }

    fn route_diagnostics_page(
        &mut self,
        request: api::RouteDiagnosticsRequest,
    ) -> Result<api::RouteDiagnosticsPage, NodeDiagnosticsPortError> {
        self.route_diagnostics += 1;
        self.observed_route_cursor = Some(request.after());
        Ok(route_diagnostics_page())
    }

    fn radio_trace_page(
        &mut self,
        request: api::RadioTracePageRequest,
    ) -> Result<api::RadioTracePage, NodeDiagnosticsPortError> {
        self.radio_trace += 1;
        self.observed_radio_trace_cursor = Some(request.after());
        Ok(radio_trace_page())
    }
}

#[cfg(feature = "lxmf")]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct CombinedPortCalls {
    submission_availability: usize,
    submission_status: usize,
    submission_accept: usize,
    lxmf_availability: usize,
    lxmf_next: usize,
    lxmf_read: usize,
    lxmf_mailbox_status: usize,
    lxmf_mailbox_acknowledge: usize,
    compose_availability: usize,
    compose_and_accept: usize,
}

#[cfg(feature = "lxmf")]
#[derive(Clone, Debug, Eq, PartialEq)]
struct ObservedLxmfComposeRequest {
    principal: PrincipalId,
    destination: DestinationHash,
    timestamp_unix_ms: u64,
    title: Vec<u8>,
    content: Vec<u8>,
    location: Option<api::LxmfMessageLocation>,
    idempotency_key: IdempotencyKey,
    authorization: AuthorizationSnapshot,
}

#[cfg(feature = "lxmf")]
struct FakeCombinedPort {
    calls: CombinedPortCalls,
    lxmf_availability: CapabilityAvailability,
    lxmf_next: Result<Option<api::LxmfMessageSummary>, LxmfInboxPortError>,
    lxmf_read: Result<Option<api::LxmfReadChunk>, LxmfInboxPortError>,
    compose_availability: CapabilityAvailability,
    compose_result: Result<LxmfComposeAcceptance, LxmfComposePortError>,
    observed_compose: Option<ObservedLxmfComposeRequest>,
}

#[cfg(feature = "lxmf")]
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

#[cfg(feature = "lxmf")]
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

#[cfg(feature = "lxmf")]
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

    fn mailbox_status(&mut self) -> Result<api::LxmfMailboxStatus, LxmfInboxPortError> {
        self.calls.lxmf_mailbox_status += 1;
        api::LxmfMailboxStatus::new(None, None).map_err(|_| LxmfInboxPortError::Faulted)
    }

    fn acknowledge_mailbox_through(
        &mut self,
        through: api::LxmfMessageHandle,
    ) -> Result<api::LxmfMailboxStatus, LxmfInboxPortError> {
        self.calls.lxmf_mailbox_acknowledge += 1;
        api::LxmfMailboxStatus::new(Some(through), Some(through))
            .map_err(|_| LxmfInboxPortError::Faulted)
    }
}

#[cfg(feature = "lxmf")]
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
            location: request.location(),
            idempotency_key: request.idempotency_key(),
            authorization: request.authorization(),
        });
        self.compose_result
    }
}

#[cfg(feature = "lxmf")]
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

#[cfg(feature = "lxmf")]
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
    durable_authorization_with_permissions(tag, AUTHORIZATION_PERMISSION_SUBMIT_RNS_DATA)
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

#[cfg(feature = "nomad")]
fn nomad_id() -> api::NomadFetchId {
    api::NomadFetchId::new([0xa5; 8], 7).unwrap()
}

#[cfg(feature = "nomad")]
fn nomad_start_request() -> api::NomadFetchStartRequest<'static> {
    api::NomadFetchStartRequest::new(
        api::DestinationHash([0x22; 16]),
        api::NomadPagePath::new("/status").unwrap(),
        api::NomadRequestTimestampUnixMs::new(1_700_000_000_123).unwrap(),
        api::IdempotencyKey([0x33; 16]),
    )
}

#[cfg(feature = "nomad")]
#[test]
fn nomad_dispatch_advertises_independent_runtime_limits_and_isolates_public_reads() {
    let mut submission = UnavailablePort::default();
    let mut nomad = FakeNomadPort::available(nomad_id());
    nomad.availability = CapabilityAvailability::Disabled;
    let response = super::dispatch_with_nomad(
        &mut submission,
        &mut nomad,
        identity_summary(),
        &DispatchContext::UNAUTHENTICATED,
        envelope(200, DeviceRequest::SystemCapabilities),
    );
    assert_eq!(
        response.response,
        DeviceResponse::SystemCapabilities(CapabilitySnapshot::for_dispatch_with_nomad(
            false,
            CapabilityAvailability::Disabled
        ))
    );
    assert_eq!(
        submission.availability_calls,
        usize::from(cfg!(feature = "rns-data"))
    );
    assert_eq!(nomad.availability_calls, 1);
    assert_eq!(nomad.start_calls, 0);
    assert_eq!(nomad.poll_calls, 0);

    let identity = super::dispatch_with_nomad(
        &mut submission,
        &mut nomad,
        identity_summary(),
        &DispatchContext::UNAUTHENTICATED,
        envelope(201, DeviceRequest::IdentitySummary),
    );
    assert_eq!(
        identity.response,
        DeviceResponse::IdentitySummary(identity_summary())
    );
    assert_eq!(
        submission.availability_calls,
        usize::from(cfg!(feature = "rns-data"))
    );
    assert_eq!(nomad.availability_calls, 1);
}

#[cfg(feature = "nomad")]
#[test]
fn nomad_start_is_authenticated_principal_scoped_and_distinguishes_replay() {
    let request = DeviceRequest::NomadFetchStart(nomad_start_request());
    let mut submission = UnavailablePort::default();
    let mut nomad = FakeNomadPort::available(nomad_id());
    let unauthenticated = super::dispatch_with_nomad(
        &mut submission,
        &mut nomad,
        identity_summary(),
        &DispatchContext::UNAUTHENTICATED,
        envelope(202, request),
    );
    assert_eq!(
        error_code(unauthenticated.response),
        ApiErrorCode::AuthenticationRequired
    );
    assert_eq!(nomad.availability_calls, 0);
    assert_eq!(nomad.start_calls, 0);

    let accepted = super::dispatch_with_nomad(
        &mut submission,
        &mut nomad,
        identity_summary(),
        &authenticated(7, Permissions::NONE),
        envelope(203, request),
    );
    assert_eq!(
        accepted.response,
        DeviceResponse::NomadFetchStartAccepted(api::NomadFetchStartAccepted {
            id: nomad_id(),
            outcome: api::NomadFetchStartOutcome::Accepted,
        })
    );
    assert_eq!(
        nomad.observed_start,
        Some(ObservedNomadStart {
            principal: api::PrincipalId([7; 16]),
            destination: api::DestinationHash([0x22; 16]),
            path: "/status".into(),
            timestamp_unix_ms: 1_700_000_000_123,
            idempotency_key: api::IdempotencyKey([0x33; 16]),
        })
    );

    nomad.start_result = Ok(NomadFetchStartDisposition::Replay(nomad_id()));
    let replayed = super::dispatch_with_nomad(
        &mut submission,
        &mut nomad,
        identity_summary(),
        &authenticated(7, Permissions::NONE),
        envelope(204, request),
    );
    assert_eq!(
        replayed.response,
        DeviceResponse::NomadFetchStartAccepted(api::NomadFetchStartAccepted {
            id: nomad_id(),
            outcome: api::NomadFetchStartOutcome::Replayed,
        })
    );

    nomad.start_result = Ok(NomadFetchStartDisposition::IdempotencyConflict);
    let conflict = super::dispatch_with_nomad(
        &mut submission,
        &mut nomad,
        identity_summary(),
        &authenticated(7, Permissions::NONE),
        envelope(205, request),
    );
    assert_eq!(
        error_code(conflict.response),
        ApiErrorCode::IdempotencyConflict
    );

    nomad.start_result = Ok(NomadFetchStartDisposition::CapacityExhausted);
    let full = super::dispatch_with_nomad(
        &mut submission,
        &mut nomad,
        identity_summary(),
        &authenticated(7, Permissions::NONE),
        envelope(206, request),
    );
    assert_eq!(error_code(full.response), ApiErrorCode::CapacityExhausted);
}

#[cfg(feature = "nomad")]
#[test]
fn nomad_poll_hides_foreign_ids_and_maps_port_failures() {
    let mut submission = UnavailablePort::default();
    let mut nomad = FakeNomadPort::available(nomad_id());
    let request = DeviceRequest::NomadFetchPoll(api::NomadFetchPollRequest { id: nomad_id() });
    let pending = super::dispatch_with_nomad(
        &mut submission,
        &mut nomad,
        identity_summary(),
        &authenticated(9, Permissions::NONE),
        envelope(207, request),
    );
    assert_eq!(
        pending.response,
        DeviceResponse::NomadFetchPoll(api::NomadFetchPollResponse::Pending(
            api::NomadFetchPhase::AwaitingResponse
        ))
    );
    assert_eq!(
        nomad.observed_poll,
        Some((api::PrincipalId([9; 16]), nomad_id()))
    );

    nomad.poll_result = Ok(None);
    let missing = super::dispatch_with_nomad(
        &mut submission,
        &mut nomad,
        identity_summary(),
        &authenticated(9, Permissions::NONE),
        envelope(208, request),
    );
    assert_eq!(error_code(missing.response), ApiErrorCode::NotFound);

    nomad.poll_result = Err(NomadFetchPortError::InvalidRequest);
    let invalid = super::dispatch_with_nomad(
        &mut submission,
        &mut nomad,
        identity_summary(),
        &authenticated(9, Permissions::NONE),
        envelope(209, request),
    );
    assert_eq!(error_code(invalid.response), ApiErrorCode::InvalidRequest);

    nomad.poll_result = Err(NomadFetchPortError::Backend);
    let failed = super::dispatch_with_nomad(
        &mut submission,
        &mut nomad,
        identity_summary(),
        &authenticated(9, Permissions::NONE),
        envelope(210, request),
    );
    assert_eq!(error_code(failed.response), ApiErrorCode::Internal);
}

#[cfg(all(feature = "lxmf", feature = "nomad"))]
#[test]
fn full_appliance_dispatch_composes_existing_capabilities_with_nomad() {
    let mut appliance = FakeLxmfOnlyPort::default();
    let mut nomad = FakeNomadPort::available(nomad_id());
    let response = super::dispatch_with_lxmf_peer_discovery_and_nomad(
        &mut appliance,
        &mut nomad,
        identity_summary(),
        &DispatchContext::UNAUTHENTICATED,
        envelope(211, DeviceRequest::SystemCapabilities),
    );
    let expected = CapabilitySnapshot::for_dispatch_with_lxmf_basic_send_and_peer_discovery(
        cfg!(feature = "rns-data"),
        CapabilityAvailability::Available,
        CapabilityAvailability::Available,
        CapabilityAvailability::Available,
        64,
    )
    .with_dispatch_nomad(CapabilityAvailability::Available);
    assert_eq!(
        response.response,
        DeviceResponse::SystemCapabilities(expected)
    );
    assert_eq!(
        expected.lxmf_peer_discovery(),
        CapabilityAvailability::Available
    );
    assert_eq!(expected.nomad(), CapabilityAvailability::Available);
    assert_eq!(
        expected.manual_service_announce(),
        CapabilityAvailability::Unavailable
    );
    assert_eq!(
        expected.max_nomad_page_path_bytes(),
        api::MAX_NOMAD_PAGE_PATH_BYTES as u16
    );
    assert_eq!(
        expected.max_nomad_page_bytes(),
        api::MAX_NOMAD_PAGE_BYTES as u16
    );

    let manual = super::dispatch_with_lxmf_peer_discovery_and_nomad(
        &mut appliance,
        &mut nomad,
        identity_summary(),
        &authenticated(11, Permissions::NONE),
        envelope(212, DeviceRequest::ManualServiceAnnounce),
    );
    assert_eq!(
        error_code(manual.response),
        ApiErrorCode::UnsupportedOperation
    );
}

fn durable_candidate(principal: u8, key: u8, payload: &[u8]) -> AcceptanceCandidate {
    AcceptanceCandidate::new(
        PrincipalId::new([principal; 16]),
        IdempotencyKey::new([key; 16]),
        RnsDataIntent::new(DestinationHash::new([0x33; 16]), payload).unwrap(),
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
            feature = "rns-data"
        )))
    );
    let DeviceResponse::SystemCapabilities(capabilities) = response.response else {
        panic!("expected capabilities response")
    };
    assert_eq!(
        capabilities.submit_rns_data(),
        cfg!(feature = "rns-data"),
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

#[test]
fn minimal_dispatch_reports_diagnostics_as_unsupported_after_authentication() {
    for request in [
        DeviceRequest::NodeDiagnostics,
        DeviceRequest::RouteDiagnosticsPage(api::RouteDiagnosticsRequest::new(None)),
    ] {
        let operation = request.operation();
        let mut port = UnavailablePort::default();
        let response = super::dispatch(
            &mut port,
            identity_summary(),
            &authenticated(1, Permissions::NONE),
            envelope(111, request),
        );
        assert_eq!(
            response.response,
            DeviceResponse::Error(ApiErrorResponse {
                code: ApiErrorCode::UnsupportedOperation,
                operation: Some(operation),
            })
        );
        assert_eq!(port.availability_calls, 0);
        assert_eq!(port.status_calls, 0);
        assert_eq!(port.acceptance_calls, 0);
    }
}

#[cfg(feature = "lxmf")]
#[test]
fn lxmf_dispatcher_uses_only_lxmf_ports() {
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
            CapabilitySnapshot::for_dispatch_with_lxmf_and_basic_send(
                cfg!(feature = "rns-data"),
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
        usize::from(cfg!(feature = "rns-data"))
    );
}

#[cfg(feature = "lxmf")]
#[test]
fn lxmf_mailbox_dispatch_is_authenticated_and_uses_the_lxmf_owner_only() {
    let latest = api::LxmfMessageHandle::new(9).unwrap();
    let mut port = FakeLxmfOnlyPort::default();
    let unauthenticated = super::dispatch_with_lxmf(
        &mut port,
        identity_summary(),
        &DispatchContext::UNAUTHENTICATED,
        envelope(113, DeviceRequest::LxmfMailboxStatus),
    );
    assert_eq!(
        error_code(unauthenticated.response),
        ApiErrorCode::AuthenticationRequired
    );
    assert_eq!(port, FakeLxmfOnlyPort::default());

    let status = super::dispatch_with_lxmf(
        &mut port,
        identity_summary(),
        &authenticated(1, Permissions::NONE),
        envelope(114, DeviceRequest::LxmfMailboxStatus),
    );
    let DeviceResponse::LxmfMailboxStatus(status) = status.response else {
        panic!("expected durable mailbox status")
    };
    assert_eq!(status.latest(), Some(latest));
    assert_eq!(status.uncollected_count(), 2);
    assert_eq!(port.lxmf_availability, 1);
    assert_eq!(port.lxmf_mailbox_status, 1);
    assert_eq!(port.lxmf_next, 0);

    let acknowledged = super::dispatch_with_lxmf(
        &mut port,
        identity_summary(),
        &authenticated(1, Permissions::NONE),
        envelope(
            115,
            DeviceRequest::LxmfMailboxAcknowledge { through: latest },
        ),
    );
    let DeviceResponse::LxmfMailboxAcknowledged(acknowledged) = acknowledged.response else {
        panic!("expected durable mailbox acknowledgement")
    };
    assert_eq!(acknowledged.acknowledged_through(), Some(latest));
    assert_eq!(acknowledged.uncollected_count(), 0);
    assert_eq!(port.lxmf_availability, 2);
    assert_eq!(port.lxmf_mailbox_acknowledge, 1);
    assert_eq!(port.observed_mailbox_acknowledgement, Some(latest));
}

#[cfg(feature = "lxmf")]
#[test]
fn nearby_peer_dispatch_is_authenticated_and_requires_peer_discovery_composition() {
    let request = envelope(113, DeviceRequest::LxmfPeerNext { after: None });

    let mut basic_port = FakeLxmfOnlyPort::default();
    let basic = super::dispatch_with_lxmf(
        &mut basic_port,
        identity_summary(),
        &authenticated(1, Permissions::NONE),
        request,
    );
    assert_eq!(
        basic.response,
        DeviceResponse::Error(ApiErrorResponse {
            code: ApiErrorCode::UnsupportedOperation,
            operation: Some(api::OP_LXMF_PEER_NEXT),
        })
    );
    assert_eq!(basic_port.peer_availability, 0);
    assert_eq!(basic_port.peer_next, 0);

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
            operation: Some(api::OP_LXMF_PEER_NEXT),
        })
    );
    assert_eq!(unauthenticated_port.peer_availability, 0);
    assert_eq!(unauthenticated_port.peer_next, 0);
}

#[cfg(feature = "lxmf")]
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
            CapabilitySnapshot::for_dispatch_with_lxmf_basic_send_and_peer_discovery(
                cfg!(feature = "rns-data"),
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

#[cfg(feature = "lxmf")]
#[test]
fn combined_identity_and_authentication_preflight_acquire_no_ports() {
    let expected = api::IdentitySummary::with_lxmf_delivery_destination(
        api::DestinationHash([0xa5; 16]),
        api::DestinationHash([0xb6; 16]),
    );
    let mut port = FakeCombinedPort::with_lxmf(Some(lxmf_summary()), Some(lxmf_chunk()));
    let identity = super::dispatch_with_lxmf(
        &mut port,
        expected,
        &authenticated(1, Permissions::NONE),
        envelope(220, DeviceRequest::IdentitySummary),
    );
    assert_eq!(identity.response, DeviceResponse::IdentitySummary(expected));
    assert_eq!(port.calls, CombinedPortCalls::default());

    let unauthenticated = super::dispatch_with_lxmf(
        &mut port,
        expected,
        &DispatchContext::UNAUTHENTICATED,
        envelope(221, DeviceRequest::LxmfNext { after: None }),
    );
    assert_eq!(
        unauthenticated.response,
        DeviceResponse::Error(ApiErrorResponse {
            code: ApiErrorCode::AuthenticationRequired,
            operation: Some(api::OP_LXMF_NEXT),
        })
    );
    assert_eq!(port.calls, CombinedPortCalls::default());
}

#[cfg(feature = "lxmf")]
#[test]
fn combined_capabilities_query_each_availability_once() {
    let mut port = FakeCombinedPort::with_lxmf(None, None);
    let response = super::dispatch_with_lxmf(
        &mut port,
        identity_summary(),
        &DispatchContext::UNAUTHENTICATED,
        envelope(222, DeviceRequest::SystemCapabilities),
    );
    assert_eq!(
        response.response,
        DeviceResponse::SystemCapabilities(
            CapabilitySnapshot::for_dispatch_with_lxmf_and_basic_send(
                cfg!(feature = "rns-data"),
                CapabilityAvailability::Available,
                CapabilityAvailability::Available,
            ),
        )
    );
    assert_eq!(
        port.calls,
        CombinedPortCalls {
            submission_availability: usize::from(cfg!(feature = "rns-data")),
            lxmf_availability: 1,
            compose_availability: 1,
            ..CombinedPortCalls::default()
        }
    );
}

#[cfg(feature = "lxmf")]
#[test]
fn combined_lxmf_next_and_read_enter_only_the_selected_port_method() {
    let summary = lxmf_summary();
    let chunk = lxmf_chunk();
    let mut next_port = FakeCombinedPort::with_lxmf(Some(summary), Some(chunk));
    let next = super::dispatch_with_lxmf(
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
    let read = super::dispatch_with_lxmf(
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

#[cfg(feature = "lxmf")]
#[test]
fn combined_lxmf_empty_unavailable_and_port_failures_map_closed() {
    let handle = api::LxmfMessageHandle::new(7).unwrap();
    let request = DeviceRequest::LxmfNext {
        after: Some(handle),
    };
    let mut empty = FakeCombinedPort::with_lxmf(None, None);
    let response = super::dispatch_with_lxmf(
        &mut empty,
        identity_summary(),
        &authenticated(1, Permissions::NONE),
        envelope(225, request),
    );
    assert_eq!(error_code(response.response), ApiErrorCode::NotFound);

    let mut disabled = FakeCombinedPort::with_lxmf(None, None);
    disabled.lxmf_availability = CapabilityAvailability::Disabled;
    let response = super::dispatch_with_lxmf(
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
        (LxmfInboxPortError::Busy, ApiErrorCode::RetryLater),
        (LxmfInboxPortError::Backend, ApiErrorCode::Internal),
        (LxmfInboxPortError::Binding, ApiErrorCode::Internal),
        (LxmfInboxPortError::Faulted, ApiErrorCode::Internal),
    ] {
        let mut port = FakeCombinedPort::with_lxmf(None, None);
        port.lxmf_next = Err(port_error);
        let response = super::dispatch_with_lxmf(
            &mut port,
            identity_summary(),
            &authenticated(1, Permissions::NONE),
            envelope(227, request),
        );
        assert_eq!(error_code(response.response), api_error);
    }
}

#[cfg(feature = "lxmf")]
fn basic_lxmf_send_request() -> DeviceRequest<'static> {
    let location = api::LxmfMessageLocation::new(
        44_123_456,
        -73_987_654,
        12_345,
        678,
        27_050,
        321,
        1_753_141_234,
    )
    .expect("fixture coordinates are valid");
    DeviceRequest::LxmfBasicSend {
        destination: ApiDestinationHash([0x28; 16]),
        timestamp_unix_ms: 1_753_141_234_567,
        title: b"title",
        content: b"content",
        location: Some(location),
        idempotency_key: ApiIdempotencyKey([0x38; 16]),
    }
}

#[cfg(feature = "lxmf")]
#[test]
fn combined_basic_lxmf_send_derives_provenance_and_enters_only_compose_port() {
    let permissions = Permissions::SUBMIT_RNS_DATA | Permissions::READ_SUBMISSION_STATUS;
    for acceptance in [
        SubmissionAcceptance::Accepted(SubmissionId::new(41)),
        SubmissionAcceptance::Replay(SubmissionId::new(41)),
    ] {
        let mut port = FakeCombinedPort::with_lxmf(None, None);
        port.compose_result = Ok(LxmfComposeAcceptance::new(acceptance, [0x67; 32]));
        let response = super::dispatch_with_lxmf(
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
                location: Some(
                    api::LxmfMessageLocation::new(
                        44_123_456,
                        -73_987_654,
                        12_345,
                        678,
                        27_050,
                        321,
                        1_753_141_234,
                    )
                    .expect("fixture coordinates are valid"),
                ),
                idempotency_key: IdempotencyKey::new([0x38; 16]),
                authorization: durable_authorization_with_permissions(5, permissions.bits()),
            })
        );
    }
}

#[cfg(feature = "lxmf")]
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
            authenticated(6, Permissions::SUBMIT_RNS_DATA),
            ApiVersion {
                major: ApiVersion::CURRENT.major + 1,
                minor: 0,
            },
            ApiErrorCode::UnsupportedVersion,
        ),
    ];
    for (context, version, expected) in cases {
        let mut port = FakeCombinedPort::with_lxmf(None, None);
        let response = super::dispatch_with_lxmf(
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

#[cfg(feature = "lxmf")]
#[test]
fn combined_basic_lxmf_send_maps_unavailability_and_closed_port_errors() {
    let context = authenticated(7, Permissions::SUBMIT_RNS_DATA);
    let mut disabled = FakeCombinedPort::with_lxmf(None, None);
    disabled.compose_availability = CapabilityAvailability::Disabled;
    let response = super::dispatch_with_lxmf(
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
        (LxmfComposePortError::Busy, ApiErrorCode::RetryLater),
        (LxmfComposePortError::Backend, ApiErrorCode::Internal),
        (LxmfComposePortError::Binding, ApiErrorCode::Internal),
        (LxmfComposePortError::Faulted, ApiErrorCode::Internal),
        (LxmfComposePortError::Invariant, ApiErrorCode::Internal),
    ] {
        let mut port = FakeCombinedPort::with_lxmf(None, None);
        port.compose_result = Err(port_error);
        let response = super::dispatch_with_lxmf(
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

#[cfg(feature = "lxmf")]
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
        let response = super::dispatch_with_lxmf(
            &mut port,
            identity_summary(),
            &authenticated(8, Permissions::SUBMIT_RNS_DATA),
            envelope(232, basic_lxmf_send_request()),
        );
        assert_eq!(error_code(response.response), expected);
        assert_eq!(port.calls.compose_and_accept, 1);
        assert_eq!(port.calls.submission_accept, 0);
    }
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
            feature = "rns-data"
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

#[cfg(feature = "rns-data")]
fn submit_request(payload: &[u8], key: u8) -> DeviceRequest<'_> {
    DeviceRequest::SubmitRnsData {
        destination: ApiDestinationHash([0x33; 16]),
        payload,
        idempotency_key: ApiIdempotencyKey([key; 16]),
    }
}

#[cfg(feature = "rns-data")]
fn submit_context(principal: u8) -> DispatchContext {
    submit_context_with_provenance(principal, principal)
}

#[cfg(feature = "rns-data")]
fn submit_context_with_provenance(principal: u8, provenance_tag: u8) -> DispatchContext {
    DispatchContext::authenticated(
        ApiPrincipalId([principal; 16]),
        Permissions::SUBMIT_RNS_DATA | Permissions::READ_SUBMISSION_STATUS,
        dispatch_provenance(provenance_tag),
    )
}

#[cfg(feature = "rns-data")]
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

#[cfg(feature = "rns-data")]
fn accepted_id(response: DeviceResponse) -> ApiSubmissionId {
    match response {
        DeviceResponse::SubmitRnsDataAccepted(accepted) => accepted.id,
        other => panic!("expected accepted response, received {other:?}"),
    }
}

#[cfg(feature = "rns-data")]
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

#[cfg(feature = "rns-data")]
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

#[cfg(feature = "rns-data")]
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
            .rns_data()
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
        Permissions::SUBMIT_RNS_DATA.bits() | Permissions::READ_SUBMISSION_STATUS.bits()
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

#[cfg(feature = "rns-data")]
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

#[cfg(feature = "rns-data")]
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
            .rns_data()
            .expect("raw RNS submission retains its intent kind")
            .payload(),
        &MAXIMUM_PAYLOAD
    );
}

#[cfg(feature = "rns-data")]
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
            Permissions::SUBMIT_RNS_DATA.bits() | Permissions::READ_SUBMISSION_STATUS.bits(),
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

#[cfg(feature = "rns-data")]
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
            api::OP_SUBMIT_RNS_DATA,
        )),
        ApiErrorCode::CapacityExhausted
    );
}

#[cfg(feature = "rns-data")]
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

#[cfg(feature = "rns-data")]
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
    assert_eq!(
        error_code(pending_status.response),
        ApiErrorCode::RetryLater
    );
    assert_eq!(actor.pending_kind(), Some(PendingKind::Acceptance));

    let busy = dispatch(
        &mut actor,
        submit_context(1),
        envelope(51, submit_request(b"different", 3)),
    );
    assert_eq!(error_code(busy.response), ApiErrorCode::RetryLater);
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

#[cfg(feature = "rns-data")]
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

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct NetworkConfigPortCalls {
    availability: usize,
    configuration: usize,
    mutate: usize,
    status: usize,
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FakeManualServiceAnnouncePort {
    availability: CapabilityAvailability,
    disposition: api::ManualServiceAnnounceDisposition,
    availability_calls: usize,
    queue_calls: usize,
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
impl FakeManualServiceAnnouncePort {
    const fn available(disposition: api::ManualServiceAnnounceDisposition) -> Self {
        Self {
            availability: CapabilityAvailability::Available,
            disposition,
            availability_calls: 0,
            queue_calls: 0,
        }
    }
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
impl ManualServiceAnnouncePort for FakeManualServiceAnnouncePort {
    fn availability(&mut self) -> CapabilityAvailability {
        self.availability_calls += 1;
        self.availability
    }

    fn queue_service_announce(&mut self) -> api::ManualServiceAnnounceDisposition {
        self.queue_calls += 1;
        self.disposition
    }
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
struct FakeNetworkConfigPort {
    availability: CapabilityAvailability,
    configuration: Result<api::NetworkConfigSnapshot, NetworkConfigPortError>,
    mutation: Result<api::NetworkConfigMutationOutcome, NetworkConfigPortError>,
    status: Result<api::NetworkRuntimeStatus, NetworkConfigPortError>,
    calls: NetworkConfigPortCalls,
    observed_mutation: Option<(
        api::PrincipalId,
        u64,
        api::IdempotencyKey,
        api::WifiNetworkProfileId,
    )>,
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
impl FakeNetworkConfigPort {
    fn available() -> Self {
        Self {
            availability: CapabilityAvailability::Available,
            configuration: Ok(network_config_snapshot()),
            mutation: Ok(api::NetworkConfigMutationOutcome::Applied {
                revision: 8,
                reboot_required: true,
            }),
            status: Ok(network_runtime_status()),
            calls: NetworkConfigPortCalls::default(),
            observed_mutation: None,
        }
    }
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
impl NetworkConfigPort for FakeNetworkConfigPort {
    fn availability(&mut self) -> CapabilityAvailability {
        self.calls.availability += 1;
        self.availability
    }

    fn configuration(&mut self) -> Result<api::NetworkConfigSnapshot, NetworkConfigPortError> {
        self.calls.configuration += 1;
        self.configuration
    }

    fn mutate(
        &mut self,
        principal: api::PrincipalId,
        request: api::NetworkConfigMutationRequest<'_>,
    ) -> Result<api::NetworkConfigMutationOutcome, NetworkConfigPortError> {
        self.calls.mutate += 1;
        let profile_id = match request.mutation() {
            api::NetworkConfigMutation::UpsertWifi { profile_id, .. }
            | api::NetworkConfigMutation::RemoveWifi { profile_id } => profile_id,
            api::NetworkConfigMutation::ReplaceTcpPeer(_)
            | api::NetworkConfigMutation::ReplaceTcpHostPeer(_)
            | api::NetworkConfigMutation::SetGatewayPolicy(_)
            | api::NetworkConfigMutation::SetRmapConfig(_)
            | api::NetworkConfigMutation::SetLoraTxPower(_)
            | api::NetworkConfigMutation::SetLoraProfile(_) => wifi_profile_id(),
        };
        self.observed_mutation = Some((
            principal,
            request.expected_revision(),
            request.idempotency_key(),
            profile_id,
        ));
        self.mutation
    }

    fn status(&mut self) -> Result<api::NetworkRuntimeStatus, NetworkConfigPortError> {
        self.calls.status += 1;
        self.status
    }
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
fn wifi_profile_id() -> api::WifiNetworkProfileId {
    api::WifiNetworkProfileId::new([0x51; 16]).unwrap()
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
fn network_config_snapshot() -> api::NetworkConfigSnapshot {
    let profile =
        api::WifiNetworkConfigSummary::new(wifi_profile_id(), true, 3, b"field-node", true)
            .unwrap();
    let address = api::ReticulumTcpPeerIpv4Address::new([192, 0, 2, 44]).unwrap();
    let peer = api::ReticulumTcpPeerConfigSummary::new(true, address, 4242).unwrap();
    api::NetworkConfigSnapshot::with_defaults(7, [Some(profile), None, None, None], Some(peer))
        .unwrap()
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
fn network_runtime_status() -> api::NetworkRuntimeStatus {
    api::NetworkRuntimeStatus::new_with_tcp_failure(
        7,
        7,
        api::WifiStationState::Connected,
        Some(wifi_profile_id()),
        Some(b"field-node"),
        Some([192, 0, 2, 90]),
        Some(-73),
        api::ReticulumTcpPeerState::Backoff,
        Some(api::ReticulumTcpFailure::DnsTimeout),
    )
    .unwrap()
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
fn network_mutation_request() -> api::NetworkConfigMutationRequest<'static> {
    let network = api::WifiNetworkUpdate::new(
        true,
        2,
        api::WifiSsid::new(b"field-node").unwrap(),
        api::WifiCredentialUpdate::replace(b"test-password").unwrap(),
    );
    api::NetworkConfigMutationRequest::new(
        api::NetworkConfigMutation::UpsertWifi {
            profile_id: wifi_profile_id(),
            network,
        },
        7,
        api::IdempotencyKey([0x61; 16]),
    )
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
fn dispatch_complete_network(
    port: &mut FakeLxmfOnlyPort,
    nomad: &mut FakeNomadPort,
    network: &mut FakeNetworkConfigPort,
    manual_announce: &mut FakeManualServiceAnnouncePort,
    context: &DispatchContext,
    request: DeviceRequest<'_>,
) -> ResponseEnvelope {
    super::dispatch_with_lxmf_peer_discovery_nomad_and_network_config_ports(
        port,
        nomad,
        network,
        manual_announce,
        identity_summary(),
        context,
        envelope(600, request),
    )
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
#[test]
fn complete_dispatch_routes_authenticated_node_route_and_radio_trace_diagnostics_to_the_primary_owner()
 {
    let mut port = FakeLxmfOnlyPort::default();
    let mut nomad = FakeNomadPort::available(nomad_id());
    let mut network = FakeNetworkConfigPort::available();
    let mut manual =
        FakeManualServiceAnnouncePort::available(api::ManualServiceAnnounceDisposition::Queued);

    let unauthenticated = dispatch_complete_network(
        &mut port,
        &mut nomad,
        &mut network,
        &mut manual,
        &DispatchContext::UNAUTHENTICATED,
        DeviceRequest::NodeDiagnostics,
    );
    assert_eq!(
        error_code(unauthenticated.response),
        ApiErrorCode::AuthenticationRequired
    );
    assert_eq!(port.node_diagnostics, 0);

    let unauthenticated_trace = dispatch_complete_network(
        &mut port,
        &mut nomad,
        &mut network,
        &mut manual,
        &DispatchContext::UNAUTHENTICATED,
        DeviceRequest::RadioTracePage(api::RadioTracePageRequest::new(None)),
    );
    assert_eq!(
        error_code(unauthenticated_trace.response),
        ApiErrorCode::AuthenticationRequired
    );
    assert_eq!(port.radio_trace, 0);

    let node = dispatch_complete_network(
        &mut port,
        &mut nomad,
        &mut network,
        &mut manual,
        &authenticated(4, Permissions::NONE),
        DeviceRequest::NodeDiagnostics,
    );
    assert_eq!(
        node.response,
        DeviceResponse::NodeDiagnostics(node_diagnostics_snapshot())
    );
    let after = api::DestinationHash([0x21; 16]);
    let routes = dispatch_complete_network(
        &mut port,
        &mut nomad,
        &mut network,
        &mut manual,
        &authenticated(4, Permissions::NONE),
        DeviceRequest::RouteDiagnosticsPage(api::RouteDiagnosticsRequest::new(Some(after))),
    );
    assert_eq!(
        routes.response,
        DeviceResponse::RouteDiagnosticsPage(route_diagnostics_page())
    );
    assert_eq!(port.node_diagnostics, 1);
    assert_eq!(port.route_diagnostics, 1);
    assert_eq!(port.observed_route_cursor, Some(Some(after)));
    let trace_cursor = api::RadioTraceCursor::new(99, 6);
    let trace = dispatch_complete_network(
        &mut port,
        &mut nomad,
        &mut network,
        &mut manual,
        &authenticated(4, Permissions::NONE),
        DeviceRequest::RadioTracePage(api::RadioTracePageRequest::new(Some(trace_cursor))),
    );
    assert_eq!(
        trace.response,
        DeviceResponse::RadioTracePage(radio_trace_page())
    );
    assert_eq!(port.radio_trace, 1);
    assert_eq!(port.observed_radio_trace_cursor, Some(Some(trace_cursor)));
    assert_eq!(nomad.availability_calls, 0);
    assert_eq!(nomad.start_calls, 0);
    assert_eq!(nomad.poll_calls, 0);
    assert_eq!(network.calls, NetworkConfigPortCalls::default());
    assert_eq!(manual.availability_calls, 0);
    assert_eq!(manual.queue_calls, 0);
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
#[test]
fn complete_dispatch_advertises_network_config_runtime_availability() {
    let mut port = FakeLxmfOnlyPort::default();
    let mut nomad = FakeNomadPort::available(nomad_id());
    let mut network = FakeNetworkConfigPort::available();
    let mut manual_announce =
        FakeManualServiceAnnouncePort::available(api::ManualServiceAnnounceDisposition::Queued);
    network.availability = CapabilityAvailability::Disabled;

    let response = dispatch_complete_network(
        &mut port,
        &mut nomad,
        &mut network,
        &mut manual_announce,
        &DispatchContext::UNAUTHENTICATED,
        DeviceRequest::SystemCapabilities,
    );
    let expected = CapabilitySnapshot::for_dispatch_with_lxmf_basic_send_and_peer_discovery(
        cfg!(feature = "rns-data"),
        CapabilityAvailability::Available,
        CapabilityAvailability::Available,
        CapabilityAvailability::Available,
        64,
    )
    .with_dispatch_nomad(CapabilityAvailability::Available)
    .with_dispatch_network_config(CapabilityAvailability::Disabled)
    .with_dispatch_manual_service_announce(CapabilityAvailability::Available);
    assert_eq!(
        response.response,
        DeviceResponse::SystemCapabilities(expected)
    );
    assert_eq!(network.calls.availability, 1);
    assert_eq!(network.calls.configuration, 0);
    assert_eq!(network.calls.mutate, 0);
    assert_eq!(network.calls.status, 0);
    assert_eq!(manual_announce.availability_calls, 1);
    assert_eq!(manual_announce.queue_calls, 0);
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
#[test]
fn complete_dispatch_authenticates_then_coalesces_manual_service_announce() {
    let mut port = FakeLxmfOnlyPort::default();
    let mut nomad = FakeNomadPort::available(nomad_id());
    let mut network = FakeNetworkConfigPort::available();
    let mut manual_announce =
        FakeManualServiceAnnouncePort::available(api::ManualServiceAnnounceDisposition::Queued);

    let unauthenticated = dispatch_complete_network(
        &mut port,
        &mut nomad,
        &mut network,
        &mut manual_announce,
        &DispatchContext::UNAUTHENTICATED,
        DeviceRequest::ManualServiceAnnounce,
    );
    assert_eq!(
        error_code(unauthenticated.response),
        ApiErrorCode::AuthenticationRequired
    );
    assert_eq!(manual_announce.availability_calls, 0);
    assert_eq!(manual_announce.queue_calls, 0);

    let queued = dispatch_complete_network(
        &mut port,
        &mut nomad,
        &mut network,
        &mut manual_announce,
        &authenticated(5, Permissions::NONE),
        DeviceRequest::ManualServiceAnnounce,
    );
    assert_eq!(
        queued.response,
        DeviceResponse::ManualServiceAnnounce(api::ManualServiceAnnounceDisposition::Queued)
    );
    manual_announce.disposition = api::ManualServiceAnnounceDisposition::AlreadyPending;
    let coalesced = dispatch_complete_network(
        &mut port,
        &mut nomad,
        &mut network,
        &mut manual_announce,
        &authenticated(5, Permissions::NONE),
        DeviceRequest::ManualServiceAnnounce,
    );
    assert_eq!(
        coalesced.response,
        DeviceResponse::ManualServiceAnnounce(
            api::ManualServiceAnnounceDisposition::AlreadyPending
        )
    );
    assert_eq!(manual_announce.availability_calls, 2);
    assert_eq!(manual_announce.queue_calls, 2);
    assert_eq!(network.calls, NetworkConfigPortCalls::default());
    assert_eq!(port, FakeLxmfOnlyPort::default());
    assert_eq!(nomad.availability_calls, 0);
    assert_eq!(nomad.start_calls, 0);
    assert_eq!(nomad.poll_calls, 0);
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
#[test]
fn complete_dispatch_rejects_disabled_manual_announce_without_queueing() {
    let mut port = FakeLxmfOnlyPort::default();
    let mut nomad = FakeNomadPort::available(nomad_id());
    let mut network = FakeNetworkConfigPort::available();
    let mut manual_announce =
        FakeManualServiceAnnouncePort::available(api::ManualServiceAnnounceDisposition::Queued);
    manual_announce.availability = CapabilityAvailability::Disabled;

    let response = dispatch_complete_network(
        &mut port,
        &mut nomad,
        &mut network,
        &mut manual_announce,
        &authenticated(5, Permissions::NONE),
        DeviceRequest::ManualServiceAnnounce,
    );
    assert_eq!(
        error_code(response.response),
        ApiErrorCode::CapabilityUnavailable
    );
    assert_eq!(manual_announce.availability_calls, 1);
    assert_eq!(manual_announce.queue_calls, 0);
    assert_eq!(network.calls, NetworkConfigPortCalls::default());
    assert_eq!(port, FakeLxmfOnlyPort::default());
    assert_eq!(nomad.availability_calls, 0);
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
#[test]
fn same_owner_complete_dispatch_enters_network_config_through_one_exclusive_borrow() {
    let mut port = FakeLxmfOnlyPort::default();
    let mut nomad = FakeNomadPort::available(nomad_id());
    let context = authenticated(6, Permissions::NONE);

    let response = super::dispatch_with_lxmf_peer_discovery_nomad_and_network_config(
        &mut port,
        &mut nomad,
        identity_summary(),
        &context,
        envelope(601, DeviceRequest::NetworkConfigGet),
    );
    assert_eq!(
        response.response,
        DeviceResponse::NetworkConfig(network_config_snapshot())
    );
    assert_eq!(port.network_availability, 1);
    assert_eq!(port.network_configuration, 1);
    assert_eq!(port.network_mutate, 0);
    assert_eq!(port.network_status, 0);
    assert_eq!(port.submission_availability, 0);
    assert_eq!(port.lxmf_availability, 0);
    assert_eq!(port.compose_availability, 0);
    assert_eq!(port.peer_availability, 0);
    assert_eq!(port.manual_announce_availability, 0);
    assert_eq!(port.manual_announce_queue, 0);
    assert_eq!(nomad.availability_calls, 0);
    assert_eq!(nomad.start_calls, 0);
    assert_eq!(nomad.poll_calls, 0);
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
#[test]
fn same_owner_complete_dispatch_queues_manual_announce_through_one_exclusive_borrow() {
    let mut port = FakeLxmfOnlyPort::default();
    let mut nomad = FakeNomadPort::available(nomad_id());

    let response = super::dispatch_with_lxmf_peer_discovery_nomad_and_network_config(
        &mut port,
        &mut nomad,
        identity_summary(),
        &authenticated(6, Permissions::NONE),
        envelope(602, DeviceRequest::ManualServiceAnnounce),
    );
    assert_eq!(
        response.response,
        DeviceResponse::ManualServiceAnnounce(api::ManualServiceAnnounceDisposition::Queued)
    );
    assert_eq!(port.manual_announce_availability, 1);
    assert_eq!(port.manual_announce_queue, 1);
    assert_eq!(port.network_availability, 0);
    assert_eq!(port.network_configuration, 0);
    assert_eq!(port.network_mutate, 0);
    assert_eq!(port.network_status, 0);
    assert_eq!(port.submission_availability, 0);
    assert_eq!(port.lxmf_availability, 0);
    assert_eq!(port.compose_availability, 0);
    assert_eq!(port.peer_availability, 0);
    assert_eq!(nomad.availability_calls, 0);
    assert_eq!(nomad.start_calls, 0);
    assert_eq!(nomad.poll_calls, 0);
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
#[test]
fn network_config_dispatch_enforces_authentication_and_management_permission_first() {
    let mut port = FakeLxmfOnlyPort::default();
    let mut nomad = FakeNomadPort::available(nomad_id());
    let mut network = FakeNetworkConfigPort::available();
    let mut manual_announce =
        FakeManualServiceAnnouncePort::available(api::ManualServiceAnnounceDisposition::Queued);

    let unauthenticated = dispatch_complete_network(
        &mut port,
        &mut nomad,
        &mut network,
        &mut manual_announce,
        &DispatchContext::UNAUTHENTICATED,
        DeviceRequest::NetworkConfigGet,
    );
    assert_eq!(
        error_code(unauthenticated.response),
        ApiErrorCode::AuthenticationRequired
    );
    let denied = dispatch_complete_network(
        &mut port,
        &mut nomad,
        &mut network,
        &mut manual_announce,
        &authenticated(7, Permissions::NONE),
        DeviceRequest::NetworkConfigMutate(network_mutation_request()),
    );
    assert_eq!(error_code(denied.response), ApiErrorCode::PermissionDenied);
    assert_eq!(network.calls, NetworkConfigPortCalls::default());
    assert_eq!(port, FakeLxmfOnlyPort::default());
    assert_eq!(nomad.availability_calls, 0);
    assert_eq!(nomad.start_calls, 0);
    assert_eq!(nomad.poll_calls, 0);
    assert_eq!(manual_announce.availability_calls, 0);
    assert_eq!(manual_announce.queue_calls, 0);
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
#[test]
fn network_config_get_and_status_invoke_only_the_network_port() {
    let mut port = FakeLxmfOnlyPort::default();
    let mut nomad = FakeNomadPort::available(nomad_id());
    let mut network = FakeNetworkConfigPort::available();
    let mut manual_announce =
        FakeManualServiceAnnouncePort::available(api::ManualServiceAnnounceDisposition::Queued);
    let context = authenticated(8, Permissions::NONE);

    let configuration = dispatch_complete_network(
        &mut port,
        &mut nomad,
        &mut network,
        &mut manual_announce,
        &context,
        DeviceRequest::NetworkConfigGet,
    );
    assert_eq!(
        configuration.response,
        DeviceResponse::NetworkConfig(network_config_snapshot())
    );
    let status = dispatch_complete_network(
        &mut port,
        &mut nomad,
        &mut network,
        &mut manual_announce,
        &context,
        DeviceRequest::NetworkStatus,
    );
    assert_eq!(
        status.response,
        DeviceResponse::NetworkStatus(network_runtime_status())
    );
    assert_eq!(
        network.calls,
        NetworkConfigPortCalls {
            availability: 2,
            configuration: 1,
            mutate: 0,
            status: 1,
        }
    );
    assert_eq!(port, FakeLxmfOnlyPort::default());
    assert_eq!(nomad.availability_calls, 0);
    assert_eq!(nomad.start_calls, 0);
    assert_eq!(nomad.poll_calls, 0);
    assert_eq!(manual_announce.availability_calls, 0);
    assert_eq!(manual_announce.queue_calls, 0);
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
#[test]
fn network_config_mutation_returns_applied_and_revision_conflict_as_normal_outcomes() {
    let mut port = FakeLxmfOnlyPort::default();
    let mut nomad = FakeNomadPort::available(nomad_id());
    let mut network = FakeNetworkConfigPort::available();
    let mut manual_announce =
        FakeManualServiceAnnouncePort::available(api::ManualServiceAnnounceDisposition::Queued);
    let context = authenticated(9, Permissions::MANAGE_NETWORK_CONFIG);

    let applied = dispatch_complete_network(
        &mut port,
        &mut nomad,
        &mut network,
        &mut manual_announce,
        &context,
        DeviceRequest::NetworkConfigMutate(network_mutation_request()),
    );
    assert_eq!(
        applied.response,
        DeviceResponse::NetworkConfigMutation(api::NetworkConfigMutationOutcome::Applied {
            revision: 8,
            reboot_required: true,
        })
    );
    assert_eq!(
        network.observed_mutation,
        Some((
            api::PrincipalId([9; 16]),
            7,
            api::IdempotencyKey([0x61; 16]),
            wifi_profile_id(),
        ))
    );

    network.mutation = Ok(api::NetworkConfigMutationOutcome::RevisionConflict {
        current_revision: 8,
    });
    let conflict = dispatch_complete_network(
        &mut port,
        &mut nomad,
        &mut network,
        &mut manual_announce,
        &context,
        DeviceRequest::NetworkConfigMutate(network_mutation_request()),
    );
    assert_eq!(
        conflict.response,
        DeviceResponse::NetworkConfigMutation(
            api::NetworkConfigMutationOutcome::RevisionConflict {
                current_revision: 8,
            }
        )
    );
    assert_eq!(network.calls.mutate, 2);
    assert_eq!(port, FakeLxmfOnlyPort::default());
    assert_eq!(nomad.availability_calls, 0);
    assert_eq!(nomad.start_calls, 0);
    assert_eq!(nomad.poll_calls, 0);
    assert_eq!(manual_announce.availability_calls, 0);
    assert_eq!(manual_announce.queue_calls, 0);
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
#[test]
fn network_config_port_failures_map_to_stable_api_errors() {
    let cases = [
        (
            NetworkConfigPortError::InvalidRequest,
            ApiErrorCode::InvalidRequest,
        ),
        (NetworkConfigPortError::Busy, ApiErrorCode::Internal),
        (
            NetworkConfigPortError::Unavailable,
            ApiErrorCode::CapabilityUnavailable,
        ),
    ];

    for (port_error, expected) in cases {
        let mut port = FakeLxmfOnlyPort::default();
        let mut nomad = FakeNomadPort::available(nomad_id());
        let mut network = FakeNetworkConfigPort::available();
        let mut manual_announce =
            FakeManualServiceAnnouncePort::available(api::ManualServiceAnnounceDisposition::Queued);
        network.mutation = Err(port_error);
        let response = dispatch_complete_network(
            &mut port,
            &mut nomad,
            &mut network,
            &mut manual_announce,
            &authenticated(10, Permissions::MANAGE_NETWORK_CONFIG),
            DeviceRequest::NetworkConfigMutate(network_mutation_request()),
        );
        assert_eq!(error_code(response.response), expected);
        assert_eq!(network.calls.mutate, 1);
        assert_eq!(port, FakeLxmfOnlyPort::default());
        assert_eq!(nomad.availability_calls, 0);
        assert_eq!(nomad.start_calls, 0);
        assert_eq!(nomad.poll_calls, 0);
        assert_eq!(manual_announce.availability_calls, 0);
        assert_eq!(manual_announce.queue_calls, 0);
    }

    for port_error in [
        NetworkConfigPortError::Backend,
        NetworkConfigPortError::Binding,
        NetworkConfigPortError::Faulted,
        NetworkConfigPortError::Invariant,
    ] {
        assert_eq!(
            error_code(network_config_port_error(
                port_error,
                api::OP_NETWORK_CONFIG_GET,
            )),
            ApiErrorCode::Internal
        );
    }
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ReticulumProbePortCalls {
    availability: usize,
    start: usize,
    poll: usize,
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
struct FakeReticulumProbePort {
    availability: CapabilityAvailability,
    start_result: Result<ReticulumProbeStartDisposition, ReticulumProbePortError>,
    poll_result: Result<Option<api::ProbePollResponse>, ReticulumProbePortError>,
    poll_owner: Option<api::PrincipalId>,
    calls: ReticulumProbePortCalls,
    observed_starts: Vec<(api::PrincipalId, api::ProbeStartRequest)>,
    observed_polls: Vec<(api::PrincipalId, api::ProbeId)>,
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
impl FakeReticulumProbePort {
    fn available() -> Self {
        Self {
            availability: CapabilityAvailability::Available,
            start_result: Ok(ReticulumProbeStartDisposition::Accepted(probe_id())),
            poll_result: Ok(Some(api::ProbePollResponse::Pending(
                api::ProbePhase::AwaitingProof,
            ))),
            poll_owner: None,
            calls: ReticulumProbePortCalls::default(),
            observed_starts: Vec::new(),
            observed_polls: Vec::new(),
        }
    }
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
impl ReticulumProbePort for FakeReticulumProbePort {
    fn availability(&mut self) -> CapabilityAvailability {
        self.calls.availability += 1;
        self.availability
    }

    fn start(
        &mut self,
        principal: api::PrincipalId,
        request: api::ProbeStartRequest,
    ) -> Result<ReticulumProbeStartDisposition, ReticulumProbePortError> {
        self.calls.start += 1;
        self.observed_starts.push((principal, request));
        self.start_result
    }

    fn poll(
        &mut self,
        principal: api::PrincipalId,
        id: api::ProbeId,
    ) -> Result<Option<api::ProbePollResponse>, ReticulumProbePortError> {
        self.calls.poll += 1;
        self.observed_polls.push((principal, id));
        if self
            .poll_owner
            .is_some_and(|expected| expected != principal)
        {
            return Ok(None);
        }
        self.poll_result
    }
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
fn probe_id() -> api::ProbeId {
    api::ProbeId::new([0x91; 16]).unwrap()
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
fn probe_start_request() -> api::ProbeStartRequest {
    api::ProbeStartRequest::new(
        api::DestinationHash([0x92; 16]),
        api::IdempotencyKey([0x93; 16]),
    )
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
fn probe_success() -> api::ProbeSuccess {
    api::ProbeSuccess::new(
        1_275,
        3,
        api::IngressObservation::new(2, Some(api::IngressSignal::new(-97, 6))),
    )
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
fn dispatch_complete_probe(
    port: &mut FakeLxmfOnlyPort,
    nomad: &mut FakeNomadPort,
    probe: &mut FakeReticulumProbePort,
    context: &DispatchContext,
    request: DeviceRequest<'_>,
) -> ResponseEnvelope {
    super::dispatch_with_lxmf_peer_discovery_nomad_network_config_and_probe(
        port,
        nomad,
        probe,
        identity_summary(),
        context,
        envelope(700, request),
    )
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
#[test]
fn complete_dispatch_advertises_exact_probe_availability_without_starting_work() {
    for availability in [
        CapabilityAvailability::Unavailable,
        CapabilityAvailability::Disabled,
        CapabilityAvailability::Available,
    ] {
        let mut port = FakeLxmfOnlyPort::default();
        let mut nomad = FakeNomadPort::available(nomad_id());
        let mut probe = FakeReticulumProbePort::available();
        probe.availability = availability;

        let response = dispatch_complete_probe(
            &mut port,
            &mut nomad,
            &mut probe,
            &DispatchContext::UNAUTHENTICATED,
            DeviceRequest::SystemCapabilities,
        );
        let DeviceResponse::SystemCapabilities(capabilities) = response.response else {
            panic!("complete dispatcher must return capabilities");
        };
        assert_eq!(capabilities.reticulum_probe(), availability);
        assert_eq!(
            probe.calls,
            ReticulumProbePortCalls {
                availability: 1,
                start: 0,
                poll: 0,
            }
        );
        assert_eq!(probe.observed_starts, []);
        assert_eq!(probe.observed_polls, []);
    }
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
#[test]
fn complete_dispatch_keeps_unrelated_requests_out_of_the_probe_port() {
    let mut port = FakeLxmfOnlyPort::default();
    let mut nomad = FakeNomadPort::available(nomad_id());
    let mut probe = FakeReticulumProbePort::available();

    let response = dispatch_complete_probe(
        &mut port,
        &mut nomad,
        &mut probe,
        &DispatchContext::UNAUTHENTICATED,
        DeviceRequest::IdentitySummary,
    );

    assert_eq!(
        response.response,
        DeviceResponse::IdentitySummary(identity_summary())
    );
    assert_eq!(probe.calls, ReticulumProbePortCalls::default());
    assert_eq!(probe.observed_starts, []);
    assert_eq!(probe.observed_polls, []);
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
#[test]
fn probe_start_maps_fresh_replay_conflict_and_capacity_outcomes() {
    let cases = [
        (
            ReticulumProbeStartDisposition::Accepted(probe_id()),
            Some(api::ProbeStartOutcome::Accepted),
            None,
        ),
        (
            ReticulumProbeStartDisposition::Replay(probe_id()),
            Some(api::ProbeStartOutcome::Replayed),
            None,
        ),
        (
            ReticulumProbeStartDisposition::IdempotencyConflict,
            None,
            Some(ApiErrorCode::IdempotencyConflict),
        ),
        (
            ReticulumProbeStartDisposition::CapacityExhausted,
            None,
            Some(ApiErrorCode::CapacityExhausted),
        ),
    ];

    for (disposition, expected_outcome, expected_error) in cases {
        let mut port = FakeLxmfOnlyPort::default();
        let mut nomad = FakeNomadPort::available(nomad_id());
        let mut probe = FakeReticulumProbePort::available();
        probe.start_result = Ok(disposition);
        let request = probe_start_request();

        let response = dispatch_complete_probe(
            &mut port,
            &mut nomad,
            &mut probe,
            &authenticated(11, Permissions::SUBMIT_RNS_DATA),
            DeviceRequest::ReticulumProbeStart(request),
        );

        if let Some(outcome) = expected_outcome {
            assert_eq!(
                response.response,
                DeviceResponse::ReticulumProbeStartAccepted(api::ProbeStartAccepted::new(
                    probe_id(),
                    outcome,
                ))
            );
        } else {
            assert_eq!(
                error_code(response.response),
                expected_error.expect("error case has an API error")
            );
        }
        assert_eq!(
            probe.calls,
            ReticulumProbePortCalls {
                availability: 1,
                start: 1,
                poll: 0,
            }
        );
        assert_eq!(
            probe.observed_starts,
            [(api::PrincipalId([11; 16]), request)]
        );
        assert_eq!(port, FakeLxmfOnlyPort::default());
        assert_eq!(nomad.availability_calls, 0);
        assert_eq!(nomad.start_calls, 0);
        assert_eq!(nomad.poll_calls, 0);
    }
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
#[test]
fn probe_dispatch_enforces_authentication_and_start_permission_before_port_entry() {
    for (context, request, expected) in [
        (
            DispatchContext::UNAUTHENTICATED,
            DeviceRequest::ReticulumProbeStart(probe_start_request()),
            ApiErrorCode::AuthenticationRequired,
        ),
        (
            authenticated(12, Permissions::NONE),
            DeviceRequest::ReticulumProbeStart(probe_start_request()),
            ApiErrorCode::PermissionDenied,
        ),
        (
            DispatchContext::UNAUTHENTICATED,
            DeviceRequest::ReticulumProbePoll(api::ProbePollRequest::new(probe_id())),
            ApiErrorCode::AuthenticationRequired,
        ),
    ] {
        let mut port = FakeLxmfOnlyPort::default();
        let mut nomad = FakeNomadPort::available(nomad_id());
        let mut probe = FakeReticulumProbePort::available();

        let response =
            dispatch_complete_probe(&mut port, &mut nomad, &mut probe, &context, request);

        assert_eq!(error_code(response.response), expected);
        assert_eq!(probe.calls, ReticulumProbePortCalls::default());
        assert_eq!(probe.observed_starts, []);
        assert_eq!(probe.observed_polls, []);
        assert_eq!(port, FakeLxmfOnlyPort::default());
        assert_eq!(nomad.availability_calls, 0);
        assert_eq!(nomad.start_calls, 0);
        assert_eq!(nomad.poll_calls, 0);
    }
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
#[test]
fn probe_poll_returns_success_hides_foreign_ids_and_reports_missing_ids() {
    let owner = api::PrincipalId([13; 16]);
    let id = probe_id();
    let success = api::ProbePollResponse::Succeeded(probe_success());
    let mut port = FakeLxmfOnlyPort::default();
    let mut nomad = FakeNomadPort::available(nomad_id());
    let mut probe = FakeReticulumProbePort::available();
    probe.poll_owner = Some(owner);
    probe.poll_result = Ok(Some(success));

    let owned = dispatch_complete_probe(
        &mut port,
        &mut nomad,
        &mut probe,
        &authenticated(13, Permissions::NONE),
        DeviceRequest::ReticulumProbePoll(api::ProbePollRequest::new(id)),
    );
    assert_eq!(owned.response, DeviceResponse::ReticulumProbePoll(success));

    let foreign = dispatch_complete_probe(
        &mut port,
        &mut nomad,
        &mut probe,
        &authenticated(14, Permissions::NONE),
        DeviceRequest::ReticulumProbePoll(api::ProbePollRequest::new(id)),
    );
    assert_eq!(error_code(foreign.response), ApiErrorCode::NotFound);
    assert_eq!(
        probe.observed_polls,
        [(owner, id), (api::PrincipalId([14; 16]), id)]
    );
    assert_eq!(
        probe.calls,
        ReticulumProbePortCalls {
            availability: 2,
            start: 0,
            poll: 2,
        }
    );
    assert_eq!(port, FakeLxmfOnlyPort::default());
    assert_eq!(nomad.availability_calls, 0);
    assert_eq!(nomad.start_calls, 0);
    assert_eq!(nomad.poll_calls, 0);

    let mut port = FakeLxmfOnlyPort::default();
    let mut nomad = FakeNomadPort::available(nomad_id());
    let mut missing_probe = FakeReticulumProbePort::available();
    missing_probe.poll_result = Ok(None);
    let missing = dispatch_complete_probe(
        &mut port,
        &mut nomad,
        &mut missing_probe,
        &authenticated(13, Permissions::NONE),
        DeviceRequest::ReticulumProbePoll(api::ProbePollRequest::new(id)),
    );
    assert_eq!(error_code(missing.response), ApiErrorCode::NotFound);
    assert_eq!(
        missing_probe.observed_polls,
        [(api::PrincipalId([13; 16]), id)]
    );
}

#[cfg(all(feature = "lxmf", feature = "nomad", feature = "network-config"))]
#[test]
fn unavailable_probe_capability_rejects_start_and_poll_before_work() {
    for availability in [
        CapabilityAvailability::Unavailable,
        CapabilityAvailability::Disabled,
    ] {
        for request in [
            DeviceRequest::ReticulumProbeStart(probe_start_request()),
            DeviceRequest::ReticulumProbePoll(api::ProbePollRequest::new(probe_id())),
        ] {
            let mut port = FakeLxmfOnlyPort::default();
            let mut nomad = FakeNomadPort::available(nomad_id());
            let mut probe = FakeReticulumProbePort::available();
            probe.availability = availability;

            let response = dispatch_complete_probe(
                &mut port,
                &mut nomad,
                &mut probe,
                &authenticated(15, Permissions::SUBMIT_RNS_DATA),
                request,
            );

            assert_eq!(
                error_code(response.response),
                ApiErrorCode::CapabilityUnavailable
            );
            assert_eq!(
                probe.calls,
                ReticulumProbePortCalls {
                    availability: 1,
                    start: 0,
                    poll: 0,
                }
            );
            assert_eq!(probe.observed_starts, []);
            assert_eq!(probe.observed_polls, []);
            assert_eq!(port, FakeLxmfOnlyPort::default());
            assert_eq!(nomad.availability_calls, 0);
            assert_eq!(nomad.start_calls, 0);
            assert_eq!(nomad.poll_calls, 0);
        }
    }
}
