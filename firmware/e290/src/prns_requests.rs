//! PRNS-native request routes and application ownership boundary.
//!
//! PRNS owns Link request parsing, authorization identity discovery, response
//! framing, and protocol settlement. Nomad's small public index page can reply
//! directly from its handler. Management requests are copied into a bounded
//! product lane so durable application services can answer them later through
//! the ordinary PRNS response command.

use allocator_api2::alloc::Allocator;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use heapless::Vec;
use personal_rns::engine::{
    AllowRequester, AllowRequesterFailure, CommandId, MAX_SEND_REQUEST_DATA_LEN,
    PacketReceiptDelivered, PrnsCommand, SendSinglePacketFailure, SetResourceStrategyFailure,
};
use personal_rns::identity::{IDENTITY_SECRET_KEY_LEN, IdentityHash, Zeroizing};
use personal_rns::routing::links::LinkId;
use personal_rns::routing::links::request::RequestId;
use personal_rns::routing::request_handlers::RequestPathHash;
use personal_rns::runtime::request_endpoints::{
    Decline, RequestContext, RequestEndpoint, RequestEndpointPolicy, RequestEndpointSet,
    RespondToken,
};
use personal_rns::runtime::{
    ManuallyAttached, PreConfiguredDestination, PrnsEvent, PrnsNodeRecipe,
};
use personal_rns::units::{InstantMillis, RttMillis};
use personal_rns::wire::DestinationHash;
use reticulum_device_api::{
    ApiErrorCode, ApiErrorResponse, ApiVersion, CapabilitySnapshot,
    DestinationHash as ApiDestinationHash, DeviceRequest, DeviceResponse, IdentitySummary,
    MAX_MESSAGE_BYTES, ResponseEnvelope, decode_request, encode_response,
};
pub use reticulum_device_api::{
    MANAGEMENT_ENROLLMENT_PATH, MANAGEMENT_ENROLLMENT_REQUEST_VALUE,
    MANAGEMENT_ENROLLMENT_SUCCESS_RESPONSE, MANAGEMENT_PUBLIC_PATH, MANAGEMENT_REQUEST_PATH,
};

use crate::ota::{OTA_NEXT_PATH, OTA_REBOOT_PATH, OTA_START_PATH, OTA_STATUS_PATH};
use crate::prns_applications::ApplicationCatalog;
use crate::prns_node::{
    APPLICATION_EVENT_CAPACITY, APPLICATION_SETTLEMENT_CAPACITY, EngineStorage,
};

/// Every identified-Link path unlocked by one durable management identity.
pub const MANAGEMENT_AUTHORIZED_PATHS: [&str; 5] = [
    MANAGEMENT_REQUEST_PATH,
    OTA_START_PATH,
    OTA_NEXT_PATH,
    OTA_STATUS_PATH,
    OTA_REBOOT_PATH,
];

/// Initial public Nomad Network page path.
pub const NOMAD_INDEX_PATH: &str = reticulum_nomad_protocol::DEFAULT_INDEX_PATH;
/// Canonical MessagePack `nil` supplied by an anonymous Nomad page request.
pub const NOMAD_ANONYMOUS_REQUEST_VALUE: [u8; 1] = [0xc0];
/// Static UTF-8 Micron page served by this appliance.
pub const NOMAD_INDEX_PAGE: &str = "\
>Metalbeard

This page is served directly by an embedded Rust Reticulum node.

The node is online and ready to exchange LXMF messages over the LoRa mesh.
";

const _: () = assert!(NOMAD_INDEX_PAGE.len() <= reticulum_nomad_protocol::MAX_PAGE_BYTES);

/// Build the ordinary PRNS command that admits one durable management identity.
///
/// Product enrollment owns when an identity becomes durable. PRNS remains the
/// sole live request gate and applies this command to its existing allow-list.
pub fn allow_management_requester(
    destination: DestinationHash,
    identity: IdentityHash,
) -> PrnsCommand {
    allow_management_requester_for_path(destination, MANAGEMENT_REQUEST_PATH, identity)
}

/// Build the ordinary PRNS command that admits one durable identity to one
/// exact management/OTA request route.
pub fn allow_management_requester_for_path(
    destination: DestinationHash,
    path: &str,
    identity: IdentityHash,
) -> PrnsCommand {
    PrnsCommand::AllowRequester(AllowRequester {
        destination,
        path_hash: RequestPathHash::of(path),
        identity,
    })
}

