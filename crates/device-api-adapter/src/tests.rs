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
use reticulum_device_api::{
    ApiErrorCode, ApiErrorResponse, ApiVersion, CapabilitySnapshot, DeviceRequest, DeviceResponse,
    DispatchContext, OP_SUBMISSION_STATUS, Permissions, PrincipalId as ApiPrincipalId,
    RequestEnvelope, RequestId, SubmissionId as ApiSubmissionId,
};
#[cfg(feature = "experimental-rns-data")]
use reticulum_device_api::{
    DestinationHash as ApiDestinationHash, IdempotencyKey as ApiIdempotencyKey,
};
use reticulum_storage_actor::{
    AcceptanceProgress, BoundJournal, DriveError, JournalBinding, StorageActor, StorageDeviceId,
};
#[cfg(feature = "experimental-rns-data")]
use reticulum_storage_actor::{PendingKind, PendingProgress};
use reticulum_storage_model::{
    AcceptanceCandidate, BootRecoveryMarker, DestinationHash, EncodedPacketSha256,
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

// Canonical generation-1 bank-A manifest emitted by storage-journal format 1
// for an empty schema-1 partition. The actor independently authenticates this
// fixture during every test mount.
const EMPTY_MANIFEST: [u8; 160] = [
    0x52, 0x54, 0x4a, 0x52, 0x4d, 0x41, 0x4e, 0x31, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00, 0x00, 0xf0, 0x07, 0x00,
    0x80, 0x02, 0x00, 0x00, 0x2c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x63, 0xfe, 0xa6, 0xeb, 0xe3, 0x62, 0xc7, 0xcc, 0x37, 0xe8, 0x8b, 0xcf, 0xd5, 0xec, 0x73, 0xa3,
    0x23, 0x23, 0xb8, 0x90, 0x98, 0x0e, 0xd8, 0xe4, 0x14, 0xe1, 0x62, 0xc0, 0x9e, 0x34, 0xdc, 0x0f,
    0x52, 0x4a, 0x3c, 0xa5, 0x0f, 0x69, 0x96, 0xc3, 0x71, 0x1e, 0xd2, 0x4b, 0x87, 0x58, 0xb4, 0x2d,
    0xca, 0x35, 0x6a, 0x93, 0xe1, 0x0d, 0x7c, 0x46, 0x9b, 0x24, 0xd8, 0x63, 0x15, 0xae, 0x49, 0x72,
];

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
        let mut bytes = vec![0xff; PARTITION_SIZE];
        bytes[..EMPTY_MANIFEST.len()].copy_from_slice(&EMPTY_MANIFEST);
        Self {
            bytes,
            lost_write_reply_after,
            dropped_write_after,
            writes: 0,
            erases: 0,
        }
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
        1, // The canonical EMPTY_MANIFEST fixture above is physical format 1.
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

fn dispatch<const SUBMISSIONS: usize>(
    actor: &mut TestActor<SUBMISSIONS>,
    context: DispatchContext,
    envelope: RequestEnvelope<'_>,
) -> ResponseEnvelope {
    super::dispatch(actor, context, envelope)
}

fn envelope<'a>(request_id: u64, request: DeviceRequest<'a>) -> RequestEnvelope<'a> {
    RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(request_id),
        request,
    }
}

fn authenticated(principal: u8, permissions: Permissions) -> DispatchContext {
    DispatchContext::authenticated(ApiPrincipalId([principal; 16]), permissions)
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
fn unavailable_service_is_not_advertised_or_called_for_status() {
    let mut port = UnavailablePort::default();
    let capabilities = super::dispatch(
        &mut port,
        DispatchContext::UNAUTHENTICATED,
        envelope(111, DeviceRequest::SystemCapabilities),
    );
    assert_eq!(
        capabilities.response,
        DeviceResponse::SystemCapabilities(CapabilitySnapshot::for_dispatch(false))
    );

    let status = super::dispatch(
        &mut port,
        authenticated(1, Permissions::READ_SUBMISSION_STATUS),
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
    authenticated(
        principal,
        Permissions::EXPERIMENTAL_SUBMIT_RNS_DATA | Permissions::READ_SUBMISSION_STATUS,
    )
}

#[cfg(feature = "experimental-rns-data")]
#[test]
fn unavailable_service_rejects_mutation_before_acceptance() {
    let mut port = UnavailablePort::default();
    let response = super::dispatch(
        &mut port,
        submit_context(1),
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
    assert_eq!(
        actor
            .index()
            .get(SubmissionId::new(10))
            .unwrap()
            .accepted()
            .intent()
            .payload(),
        b"same"
    );

    let replay = dispatch(
        &mut actor,
        submit_context(1),
        envelope(21, submit_request(b"same", 2)),
    );
    assert_eq!(accepted_id(replay.response), ApiSubmissionId(10));
    assert_eq!(actor.state().committed_records(), 1);

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
        submit_context(1),
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