/// One owned management request waiting for product application dispatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagementRequest {
    kind: ManagementRequestKind,
    destination: DestinationHash,
    link_id: LinkId,
    request_id: RequestId,
    requester: Option<IdentityHash>,
    requested_at: InstantMillis,
    rtt: RttMillis,
    respond_token: RespondToken,
    data: Vec<u8, MAX_SEND_REQUEST_DATA_LEN>,
}

impl ManagementRequest {
    /// Product policy lane that admitted the request.
    pub const fn kind(&self) -> ManagementRequestKind {
        self.kind
    }

    /// Destination on which PRNS admitted this request.
    pub const fn destination(&self) -> DestinationHash {
        self.destination
    }

    /// Link carrying this request.
    pub const fn link_id(&self) -> LinkId {
        self.link_id
    }

    /// PRNS request identifier used for response correlation.
    pub const fn request_id(&self) -> RequestId {
        self.request_id
    }

    /// Identified remote application identity, when the Link peer identified.
    pub const fn requester(&self) -> Option<IdentityHash> {
        self.requester
    }

    /// PRNS monotonic timestamp recovered from the request.
    pub const fn requested_at(&self) -> InstantMillis {
        self.requested_at
    }

    /// Link RTT observed when PRNS admitted the request.
    pub const fn rtt(&self) -> RttMillis {
        self.rtt
    }

    /// Token required to answer later through PRNS.
    pub const fn respond_token(&self) -> RespondToken {
        self.respond_token
    }

    /// Exact application bytes copied out of PRNS's synchronous request grant.
    pub fn data(&self) -> &[u8] {
        self.data.as_slice()
    }
}

/// Product management operation class selected by the registered PRNS path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagementRequestKind {
    /// Public read-only management route admitted without requester authorization.
    Public,
    /// Request admitted by PRNS's durable requester allow-list.
    Authorized,
    /// Enrollment candidate requiring identified Link and physical presence.
    Enrollment,
    /// Authorized request to validate a manifest and prepare the inactive slot.
    OtaStart,
    /// Authorized request to arm exactly the next OTA Resource on this Link.
    OtaNext,
    /// Authorized read of the product-owned OTA coordinator state.
    OtaStatus,
    /// Authorized reboot into an already verified and activated OTA slot.
    OtaReboot,
}

/// Bounded management-request lane shared by the PRNS and product owners.
pub type ManagementRequestChannel =
    Channel<CriticalSectionRawMutex, ManagementRequest, APPLICATION_EVENT_CAPACITY>;

/// Owned settlement for one PRNS-native management allow-list mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManagementAuthorizationSettlement {
    /// Command identifier returned when the product queued the mutation.
    pub id: CommandId,
    /// Exact result returned by PRNS after applying its own request policy.
    pub result: Result<(), AllowRequesterFailure>,
}

/// Bounded lane used to correlate product enrollment with PRNS settlement.
pub type ManagementAuthorizationSettlementChannel = Channel<
    CriticalSectionRawMutex,
    ManagementAuthorizationSettlement,
    APPLICATION_SETTLEMENT_CAPACITY,
>;

/// Owned settlement for one PRNS per-Link Resource gate mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OtaResourceStrategySettlement {
    /// Command identifier returned when the product queued the mutation.
    pub id: CommandId,
    /// Exact result returned by PRNS after changing the active Link.
    pub result: Result<(), SetResourceStrategyFailure>,
}

/// Bounded lane used to correlate OTA arming with PRNS settlement.
pub type OtaResourceStrategySettlementChannel = Channel<
    CriticalSectionRawMutex,
    OtaResourceStrategySettlement,
    APPLICATION_SETTLEMENT_CAPACITY,
>;

/// Owned settlement for one product-issued ordinary PRNS Single send.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LxmfOutboundSettlement {
    /// Command identifier returned when the product queued the send.
    pub id: CommandId,
    /// Exact ordinary PRNS packet-receipt result.
    pub result: Result<PacketReceiptDelivered, SendSinglePacketFailure>,
}

/// Bounded lane used to correlate durable LXMF intents with PRNS settlement.
pub type LxmfOutboundSettlementChannel =
    Channel<CriticalSectionRawMutex, LxmfOutboundSettlement, APPLICATION_SETTLEMENT_CAPACITY>;

/// One complete Device API response ready for PRNS's ordinary Link response command.
pub type ManagementResponse = Vec<u8, MAX_MESSAGE_BYTES>;

/// Why a copied management request could not produce a protocol response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagementDispatchError {
    /// The application bytes were not a valid current Device API request.
    MalformedRequest,
    /// A response that should fit the fixed Device API bound failed to encode.
    ResponseEncoding,
    /// The encoded response could not fit the matching fixed-capacity owner.
    ResponseCapacity,
}

/// Dispatch the read-only operations available before management enrollment.
///
/// The product owner supplies the capabilities it can actually serve; this
/// request layer only applies the public route and response framing.
pub fn dispatch_public_management_payload(
    data: &[u8],
    management: DestinationHash,
    lxmf: Option<DestinationHash>,
    capabilities: CapabilitySnapshot,
) -> Result<ManagementResponse, ManagementDispatchError> {
    dispatch_management_payload(
        data,
        management,
        lxmf,
        ManagementDispatchScope::Public,
        capabilities,
    )
}

/// Dispatch operations admitted by PRNS's identified-requester allow-list.
///
/// Known operations whose product owners have not moved receive
/// `CapabilityUnavailable`; they are not forwarded into the retired alpha
/// dispatcher.
pub fn dispatch_authorized_management_payload(
    data: &[u8],
    management: DestinationHash,
    lxmf: Option<DestinationHash>,
) -> Result<ManagementResponse, ManagementDispatchError> {
    dispatch_management_payload(
        data,
        management,
        lxmf,
        ManagementDispatchScope::Authorized,
        CapabilitySnapshot::for_dispatch(false),
    )
}

#[derive(Clone, Copy)]
enum ManagementDispatchScope {
    Public,
    Authorized,
}

fn dispatch_management_payload(
    data: &[u8],
    management: DestinationHash,
    lxmf: Option<DestinationHash>,
    scope: ManagementDispatchScope,
    capabilities: CapabilitySnapshot,
) -> Result<ManagementResponse, ManagementDispatchError> {
    let request = decode_request(data).map_err(|_| ManagementDispatchError::MalformedRequest)?;
    let response = match (scope, request.request) {
        (_, DeviceRequest::SystemCapabilities) => DeviceResponse::SystemCapabilities(capabilities),
        (_, DeviceRequest::IdentitySummary) => {
            let management = ApiDestinationHash(*management.as_bytes());
            let summary = match lxmf {
                Some(lxmf) => IdentitySummary::with_lxmf_delivery_destination(
                    management,
                    ApiDestinationHash(*lxmf.as_bytes()),
                ),
                None => IdentitySummary::new(management),
            };
            DeviceResponse::IdentitySummary(summary)
        }
        (ManagementDispatchScope::Public | ManagementDispatchScope::Authorized, operation) => {
            DeviceResponse::Error(ApiErrorResponse {
                code: ApiErrorCode::CapabilityUnavailable,
                operation: Some(operation.operation()),
            })
        }
    };
    let envelope = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: request.request_id,
        response,
    };
    let mut encoded = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_response(&envelope, &mut encoded)
        .map_err(|_| ManagementDispatchError::ResponseEncoding)?;
    Vec::from_slice(&encoded[..written]).map_err(|_| ManagementDispatchError::ResponseCapacity)
}

/// Immutable application state borrowed by PRNS request handlers.
pub struct PrnsApplicationState {
    management: DestinationHash,
    nomad: DestinationHash,
    lxmf: Option<DestinationHash>,
    management_requests: &'static ManagementRequestChannel,
}

impl PrnsApplicationState {
    /// Bind protocol destinations to the product's bounded request lane.
    pub const fn new(
        management: DestinationHash,
        nomad: DestinationHash,
        lxmf: Option<DestinationHash>,
        management_requests: &'static ManagementRequestChannel,
    ) -> Self {
        Self {
            management,
            nomad,
            lxmf,
            management_requests,
        }
    }

    /// Shared management and OTA destination.
    pub const fn management(&self) -> DestinationHash {
        self.management
    }

    /// Nomad Network node destination.
    pub const fn nomad(&self) -> DestinationHash {
        self.nomad
    }

    /// LXMF delivery destination when the messaging application is enabled.
    pub const fn lxmf(&self) -> Option<DestinationHash> {
        self.lxmf
    }

    /// Bounded lane consumed by the product application owner.
    pub const fn management_requests(&self) -> &'static ManagementRequestChannel {
        self.management_requests
    }
}

/// PRNS request-route set installed on every destination that elects to serve it.
#[derive(Clone, Copy, Debug, Default)]
pub struct ApplicationRequestRoutes;

impl RequestEndpointSet<PrnsApplicationState> for ApplicationRequestRoutes {
    const REGISTRATIONS: &'static [(&'static str, RequestEndpointPolicy)] = &[
        (MANAGEMENT_PUBLIC_PATH, RequestEndpointPolicy::AllowAll),
        (
            MANAGEMENT_REQUEST_PATH,
            RequestEndpointPolicy::AllowList(&[]),
        ),
        (MANAGEMENT_ENROLLMENT_PATH, RequestEndpointPolicy::AllowAll),
        (OTA_START_PATH, RequestEndpointPolicy::AllowList(&[])),
        (OTA_NEXT_PATH, RequestEndpointPolicy::AllowList(&[])),
        (OTA_STATUS_PATH, RequestEndpointPolicy::AllowList(&[])),
        (OTA_REBOOT_PATH, RequestEndpointPolicy::AllowList(&[])),
        (NOMAD_INDEX_PATH, RequestEndpointPolicy::AllowAll),
    ];

    async fn dispatch(
        context: RequestContext<'_, PrnsApplicationState>,
        path_hash: RequestPathHash,
    ) -> Result<(), Decline> {
        if path_hash == RequestPathHash::of(MANAGEMENT_PUBLIC_PATH) {
            return PublicManagementEndpoint::handle(context).await;
        }
        if path_hash == RequestPathHash::of(MANAGEMENT_REQUEST_PATH) {
            return ManagementEndpoint::handle(context).await;
        }
        if path_hash == RequestPathHash::of(MANAGEMENT_ENROLLMENT_PATH) {
            return EnrollmentEndpoint::handle(context).await;
        }
        if path_hash == RequestPathHash::of(OTA_START_PATH) {
            return OtaStartEndpoint::handle(context).await;
        }
        if path_hash == RequestPathHash::of(OTA_NEXT_PATH) {
            return OtaNextEndpoint::handle(context).await;
        }
        if path_hash == RequestPathHash::of(OTA_STATUS_PATH) {
            return OtaStatusEndpoint::handle(context).await;
        }
        if path_hash == RequestPathHash::of(OTA_REBOOT_PATH) {
            return OtaRebootEndpoint::handle(context).await;
        }
        if path_hash == RequestPathHash::of(NOMAD_INDEX_PATH) {
            return NomadIndexEndpoint::handle(context).await;
        }
        Err(Decline::Ignore)
    }
}

struct PublicManagementEndpoint;

impl RequestEndpoint<PrnsApplicationState> for PublicManagementEndpoint {
    const ENDPOINT_ID: &'static str = MANAGEMENT_PUBLIC_PATH;
    const POLICY: RequestEndpointPolicy = RequestEndpointPolicy::AllowAll;

    async fn handle(context: RequestContext<'_, PrnsApplicationState>) -> Result<(), Decline> {
        copy_management_request(context, ManagementRequestKind::Public)
    }
}

struct ManagementEndpoint;

impl RequestEndpoint<PrnsApplicationState> for ManagementEndpoint {
    const ENDPOINT_ID: &'static str = MANAGEMENT_REQUEST_PATH;
    const POLICY: RequestEndpointPolicy = RequestEndpointPolicy::AllowList(&[]);

    async fn handle(context: RequestContext<'_, PrnsApplicationState>) -> Result<(), Decline> {
        copy_management_request(context, ManagementRequestKind::Authorized)
    }
}

struct EnrollmentEndpoint;

impl RequestEndpoint<PrnsApplicationState> for EnrollmentEndpoint {
    const ENDPOINT_ID: &'static str = MANAGEMENT_ENROLLMENT_PATH;
    const POLICY: RequestEndpointPolicy = RequestEndpointPolicy::AllowAll;

    async fn handle(context: RequestContext<'_, PrnsApplicationState>) -> Result<(), Decline> {
        copy_management_request(context, ManagementRequestKind::Enrollment)
    }
}

struct OtaStartEndpoint;

impl RequestEndpoint<PrnsApplicationState> for OtaStartEndpoint {
    const ENDPOINT_ID: &'static str = OTA_START_PATH;
    const POLICY: RequestEndpointPolicy = RequestEndpointPolicy::AllowList(&[]);

    async fn handle(context: RequestContext<'_, PrnsApplicationState>) -> Result<(), Decline> {
        copy_management_request(context, ManagementRequestKind::OtaStart)
    }
}

struct OtaNextEndpoint;

impl RequestEndpoint<PrnsApplicationState> for OtaNextEndpoint {
    const ENDPOINT_ID: &'static str = OTA_NEXT_PATH;
    const POLICY: RequestEndpointPolicy = RequestEndpointPolicy::AllowList(&[]);

    async fn handle(context: RequestContext<'_, PrnsApplicationState>) -> Result<(), Decline> {
        copy_management_request(context, ManagementRequestKind::OtaNext)
    }
}

struct OtaStatusEndpoint;

impl RequestEndpoint<PrnsApplicationState> for OtaStatusEndpoint {
    const ENDPOINT_ID: &'static str = OTA_STATUS_PATH;
    const POLICY: RequestEndpointPolicy = RequestEndpointPolicy::AllowList(&[]);

    async fn handle(context: RequestContext<'_, PrnsApplicationState>) -> Result<(), Decline> {
        copy_management_request(context, ManagementRequestKind::OtaStatus)
    }
}

struct OtaRebootEndpoint;

impl RequestEndpoint<PrnsApplicationState> for OtaRebootEndpoint {
    const ENDPOINT_ID: &'static str = OTA_REBOOT_PATH;
    const POLICY: RequestEndpointPolicy = RequestEndpointPolicy::AllowList(&[]);

    async fn handle(context: RequestContext<'_, PrnsApplicationState>) -> Result<(), Decline> {
        copy_management_request(context, ManagementRequestKind::OtaReboot)
    }
}

fn copy_management_request(
    context: RequestContext<'_, PrnsApplicationState>,
    kind: ManagementRequestKind,
) -> Result<(), Decline> {
    if context.destination != context.state.management {
        return Err(Decline::Ignore);
    }
    let data = Vec::from_slice(context.data).map_err(|_| Decline::Ignore)?;
    let token = context.respond_token();
    let request = ManagementRequest {
        kind,
        destination: context.destination,
        link_id: token.link_id,
        request_id: token.request_id,
        requester: context.requester,
        requested_at: context.requested_at,
        rtt: token.rtt,
        respond_token: token,
        data,
    };
    context
        .state
        .management_requests
        .try_send(request)
        .map_err(|_| Decline::Ignore)?;
    Err(Decline::Ignore)
}

struct NomadIndexEndpoint;

impl RequestEndpoint<PrnsApplicationState> for NomadIndexEndpoint {
    const ENDPOINT_ID: &'static str = NOMAD_INDEX_PATH;
    const POLICY: RequestEndpointPolicy = RequestEndpointPolicy::AllowAll;

    async fn handle(mut context: RequestContext<'_, PrnsApplicationState>) -> Result<(), Decline> {
        if context.destination != context.state.nomad
            || context.data != NOMAD_ANONYMOUS_REQUEST_VALUE
        {
            return Err(Decline::Ignore);
        }
        context.respond(NOMAD_INDEX_PAGE.as_bytes())
    }
}

/// Concrete application recipe type accepted by unchanged PRNS.
pub type E290ApplicationRecipe<'a, A, F, P> = PrnsNodeRecipe<
    Vec<PreConfiguredDestination<'a>, { crate::prns_storage::APPLICATION_DESTINATION_CAPACITY }>,
    PrnsApplicationState,
    ApplicationRequestRoutes,
    F,
    ManuallyAttached,
    EngineStorage<A>,
    P,
>;

/// Compose the product destinations, request routes, storage, and persistence
/// through PRNS's public recipe.
pub fn application_recipe<'a, A, F, P>(
    transport_identity: Option<Zeroizing<[u8; IDENTITY_SECRET_KEY_LEN]>>,
    catalog: ApplicationCatalog<'a>,
    management_requests: &'static ManagementRequestChannel,
    persistence: P,
    on_event: F,
) -> E290ApplicationRecipe<'a, A, F, P>
where
    A: Allocator + Default,
    F: FnMut(PrnsEvent<'_>, &PrnsApplicationState),
{
    let state = PrnsApplicationState::new(
        catalog.management,
        catalog.nomad,
        catalog.lxmf,
        management_requests,
    );
    PrnsNodeRecipe {
        transport_identity,
        pre_configured_destinations: catalog.destinations,
        app_state: state,
        storage: EngineStorage::<A>::default(),
        request_endpoints: ApplicationRequestRoutes,
        interfaces: ManuallyAttached,
        persistence,
        on_event,
    }
}

#[cfg(test)]
mod tests {
    use allocator_api2::alloc::Global;
    use personal_rns::engine::InstantMillis;
    use personal_rns::routing::links::LinkId;
    use personal_rns::routing::links::request::RequestId;
    use personal_rns::routing::request_handlers::{RequestHandlerError, RequestPathHash};
    use personal_rns::runtime::request_endpoints::{Decline, InboundRequest, dispatch_request};
    use personal_rns::runtime::{NoPersistence, assemble_node};
    use personal_rns::units::RttMillis;
    use reticulum_device_api::{
        ApiVersion, DeviceRequest, DeviceResponse, RequestEnvelope, RequestId as ApiRequestId,
        decode_response, encode_request,
    };

    use super::*;
    use crate::prns_applications::{ApplicationProfile, application_catalog};

    const IDENTITY: [u8; IDENTITY_SECRET_KEY_LEN] = [0x43; IDENTITY_SECRET_KEY_LEN];
    static MANAGEMENT_REQUESTS: ManagementRequestChannel = ManagementRequestChannel::new();

    fn state() -> PrnsApplicationState {
        let catalog =
            application_catalog(&IDENTITY, b"", ApplicationProfile::new(true, false)).unwrap();
        PrnsApplicationState::new(
            catalog.management,
            catalog.nomad,
            catalog.lxmf,
            &MANAGEMENT_REQUESTS,
        )
    }

    fn inbound(destination: DestinationHash, data: &[u8]) -> InboundRequest<'_> {
        InboundRequest::new(
            destination,
            LinkId::new([0x21; 16]),
            RequestId([0x22; 16]),
            Some(IdentityHash::new([0x23; 16])),
            InstantMillis(24),
            RttMillis::new(25),
            data,
        )
    }

    #[test]
    fn unchanged_prns_assembles_the_complete_default_application_recipe() {
        let catalog = application_catalog(
            &IDENTITY,
            b"lxmf announce",
            ApplicationProfile::new(true, false),
        )
        .unwrap();
        let management = catalog.management;
        let nomad = catalog.nomad;
        let lxmf = catalog.lxmf.unwrap();
        let recipe = application_recipe::<Global, _, _>(
            None,
            catalog,
            &MANAGEMENT_REQUESTS,
            NoPersistence,
            |_, _| {},
        );
        let (mut node, ManuallyAttached, NoPersistence) = assemble_node(recipe);

        assert_eq!(node.engine.upstream_app_destinations().count(), 3);
        assert_eq!(node.engine.held_identity_hashes().len(), 1);
        let requester = IdentityHash::new([0x81; 16]);
        for destination in [management, nomad] {
            assert_eq!(
                node.engine
                    .allow_requester(&destination, MANAGEMENT_REQUEST_PATH, requester),
                Ok(()),
            );
            assert_eq!(
                node.engine
                    .allow_requester(&destination, NOMAD_INDEX_PATH, requester),
                Err(RequestHandlerError::NoAllowList),
            );
        }
        assert_eq!(
            node.engine
                .allow_requester(&lxmf, MANAGEMENT_REQUEST_PATH, requester),
            Err(RequestHandlerError::NoSuchHandler),
        );
    }

    #[test]
    fn nomad_route_only_answers_on_the_nomad_destination_for_nil() {
        let state = state();
        let mut response = std::vec::Vec::new();
        let result = embassy_futures::block_on(dispatch_request::<_, ApplicationRequestRoutes>(
            &state,
            RequestPathHash::of(NOMAD_INDEX_PATH),
            inbound(state.nomad(), &NOMAD_ANONYMOUS_REQUEST_VALUE),
            &mut response,
        ));
        assert_eq!(result, Ok(()));
        assert_eq!(response, NOMAD_INDEX_PAGE.as_bytes());

        let wrong_destination =
            embassy_futures::block_on(dispatch_request::<_, ApplicationRequestRoutes>(
                &state,
                RequestPathHash::of(NOMAD_INDEX_PATH),
                inbound(state.management(), &NOMAD_ANONYMOUS_REQUEST_VALUE),
                &mut std::vec::Vec::new(),
            ));
        assert_eq!(wrong_destination, Err(Decline::Ignore));
    }

    #[test]
    fn management_route_copies_the_request_before_deferring_its_response() {
        while MANAGEMENT_REQUESTS.try_receive().is_ok() {}
        let state = state();
        let bytes = b"bounded management request";
        let result = embassy_futures::block_on(dispatch_request::<_, ApplicationRequestRoutes>(
            &state,
            RequestPathHash::of(MANAGEMENT_REQUEST_PATH),
            inbound(state.management(), bytes),
            &mut std::vec::Vec::new(),
        ));
        assert_eq!(result, Err(Decline::Ignore));

        let copied = MANAGEMENT_REQUESTS.try_receive().unwrap();
        assert_eq!(copied.kind(), ManagementRequestKind::Authorized);
        assert_eq!(copied.destination(), state.management());
        assert_eq!(copied.requester(), Some(IdentityHash::new([0x23; 16])));
        assert_eq!(copied.data(), bytes);
    }

    #[test]
    fn public_management_route_is_separate_from_the_authorized_path() {
        while MANAGEMENT_REQUESTS.try_receive().is_ok() {}
        let state = state();
        let result = embassy_futures::block_on(dispatch_request::<_, ApplicationRequestRoutes>(
            &state,
            RequestPathHash::of(MANAGEMENT_PUBLIC_PATH),
            inbound(state.management(), b"public read"),
            &mut std::vec::Vec::new(),
        ));
        assert_eq!(result, Err(Decline::Ignore));
        assert_eq!(
            MANAGEMENT_REQUESTS.try_receive().unwrap().kind(),
            ManagementRequestKind::Public
        );
    }

    #[test]
    fn enrollment_route_copies_an_identified_unknown_peer_for_product_policy() {
        while MANAGEMENT_REQUESTS.try_receive().is_ok() {}
        let state = state();
        let result = embassy_futures::block_on(dispatch_request::<_, ApplicationRequestRoutes>(
            &state,
            RequestPathHash::of(MANAGEMENT_ENROLLMENT_PATH),
            inbound(state.management(), &MANAGEMENT_ENROLLMENT_REQUEST_VALUE),
            &mut std::vec::Vec::new(),
        ));
        assert_eq!(result, Err(Decline::Ignore));

        let copied = MANAGEMENT_REQUESTS.try_receive().unwrap();
        assert_eq!(copied.kind(), ManagementRequestKind::Enrollment);
        assert_eq!(copied.destination(), state.management());
        assert_eq!(copied.requester(), Some(IdentityHash::new([0x23; 16])));
        assert_eq!(copied.data(), MANAGEMENT_ENROLLMENT_REQUEST_VALUE);
    }

    #[test]
    fn ota_control_routes_copy_only_to_the_shared_management_destination() {
        while MANAGEMENT_REQUESTS.try_receive().is_ok() {}
        let state = state();
        for (path, kind) in [
            (OTA_START_PATH, ManagementRequestKind::OtaStart),
            (OTA_NEXT_PATH, ManagementRequestKind::OtaNext),
            (OTA_STATUS_PATH, ManagementRequestKind::OtaStatus),
            (OTA_REBOOT_PATH, ManagementRequestKind::OtaReboot),
        ] {
            let result =
                embassy_futures::block_on(dispatch_request::<_, ApplicationRequestRoutes>(
                    &state,
                    RequestPathHash::of(path),
                    inbound(state.management(), b"ota"),
                    &mut std::vec::Vec::new(),
                ));
            assert_eq!(result, Err(Decline::Ignore));
            let copied = MANAGEMENT_REQUESTS.try_receive().unwrap();
            assert_eq!(copied.kind(), kind);
            assert_eq!(copied.destination(), state.management());
        }
    }

    #[test]
    fn management_authorization_uses_prns_native_allow_list_command() {
        let state = state();
        let identity = IdentityHash::new([0x91; 16]);
        assert_eq!(
            allow_management_requester(state.management(), identity),
            PrnsCommand::AllowRequester(AllowRequester {
                destination: state.management(),
                path_hash: RequestPathHash::of(MANAGEMENT_REQUEST_PATH),
                identity,
            })
        );
    }

    #[test]
    fn only_privileged_management_uses_the_prns_allow_list() {
        assert!(ApplicationRequestRoutes::REGISTRATIONS.contains(&(
            MANAGEMENT_REQUEST_PATH,
            RequestEndpointPolicy::AllowList(&[]),
        )));
        assert!(
            ApplicationRequestRoutes::REGISTRATIONS
                .contains(&(MANAGEMENT_PUBLIC_PATH, RequestEndpointPolicy::AllowAll,))
        );
        assert!(
            ApplicationRequestRoutes::REGISTRATIONS
                .contains(&(MANAGEMENT_ENROLLMENT_PATH, RequestEndpointPolicy::AllowAll,))
        );
        for path in MANAGEMENT_AUTHORIZED_PATHS {
            assert!(
                ApplicationRequestRoutes::REGISTRATIONS
                    .contains(&(path, RequestEndpointPolicy::AllowList(&[]),))
            );
        }
    }

    #[test]
    fn prns_native_management_dispatches_public_identity_without_legacy_node_state() {
        let state = state();
        let mut request_bytes = [0_u8; MAX_MESSAGE_BYTES];
        let request_len = encode_request(
            &RequestEnvelope {
                version: ApiVersion::CURRENT,
                request_id: ApiRequestId(41),
                request: DeviceRequest::IdentitySummary,
            },
            &mut request_bytes,
        )
        .unwrap();

        let response = dispatch_public_management_payload(
            &request_bytes[..request_len],
            state.management(),
            state.lxmf(),
            CapabilitySnapshot::for_dispatch(false),
        )
        .unwrap();
        let response = decode_response(response.as_slice()).unwrap();

        assert_eq!(response.request_id, ApiRequestId(41));
        let DeviceResponse::IdentitySummary(summary) = response.response else {
            panic!("identity request must return the identity response");
        };
        assert_eq!(
            summary.primary_destination().0,
            *state.management().as_bytes()
        );
        assert_eq!(
            summary.lxmf_delivery_destination().map(|hash| hash.0),
            state.lxmf().map(|hash| *hash.as_bytes())
        );
    }

    #[test]
    fn public_capability_read_preserves_the_product_owned_availability() {
        let state = state();
        let capabilities = CapabilitySnapshot::for_dispatch(false)
            .with_dispatch_network_config(reticulum_device_api::CapabilityAvailability::Available);
        let mut request_bytes = [0_u8; MAX_MESSAGE_BYTES];
        let request_len = encode_request(
            &RequestEnvelope {
                version: ApiVersion::CURRENT,
                request_id: ApiRequestId(43),
                request: DeviceRequest::SystemCapabilities,
            },
            &mut request_bytes,
        )
        .unwrap();

        let response = dispatch_public_management_payload(
            &request_bytes[..request_len],
            state.management(),
            state.lxmf(),
            capabilities,
        )
        .unwrap();
        let response = decode_response(response.as_slice()).unwrap();
        let DeviceResponse::SystemCapabilities(response) = response.response else {
            panic!("capability request must return the supplied product snapshot");
        };
        assert_eq!(
            response.network_config(),
            reticulum_device_api::CapabilityAvailability::Available
        );
    }

    #[test]
    fn unported_management_operations_fail_as_capability_unavailable() {
        let state = state();
        let mut request_bytes = [0_u8; MAX_MESSAGE_BYTES];
        let request_len = encode_request(
            &RequestEnvelope {
                version: ApiVersion::CURRENT,
                request_id: ApiRequestId(42),
                request: DeviceRequest::ManualServiceAnnounce,
            },
            &mut request_bytes,
        )
        .unwrap();

        let response = dispatch_authorized_management_payload(
            &request_bytes[..request_len],
            state.management(),
            state.lxmf(),
        )
        .unwrap();
        let response = decode_response(response.as_slice()).unwrap();

        assert_eq!(
            response.response,
            DeviceResponse::Error(ApiErrorResponse {
                code: ApiErrorCode::CapabilityUnavailable,
                operation: Some(reticulum_device_api::OP_MANUAL_SERVICE_ANNOUNCE),
            })
        );
    }
}
