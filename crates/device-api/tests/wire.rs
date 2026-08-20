use reticulum_device_api::{
    API_VERSION_MAJOR, API_VERSION_MINOR, ApiErrorCode, ApiErrorResponse, ApiVersion,
    AuthorizationError, CapabilityAvailability, CapabilitySnapshot, DecodeError, DestinationHash,
    DeviceRequest, DeviceResponse, DiagnosticInterfaceKind, DiagnosticInterfaceRecord,
    DiagnosticInterfaceState, DiagnosticLoraDataTxEvidence, DiagnosticLoraLastDataTx,
    DiagnosticLoraLastRx, DiagnosticLoraLastTx, DiagnosticLoraTxFamily, DiagnosticLoraTxOutcome,
    DispatchContext, DispatchProvenance, DispatchProvenanceError, EncodeError, EncodedPacketSha256,
    IdempotencyKey, IdentityHash, IdentitySummary, IngressObservation, IngressSignal,
    LoraDiagnostics, MAX_BODY_BYTES, MAX_CBOR_NESTING_DEPTH, MAX_DIAGNOSTIC_INTERFACES,
    MAX_LXMF_BASIC_CONTENT_BYTES, MAX_LXMF_BASIC_TITLE_BYTES, MAX_LXMF_PEER_APP_DATA_BYTES,
    MAX_MESSAGE_BYTES, MAX_NOMAD_PAGE_BYTES, MAX_NOMAD_PAGE_PATH_BYTES,
    MAX_RADIO_TRACE_PAGE_ENTRIES, MAX_ROUTE_DIAGNOSTIC_PAGE_ENTRIES,
    MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES, ManualServiceAnnounceDisposition, NodeDiagnosticsSnapshot,
    OP_IDENTITY_SUMMARY, OP_MANUAL_SERVICE_ANNOUNCE, OP_NODE_DIAGNOSTICS, OP_RADIO_TRACE_PAGE,
    OP_RETICULUM_PROBE_POLL, OP_RETICULUM_PROBE_START, OP_ROUTE_DIAGNOSTICS_PAGE,
    OP_SUBMISSION_STATUS, Permissions, PreparedPacketDetails, PrincipalId, ProbeFailure, ProbeId,
    ProbePhase, ProbePollRequest, ProbePollResponse, ProbeStartAccepted, ProbeStartOutcome,
    ProbeStartRequest, ProbeSuccess, RadioTraceAppliedLoraProfile, RadioTraceAttemptOutcome,
    RadioTraceAttemptTerminal, RadioTraceAttemptToken, RadioTraceCursor, RadioTraceDataTx,
    RadioTraceEvent, RadioTraceEventKind, RadioTraceInboundProof, RadioTraceInboundProofPacket,
    RadioTraceInboundProofStage, RadioTraceLogicalRx, RadioTracePacketEvidence, RadioTracePage,
    RadioTracePageRequest, RadioTraceRouteSelected, RadioTraceTxOutcome, RequestEnvelope,
    RequestId, RequiredField, RequiredPermission, ResponseEnvelope, RnsDiagnostics,
    RouteDiagnosticEntry, RouteDiagnosticResolution, RouteDiagnosticsPage, RouteDiagnosticsRequest,
    SubmissionFailure, SubmissionId, SubmissionState, SubmissionStatus, authorize_request,
    decode_request, decode_response, encode_request, encode_response,
};
#[cfg(feature = "network-config")]
use reticulum_device_api::{
    DEFAULT_RETICULUM_TCP_PORT, DeviceName, DeviceNameSummary, GatewayPolicy, LoraRadioProfile,
    LoraTransmitPowerDbm, MAX_DEVICE_NAME_BYTES, MAX_RETICULUM_DNS_DHCP_SERVERS,
    MAX_RETICULUM_DNS_RAW_ATTEMPTS, MAX_RETICULUM_TCP_PEER_HOSTNAME_BYTES,
    MAX_WIFI_NETWORK_PROFILES, MAX_WIFI_PASSPHRASE_BYTES, MAX_WIFI_SSID_BYTES,
    MIN_WIFI_PASSPHRASE_BYTES, NetworkConfigMutation, NetworkConfigMutationOutcome,
    NetworkConfigMutationRequest, NetworkConfigSnapshot, NetworkRuntimeStatus,
    OP_NETWORK_CONFIG_GET, OP_NETWORK_CONFIG_MUTATE, OP_NETWORK_STATUS, ReticulumDnsDiagnostics,
    ReticulumDnsPrimaryOutcome, ReticulumDnsRawAttempt, ReticulumDnsRawOutcome,
    ReticulumDnsRawSetupState, ReticulumDnsRawSource, ReticulumDnsResolution,
    ReticulumDnsResolutionSource, ReticulumTcpFailure, ReticulumTcpPeerConfigSummary,
    ReticulumTcpPeerHostConfigSummary, ReticulumTcpPeerHostUpdate, ReticulumTcpPeerHostname,
    ReticulumTcpPeerIpv4Address, ReticulumTcpPeerState, ReticulumTcpPeerUpdate, RmapConfig,
    RmapDeferredReason, RmapEgressConfirmation, RmapInitialTcpGateState, RmapLocation,
    RmapQueueOutcome, RmapRuntimeStatus, RmapStampPhase, WifiCredentialUpdate,
    WifiNetworkConfigSummary, WifiNetworkProfileId, WifiNetworkUpdate, WifiStationState,
};
#[cfg(feature = "lxmf")]
use reticulum_device_api::{
    LxmfBasicSendAccepted, LxmfDiscoveredPeer, LxmfMailboxStatus, LxmfMessageHandle,
    LxmfMessageLocation, LxmfMessageSummary, LxmfPeerDiscoveryCursor, LxmfPeerDiscoveryIncarnation,
    LxmfPeerDiscoveryPage, LxmfPeerGeneration, LxmfReadChunk, LxmfReadLength,
    MAX_LXMF_READ_CHUNK_BYTES, OP_LXMF_BASIC_SEND, OP_LXMF_MAILBOX_ACKNOWLEDGE,
    OP_LXMF_MAILBOX_STATUS, OP_LXMF_NEXT, OP_LXMF_PEER_NEXT, OP_LXMF_READ,
};
#[cfg(feature = "nomad")]
use reticulum_device_api::{
    MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS, NomadFetchFailure, NomadFetchId, NomadFetchPhase,
    NomadFetchPollRequest, NomadFetchPollResponse, NomadFetchStartAccepted, NomadFetchStartOutcome,
    NomadFetchStartRequest, NomadPage, NomadPagePath, NomadRequestTimestampUnixMs,
    OP_NOMAD_FETCH_POLL, OP_NOMAD_FETCH_START,
};
#[cfg(feature = "rns-data")]
use reticulum_device_api::{OP_SUBMIT_RNS_DATA, SubmissionAccepted};

const GOLDEN_CAPABILITIES_REQUEST: &[u8] = &[
    0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x01, 0x03, 0xa0,
];
const GOLDEN_PROBE_START_REQUEST: &[u8] = &[
    0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x12, 0x03, 0xa2,
    0x00, 0x50, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
    0x0e, 0x0f, 0x01, 0x50, 0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xfb,
    0xfc, 0xfd, 0xfe, 0xff,
];
const GOLDEN_PROBE_POLL_REQUEST: &[u8] = &[
    0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x13, 0x03, 0xa1,
    0x00, 0x50, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xab, 0xac, 0xad,
    0xae, 0xaf,
];

fn golden_capabilities_response() -> Vec<u8> {
    let mut encoded = vec![
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x01, 0x03, 0xb4, 0x00,
        0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0xf4, 0x02, 0x00, 0x03,
    ];
    encoded.push(if cfg!(feature = "rns-data") {
        0xf5
    } else {
        0xf4
    });
    encoded.extend_from_slice(&[
        0x04, 0x19, 0x02, 0x00, 0x05, 0x19, 0x01, 0xc0, 0x06, 0x19, 0x01, 0x7f,
    ]);
    encoded.extend_from_slice(&[0x09, if cfg!(feature = "lxmf") { 0x02 } else { 0x00 }, 0x0a]);
    if cfg!(feature = "lxmf") {
        encoded.extend_from_slice(&[0x19, 0x01, 0xa0]);
    } else {
        encoded.push(0x00);
    }
    encoded.extend_from_slice(&[0x0b, if cfg!(feature = "lxmf") { 0x02 } else { 0x00 }, 0x0c]);
    if cfg!(feature = "lxmf") {
        encoded.extend_from_slice(&[0x19, 0x01, 0x27]);
    } else {
        encoded.push(0x00);
    }
    encoded.push(0x0d);
    if cfg!(feature = "lxmf") {
        encoded.extend_from_slice(&[0x19, 0x01, 0x27]);
    } else {
        encoded.push(0x00);
    }
    encoded.extend_from_slice(&[0x0e, if cfg!(feature = "lxmf") { 0x02 } else { 0x00 }, 0x0f]);
    if cfg!(feature = "lxmf") {
        encoded.extend_from_slice(&[0x19, 0x01, 0x00]);
    } else {
        encoded.push(0x00);
    }
    encoded.extend_from_slice(&[
        0x10,
        if cfg!(feature = "nomad") { 0x02 } else { 0x00 },
        0x11,
    ]);
    if cfg!(feature = "nomad") {
        encoded.extend_from_slice(&[0x18, 0x80]);
    } else {
        encoded.push(0x00);
    }
    encoded.push(0x12);
    if cfg!(feature = "nomad") {
        encoded.extend_from_slice(&[0x19, 0x01, 0x90]);
    } else {
        encoded.push(0x00);
    }
    encoded.extend_from_slice(&[
        0x13,
        if cfg!(feature = "network-config") {
            0x02
        } else {
            0x00
        },
        0x14,
        0x02,
        0x15,
        0x02,
    ]);
    encoded
}

fn capabilities_request() -> RequestEnvelope<'static> {
    RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        request: DeviceRequest::SystemCapabilities,
    }
}

const PRIMARY_DESTINATION: DestinationHash = DestinationHash([
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
]);
const LXMF_DELIVERY_DESTINATION: DestinationHash = DestinationHash([
    0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f,
]);
const PROBE_DESTINATION: DestinationHash = DestinationHash([
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
]);

fn probe_id() -> ProbeId {
    ProbeId::new([
        0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xab, 0xac, 0xad, 0xae,
        0xaf,
    ])
    .unwrap()
}

fn probe_start_request() -> RequestEnvelope<'static> {
    RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        request: DeviceRequest::ReticulumProbeStart(ProbeStartRequest::new(
            PROBE_DESTINATION,
            IdempotencyKey([
                0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc, 0xfd,
                0xfe, 0xff,
            ]),
        )),
    }
}

fn identity_request() -> RequestEnvelope<'static> {
    RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        request: DeviceRequest::IdentitySummary,
    }
}

fn dispatch_provenance() -> DispatchProvenance {
    DispatchProvenance::new([0x22; 16], 7, 11, 3)
        .unwrap_or_else(|fault| panic!("valid dispatch provenance rejected: {fault:?}"))
}

#[test]
fn exact_capabilities_request_golden_round_trip() {
    let mut output = [0u8; MAX_MESSAGE_BYTES];
    let written = encode_request(&capabilities_request(), &mut output).unwrap();
    assert_eq!(&output[..written], GOLDEN_CAPABILITIES_REQUEST);
    assert_eq!(
        decode_request(GOLDEN_CAPABILITIES_REQUEST).unwrap(),
        capabilities_request()
    );
}

#[test]
fn exact_capabilities_response_golden_round_trip() {
    let envelope = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        response: DeviceResponse::SystemCapabilities(CapabilitySnapshot::current()),
    };
    let mut output = [0u8; MAX_MESSAGE_BYTES];
    let written = encode_response(&envelope, &mut output).unwrap();
    let golden = golden_capabilities_response();
    assert_eq!(&output[..written], golden);
    assert_eq!(decode_response(&golden).unwrap(), envelope);
}

#[test]
fn capability_snapshot_requires_the_complete_v3_surface() {
    let mut incomplete = golden_capabilities_response();
    incomplete[13] = 0xb3;
    incomplete.truncate(incomplete.len() - 2);
    assert_eq!(
        decode_response(&incomplete),
        Err(DecodeError::MissingField(
            RequiredField::CapabilityReticulumProbe
        ))
    );
}

#[test]
fn lxmf_capability_availability_is_a_closed_wire_vocabulary() {
    let mut encoded = golden_capabilities_response();
    let key = encoded
        .windows(2)
        .rposition(|window| window == [0x09, CapabilitySnapshot::current().lxmf().wire_code()])
        .expect("capability key 9");
    encoded.splice(key + 1..=key + 1, [0x18, 99]);
    assert_eq!(
        decode_response(&encoded),
        Err(DecodeError::InvalidValue {
            field: RequiredField::CapabilityLxmf,
            value: 99,
        })
    );
}

#[test]
fn exact_identity_summary_goldens_round_trip() {
    const REQUEST: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x03, 0x03, 0xa0,
    ];
    const RESPONSE: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x03, 0x03, 0xa1, 0x00,
        0x50, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e, 0x1f,
    ];
    let response = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        response: DeviceResponse::IdentitySummary(IdentitySummary::new(PRIMARY_DESTINATION)),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let request_len = encode_request(&identity_request(), &mut output).unwrap();
    assert_eq!(&output[..request_len], REQUEST);
    assert_eq!(decode_request(REQUEST).unwrap(), identity_request());
    let response_len = encode_response(&response, &mut output).unwrap();
    assert_eq!(&output[..response_len], RESPONSE);
    assert_eq!(decode_response(RESPONSE).unwrap(), response);
}

#[test]
fn exact_identity_summary_with_lxmf_destination_golden_round_trip() {
    const RESPONSE: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x03, 0x03, 0xa2, 0x00,
        0x50, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e, 0x1f, 0x01, 0x50, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a,
        0x2b, 0x2c, 0x2d, 0x2e, 0x2f,
    ];
    let summary = IdentitySummary::with_lxmf_delivery_destination(
        PRIMARY_DESTINATION,
        LXMF_DELIVERY_DESTINATION,
    );
    let response = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        response: DeviceResponse::IdentitySummary(summary),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let response_len = encode_response(&response, &mut output).unwrap();
    assert_eq!(&output[..response_len], RESPONSE);
    assert_eq!(decode_response(RESPONSE).unwrap(), response);
    assert_eq!(summary.primary_destination(), PRIMARY_DESTINATION);
    assert_eq!(
        summary.lxmf_delivery_destination(),
        Some(LXMF_DELIVERY_DESTINATION)
    );
}

#[test]
fn identity_summary_is_copy_only_public_and_read_only() {
    fn assert_copy<T: Copy>() {}
    assert_copy::<IdentitySummary>();
    assert_eq!((API_VERSION_MAJOR, API_VERSION_MINOR), (3, 0));
    assert_eq!(OP_IDENTITY_SUMMARY, 0x0003);

    let summary = IdentitySummary::new(PRIMARY_DESTINATION);
    assert_eq!(summary.primary_destination(), PRIMARY_DESTINATION);
    assert_eq!(summary.lxmf_delivery_destination(), None);
    assert_eq!(identity_request().request.operation(), OP_IDENTITY_SUMMARY);
    assert!(!identity_request().request.is_mutating());
    assert_eq!(
        authorize_request(
            &DispatchContext::UNAUTHENTICATED,
            &DeviceRequest::IdentitySummary,
        ),
        Ok(())
    );
    assert_eq!(
        authorize_request(
            &DispatchContext::authenticated(
                PrincipalId([0x31; 16]),
                Permissions::NONE,
                dispatch_provenance(),
            ),
            &DeviceRequest::IdentitySummary,
        ),
        Ok(())
    );
}

#[test]
fn manual_service_announce_is_authenticated_coalescing_and_wire_stable() {
    const REQUEST: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x0d, 0x03,
        0xa0,
    ];
    const QUEUED_RESPONSE: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x0d, 0x03,
        0xa1, 0x00, 0x00,
    ];
    let request = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        request: DeviceRequest::ManualServiceAnnounce,
    };
    let response = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        response: DeviceResponse::ManualServiceAnnounce(ManualServiceAnnounceDisposition::Queued),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_request(&request, &mut output).unwrap();
    assert_eq!(&output[..written], REQUEST);
    assert_eq!(decode_request(REQUEST).unwrap(), request);
    let written = encode_response(&response, &mut output).unwrap();
    assert_eq!(&output[..written], QUEUED_RESPONSE);
    assert_eq!(decode_response(QUEUED_RESPONSE).unwrap(), response);

    let pending = ResponseEnvelope {
        response: DeviceResponse::ManualServiceAnnounce(
            ManualServiceAnnounceDisposition::AlreadyPending,
        ),
        ..response
    };
    let written = encode_response(&pending, &mut output).unwrap();
    assert_eq!(decode_response(&output[..written]).unwrap(), pending);

    assert_eq!(OP_MANUAL_SERVICE_ANNOUNCE, 0xf00d);
    assert_eq!(request.request.operation(), OP_MANUAL_SERVICE_ANNOUNCE);
    assert!(request.request.is_mutating());
    assert_eq!(
        authorize_request(&DispatchContext::UNAUTHENTICATED, &request.request),
        Err(AuthorizationError::AuthenticationRequired)
    );
    assert_eq!(
        authorize_request(
            &DispatchContext::authenticated(
                PrincipalId([0x19; 16]),
                Permissions::NONE,
                dispatch_provenance(),
            ),
            &request.request,
        ),
        Ok(())
    );
}

#[test]
fn manual_service_announce_capability_and_disposition_are_strict() {
    assert_eq!(
        CapabilitySnapshot::current().manual_service_announce(),
        CapabilityAvailability::Available
    );
    assert_eq!(
        CapabilitySnapshot::for_dispatch(false).manual_service_announce(),
        CapabilityAvailability::Unavailable
    );
    assert_eq!(
        CapabilitySnapshot::for_dispatch(false)
            .with_dispatch_manual_service_announce(CapabilityAvailability::Disabled)
            .manual_service_announce(),
        CapabilityAvailability::Disabled
    );

    let mut invalid_capability = golden_capabilities_response();
    let capability_key = invalid_capability
        .windows(2)
        .rposition(|window| window == [0x14, 0x02])
        .expect("manual announce capability key");
    invalid_capability.splice(capability_key + 1..=capability_key + 1, [0x18, 99]);
    assert_eq!(
        decode_response(&invalid_capability),
        Err(DecodeError::InvalidValue {
            field: RequiredField::CapabilityManualServiceAnnounce,
            value: 99,
        })
    );

    let invalid_disposition = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x0d, 0x03, 0xa1,
        0x00, 0x02,
    ];
    assert_eq!(
        decode_response(&invalid_disposition),
        Err(DecodeError::InvalidValue {
            field: RequiredField::ManualServiceAnnounceDisposition,
            value: 2,
        })
    );

    let duplicate_disposition = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x0d, 0x03, 0xa2,
        0x00, 0x00, 0x00, 0x01,
    ];
    assert_eq!(
        decode_response(&duplicate_disposition),
        Err(DecodeError::DuplicateField(
            RequiredField::ManualServiceAnnounceDisposition,
        ))
    );
}

#[test]
fn exact_reticulum_probe_start_request_is_mutating_and_permission_gated() {
    let expected = probe_start_request();
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_request(&expected, &mut output).unwrap();
    assert_eq!(&output[..written], GOLDEN_PROBE_START_REQUEST);
    assert_eq!(
        decode_request(GOLDEN_PROBE_START_REQUEST).unwrap(),
        expected
    );
    assert_eq!(OP_RETICULUM_PROBE_START, 0xf012);
    assert_eq!(expected.request.operation(), OP_RETICULUM_PROBE_START);
    assert!(expected.request.is_mutating());
    assert_eq!(
        authorize_request(&DispatchContext::UNAUTHENTICATED, &expected.request),
        Err(AuthorizationError::AuthenticationRequired)
    );
    let principal = PrincipalId([0x31; 16]);
    assert_eq!(
        authorize_request(
            &DispatchContext::authenticated(principal, Permissions::NONE, dispatch_provenance(),),
            &expected.request,
        ),
        Err(AuthorizationError::PermissionDenied(
            RequiredPermission::SubmitRnsData
        ))
    );
    assert_eq!(
        authorize_request(
            &DispatchContext::authenticated(
                principal,
                Permissions::SUBMIT_RNS_DATA,
                dispatch_provenance(),
            ),
            &expected.request,
        ),
        Ok(())
    );
}

#[test]
fn exact_reticulum_probe_start_responses_distinguish_fresh_and_replayed() {
    const ACCEPTED: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x12, 0x03,
        0xa2, 0x00, 0x50, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xab,
        0xac, 0xad, 0xae, 0xaf, 0x01, 0x00,
    ];
    const REPLAYED: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x12, 0x03,
        0xa2, 0x00, 0x50, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xab,
        0xac, 0xad, 0xae, 0xaf, 0x01, 0x01,
    ];
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    for (outcome, golden) in [
        (ProbeStartOutcome::Accepted, ACCEPTED),
        (ProbeStartOutcome::Replayed, REPLAYED),
    ] {
        let expected = ResponseEnvelope {
            version: ApiVersion::CURRENT,
            request_id: RequestId(42),
            response: DeviceResponse::ReticulumProbeStartAccepted(ProbeStartAccepted::new(
                probe_id(),
                outcome,
            )),
        };
        let written = encode_response(&expected, &mut output).unwrap();
        assert_eq!(&output[..written], golden);
        assert_eq!(decode_response(golden).unwrap(), expected);
        assert!(written <= MAX_MESSAGE_BYTES);
    }
}

#[test]
fn exact_reticulum_probe_poll_request_is_authenticated_read_only() {
    let expected = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        request: DeviceRequest::ReticulumProbePoll(ProbePollRequest::new(probe_id())),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_request(&expected, &mut output).unwrap();
    assert_eq!(&output[..written], GOLDEN_PROBE_POLL_REQUEST);
    assert_eq!(decode_request(GOLDEN_PROBE_POLL_REQUEST).unwrap(), expected);
    assert_eq!(OP_RETICULUM_PROBE_POLL, 0xf013);
    assert_eq!(expected.request.operation(), OP_RETICULUM_PROBE_POLL);
    assert!(!expected.request.is_mutating());
    assert_eq!(
        authorize_request(&DispatchContext::UNAUTHENTICATED, &expected.request),
        Err(AuthorizationError::AuthenticationRequired)
    );
    assert_eq!(
        authorize_request(
            &DispatchContext::authenticated(
                PrincipalId([0x31; 16]),
                Permissions::NONE,
                dispatch_provenance(),
            ),
            &expected.request,
        ),
        Ok(())
    );
}

#[test]
fn reticulum_probe_poll_round_trips_all_pending_and_failure_values() {
    let phases = [
        ProbePhase::PathLookup,
        ProbePhase::AwaitingDispatch,
        ProbePhase::AwaitingProof,
    ];
    let failures = [
        ProbeFailure::IdentityUnavailable,
        ProbeFailure::NoPath,
        ProbeFailure::Dispatch,
        ProbeFailure::Timeout,
        ProbeFailure::Internal,
    ];
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    for response in phases
        .map(ProbePollResponse::Pending)
        .into_iter()
        .chain(failures.map(ProbePollResponse::Failed))
    {
        let expected = ResponseEnvelope {
            version: ApiVersion::CURRENT,
            request_id: RequestId(42),
            response: DeviceResponse::ReticulumProbePoll(response),
        };
        let written = encode_response(&expected, &mut output).unwrap();
        assert!(written <= MAX_MESSAGE_BYTES);
        assert_eq!(decode_response(&output[..written]).unwrap(), expected);
    }
}

#[test]
fn exact_reticulum_probe_success_preserves_final_hop_signal_pair() {
    const SUCCESS: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x13, 0x03,
        0xa2, 0x00, 0x01, 0x01, 0xa3, 0x00, 0x19, 0x04, 0xd2, 0x01, 0x02, 0x02, 0xa3, 0x00, 0x07,
        0x01, 0x38, 0x60, 0x02, 0x04,
    ];
    let success = ProbeSuccess::new(
        1_234,
        2,
        IngressObservation::new(7, Some(IngressSignal::new(-97, 4))),
    );
    let expected = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        response: DeviceResponse::ReticulumProbePoll(ProbePollResponse::Succeeded(success)),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_response(&expected, &mut output).unwrap();
    assert_eq!(&output[..written], SUCCESS);
    assert_eq!(decode_response(SUCCESS).unwrap(), expected);

    let interface_only = ResponseEnvelope {
        response: DeviceResponse::ReticulumProbePoll(ProbePollResponse::Succeeded(
            ProbeSuccess::new(0, 0, IngressObservation::new(4, None)),
        )),
        ..expected
    };
    let written = encode_response(&interface_only, &mut output).unwrap();
    assert_eq!(decode_response(&output[..written]).unwrap(), interface_only);
}

#[test]
fn reticulum_probe_capability_is_available_and_strict() {
    assert_eq!(
        CapabilitySnapshot::current().reticulum_probe(),
        CapabilityAvailability::Available
    );
    assert_eq!(
        CapabilitySnapshot::for_dispatch(false).reticulum_probe(),
        CapabilityAvailability::Unavailable
    );
    assert_eq!(
        CapabilitySnapshot::for_dispatch(false)
            .with_dispatch_reticulum_probe(CapabilityAvailability::Disabled)
            .reticulum_probe(),
        CapabilityAvailability::Disabled
    );

    let mut invalid = golden_capabilities_response();
    invalid.splice(invalid.len() - 1.., [0x18, 99]);
    assert_eq!(
        decode_response(&invalid),
        Err(DecodeError::InvalidValue {
            field: RequiredField::CapabilityReticulumProbe,
            value: 99,
        })
    );

    let mut duplicate = golden_capabilities_response();
    duplicate[13] = 0xb5;
    duplicate.extend_from_slice(&[0x15, 0x02]);
    assert_eq!(
        decode_response(&duplicate),
        Err(DecodeError::DuplicateField(
            RequiredField::CapabilityReticulumProbe
        ))
    );
}

#[test]
fn reticulum_probe_rejects_zero_ids_duplicates_and_half_signal_observations() {
    assert!(ProbeId::new([0; 16]).is_err());
    assert!(ProbeId::new([0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).is_ok());

    let mut zero_id = GOLDEN_PROBE_POLL_REQUEST.to_vec();
    let id_start = zero_id.len() - 16;
    zero_id[id_start..].fill(0);
    assert_eq!(decode_request(&zero_id), Err(DecodeError::InvalidProbeId));

    let mut wrong_width_id = GOLDEN_PROBE_POLL_REQUEST.to_vec();
    let id_header = wrong_width_id.len() - 17;
    wrong_width_id[id_header] = 0x4f;
    wrong_width_id.pop();
    assert_eq!(
        decode_request(&wrong_width_id),
        Err(DecodeError::InvalidByteStringLength {
            field: RequiredField::ProbeId,
            expected: 16,
            actual: 15,
        })
    );

    let mut missing_idempotency = GOLDEN_PROBE_START_REQUEST.to_vec();
    let body_header = missing_idempotency
        .windows(2)
        .position(|window| window == [0x03, 0xa2])
        .expect("probe start body")
        + 1;
    missing_idempotency[body_header] = 0xa1;
    missing_idempotency.truncate(missing_idempotency.len() - 18);
    assert_eq!(
        decode_request(&missing_idempotency),
        Err(DecodeError::MissingField(
            RequiredField::ProbeStartIdempotencyKey
        ))
    );

    let mut duplicate_destination = GOLDEN_PROBE_START_REQUEST.to_vec();
    let body_header = duplicate_destination
        .windows(2)
        .position(|window| window == [0x03, 0xa2])
        .expect("probe start body")
        + 1;
    duplicate_destination[body_header] = 0xa3;
    duplicate_destination.extend_from_slice(&[
        0x00, 0x50, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
        0x0d, 0x0e, 0x0f,
    ]);
    assert_eq!(
        decode_request(&duplicate_destination),
        Err(DecodeError::DuplicateField(
            RequiredField::ProbeStartDestination
        ))
    );

    let response = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        response: DeviceResponse::ReticulumProbePoll(ProbePollResponse::Succeeded(
            ProbeSuccess::new(
                1_234,
                2,
                IngressObservation::new(7, Some(IngressSignal::new(-97, 4))),
            ),
        )),
    };
    let mut encoded = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_response(&response, &mut encoded).unwrap();
    let encoded = &encoded[..written];
    let observation = encoded
        .windows(6)
        .position(|window| window == [0x02, 0xa3, 0x00, 0x07, 0x01, 0x38])
        .expect("probe ingress observation");
    let mut missing_snr = encoded.to_vec();
    missing_snr[observation + 1] = 0xa2;
    missing_snr.truncate(missing_snr.len() - 2);
    assert_eq!(
        decode_response(&missing_snr),
        Err(DecodeError::InvalidProbePollResponse)
    );
}

#[test]
fn reticulum_probe_closed_states_and_duplicate_poll_fields_are_rejected() {
    let start = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        response: DeviceResponse::ReticulumProbeStartAccepted(ProbeStartAccepted::new(
            probe_id(),
            ProbeStartOutcome::Accepted,
        )),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_response(&start, &mut output).unwrap();
    let mut invalid_start = output[..written].to_vec();
    *invalid_start.last_mut().unwrap() = 2;
    assert_eq!(
        decode_response(&invalid_start),
        Err(DecodeError::InvalidValue {
            field: RequiredField::ProbeStartOutcome,
            value: 2,
        })
    );

    let pending = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        response: DeviceResponse::ReticulumProbePoll(ProbePollResponse::Pending(
            ProbePhase::PathLookup,
        )),
    };
    let written = encode_response(&pending, &mut output).unwrap();
    let mut invalid_phase = output[..written].to_vec();
    *invalid_phase.last_mut().unwrap() = 3;
    assert_eq!(
        decode_response(&invalid_phase),
        Err(DecodeError::InvalidValue {
            field: RequiredField::ProbePhase,
            value: 3,
        })
    );

    let mut invalid_state = output[..written].to_vec();
    let state_index = invalid_state.len() - 3;
    invalid_state[state_index] = 3;
    assert_eq!(
        decode_response(&invalid_state),
        Err(DecodeError::InvalidValue {
            field: RequiredField::ProbePollState,
            value: 3,
        })
    );

    let mut duplicate_state = output[..written].to_vec();
    let body_header = duplicate_state
        .windows(5)
        .position(|window| window == [0x03, 0xa2, 0x00, 0x00, 0x01])
        .expect("probe poll body")
        + 1;
    duplicate_state[body_header] = 0xa3;
    duplicate_state.extend_from_slice(&[0x00, 0x00]);
    assert_eq!(
        decode_response(&duplicate_state),
        Err(DecodeError::DuplicateField(RequiredField::ProbePollState))
    );

    let failed = ResponseEnvelope {
        response: DeviceResponse::ReticulumProbePoll(ProbePollResponse::Failed(
            ProbeFailure::Internal,
        )),
        ..pending
    };
    let written = encode_response(&failed, &mut output).unwrap();
    let mut invalid_failure = output[..written].to_vec();
    *invalid_failure.last_mut().unwrap() = 5;
    assert_eq!(
        decode_response(&invalid_failure),
        Err(DecodeError::InvalidValue {
            field: RequiredField::ProbeFailure,
            value: 5,
        })
    );
}

fn sample_rns_diagnostics() -> RnsDiagnostics {
    RnsDiagnostics::new(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 13)
}

fn sample_node_diagnostics() -> NodeDiagnosticsSnapshot {
    NodeDiagnosticsSnapshot::new(
        1_000,
        [
            Some(DiagnosticInterfaceRecord::new(
                1,
                DiagnosticInterfaceKind::LoRa,
                DiagnosticInterfaceState::Online,
                2,
                500,
                Some(125_000),
            )),
            None,
            None,
            None,
        ],
        None,
        sample_rns_diagnostics(),
        2,
        3,
        1,
    )
}

#[test]
fn diagnostics_operations_have_exact_authenticated_read_only_wire_shapes() {
    const NODE_REQUEST: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x0e, 0x03,
        0xa0,
    ];
    const NODE_RESPONSE: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x0e, 0x03,
        0xa6, 0x00, 0x19, 0x03, 0xe8, 0x01, 0x84, 0xa6, 0x00, 0x01, 0x01, 0x00, 0x02, 0x01, 0x03,
        0x02, 0x04, 0x19, 0x01, 0xf4, 0x05, 0x1a, 0x00, 0x01, 0xe8, 0x48, 0xf6, 0xf6, 0xf6, 0x03,
        0xab, 0x00, 0x01, 0x01, 0x02, 0x02, 0x03, 0x03, 0x04, 0x04, 0x05, 0x05, 0x06, 0x06, 0x07,
        0x07, 0x08, 0x08, 0x09, 0x09, 0x0a, 0x0a, 0x0d, 0x04, 0x02, 0x05, 0x03, 0x06, 0x01,
    ];
    const ROUTE_REQUEST: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x0f, 0x03,
        0xa1, 0x00, 0x50, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20,
        0x20, 0x20, 0x20, 0x20,
    ];
    const ROUTE_RESPONSE: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x0f, 0x03,
        0xa3, 0x00, 0x09, 0x01, 0x01, 0x02, 0x84, 0xa4, 0x00, 0x50, 0x10, 0x10, 0x10, 0x10, 0x10,
        0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x02, 0x02, 0x03, 0x03,
        0x04, 0x00, 0xf6, 0xf6, 0xf6,
    ];

    let node_request = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        request: DeviceRequest::NodeDiagnostics,
    };
    let node_response = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        response: DeviceResponse::NodeDiagnostics(sample_node_diagnostics()),
    };
    let route_request = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        request: DeviceRequest::RouteDiagnosticsPage(RouteDiagnosticsRequest::new(Some(
            DestinationHash([0x20; 16]),
        ))),
    };
    let route_entry = RouteDiagnosticEntry::new(
        DestinationHash([0x10; 16]),
        None,
        2,
        Some(3),
        RouteDiagnosticResolution::ExactReady,
        None,
        None,
        None,
    );
    let route_response = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        response: DeviceResponse::RouteDiagnosticsPage(
            RouteDiagnosticsPage::new(9, 1, [Some(route_entry), None, None, None], None).unwrap(),
        ),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    for (request, golden) in [(node_request, NODE_REQUEST), (route_request, ROUTE_REQUEST)] {
        let written = encode_request(&request, &mut output).unwrap();
        assert_eq!(&output[..written], golden);
        assert_eq!(decode_request(golden).unwrap(), request);
        assert!(!request.request.is_mutating());
        assert_eq!(
            authorize_request(&DispatchContext::UNAUTHENTICATED, &request.request),
            Err(AuthorizationError::AuthenticationRequired)
        );
        assert_eq!(
            authorize_request(
                &DispatchContext::authenticated(
                    PrincipalId([0x31; 16]),
                    Permissions::NONE,
                    dispatch_provenance(),
                ),
                &request.request,
            ),
            Ok(())
        );
    }
    for (response, golden) in [
        (node_response, NODE_RESPONSE),
        (route_response, ROUTE_RESPONSE),
    ] {
        let written = encode_response(&response, &mut output).unwrap();
        assert_eq!(&output[..written], golden);
        assert_eq!(decode_response(golden).unwrap(), response);
    }
    assert_eq!(OP_NODE_DIAGNOSTICS, 0xf00e);
    assert_eq!(OP_ROUTE_DIAGNOSTICS_PAGE, 0xf00f);
}

#[test]
fn diagnostics_models_enforce_route_page_order_and_cursor_invariants() {
    let route = |destination| {
        RouteDiagnosticEntry::new(
            DestinationHash([destination; 16]),
            None,
            1,
            None,
            RouteDiagnosticResolution::BroadcastReady,
            None,
            None,
            None,
        )
    };
    assert_eq!(MAX_DIAGNOSTIC_INTERFACES, 4);
    assert_eq!(MAX_ROUTE_DIAGNOSTIC_PAGE_ENTRIES, 4);
    assert_eq!(sample_rns_diagnostics().route_revision(), 13);
    assert_eq!(
        RnsDiagnostics::new(0, 0, 0, 0, 0, u64::MAX, 1, 0, 0, 0, u64::MAX).route_revision(),
        u64::MAX
    );
    assert!(matches!(
        RouteDiagnosticsPage::new(1, 2, [Some(route(1)), None, Some(route(2)), None], None,),
        Err(reticulum_device_api::InvalidRouteDiagnosticsPage::SparseEntries)
    ));
    assert!(matches!(
        RouteDiagnosticsPage::new(1, 2, [Some(route(2)), Some(route(1)), None, None], None,),
        Err(reticulum_device_api::InvalidRouteDiagnosticsPage::NotStrictlyOrdered)
    ));
    assert!(matches!(
        RouteDiagnosticsPage::new(
            1,
            1,
            [Some(route(1)), None, None, None],
            Some(DestinationHash([2; 16])),
        ),
        Err(reticulum_device_api::InvalidRouteDiagnosticsPage::InvalidNextCursor)
    ));
}

fn sample_radio_trace_profile() -> RadioTraceAppliedLoraProfile {
    RadioTraceAppliedLoraProfile::new(
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
    )
}

fn radio_trace_packet(seed: u8, with_attempt: bool) -> RadioTracePacketEvidence {
    RadioTracePacketEvidence::try_new(
        1,
        500,
        EncodedPacketSha256::new([seed; 32]),
        with_attempt.then(|| RadioTraceAttemptToken::new([seed.wrapping_add(1); 32])),
    )
    .unwrap()
}

#[test]
fn radio_trace_request_has_an_exact_boot_bound_authenticated_read_only_wire_shape() {
    const REQUEST: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x14, 0x03,
        0xa1, 0x00, 0x82, 0x18, 0x63, 0x18, 0x27,
    ];
    let request = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        request: DeviceRequest::RadioTracePage(RadioTracePageRequest::new(Some(
            RadioTraceCursor::new(99, 39),
        ))),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_request(&request, &mut output).unwrap();
    assert_eq!(&output[..written], REQUEST);
    assert_eq!(decode_request(REQUEST).unwrap(), request);
    assert!(!request.request.is_mutating());
    assert_eq!(
        authorize_request(&DispatchContext::UNAUTHENTICATED, &request.request),
        Err(AuthorizationError::AuthenticationRequired)
    );
    assert_eq!(
        authorize_request(
            &DispatchContext::authenticated(
                PrincipalId([0x31; 16]),
                Permissions::NONE,
                dispatch_provenance(),
            ),
            &request.request,
        ),
        Ok(())
    );
    assert_eq!(OP_RADIO_TRACE_PAGE, 0xf014);

    let partial_cursor = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x14, 0x03,
        0xa1, 0x00, 0x81, 0x18, 0x63,
    ];
    assert_eq!(
        decode_request(&partial_cursor),
        Err(DecodeError::InvalidArrayLength {
            field: RequiredField::RadioTraceAfterCursor,
            expected: 2,
            actual: 1,
        })
    );
}

#[test]
fn all_radio_trace_event_variants_round_trip_with_packet_and_proof_correlation() {
    let tx = RadioTraceEventKind::DataTx(
        RadioTraceDataTx::try_new(
            radio_trace_packet(0x11, true),
            RadioTraceTxOutcome::Transmitted,
            2,
            2,
            true,
            [Some(1_010), Some(1_020)],
        )
        .unwrap(),
    );
    let rx = RadioTraceEventKind::LogicalRx(RadioTraceLogicalRx::new(
        radio_trace_packet(0x21, false),
        -104,
        7,
    ));
    let route = RadioTraceEventKind::RouteSelected(
        RadioTraceRouteSelected::try_new(
            SubmissionId(77),
            DestinationHash([0x31; 16]),
            Some(IdentityHash::new([0x41; 16])),
            2,
            RouteDiagnosticResolution::ExactReady,
            radio_trace_packet(0x31, true),
        )
        .unwrap(),
    );
    let terminal = RadioTraceEventKind::AttemptTerminal(RadioTraceAttemptTerminal::new(
        RadioTraceAttemptToken::new([0x32; 32]),
        RadioTraceAttemptOutcome::Delivered,
        Some(IngressObservation::new(1, Some(IngressSignal::new(-99, 4)))),
    ));
    let inbound = RadioTraceEventKind::InboundProof(
        RadioTraceInboundProof::try_new(
            RadioTraceAttemptToken::new([0x33; 32]),
            RadioTraceInboundProofStage::PhysicalTxFailed,
            Some([0x34; 32]),
            Some(
                RadioTraceInboundProofPacket::try_new(113, EncodedPacketSha256::new([0x35; 32]))
                    .unwrap(),
            ),
            Some(1),
            Some(IngressSignal::new(-101, 3)),
            Some(RadioTraceTxOutcome::TxFault),
        )
        .unwrap(),
    );

    for (offset, kind) in [tx, rx, route, terminal, inbound].into_iter().enumerate() {
        let sequence = 40 + offset as u64;
        let page = RadioTracePage::new(
            99,
            sample_radio_trace_profile(),
            sequence,
            sequence + 1,
            false,
            [
                Some(RadioTraceEvent::new(sequence, 1_000 + sequence, kind)),
                None,
            ],
            None,
        )
        .unwrap();
        let response = ResponseEnvelope {
            version: ApiVersion::CURRENT,
            request_id: RequestId(42),
            response: DeviceResponse::RadioTracePage(page),
        };
        let mut output = [0_u8; MAX_MESSAGE_BYTES];
        let written = encode_response(&response, &mut output).unwrap();
        assert_eq!(decode_response(&output[..written]).unwrap(), response);
    }
}

#[test]
fn maximum_two_entry_radio_trace_page_fits_the_frozen_transport_body() {
    const ENVELOPE_PREFIX_BYTES: usize = 22;
    let packet = RadioTracePacketEvidence::try_new(
        u8::MAX,
        u16::MAX,
        EncodedPacketSha256::new([0xff; 32]),
        Some(RadioTraceAttemptToken::new([0xfe; 32])),
    )
    .unwrap();
    let tx = RadioTraceDataTx::try_new(
        packet,
        RadioTraceTxOutcome::CancelledRadioOperation,
        2,
        2,
        true,
        [Some(u64::MAX - 1), Some(u64::MAX)],
    )
    .unwrap();
    let events = [
        Some(RadioTraceEvent::new(
            u64::MAX - 2,
            u64::MAX,
            RadioTraceEventKind::DataTx(tx),
        )),
        Some(RadioTraceEvent::new(
            u64::MAX - 1,
            u64::MAX,
            RadioTraceEventKind::DataTx(tx),
        )),
    ];
    let maximum_profile = RadioTraceAppliedLoraProfile::new(
        [0xff; 16],
        u32::MAX,
        u32::MAX,
        u16::MAX,
        i16::MIN,
        u8::MAX,
        u8::MAX,
        true,
        true,
        true,
    );
    let page = RadioTracePage::new(
        u64::MAX,
        maximum_profile,
        u64::MAX - 2,
        u64::MAX,
        true,
        events,
        Some(RadioTraceCursor::new(u64::MAX, u64::MAX - 1)),
    )
    .unwrap();
    let response = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(u64::MAX),
        response: DeviceResponse::RadioTracePage(page),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_response(&response, &mut output).unwrap();
    assert!(written <= MAX_MESSAGE_BYTES);
    assert!(written - ENVELOPE_PREFIX_BYTES <= MAX_BODY_BYTES);
    assert_eq!(decode_response(&output[..written]).unwrap(), response);

    let route = RadioTraceRouteSelected::try_new(
        SubmissionId(u64::MAX),
        DestinationHash([0xff; 16]),
        Some(IdentityHash::new([0xfe; 16])),
        u8::MAX,
        RouteDiagnosticResolution::BroadcastUnavailable,
        packet,
    )
    .unwrap();
    let rx = RadioTraceLogicalRx::new(packet, i16::MIN, i16::MIN);
    let mixed_events = [
        Some(RadioTraceEvent::new(
            u64::MAX - 2,
            u64::MAX,
            RadioTraceEventKind::RouteSelected(route),
        )),
        Some(RadioTraceEvent::new(
            u64::MAX - 1,
            u64::MAX,
            RadioTraceEventKind::LogicalRx(rx),
        )),
    ];
    let mixed_page = RadioTracePage::new(
        u64::MAX,
        maximum_profile,
        u64::MAX - 2,
        u64::MAX,
        true,
        mixed_events,
        Some(RadioTraceCursor::new(u64::MAX, u64::MAX - 1)),
    )
    .unwrap();
    let mixed_response = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(u64::MAX),
        response: DeviceResponse::RadioTracePage(mixed_page),
    };
    let written = encode_response(&mixed_response, &mut output).unwrap();
    assert!(written - ENVELOPE_PREFIX_BYTES <= MAX_BODY_BYTES);
    assert_eq!(decode_response(&output[..written]).unwrap(), mixed_response);

    let inbound = RadioTraceInboundProof::try_new(
        RadioTraceAttemptToken::new([0xff; 32]),
        RadioTraceInboundProofStage::PhysicalTxFailed,
        Some([0xff; 32]),
        Some(
            RadioTraceInboundProofPacket::try_new(u16::MAX, EncodedPacketSha256::new([0xff; 32]))
                .unwrap(),
        ),
        Some(u8::MAX),
        Some(IngressSignal::new(i16::MIN, i16::MIN)),
        Some(RadioTraceTxOutcome::CancelledRadioOperation),
    )
    .unwrap();
    let inbound_page = RadioTracePage::new(
        u64::MAX,
        maximum_profile,
        u64::MAX - 2,
        u64::MAX,
        true,
        [
            Some(RadioTraceEvent::new(
                u64::MAX - 2,
                u64::MAX,
                RadioTraceEventKind::InboundProof(inbound),
            )),
            Some(RadioTraceEvent::new(
                u64::MAX - 1,
                u64::MAX,
                RadioTraceEventKind::InboundProof(inbound),
            )),
        ],
        Some(RadioTraceCursor::new(u64::MAX, u64::MAX - 1)),
    )
    .unwrap();
    let inbound_response = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(u64::MAX),
        response: DeviceResponse::RadioTracePage(inbound_page),
    };
    let written = encode_response(&inbound_response, &mut output).unwrap();
    assert!(written - ENVELOPE_PREFIX_BYTES <= MAX_BODY_BYTES);
    assert_eq!(
        decode_response(&output[..written]).unwrap(),
        inbound_response
    );
}

#[test]
fn radio_trace_models_reject_ambiguous_pages_and_tx_timestamps() {
    let event = RadioTraceEvent::new(
        7,
        100,
        RadioTraceEventKind::LogicalRx(RadioTraceLogicalRx::new(
            radio_trace_packet(1, false),
            -100,
            3,
        )),
    );
    assert!(matches!(
        RadioTracePage::new(
            9,
            sample_radio_trace_profile(),
            7,
            8,
            false,
            [Some(event), None],
            Some(RadioTraceCursor::new(10, 7)),
        ),
        Err(reticulum_device_api::InvalidRadioTracePage::InvalidNextCursor)
    ));
    assert!(matches!(
        RadioTraceDataTx::try_new(
            radio_trace_packet(2, true),
            RadioTraceTxOutcome::Transmitted,
            2,
            1,
            true,
            [Some(10), Some(20)],
        ),
        Err(reticulum_device_api::InvalidRadioTraceDataTx::CompletionTimestampCountMismatch)
    ));
    assert!(matches!(
        RadioTraceRouteSelected::try_new(
            SubmissionId(0),
            DestinationHash([1; 16]),
            None,
            1,
            RouteDiagnosticResolution::BroadcastReady,
            radio_trace_packet(3, true),
        ),
        Err(reticulum_device_api::InvalidRadioTraceRouteSelected::ZeroSubmissionId)
    ));
    assert_eq!(MAX_RADIO_TRACE_PAGE_ENTRIES, 2);
}

#[test]
fn maximum_diagnostics_responses_fit_message_and_body_limits() {
    const ENVELOPE_PREFIX_BYTES: usize = 22;
    let interfaces = [
        Some(DiagnosticInterfaceRecord::new(
            u8::MAX,
            DiagnosticInterfaceKind::LoRa,
            DiagnosticInterfaceState::Online,
            u64::MAX,
            u16::MAX,
            Some(u32::MAX),
        )),
        Some(DiagnosticInterfaceRecord::new(
            u8::MAX - 1,
            DiagnosticInterfaceKind::TcpClient,
            DiagnosticInterfaceState::Offline,
            u64::MAX,
            u16::MAX,
            Some(u32::MAX),
        )),
        Some(DiagnosticInterfaceRecord::new(
            u8::MAX - 2,
            DiagnosticInterfaceKind::Other,
            DiagnosticInterfaceState::Faulted,
            u64::MAX,
            u16::MAX,
            Some(u32::MAX),
        )),
        Some(DiagnosticInterfaceRecord::new(
            u8::MAX - 3,
            DiagnosticInterfaceKind::TcpServer,
            DiagnosticInterfaceState::Online,
            u64::MAX,
            u16::MAX,
            Some(u32::MAX),
        )),
    ];
    let lora = LoraDiagnostics::new(
        i16::MIN,
        u32::MAX,
        u32::MAX,
        u8::MAX,
        u8::MAX,
        u64::MAX,
        u64::MAX,
        u64::MAX,
        u64::MAX,
        u64::MAX,
        u64::MAX,
        u64::MAX,
        u64::MAX,
        u64::MAX,
        u64::MAX,
        u64::MAX,
        Some(DiagnosticLoraLastRx::new(u64::MAX, i16::MIN, i16::MAX)),
        Some(DiagnosticLoraLastTx::ordinary(
            u64::MAX,
            DiagnosticLoraTxOutcome::Failed,
        )),
        None,
    );
    let node = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(u64::MAX),
        response: DeviceResponse::NodeDiagnostics(NodeDiagnosticsSnapshot::new(
            u64::MAX,
            interfaces,
            Some(lora),
            RnsDiagnostics::new(
                u64::MAX,
                u64::MAX,
                u64::MAX,
                u64::MAX,
                u64::MAX,
                u64::MAX,
                u64::MAX,
                u64::MAX,
                u64::MAX,
                u64::MAX,
                u64::MAX,
            ),
            u32::MAX,
            u32::MAX,
            u32::MAX,
        )),
    };

    let route = |byte, resolution| {
        RouteDiagnosticEntry::new(
            DestinationHash([byte; 16]),
            Some(IdentityHash::new([u8::MAX - byte; 16])),
            u8::MAX,
            Some(u8::MAX),
            resolution,
            Some(u64::MAX),
            Some(u64::MAX),
            Some(u64::MAX),
        )
    };
    let route_page = RouteDiagnosticsPage::new(
        u64::MAX,
        u32::MAX,
        [
            Some(route(1, RouteDiagnosticResolution::ExactReady)),
            Some(route(2, RouteDiagnosticResolution::ExactOffline)),
            Some(route(3, RouteDiagnosticResolution::ExactMissing)),
            Some(route(4, RouteDiagnosticResolution::BroadcastUnavailable)),
        ],
        Some(DestinationHash([4; 16])),
    )
    .unwrap();
    let routes = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(u64::MAX),
        response: DeviceResponse::RouteDiagnosticsPage(route_page),
    };

    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    for (envelope, expected_body_bytes, expected_message_bytes) in
        [(node, 425, 447), (routes, 337, 359)]
    {
        let written = encode_response(&envelope, &mut output).unwrap();
        assert_eq!(written, expected_message_bytes);
        assert_eq!(written - ENVELOPE_PREFIX_BYTES, expected_body_bytes);
        assert!(written <= MAX_MESSAGE_BYTES);
        assert!(written - ENVELOPE_PREFIX_BYTES <= MAX_BODY_BYTES);
        assert_eq!(decode_response(&output[..written]).unwrap(), envelope);
    }
}

#[test]
fn lora_data_tx_evidence_round_trips_and_retained_slot_rejects_ordinary_records() {
    assert!(
        DiagnosticLoraDataTxEvidence::try_new(7, 0, EncodedPacketSha256::new([0xab; 32])).is_none()
    );
    let evidence =
        DiagnosticLoraDataTxEvidence::try_new(7, 183, EncodedPacketSha256::new([0xab; 32]))
            .unwrap();
    let lora = LoraDiagnostics::new(
        22,
        915_000_000,
        125_000,
        10,
        5,
        8,
        3,
        1,
        2,
        5,
        2,
        2,
        2,
        1,
        4,
        6,
        None,
        Some(DiagnosticLoraLastTx::ordinary(
            5,
            DiagnosticLoraTxOutcome::Completed,
        )),
        Some(DiagnosticLoraLastDataTx::new(
            8,
            DiagnosticLoraTxOutcome::AccessRejected,
            evidence,
        )),
    );
    let envelope = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        response: DeviceResponse::NodeDiagnostics(NodeDiagnosticsSnapshot::new(
            10_000,
            [None; MAX_DIAGNOSTIC_INTERFACES],
            Some(lora),
            RnsDiagnostics::new(0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0),
            0,
            0,
            0,
        )),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_response(&envelope, &mut output).unwrap();
    assert_eq!(decode_response(&output[..written]).unwrap(), envelope);
    assert!(written <= MAX_MESSAGE_BYTES);

    let DeviceResponse::NodeDiagnostics(decoded) =
        decode_response(&output[..written]).unwrap().response
    else {
        panic!("expected node diagnostics")
    };
    let decoded = decoded.lora().expect("LoRa diagnostics");
    assert_eq!(
        decoded.last_tx().and_then(|last_tx| last_tx.family()),
        Some(DiagnosticLoraTxFamily::Ordinary)
    );
    let retained = decoded.last_data_tx().expect("retained DATA TX");
    assert_eq!(retained.data_evidence(), evidence);

    let marker = [0x12, 0xa6, 0x00, 0x08, 0x01, 0x01, 0x02, 0x00];
    let family = output[..written]
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("retained DATA TX map")
        + marker.len()
        - 1;
    let mut ordinary_in_data_slot = output[..written].to_vec();
    ordinary_in_data_slot[family] = DiagnosticLoraTxFamily::Ordinary.wire_code();
    assert_eq!(
        decode_response(&ordinary_in_data_slot),
        Err(DecodeError::InvalidLoraLastTx)
    );
}

#[test]
fn route_diagnostics_request_rejects_duplicate_cursor() {
    let mut bytes = vec![
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x0f, 0x03, 0xa2,
        0x00, 0x50,
    ];
    bytes.extend_from_slice(&[0x11; 16]);
    bytes.extend_from_slice(&[0x00, 0x50]);
    bytes.extend_from_slice(&[0x22; 16]);
    assert_eq!(
        decode_request(&bytes),
        Err(DecodeError::DuplicateField(
            RequiredField::RouteDiagnosticsAfter
        ))
    );
}

#[test]
fn identity_summary_unknown_fields_are_skipped_but_required_field_is_strict() {
    const UNKNOWN_REQUEST_FIELD: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x03, 0x03, 0xa1, 0x18,
        0x63, 0x82, 0x01, 0x02,
    ];
    const UNKNOWN_RESPONSE_FIELD: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x03, 0x03, 0xa2, 0x00,
        0x50, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e, 0x1f, 0x18, 0x63, 0x82, 0x01, 0x02,
    ];
    assert_eq!(
        decode_request(UNKNOWN_REQUEST_FIELD).unwrap(),
        identity_request()
    );
    assert_eq!(
        decode_response(UNKNOWN_RESPONSE_FIELD).unwrap().response,
        DeviceResponse::IdentitySummary(IdentitySummary::new(PRIMARY_DESTINATION))
    );

    let missing = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x03, 0x03, 0xa0,
    ];
    assert_eq!(
        decode_response(&missing),
        Err(DecodeError::MissingField(
            RequiredField::IdentityPrimaryDestination,
        ))
    );

    let duplicate = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x03, 0x03, 0xa2, 0x00,
        0x50, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e, 0x1f, 0x00, 0x50, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a,
        0x2b, 0x2c, 0x2d, 0x2e, 0x2f,
    ];
    assert_eq!(
        decode_response(&duplicate),
        Err(DecodeError::DuplicateField(
            RequiredField::IdentityPrimaryDestination,
        ))
    );

    let wrong_length = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x03, 0x03, 0xa1, 0x00,
        0x4f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e,
    ];
    assert_eq!(
        decode_response(&wrong_length),
        Err(DecodeError::InvalidByteStringLength {
            field: RequiredField::IdentityPrimaryDestination,
            expected: 16,
            actual: 15,
        })
    );

    let wrong_lxmf_length = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x03, 0x03, 0xa2, 0x00,
        0x50, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e, 0x1f, 0x01, 0x4f, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a,
        0x2b, 0x2c, 0x2d, 0x2e,
    ];
    assert_eq!(
        decode_response(&wrong_lxmf_length),
        Err(DecodeError::InvalidByteStringLength {
            field: RequiredField::IdentityLxmfDeliveryDestination,
            expected: 16,
            actual: 15,
        })
    );
}

#[cfg(feature = "lxmf")]
fn lxmf_summary() -> LxmfMessageSummary {
    LxmfMessageSummary::new(
        LxmfMessageHandle::new(7).unwrap(),
        [0x11; 32],
        DestinationHash([0x22; 16]),
        DestinationHash([0x33; 16]),
        0x0102_0304_0506_0708,
        0x0123,
        5,
        9,
        1,
        [0x44; 32],
    )
    .unwrap()
}

#[cfg(feature = "lxmf")]
#[test]
fn exact_lxmf_next_goldens_round_trip() {
    const REQUEST: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x04, 0x03,
        0xa1, 0x00, 0x07,
    ];
    const RESPONSE: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x04, 0x03,
        0xaa, 0x00, 0x07, 0x01, 0x58, 0x20, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
        0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
        0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x02, 0x50, 0x22, 0x22, 0x22, 0x22, 0x22,
        0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x03, 0x50, 0x33, 0x33,
        0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x04,
        0x1b, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x05, 0x19, 0x01, 0x23, 0x06, 0x05,
        0x07, 0x09, 0x08, 0x01, 0x09, 0x58, 0x20, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44,
        0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44,
        0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44,
    ];
    let request = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        request: DeviceRequest::LxmfNext {
            after: Some(LxmfMessageHandle::new(7).unwrap()),
        },
    };
    let response = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        response: DeviceResponse::LxmfNext(lxmf_summary()),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let request_len = encode_request(&request, &mut output).unwrap();
    assert_eq!(&output[..request_len], REQUEST);
    assert_eq!(decode_request(REQUEST).unwrap(), request);
    let response_len = encode_response(&response, &mut output).unwrap();
    assert_eq!(&output[..response_len], RESPONSE);
    assert_eq!(decode_response(RESPONSE).unwrap(), response);
    assert_eq!(request.request.operation(), OP_LXMF_NEXT);
    assert_eq!(response.response.kind(), OP_LXMF_NEXT);
}

#[cfg(feature = "lxmf")]
#[test]
fn lxmf_summary_round_trips_optional_first_arrival_evidence() {
    const RESPONSE: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x04, 0x03,
        0xab, 0x00, 0x07, 0x01, 0x58, 0x20, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
        0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
        0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x02, 0x50, 0x22, 0x22, 0x22, 0x22, 0x22,
        0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x03, 0x50, 0x33, 0x33,
        0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x04,
        0x1b, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x05, 0x19, 0x01, 0x23, 0x06, 0x05,
        0x07, 0x09, 0x08, 0x01, 0x09, 0x58, 0x20, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44,
        0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44,
        0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x0a, 0xa3, 0x00, 0x07, 0x01, 0x38,
        0x68, 0x02, 0x07,
    ];
    let summary = lxmf_summary().with_ingress_observation(Some(IngressObservation::new(
        7,
        Some(IngressSignal::new(-105, 7)),
    )));
    let response = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        response: DeviceResponse::LxmfNext(summary),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let response_len = encode_response(&response, &mut output).unwrap();
    assert_eq!(&output[..response_len], RESPONSE);
    assert_eq!(decode_response(RESPONSE).unwrap(), response);

    let interface_only =
        lxmf_summary().with_ingress_observation(Some(IngressObservation::new(4, None)));
    let interface_only_response = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        response: DeviceResponse::LxmfNext(interface_only),
    };
    let response_len = encode_response(&interface_only_response, &mut output).unwrap();
    assert_eq!(
        decode_response(&output[..response_len]).unwrap(),
        interface_only_response
    );
}

#[cfg(feature = "lxmf")]
#[test]
fn lxmf_summary_rejects_half_present_signal_observations() {
    let response = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        response: DeviceResponse::LxmfNext(lxmf_summary().with_ingress_observation(Some(
            IngressObservation::new(7, Some(IngressSignal::new(-105, 7))),
        ))),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let response_len = encode_response(&response, &mut output).unwrap();
    let encoded = &output[..response_len];
    let observation = encoded
        .windows(6)
        .position(|window| window == [0x0a, 0xa3, 0x00, 0x07, 0x01, 0x38])
        .expect("encoded ingress observation");
    let observation_map = observation + 1;

    let mut missing_snr = encoded.to_vec();
    missing_snr[observation_map] = 0xa2;
    missing_snr.truncate(missing_snr.len() - 2);
    assert_eq!(
        decode_response(&missing_snr),
        Err(DecodeError::InvalidLxmfMessageSummary)
    );

    let mut missing_rssi = encoded.to_vec();
    missing_rssi[observation_map] = 0xa2;
    missing_rssi.drain(observation_map + 3..observation_map + 6);
    assert_eq!(
        decode_response(&missing_rssi),
        Err(DecodeError::InvalidLxmfMessageSummary)
    );
}

#[cfg(feature = "lxmf")]
#[test]
fn exact_lxmf_read_goldens_round_trip() {
    const REQUEST: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x05, 0x03,
        0xa3, 0x00, 0x07, 0x01, 0x19, 0x01, 0x00, 0x02, 0x19, 0x01, 0xa0,
    ];
    const RESPONSE: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x05, 0x03,
        0xa4, 0x00, 0x07, 0x01, 0x19, 0x01, 0x00, 0x02, 0x19, 0x02, 0x00, 0x03, 0x43, 0x61, 0x62,
        0x63,
    ];
    let handle = LxmfMessageHandle::new(7).unwrap();
    let request = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        request: DeviceRequest::LxmfRead {
            handle,
            offset: 256,
            max_bytes: LxmfReadLength::new(MAX_LXMF_READ_CHUNK_BYTES as u16).unwrap(),
        },
    };
    let response = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        response: DeviceResponse::LxmfRead(LxmfReadChunk::new(handle, 256, 512, b"abc").unwrap()),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let request_len = encode_request(&request, &mut output).unwrap();
    assert_eq!(&output[..request_len], REQUEST);
    assert_eq!(decode_request(REQUEST).unwrap(), request);
    let response_len = encode_response(&response, &mut output).unwrap();
    assert_eq!(&output[..response_len], RESPONSE);
    assert_eq!(decode_response(RESPONSE).unwrap(), response);
    assert_eq!(request.request.operation(), OP_LXMF_READ);
    assert_eq!(response.response.kind(), OP_LXMF_READ);
}

#[cfg(feature = "lxmf")]
#[test]
fn exact_lxmf_mailbox_goldens_are_authenticated_and_strict() {
    const STATUS_REQUEST: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x10, 0x03,
        0xa0,
    ];
    const STATUS_RESPONSE: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x10, 0x03,
        0xa3, 0x00, 0x09, 0x01, 0x07, 0x02, 0x02,
    ];
    const ACKNOWLEDGE_REQUEST: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x11, 0x03,
        0xa1, 0x00, 0x09,
    ];
    const ACKNOWLEDGE_RESPONSE: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x11, 0x03,
        0xa3, 0x00, 0x09, 0x01, 0x09, 0x02, 0x00,
    ];
    let latest = LxmfMessageHandle::new(9).unwrap();
    let acknowledged = LxmfMessageHandle::new(7).unwrap();
    let status = LxmfMailboxStatus::new(Some(latest), Some(acknowledged)).unwrap();
    let status_request = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        request: DeviceRequest::LxmfMailboxStatus,
    };
    let status_response = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        response: DeviceResponse::LxmfMailboxStatus(status),
    };
    let acknowledge_request = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        request: DeviceRequest::LxmfMailboxAcknowledge { through: latest },
    };
    let acknowledge_response = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        response: DeviceResponse::LxmfMailboxAcknowledged(
            LxmfMailboxStatus::new(Some(latest), Some(latest)).unwrap(),
        ),
    };

    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    for (request, golden) in [
        (status_request, STATUS_REQUEST),
        (acknowledge_request, ACKNOWLEDGE_REQUEST),
    ] {
        let length = encode_request(&request, &mut output).unwrap();
        assert_eq!(&output[..length], golden);
        assert_eq!(decode_request(golden).unwrap(), request);
        assert_eq!(
            authorize_request(&DispatchContext::UNAUTHENTICATED, &request.request),
            Err(AuthorizationError::AuthenticationRequired)
        );
        assert_eq!(
            authorize_request(
                &DispatchContext::authenticated(
                    PrincipalId([0x72; 16]),
                    Permissions::NONE,
                    dispatch_provenance(),
                ),
                &request.request,
            ),
            Ok(())
        );
    }
    for (response, golden) in [
        (status_response, STATUS_RESPONSE),
        (acknowledge_response, ACKNOWLEDGE_RESPONSE),
    ] {
        let length = encode_response(&response, &mut output).unwrap();
        assert_eq!(&output[..length], golden);
        assert_eq!(decode_response(golden).unwrap(), response);
    }

    assert!(!status_request.request.is_mutating());
    assert!(acknowledge_request.request.is_mutating());
    assert_eq!(status_request.request.operation(), OP_LXMF_MAILBOX_STATUS);
    assert_eq!(
        acknowledge_request.request.operation(),
        OP_LXMF_MAILBOX_ACKNOWLEDGE
    );
    assert_eq!(status.uncollected_count(), 2);
    assert!(LxmfMailboxStatus::new(Some(acknowledged), Some(latest)).is_err());

    let mut contradictory_count = STATUS_RESPONSE.to_vec();
    *contradictory_count.last_mut().unwrap() = 3;
    assert_eq!(
        decode_response(&contradictory_count),
        Err(DecodeError::InvalidLxmfMailboxStatus)
    );
}

#[cfg(feature = "lxmf")]
fn basic_lxmf_send_request(
    title: &'static [u8],
    content: &'static [u8],
) -> RequestEnvelope<'static> {
    RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(9),
        request: DeviceRequest::LxmfBasicSend {
            destination: DestinationHash([
                0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
                0x0e, 0x0f,
            ]),
            timestamp_unix_ms: u64::MAX,
            title,
            content,
            location: None,
            idempotency_key: IdempotencyKey([
                0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc, 0xfd,
                0xfe, 0xff,
            ]),
        },
    }
}

#[cfg(feature = "lxmf")]
#[test]
fn exact_basic_lxmf_send_goldens_are_source_free_and_borrowed() {
    const REQUEST: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x06, 0x03, 0xa5,
        0x00, 0x50, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
        0x0d, 0x0e, 0x0f, 0x01, 0x1b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02, 0x43,
        0x74, 0x74, 0x6c, 0x03, 0x47, 0x63, 0x6f, 0x6e, 0x74, 0x65, 0x6e, 0x74, 0x04, 0x50, 0xf0,
        0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc, 0xfd, 0xfe, 0xff,
    ];
    const RESPONSE: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x06, 0x03, 0xa2,
        0x00, 0x1b, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x01, 0x58, 0x20, 0x20, 0x21,
        0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f, 0x30,
        0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3b, 0x3c, 0x3d, 0x3e, 0x3f,
    ];
    let request = basic_lxmf_send_request(b"ttl", b"content");
    let response = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(9),
        response: DeviceResponse::LxmfBasicSendAccepted(LxmfBasicSendAccepted::new(
            SubmissionId(0x0102_0304_0506_0708),
            core::array::from_fn(|index| 0x20 + index as u8),
        )),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let request_len = encode_request(&request, &mut output).unwrap();
    assert_eq!(&output[..request_len], REQUEST);
    let decoded = decode_request(REQUEST).unwrap();
    let DeviceRequest::LxmfBasicSend {
        timestamp_unix_ms,
        title,
        content,
        ..
    } = decoded.request
    else {
        panic!("wrong decoded operation")
    };
    assert_eq!(timestamp_unix_ms, u64::MAX);
    assert_eq!(title, b"ttl");
    assert_eq!(content, b"content");
    assert!(REQUEST.as_ptr_range().contains(&title.as_ptr()));
    assert!(REQUEST.as_ptr_range().contains(&content.as_ptr()));
    assert_eq!(decoded, request);
    assert!(request.request.is_mutating());
    assert_eq!(request.request.operation(), OP_LXMF_BASIC_SEND);

    let response_len = encode_response(&response, &mut output).unwrap();
    assert_eq!(&output[..response_len], RESPONSE);
    assert_eq!(decode_response(RESPONSE).unwrap(), response);
    assert_eq!(response.response.kind(), OP_LXMF_BASIC_SEND);
}

#[cfg(feature = "lxmf")]
#[test]
fn basic_lxmf_send_round_trips_optional_message_location() {
    let location = LxmfMessageLocation::new(
        44_123_456,
        -73_987_654,
        12_345,
        678,
        12_345,
        250,
        1_785_700_123,
    )
    .unwrap();
    let request = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(10),
        request: DeviceRequest::LxmfBasicSend {
            destination: DestinationHash([0x31; 16]),
            timestamp_unix_ms: 1_785_700_123_456,
            title: b"where",
            content: b"meet me here",
            location: Some(location),
            idempotency_key: IdempotencyKey([0x42; 16]),
        },
    };
    let mut encoded = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_request(&request, &mut encoded).unwrap();
    assert_eq!(decode_request(&encoded[..written]).unwrap(), request);
    let DeviceRequest::LxmfBasicSend {
        location: Some(decoded),
        ..
    } = decode_request(&encoded[..written]).unwrap().request
    else {
        panic!("location was not retained")
    };
    assert_eq!(decoded, location);
    assert!(LxmfMessageLocation::new(90_000_001, 0, 0, 0, 0, 0, 0).is_err());
    assert!(LxmfMessageLocation::new(0, 180_000_001, 0, 0, 0, 0, 0).is_err());
}

#[cfg(feature = "lxmf")]
fn discovered_peer(app_data: &[u8]) -> LxmfDiscoveredPeer {
    LxmfDiscoveredPeer::new(
        DestinationHash(core::array::from_fn(|index| 0x10 + index as u8)),
        IdentityHash::new(core::array::from_fn(|index| 0x20 + index as u8)),
        app_data,
        2,
        7,
        Some(-91),
        Some(6),
        1_000,
        LxmfPeerGeneration::new(9).unwrap(),
    )
    .unwrap()
}

#[cfg(feature = "lxmf")]
#[test]
fn exact_nearby_lxmf_peer_goldens_use_complete_boot_scoped_cursors() {
    const FIRST_REQUEST: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x07, 0x03,
        0xa0,
    ];
    const NEXT_REQUEST: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x07, 0x03,
        0xa2, 0x00, 0x48, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0x01, 0x09,
    ];
    const RESPONSE: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x07, 0x03,
        0xa6, 0x00, 0x48, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0x01, 0x09, 0x02, 0x0b,
        0x03, 0x03, 0x04, 0xf5, 0x05, 0xa9, 0x00, 0x50, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16,
        0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x01, 0x50, 0x20, 0x21, 0x22, 0x23,
        0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f, 0x02, 0x44, 0x6e,
        0x61, 0x6d, 0x65, 0x03, 0x02, 0x04, 0x07, 0x05, 0x38, 0x5a, 0x06, 0x06, 0x07, 0x19, 0x03,
        0xe8, 0x08, 0x09,
    ];
    let incarnation =
        LxmfPeerDiscoveryIncarnation::new([0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7]);
    let first_request = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        request: DeviceRequest::LxmfPeerNext { after: None },
    };
    let cursor = LxmfPeerDiscoveryCursor::new(incarnation, 9);
    let next_request = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        request: DeviceRequest::LxmfPeerNext {
            after: Some(cursor),
        },
    };
    let page = LxmfPeerDiscoveryPage::new(
        cursor,
        Some(LxmfPeerGeneration::new(11).unwrap()),
        Some(LxmfPeerGeneration::new(3).unwrap()),
        true,
        Some(discovered_peer(b"name")),
    );
    let response = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        response: DeviceResponse::LxmfPeerNext(page),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];

    let written = encode_request(&first_request, &mut output).unwrap();
    assert_eq!(&output[..written], FIRST_REQUEST);
    assert_eq!(decode_request(FIRST_REQUEST).unwrap(), first_request);
    let written = encode_request(&next_request, &mut output).unwrap();
    assert_eq!(&output[..written], NEXT_REQUEST);
    assert_eq!(decode_request(NEXT_REQUEST).unwrap(), next_request);
    let written = encode_response(&response, &mut output).unwrap();
    assert_eq!(&output[..written], RESPONSE);
    assert_eq!(decode_response(RESPONSE).unwrap(), response);
    assert!(!first_request.request.is_mutating());
    assert_eq!(first_request.request.operation(), OP_LXMF_PEER_NEXT);
    assert_eq!(response.response.kind(), OP_LXMF_PEER_NEXT);
}

#[cfg(feature = "lxmf")]
#[test]
fn nearby_lxmf_peer_cursor_and_response_shapes_are_strict() {
    const ONLY_INCARNATION: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x07, 0x03, 0xa1,
        0x00, 0x48, 0, 1, 2, 3, 4, 5, 6, 7,
    ];
    const ONLY_GENERATION: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x07, 0x03, 0xa1,
        0x01, 0x00,
    ];
    const SHORT_INCARNATION: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x07, 0x03, 0xa2,
        0x00, 0x47, 0, 1, 2, 3, 4, 5, 6, 0x01, 0x00,
    ];
    assert_eq!(
        decode_request(ONLY_INCARNATION),
        Err(DecodeError::MissingField(
            RequiredField::LxmfPeerCursorGeneration
        ))
    );
    assert_eq!(
        decode_request(ONLY_GENERATION),
        Err(DecodeError::MissingField(
            RequiredField::LxmfPeerCursorIncarnation
        ))
    );
    assert_eq!(
        decode_request(SHORT_INCARNATION),
        Err(DecodeError::InvalidByteStringLength {
            field: RequiredField::LxmfPeerCursorIncarnation,
            expected: 8,
            actual: 7,
        })
    );

    let peer = discovered_peer(&[0x5a; MAX_LXMF_PEER_APP_DATA_BYTES]);
    let debug = format!("{peer:?}");
    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("90, 90"));
    assert_eq!(
        LxmfDiscoveredPeer::new(
            peer.destination(),
            peer.identity_hash(),
            &[0; MAX_LXMF_PEER_APP_DATA_BYTES + 1],
            peer.hops(),
            peer.interface_id(),
            peer.rssi_dbm(),
            peer.snr_db(),
            peer.observed_age_ms(),
            peer.generation(),
        )
        .unwrap_err()
        .actual(),
        MAX_LXMF_PEER_APP_DATA_BYTES + 1
    );
}

#[cfg(feature = "lxmf")]
fn append_cbor_bytes(encoded: &mut Vec<u8>, bytes: &[u8]) {
    match bytes.len() {
        0..=23 => encoded.push(0x40 + bytes.len() as u8),
        24..=255 => encoded.extend([0x58, bytes.len() as u8]),
        _ => encoded.extend([0x59, (bytes.len() >> 8) as u8, bytes.len() as u8]),
    }
    encoded.extend_from_slice(bytes);
}

#[cfg(feature = "lxmf")]
fn raw_peer_response(app_data: &[u8], peer_generation: u8) -> Vec<u8> {
    let mut encoded = vec![
        0xa4,
        0x00,
        0xa2,
        0x00,
        0x03,
        0x01,
        0x00,
        0x01,
        0x01,
        0x02,
        0x19,
        0xf0,
        0x07,
        0x03,
        0xa6,
        0x00,
        0x48,
        0,
        1,
        2,
        3,
        4,
        5,
        6,
        7,
        0x01,
        peer_generation,
        0x02,
        0x01,
        0x03,
        0x01,
        0x04,
        0xf4,
        0x05,
        0xa7,
        0x00,
        0x50,
    ];
    encoded.extend(0x10..=0x1f);
    encoded.extend([0x01, 0x50]);
    encoded.extend(0x20..=0x2f);
    encoded.push(0x02);
    append_cbor_bytes(&mut encoded, app_data);
    encoded.extend([0x03, 0x00, 0x04, 0x01, 0x07, 0x00, 0x08, peer_generation]);
    encoded
}

#[cfg(feature = "lxmf")]
#[test]
fn nearby_lxmf_peer_cbor_rejects_zero_generation_and_oversize_app_data() {
    assert_eq!(
        decode_response(&raw_peer_response(b"name", 0)),
        Err(DecodeError::InvalidValue {
            field: RequiredField::LxmfPeerGeneration,
            value: 0,
        })
    );
    assert_eq!(
        decode_response(&raw_peer_response(
            &[0x5a; MAX_LXMF_PEER_APP_DATA_BYTES + 1],
            1,
        )),
        Err(DecodeError::LxmfPeerAppDataTooLarge {
            actual: MAX_LXMF_PEER_APP_DATA_BYTES + 1,
            max: MAX_LXMF_PEER_APP_DATA_BYTES,
        })
    );
}

#[cfg(feature = "lxmf")]
#[test]
fn maximum_nearby_peer_record_fits_the_frozen_message_and_body_limits() {
    let incarnation = LxmfPeerDiscoveryIncarnation::new([0xa5; 8]);
    let generation = LxmfPeerGeneration::new(u64::MAX).unwrap();
    let page = LxmfPeerDiscoveryPage::new(
        LxmfPeerDiscoveryCursor::new(incarnation, generation.get()),
        Some(generation),
        Some(generation),
        false,
        Some(
            LxmfDiscoveredPeer::new(
                DestinationHash([0x11; 16]),
                IdentityHash::new([0x22; 16]),
                &[0x5a; MAX_LXMF_PEER_APP_DATA_BYTES],
                u8::MAX,
                u8::MAX,
                Some(i16::MIN),
                Some(i16::MAX),
                u64::MAX,
                generation,
            )
            .unwrap(),
        ),
    );
    let envelope = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(u64::MAX),
        response: DeviceResponse::LxmfPeerNext(page),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_response(&envelope, &mut output).unwrap();
    assert!(written <= MAX_MESSAGE_BYTES);
    assert_eq!(decode_response(&output[..written]).unwrap(), envelope);
}

#[cfg(feature = "lxmf")]
fn raw_basic_lxmf_send(title: &[u8], content: &[u8]) -> Vec<u8> {
    let mut encoded = vec![
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x06, 0x03, 0xa5,
        0x00, 0x50,
    ];
    encoded.extend_from_slice(&[0; 16]);
    encoded.extend([0x01, 0x00, 0x02]);
    append_cbor_bytes(&mut encoded, title);
    encoded.push(0x03);
    append_cbor_bytes(&mut encoded, content);
    encoded.extend([0x04, 0x50]);
    encoded.extend_from_slice(&[0; 16]);
    encoded
}

#[cfg(feature = "lxmf")]
#[test]
fn basic_lxmf_send_enforces_individual_and_total_cbor_limits() {
    static TITLE_MAX: [u8; MAX_LXMF_BASIC_TITLE_BYTES] = [0x74; MAX_LXMF_BASIC_TITLE_BYTES];
    static CONTENT_MAX: [u8; MAX_LXMF_BASIC_CONTENT_BYTES] = [0x63; MAX_LXMF_BASIC_CONTENT_BYTES];
    static TITLE_TOO_LARGE: [u8; MAX_LXMF_BASIC_TITLE_BYTES + 1] =
        [0x74; MAX_LXMF_BASIC_TITLE_BYTES + 1];
    static CONTENT_TOO_LARGE: [u8; MAX_LXMF_BASIC_CONTENT_BYTES + 1] =
        [0x63; MAX_LXMF_BASIC_CONTENT_BYTES + 1];
    let mut output = [0_u8; MAX_MESSAGE_BYTES];

    for request in [
        basic_lxmf_send_request(&TITLE_MAX, b""),
        basic_lxmf_send_request(b"", &CONTENT_MAX),
    ] {
        let written = encode_request(&request, &mut output).unwrap();
        assert_eq!(decode_request(&output[..written]).unwrap(), request);
    }
    assert_eq!(
        encode_request(&basic_lxmf_send_request(&TITLE_TOO_LARGE, b""), &mut output),
        Err(EncodeError::LxmfBasicTitleTooLarge {
            actual: MAX_LXMF_BASIC_TITLE_BYTES + 1,
            max: MAX_LXMF_BASIC_TITLE_BYTES,
        })
    );
    assert_eq!(
        encode_request(
            &basic_lxmf_send_request(b"", &CONTENT_TOO_LARGE),
            &mut output,
        ),
        Err(EncodeError::LxmfBasicContentTooLarge {
            actual: MAX_LXMF_BASIC_CONTENT_BYTES + 1,
            max: MAX_LXMF_BASIC_CONTENT_BYTES,
        })
    );
    assert_eq!(
        decode_request(&raw_basic_lxmf_send(&TITLE_TOO_LARGE, b"")),
        Err(DecodeError::LxmfBasicTitleTooLarge {
            actual: MAX_LXMF_BASIC_TITLE_BYTES + 1,
            max: MAX_LXMF_BASIC_TITLE_BYTES,
        })
    );
    assert_eq!(
        decode_request(&raw_basic_lxmf_send(b"", &CONTENT_TOO_LARGE)),
        Err(DecodeError::LxmfBasicContentTooLarge {
            actual: MAX_LXMF_BASIC_CONTENT_BYTES + 1,
            max: MAX_LXMF_BASIC_CONTENT_BYTES,
        })
    );

    static TITLE_220: [u8; 220] = [0x74; 220];
    static CONTENT_220: [u8; 220] = [0x63; 220];
    assert_eq!(
        encode_request(
            &basic_lxmf_send_request(&TITLE_220, &CONTENT_220),
            &mut output,
        ),
        Err(EncodeError::BodyTooLarge {
            actual: 493,
            max: MAX_BODY_BYTES,
        })
    );
    assert_eq!(
        decode_request(&raw_basic_lxmf_send(&TITLE_220, &CONTENT_220)),
        Err(DecodeError::BodyTooLarge {
            actual: 485,
            max: MAX_BODY_BYTES,
        })
    );
}

#[cfg(feature = "lxmf")]
#[test]
fn basic_lxmf_send_reuses_submit_permission() {
    let request = basic_lxmf_send_request(b"", b"").request;
    let principal = PrincipalId([0x74; 16]);
    assert_eq!(
        authorize_request(&DispatchContext::UNAUTHENTICATED, &request),
        Err(AuthorizationError::AuthenticationRequired)
    );
    assert_eq!(
        authorize_request(
            &DispatchContext::authenticated(principal, Permissions::NONE, dispatch_provenance()),
            &request,
        ),
        Err(AuthorizationError::PermissionDenied(
            RequiredPermission::SubmitRnsData
        ))
    );
    assert_eq!(
        authorize_request(
            &DispatchContext::authenticated(
                principal,
                Permissions::SUBMIT_RNS_DATA,
                dispatch_provenance(),
            ),
            &request,
        ),
        Ok(())
    );
}

#[cfg(feature = "lxmf")]
#[test]
fn lxmf_reads_require_authentication_but_no_persisted_permission_bit() {
    let authenticated = DispatchContext::authenticated(
        PrincipalId([0x73; 16]),
        Permissions::NONE,
        dispatch_provenance(),
    );
    let requests = [
        DeviceRequest::LxmfNext { after: None },
        DeviceRequest::LxmfRead {
            handle: LxmfMessageHandle::new(1).unwrap(),
            offset: 0,
            max_bytes: LxmfReadLength::new(1).unwrap(),
        },
        DeviceRequest::LxmfPeerNext { after: None },
    ];
    for request in requests {
        assert!(!request.is_mutating());
        assert_eq!(
            authorize_request(&DispatchContext::UNAUTHENTICATED, &request),
            Err(AuthorizationError::AuthenticationRequired)
        );
        assert_eq!(authorize_request(&authenticated, &request), Ok(()));
    }
}

#[cfg(feature = "lxmf")]
#[test]
fn lxmf_model_is_owned_bounded_and_redacts_message_bytes() {
    assert!(LxmfMessageHandle::new(0).is_err());
    assert_eq!(LxmfMessageHandle::new(u64::MAX).unwrap().get(), u64::MAX);

    for invalid in [0, MAX_LXMF_READ_CHUNK_BYTES as u16 + 1] {
        let error = LxmfReadLength::new(invalid).unwrap_err();
        assert_eq!(error.actual(), invalid);
        assert_eq!(error.maximum() as usize, MAX_LXMF_READ_CHUNK_BYTES);
    }

    assert!(
        LxmfMessageSummary::new(
            LxmfMessageHandle::new(1).unwrap(),
            [0; 32],
            DestinationHash([0; 16]),
            DestinationHash([0; 16]),
            0,
            0,
            0,
            0,
            0,
            [0; 32],
        )
        .is_err()
    );

    let summary_arguments = |normalized_wire_len, title_len, content_len, fields_encoded_len| {
        LxmfMessageSummary::new(
            LxmfMessageHandle::new(1).unwrap(),
            [0; 32],
            DestinationHash([0; 16]),
            DestinationHash([0; 16]),
            0,
            normalized_wire_len,
            title_len,
            content_len,
            fields_encoded_len,
            [0; 32],
        )
    };
    // 96-byte destination/source/signature prefix, one-byte array header,
    // nine-byte float64, two two-byte empty binary values, and one-byte map.
    assert!(summary_arguments(111, 0, 0, 1).is_ok());
    assert!(summary_arguments(110, 0, 0, 1).is_err());
    assert!(summary_arguments(u32::MAX, 0, 0, 0).is_err());
    assert!(summary_arguments(u32::MAX, u32::MAX, 0, 1).is_err());
    assert!(summary_arguments(u32::MAX, 0, 0, u32::MAX).is_err());

    let handle = LxmfMessageHandle::new(1).unwrap();
    let mut source = [0x5a; MAX_LXMF_READ_CHUNK_BYTES];
    let chunk = LxmfReadChunk::new(handle, 0, MAX_LXMF_READ_CHUNK_BYTES as u32, &source).unwrap();
    source.fill(0);
    assert_eq!(chunk.bytes(), &[0x5a; MAX_LXMF_READ_CHUNK_BYTES]);
    assert!(chunk.is_final());
    let debug = std::format!("{chunk:?}");
    assert!(debug.contains("bytes_len: 416"));
    assert!(!debug.contains("90, 90"));

    assert!(matches!(
        LxmfReadChunk::new(handle, 0, 1, &[]),
        Err(reticulum_device_api::InvalidLxmfReadChunk::Empty)
    ));
    assert!(matches!(
        LxmfReadChunk::new(handle, 1, 1, b"x"),
        Err(reticulum_device_api::InvalidLxmfReadChunk::OutsideMessage { .. })
    ));
    assert!(matches!(
        LxmfReadChunk::new(handle, 0, (MAX_LXMF_READ_CHUNK_BYTES + 1) as u32, &[0; MAX_LXMF_READ_CHUNK_BYTES + 1]),
        Err(reticulum_device_api::InvalidLxmfReadChunk::TooLarge { actual })
            if actual == MAX_LXMF_READ_CHUNK_BYTES + 1
    ));
}

#[cfg(feature = "lxmf")]
#[test]
fn maximum_lxmf_read_chunk_fits_frozen_message_limit() {
    let bytes = [0xa5; MAX_LXMF_READ_CHUNK_BYTES];
    let offset = u32::MAX - MAX_LXMF_READ_CHUNK_BYTES as u32;
    let envelope = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(u64::MAX),
        response: DeviceResponse::LxmfRead(
            LxmfReadChunk::new(
                LxmfMessageHandle::new(u64::MAX).unwrap(),
                offset,
                u32::MAX,
                &bytes,
            )
            .unwrap(),
        ),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_response(&envelope, &mut output).unwrap();
    assert!(written <= MAX_MESSAGE_BYTES);
    assert_eq!(decode_response(&output[..written]).unwrap(), envelope);
}

#[cfg(feature = "lxmf")]
#[test]
fn lxmf_wire_fields_are_required_unique_and_strictly_bounded() {
    let missing_read_max = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x05, 0x03, 0xa2,
        0x00, 0x01, 0x01, 0x00,
    ];
    assert_eq!(
        decode_request(&missing_read_max),
        Err(DecodeError::MissingField(RequiredField::LxmfReadMaxBytes))
    );

    let duplicate_handle = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x05, 0x03, 0xa4,
        0x00, 0x01, 0x00, 0x02, 0x01, 0x00, 0x02, 0x01,
    ];
    assert_eq!(
        decode_request(&duplicate_handle),
        Err(DecodeError::DuplicateField(RequiredField::LxmfHandle))
    );

    let zero_after = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x04, 0x03, 0xa1,
        0x00, 0x00,
    ];
    assert_eq!(
        decode_request(&zero_after),
        Err(DecodeError::InvalidValue {
            field: RequiredField::LxmfAfterHandle,
            value: 0,
        })
    );

    for (encoded_max, value) in [([0x00, 0x00, 0x00], 0_u64), ([0x19, 0x01, 0xa1], 417)] {
        let mut request = vec![
            0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x05, 0x03,
            0xa3, 0x00, 0x01, 0x01, 0x00, 0x02,
        ];
        if value == 0 {
            request.push(encoded_max[0]);
        } else {
            request.extend_from_slice(&encoded_max);
        }
        assert_eq!(
            decode_request(&request),
            Err(DecodeError::InvalidValue {
                field: RequiredField::LxmfReadMaxBytes,
                value,
            })
        );
    }

    let empty_chunk = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x05, 0x03, 0xa4,
        0x00, 0x01, 0x01, 0x00, 0x02, 0x01, 0x03, 0x40,
    ];
    assert_eq!(
        decode_response(&empty_chunk),
        Err(DecodeError::InvalidLxmfReadChunk)
    );

    let outside_chunk = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x05, 0x03, 0xa4,
        0x00, 0x01, 0x01, 0x01, 0x02, 0x01, 0x03, 0x41, 0x61,
    ];
    assert_eq!(
        decode_response(&outside_chunk),
        Err(DecodeError::InvalidLxmfReadChunk)
    );

    let mut oversized_chunk = vec![
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x05, 0x03, 0xa4,
        0x00, 0x01, 0x01, 0x00, 0x02, 0x19, 0x01, 0xa1, 0x03, 0x59, 0x01, 0xa1,
    ];
    oversized_chunk.extend_from_slice(&[0; MAX_LXMF_READ_CHUNK_BYTES + 1]);
    assert_eq!(
        decode_response(&oversized_chunk),
        Err(DecodeError::LxmfReadChunkTooLarge {
            actual: MAX_LXMF_READ_CHUNK_BYTES + 1,
            max: MAX_LXMF_READ_CHUNK_BYTES,
        })
    );

    let summary_envelope = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(1),
        response: DeviceResponse::LxmfNext(lxmf_summary()),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_response(&summary_envelope, &mut output).unwrap();

    let mut wrong_message_id_width = output[..written].to_vec();
    let message_id = wrong_message_id_width
        .windows(3)
        .position(|window| window == [0x01, 0x58, 0x20])
        .unwrap();
    wrong_message_id_width[message_id + 2] = 0x1f;
    wrong_message_id_width.remove(message_id + 3);
    assert_eq!(
        decode_response(&wrong_message_id_width),
        Err(DecodeError::InvalidByteStringLength {
            field: RequiredField::LxmfMessageId,
            expected: 32,
            actual: 31,
        })
    );

    let mut zero_wire_length = output[..written].to_vec();
    let wire_length = zero_wire_length
        .windows(4)
        .position(|window| window == [0x05, 0x19, 0x01, 0x23])
        .unwrap();
    zero_wire_length.splice(wire_length + 1..wire_length + 4, [0x00]);
    assert_eq!(
        decode_response(&zero_wire_length),
        Err(DecodeError::InvalidLxmfMessageSummary)
    );

    let mut contradictory_wire_length = output[..written].to_vec();
    let wire_length = contradictory_wire_length
        .windows(4)
        .position(|window| window == [0x05, 0x19, 0x01, 0x23])
        .unwrap();
    contradictory_wire_length.splice(wire_length + 1..wire_length + 4, [0x19, 0x00, 0x6e]);
    assert_eq!(
        decode_response(&contradictory_wire_length),
        Err(DecodeError::InvalidLxmfMessageSummary)
    );

    let mut overflowing_title_length = output[..written].to_vec();
    let title_length = overflowing_title_length
        .windows(6)
        .position(|window| window == [0x06, 0x05, 0x07, 0x09, 0x08, 0x01])
        .unwrap();
    overflowing_title_length.splice(
        title_length + 1..title_length + 2,
        [0x1a, 0xff, 0xff, 0xff, 0xff],
    );
    assert_eq!(
        decode_response(&overflowing_title_length),
        Err(DecodeError::InvalidLxmfMessageSummary)
    );

    let unknown_next_field = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x04, 0x03, 0xa1,
        0x18, 0x63, 0x82, 0x01, 0x02,
    ];
    assert_eq!(
        decode_request(&unknown_next_field).unwrap().request,
        DeviceRequest::LxmfNext { after: None }
    );
}

#[test]
fn exact_typed_error_response_golden_round_trip() {
    const GOLDEN: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x00, 0x03, 0xa2, 0x00,
        0x04, 0x01, 0x02,
    ];
    let envelope = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        response: DeviceResponse::Error(ApiErrorResponse {
            code: ApiErrorCode::PermissionDenied,
            operation: Some(OP_SUBMISSION_STATUS),
        }),
    };
    let mut output = [0u8; MAX_MESSAGE_BYTES];
    let written = encode_response(&envelope, &mut output).unwrap();
    assert_eq!(&output[..written], GOLDEN);
    assert_eq!(decode_response(GOLDEN).unwrap(), envelope);
}

#[test]
fn immediate_submission_rejections_have_distinct_error_goldens() {
    const MUTATING_OPERATION: u16 = 0xf001;
    const CAPACITY_GOLDEN: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x00, 0x03, 0xa2, 0x00,
        0x09, 0x01, 0x19, 0xf0, 0x01,
    ];
    const IDEMPOTENCY_GOLDEN: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x00, 0x03, 0xa2, 0x00,
        0x0a, 0x01, 0x19, 0xf0, 0x01,
    ];
    const RETRY_LATER_GOLDEN: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x00, 0x03, 0xa2, 0x00,
        0x0b, 0x01, 0x19, 0xf0, 0x01,
    ];
    for (code, golden) in [
        (ApiErrorCode::CapacityExhausted, CAPACITY_GOLDEN),
        (ApiErrorCode::IdempotencyConflict, IDEMPOTENCY_GOLDEN),
        (ApiErrorCode::RetryLater, RETRY_LATER_GOLDEN),
    ] {
        let envelope = ResponseEnvelope {
            version: ApiVersion::CURRENT,
            request_id: RequestId(42),
            response: DeviceResponse::Error(ApiErrorResponse {
                code,
                operation: Some(MUTATING_OPERATION),
            }),
        };
        let mut output = [0u8; MAX_MESSAGE_BYTES];
        let written = encode_response(&envelope, &mut output).unwrap();
        assert_eq!(&output[..written], golden);
        assert_eq!(decode_response(golden).unwrap(), envelope);
    }
}

#[test]
fn status_request_and_scalar_response_round_trip_without_packet_bytes() {
    let request = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(7),
        request: DeviceRequest::SubmissionStatus {
            id: SubmissionId(0x0102_0304_0506_0708),
        },
    };
    let status = SubmissionStatus {
        id: SubmissionId(0x0102_0304_0506_0708),
        state: SubmissionState::AwaitingDelivery(PreparedPacketDetails {
            packet_len: 97,
            encoded_packet_sha256: EncodedPacketSha256::new([0x5a; 32]),
        }),
    };
    let response = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(7),
        response: DeviceResponse::SubmissionStatus(status),
    };
    let mut output = [0u8; MAX_MESSAGE_BYTES];
    let request_len = encode_request(&request, &mut output).unwrap();
    assert_eq!(decode_request(&output[..request_len]).unwrap(), request);
    let response_len = encode_response(&response, &mut output).unwrap();
    assert_eq!(decode_response(&output[..response_len]).unwrap(), response);
}

#[test]
fn every_valid_submission_state_round_trips_with_only_its_own_details() {
    let details = PreparedPacketDetails {
        packet_len: 97,
        encoded_packet_sha256: EncodedPacketSha256::new([0x5a; 32]),
    };
    let states = [
        SubmissionState::Queued,
        SubmissionState::Preparing,
        SubmissionState::AwaitingDelivery(details),
        SubmissionState::Delivered(details),
        SubmissionState::Failed(SubmissionFailure::NoPath),
        SubmissionState::Failed(SubmissionFailure::DeliveryTimeout),
        SubmissionState::Failed(SubmissionFailure::Rejected),
        SubmissionState::Failed(SubmissionFailure::Internal),
        SubmissionState::Cancelled,
    ];
    let mut output = [0u8; MAX_MESSAGE_BYTES];
    for state in states {
        let envelope = ResponseEnvelope {
            version: ApiVersion::CURRENT,
            request_id: RequestId(7),
            response: DeviceResponse::SubmissionStatus(SubmissionStatus {
                id: SubmissionId(42),
                state,
            }),
        };
        let written = encode_response(&envelope, &mut output).unwrap();
        assert_eq!(decode_response(&output[..written]).unwrap(), envelope);
    }
}

#[test]
fn contradictory_submission_status_wire_shapes_are_rejected() {
    let queued_with_length = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x07, 0x02, 0x02, 0x03, 0xa3, 0x00, 0x01,
        0x01, 0x00, 0x02, 0x18, 0x61,
    ];
    let awaiting_delivery_without_hash = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x07, 0x02, 0x02, 0x03, 0xa3, 0x00, 0x01,
        0x01, 0x02, 0x02, 0x18, 0x61,
    ];
    let failed_without_failure = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x07, 0x02, 0x02, 0x03, 0xa2, 0x00, 0x01,
        0x01, 0x04,
    ];
    for malformed in [
        queued_with_length.as_slice(),
        awaiting_delivery_without_hash.as_slice(),
        failed_without_failure.as_slice(),
    ] {
        assert_eq!(
            decode_response(malformed),
            Err(DecodeError::InvalidSubmissionStatus)
        );
    }
}

#[test]
fn unknown_envelope_version_and_body_fields_are_skipped() {
    // Envelope key 99, version key 7, and empty-body key 55 are all unknown.
    let bytes = [
        0xa5, 0x00, 0xa3, 0x00, 0x03, 0x01, 0x00, 0x07, 0x82, 0x01, 0x02, 0x01, 0x18, 0x2a, 0x02,
        0x01, 0x03, 0xa1, 0x18, 0x37, 0xa1, 0x00, 0xf5, 0x18, 0x63, 0x82, 0x01, 0x02,
    ];
    assert_eq!(decode_request(&bytes).unwrap(), capabilities_request());

    let mut large_unknown_key = GOLDEN_CAPABILITIES_REQUEST.to_vec();
    large_unknown_key[0] = 0xa5;
    large_unknown_key.extend([0x1a, 0x00, 0x01, 0x00, 0x00, 0xf6]);
    assert_eq!(
        decode_request(&large_unknown_key).unwrap(),
        capabilities_request()
    );
}

#[test]
fn unknown_status_body_field_is_skipped() {
    let bytes = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x07, 0x02, 0x02, 0x03, 0xa2, 0x00, 0x18,
        0x2a, 0x18, 0x63, 0x82, 0x01, 0x02,
    ];
    let decoded = decode_request(&bytes).unwrap();
    assert_eq!(decoded.request_id, RequestId(7));
    assert_eq!(
        decoded.request,
        DeviceRequest::SubmissionStatus {
            id: SubmissionId(42)
        }
    );
}

#[test]
fn unknown_operation_is_typed() {
    let bytes = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0x12, 0x34, 0x03,
        0xa0,
    ];
    assert_eq!(
        decode_request(&bytes),
        Err(DecodeError::UnsupportedOperation(0x1234))
    );
}

#[test]
fn incompatible_major_version_is_typed_but_same_major_minors_are_accepted() {
    let incompatible = [
        0xa4, 0x00, 0xa2, 0x00, 0x04, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x01, 0x03, 0xa0,
    ];
    assert_eq!(
        decode_request(&incompatible),
        Err(DecodeError::UnsupportedVersion(ApiVersion {
            major: API_VERSION_MAJOR + 1,
            minor: 0,
        }))
    );

    let newer_minor = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x09, 0x01, 0x18, 0x2a, 0x02, 0x01, 0x03, 0xa0,
    ];
    assert_eq!(decode_request(&newer_minor).unwrap().version.minor, 9);

    let current_minor = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x01, 0x03, 0xa0,
    ];
    assert_eq!(decode_request(&current_minor).unwrap().version.minor, 0);
}

#[test]
fn incompatible_major_version_is_rejected_before_encoding() {
    let unsupported = ApiVersion {
        major: API_VERSION_MAJOR + 1,
        minor: 0,
    };
    let request = RequestEnvelope {
        version: unsupported,
        ..capabilities_request()
    };
    let response = ResponseEnvelope {
        version: unsupported,
        request_id: RequestId(42),
        response: DeviceResponse::SystemCapabilities(CapabilitySnapshot::current()),
    };
    let mut output = [0u8; MAX_MESSAGE_BYTES];
    assert_eq!(
        encode_request(&request, &mut output),
        Err(EncodeError::UnsupportedVersion(unsupported))
    );
    assert_eq!(
        encode_response(&response, &mut output),
        Err(EncodeError::UnsupportedVersion(unsupported))
    );
}

#[test]
fn missing_required_envelope_and_body_fields_are_typed() {
    let missing_body = [
        0xa3, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x01,
    ];
    assert_eq!(
        decode_request(&missing_body),
        Err(DecodeError::MissingField(RequiredField::EnvelopeBody))
    );

    let missing_submission_id = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x07, 0x02, 0x02, 0x03, 0xa0,
    ];
    assert_eq!(
        decode_request(&missing_submission_id),
        Err(DecodeError::MissingField(RequiredField::SubmissionId))
    );
}

#[test]
fn duplicate_required_fields_are_rejected_at_each_level() {
    let duplicate_envelope_id = [
        0xa5, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x01, 0x18, 0x2b, 0x02, 0x01,
        0x03, 0xa0,
    ];
    assert_eq!(
        decode_request(&duplicate_envelope_id),
        Err(DecodeError::DuplicateField(
            RequiredField::EnvelopeRequestId
        ))
    );

    let duplicate_version_major = [
        0xa4, 0x00, 0xa3, 0x00, 0x01, 0x00, 0x01, 0x01, 0x01, 0x01, 0x18, 0x2a, 0x02, 0x01, 0x03,
        0xa0,
    ];
    assert_eq!(
        decode_request(&duplicate_version_major),
        Err(DecodeError::DuplicateField(RequiredField::VersionMajor))
    );

    let duplicate_submission_id = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x07, 0x02, 0x02, 0x03, 0xa2, 0x00, 0x01,
        0x00, 0x02,
    ];
    assert_eq!(
        decode_request(&duplicate_submission_id),
        Err(DecodeError::DuplicateField(RequiredField::SubmissionId))
    );
}

#[test]
fn every_truncated_golden_request_is_rejected() {
    for end in 0..GOLDEN_CAPABILITIES_REQUEST.len() {
        assert!(
            decode_request(&GOLDEN_CAPABILITIES_REQUEST[..end]).is_err(),
            "prefix of length {end} unexpectedly decoded"
        );
    }
    let golden_capabilities_response = golden_capabilities_response();
    for end in 0..golden_capabilities_response.len() {
        assert!(
            decode_response(&golden_capabilities_response[..end]).is_err(),
            "response prefix of length {end} unexpectedly decoded"
        );
    }
}

#[test]
fn malformed_and_unknown_responses_are_typed() {
    let unknown_kind = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x18, 0x63, 0x03, 0xa0,
    ];
    assert_eq!(
        decode_response(&unknown_kind),
        Err(DecodeError::UnsupportedResponseKind(99))
    );

    let duplicate_error_code = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x00, 0x03, 0xa2, 0x00, 0x01,
        0x00, 0x02,
    ];
    assert_eq!(
        decode_response(&duplicate_error_code),
        Err(DecodeError::DuplicateField(RequiredField::ErrorCode))
    );

    let invalid_error_code = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x00, 0x03, 0xa1, 0x00, 0x18,
        0x63,
    ];
    assert_eq!(
        decode_response(&invalid_error_code),
        Err(DecodeError::InvalidValue {
            field: RequiredField::ErrorCode,
            value: 99,
        })
    );
}

#[test]
fn api_numeric_enum_vocabularies_are_closed_and_stable() {
    assert_eq!(
        [
            CapabilityAvailability::Unavailable.wire_code(),
            CapabilityAvailability::Disabled.wire_code(),
            CapabilityAvailability::Available.wire_code(),
        ],
        [0, 1, 2]
    );
    let details = PreparedPacketDetails {
        packet_len: 1,
        encoded_packet_sha256: EncodedPacketSha256::new([0; 32]),
    };
    assert_eq!(
        [
            SubmissionState::Queued.wire_code(),
            SubmissionState::Preparing.wire_code(),
            SubmissionState::AwaitingDelivery(details).wire_code(),
            SubmissionState::Delivered(details).wire_code(),
            SubmissionState::Failed(SubmissionFailure::Internal).wire_code(),
            SubmissionState::Cancelled.wire_code(),
        ],
        [0, 1, 2, 3, 4, 5]
    );
    assert_eq!(
        [
            SubmissionFailure::NoPath.wire_code(),
            SubmissionFailure::DeliveryTimeout.wire_code(),
            SubmissionFailure::Rejected.wire_code(),
            SubmissionFailure::Internal.wire_code(),
        ],
        [0, 1, 2, 3]
    );
    assert_eq!(
        [
            ApiErrorCode::UnsupportedOperation.wire_code(),
            ApiErrorCode::UnsupportedVersion.wire_code(),
            ApiErrorCode::AuthenticationRequired.wire_code(),
            ApiErrorCode::PermissionDenied.wire_code(),
            ApiErrorCode::NotFound.wire_code(),
            ApiErrorCode::InvalidRequest.wire_code(),
            ApiErrorCode::CapabilityUnavailable.wire_code(),
            ApiErrorCode::Internal.wire_code(),
            ApiErrorCode::CapacityExhausted.wire_code(),
            ApiErrorCode::IdempotencyConflict.wire_code(),
            ApiErrorCode::RetryLater.wire_code(),
        ],
        [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]
    );
    assert_eq!(
        [
            RadioTraceTxOutcome::Transmitted.wire_code(),
            RadioTraceTxOutcome::AccessRejected.wire_code(),
            RadioTraceTxOutcome::PermitDenied.wire_code(),
            RadioTraceTxOutcome::AuthorizationExpired.wire_code(),
            RadioTraceTxOutcome::PostGrantAccessRejected.wire_code(),
            RadioTraceTxOutcome::AirtimeRejected.wire_code(),
            RadioTraceTxOutcome::DeadlineConversionOverflow.wire_code(),
            RadioTraceTxOutcome::RadioInactive.wire_code(),
            RadioTraceTxOutcome::InterfaceConfigurationMismatch.wire_code(),
            RadioTraceTxOutcome::RadioConfigurationChangedBeforePermit.wire_code(),
            RadioTraceTxOutcome::RadioConfigurationChangedAfterPermit.wire_code(),
            RadioTraceTxOutcome::CadFault.wire_code(),
            RadioTraceTxOutcome::TxFault.wire_code(),
            RadioTraceTxOutcome::ControlPlaneRecovery.wire_code(),
            RadioTraceTxOutcome::FrameInvariantRecovery.wire_code(),
            RadioTraceTxOutcome::CancelledRadioOperation.wire_code(),
        ],
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
    );
    assert_eq!(
        [
            RadioTraceAttemptOutcome::Delivered.wire_code(),
            RadioTraceAttemptOutcome::DeliveryTimeout.wire_code(),
            RadioTraceAttemptOutcome::Unsent.wire_code(),
        ],
        [0, 1, 2]
    );

    let unknown_state = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x02, 0x03, 0xa2, 0x00, 0x01,
        0x01, 0x18, 0x63,
    ];
    assert_eq!(
        decode_response(&unknown_state),
        Err(DecodeError::InvalidValue {
            field: RequiredField::SubmissionState,
            value: 99,
        })
    );

    let unknown_failure = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x02, 0x03, 0xa3, 0x00, 0x01,
        0x01, 0x04, 0x04, 0x18, 0x63,
    ];
    assert_eq!(
        decode_response(&unknown_failure),
        Err(DecodeError::InvalidValue {
            field: RequiredField::SubmissionFailure,
            value: 99,
        })
    );
}

#[test]
fn trailing_and_oversized_messages_are_rejected_before_dispatch() {
    let mut trailing = GOLDEN_CAPABILITIES_REQUEST.to_vec();
    trailing.push(0x00);
    assert_eq!(decode_request(&trailing), Err(DecodeError::TrailingData));

    let oversized = vec![0u8; MAX_MESSAGE_BYTES + 1];
    assert_eq!(
        decode_request(&oversized),
        Err(DecodeError::MessageTooLarge {
            actual: MAX_MESSAGE_BYTES + 1,
            max: MAX_MESSAGE_BYTES,
        })
    );
}

#[test]
fn oversized_encoded_body_is_rejected_independently_of_message_limit() {
    let unknown_value_len = MAX_BODY_BYTES - 3;
    let mut bytes = vec![
        0xa4,
        0x00,
        0xa2,
        0x00,
        0x03,
        0x01,
        0x00,
        0x01,
        0x01,
        0x02,
        0x01,
        0x03,
        0xa1,
        0x18,
        0x63,
        0x59,
        ((unknown_value_len >> 8) & 0xff) as u8,
        (unknown_value_len & 0xff) as u8,
    ];
    bytes.resize(bytes.len() + unknown_value_len, 0);
    let body_size = 1 + 2 + 3 + unknown_value_len;
    assert!(bytes.len() <= MAX_MESSAGE_BYTES);
    assert_eq!(
        decode_request(&bytes),
        Err(DecodeError::BodyTooLarge {
            actual: body_size,
            max: MAX_BODY_BYTES,
        })
    );
}

#[test]
fn indefinite_values_and_excessive_field_counts_are_rejected() {
    assert_eq!(
        decode_request(&[0xbf, 0xff]),
        Err(DecodeError::IndefiniteLength)
    );
    assert_eq!(
        decode_request(&[0xb8, 0x21]),
        Err(DecodeError::TooManyMapEntries {
            actual: 33,
            max: 32,
        })
    );

    let mut unknown_envelope_array = GOLDEN_CAPABILITIES_REQUEST.to_vec();
    unknown_envelope_array[0] = 0xa5;
    unknown_envelope_array.extend([0x18, 0x63, 0x9f, 0x01, 0xff]);
    assert_eq!(
        decode_request(&unknown_envelope_array),
        Err(DecodeError::IndefiniteLength)
    );

    let unknown_body_bytes = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x01, 0x03, 0xa1, 0x18,
        0x37, 0x5f, 0x41, 0x00, 0xff,
    ];
    assert_eq!(
        decode_request(&unknown_body_bytes),
        Err(DecodeError::IndefiniteLength)
    );
}

#[test]
fn unknown_value_nesting_is_strictly_bounded_without_allocation() {
    let nested_unknown = |depth: usize| {
        let mut message = GOLDEN_CAPABILITIES_REQUEST.to_vec();
        message[0] = 0xa5;
        message.extend([0x18, 0x63]);
        message.extend(core::iter::repeat_n(0x81, depth));
        message.push(0xf6);
        message
    };

    assert_eq!(
        decode_request(&nested_unknown(MAX_CBOR_NESTING_DEPTH)).unwrap(),
        capabilities_request()
    );
    assert_eq!(
        decode_request(&nested_unknown(MAX_CBOR_NESTING_DEPTH + 1)),
        Err(DecodeError::NestingTooDeep {
            actual: MAX_CBOR_NESTING_DEPTH + 1,
            max: MAX_CBOR_NESTING_DEPTH,
        })
    );
}

#[test]
fn output_buffer_is_bounded() {
    let mut output = [0u8; 4];
    assert_eq!(
        encode_request(&capabilities_request(), &mut output),
        Err(EncodeError::OutputTooSmall)
    );
}

#[test]
fn authorization_uses_separate_trusted_context() {
    let capabilities = DeviceRequest::SystemCapabilities;
    assert_eq!(
        authorize_request(&DispatchContext::UNAUTHENTICATED, &capabilities),
        Ok(())
    );

    let status = DeviceRequest::SubmissionStatus {
        id: SubmissionId(1),
    };
    assert_eq!(
        authorize_request(&DispatchContext::UNAUTHENTICATED, &status),
        Err(AuthorizationError::AuthenticationRequired)
    );
    let principal = PrincipalId([0x44; 16]);
    assert_eq!(
        authorize_request(
            &DispatchContext::authenticated(principal, Permissions::NONE, dispatch_provenance(),),
            &status,
        ),
        Err(AuthorizationError::PermissionDenied(
            RequiredPermission::ReadSubmissionStatus
        ))
    );
    assert_eq!(
        authorize_request(
            &DispatchContext::authenticated(
                principal,
                Permissions::READ_SUBMISSION_STATUS,
                dispatch_provenance(),
            ),
            &status,
        ),
        Ok(())
    );
}

#[test]
fn authenticated_context_carries_validated_dispatch_provenance() {
    let provenance = dispatch_provenance();
    let context = DispatchContext::authenticated(
        PrincipalId([0x44; 16]),
        Permissions::READ_SUBMISSION_STATUS,
        provenance,
    );

    assert_eq!(context.provenance(), Some(provenance));
    assert_eq!(provenance.credential_id(), [0x22; 16]);
    assert_eq!(provenance.credential_generation(), 7);
    assert_eq!(provenance.authority_revision(), 11);
    assert_eq!(provenance.policy_version(), 3);
    assert_eq!(DispatchContext::UNAUTHENTICATED.provenance(), None);
}

#[test]
fn dispatch_provenance_rejects_erased_and_impossible_facts() {
    assert_eq!(
        DispatchProvenance::new([0; 16], 1, 1, 1),
        Err(DispatchProvenanceError::ZeroCredentialId)
    );
    assert_eq!(
        DispatchProvenance::new([1; 16], 0, 1, 1),
        Err(DispatchProvenanceError::ZeroCredentialGeneration)
    );
    assert_eq!(
        DispatchProvenance::new([1; 16], 1, 0, 1),
        Err(DispatchProvenanceError::ZeroAuthorityRevision)
    );
    assert_eq!(
        DispatchProvenance::new([1; 16], 1, 1, 0),
        Err(DispatchProvenanceError::ZeroPolicyVersion)
    );
    assert_eq!(
        DispatchProvenance::new([1; 16], 12, 11, 1),
        Err(
            DispatchProvenanceError::GenerationExceedsAuthorityRevision {
                credential_generation: 12,
                authority_revision: 11,
            }
        )
    );
}

#[test]
fn capability_snapshot_separates_direct_radio_from_outbound_rns_submission() {
    let capabilities = CapabilitySnapshot::current();
    assert!(!capabilities.packet_output());
    assert_eq!(
        capabilities.direct_radio_tx(),
        CapabilityAvailability::Unavailable
    );
    assert_eq!(capabilities.max_message_bytes() as usize, MAX_MESSAGE_BYTES);
    assert_eq!(capabilities.max_body_bytes() as usize, MAX_BODY_BYTES);
    assert_eq!(
        capabilities.max_submit_rns_data_payload_bytes() as usize,
        MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES
    );
    assert_eq!(capabilities.submit_rns_data(), cfg!(feature = "rns-data"));
    assert_eq!(
        capabilities.lxmf(),
        if cfg!(feature = "lxmf") {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Unavailable
        }
    );
    assert_eq!(
        capabilities.max_lxmf_read_chunk_bytes(),
        if cfg!(feature = "lxmf") { 416 } else { 0 }
    );
    assert_eq!(
        capabilities.lxmf_basic_send(),
        if cfg!(feature = "lxmf") {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Unavailable
        }
    );
    assert_eq!(
        capabilities.max_lxmf_basic_title_bytes(),
        if cfg!(feature = "lxmf") {
            MAX_LXMF_BASIC_TITLE_BYTES as u16
        } else {
            0
        }
    );
    assert_eq!(
        capabilities.max_lxmf_basic_content_bytes(),
        if cfg!(feature = "lxmf") {
            MAX_LXMF_BASIC_CONTENT_BYTES as u16
        } else {
            0
        }
    );
    assert_eq!(
        capabilities.lxmf_peer_discovery(),
        if cfg!(feature = "lxmf") {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Unavailable
        }
    );
    assert_eq!(
        capabilities.max_lxmf_peer_app_data_bytes(),
        if cfg!(feature = "lxmf") {
            MAX_LXMF_PEER_APP_DATA_BYTES as u16
        } else {
            0
        }
    );
    assert_eq!(
        capabilities.nomad(),
        if cfg!(feature = "nomad") {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Unavailable
        }
    );
    assert_eq!(
        capabilities.max_nomad_page_path_bytes(),
        if cfg!(feature = "nomad") {
            MAX_NOMAD_PAGE_PATH_BYTES as u16
        } else {
            0
        }
    );
    assert_eq!(
        capabilities.max_nomad_page_bytes(),
        if cfg!(feature = "nomad") {
            MAX_NOMAD_PAGE_BYTES as u16
        } else {
            0
        }
    );
    assert_eq!(
        capabilities.network_config(),
        if cfg!(feature = "network-config") {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Unavailable
        }
    );

    let base_dispatch = CapabilitySnapshot::for_dispatch(true);
    assert_eq!(base_dispatch.submit_rns_data(), cfg!(feature = "rns-data"));
    assert_eq!(base_dispatch.lxmf(), CapabilityAvailability::Unavailable);
    assert_eq!(base_dispatch.max_lxmf_read_chunk_bytes(), 0);
    assert_eq!(
        base_dispatch.lxmf_basic_send(),
        CapabilityAvailability::Unavailable
    );
    assert_eq!(base_dispatch.max_lxmf_basic_title_bytes(), 0);
    assert_eq!(base_dispatch.max_lxmf_basic_content_bytes(), 0);
    assert_eq!(
        base_dispatch.lxmf_peer_discovery(),
        CapabilityAvailability::Unavailable
    );
    assert_eq!(base_dispatch.max_lxmf_peer_app_data_bytes(), 0);
    assert_eq!(base_dispatch.nomad(), CapabilityAvailability::Unavailable);
    assert_eq!(base_dispatch.max_nomad_page_path_bytes(), 0);
    assert_eq!(base_dispatch.max_nomad_page_bytes(), 0);
    assert_eq!(
        base_dispatch.network_config(),
        CapabilityAvailability::Unavailable
    );
    assert!(!CapabilitySnapshot::for_dispatch(false).submit_rns_data());

    let lxmf_dispatch =
        CapabilitySnapshot::for_dispatch_with_lxmf(true, CapabilityAvailability::Disabled);
    assert_eq!(
        lxmf_dispatch.lxmf(),
        if cfg!(feature = "lxmf") {
            CapabilityAvailability::Disabled
        } else {
            CapabilityAvailability::Unavailable
        }
    );
    assert_eq!(
        lxmf_dispatch.max_lxmf_read_chunk_bytes(),
        if cfg!(feature = "lxmf") { 416 } else { 0 }
    );
    assert_eq!(
        lxmf_dispatch.lxmf_basic_send(),
        CapabilityAvailability::Unavailable
    );

    let send_dispatch = CapabilitySnapshot::for_dispatch_with_lxmf_and_basic_send(
        true,
        CapabilityAvailability::Disabled,
        CapabilityAvailability::Disabled,
    );
    assert_eq!(
        send_dispatch.lxmf_basic_send(),
        if cfg!(feature = "lxmf") {
            CapabilityAvailability::Disabled
        } else {
            CapabilityAvailability::Unavailable
        }
    );
    assert_eq!(
        send_dispatch.max_lxmf_basic_title_bytes(),
        if cfg!(feature = "lxmf") {
            MAX_LXMF_BASIC_TITLE_BYTES as u16
        } else {
            0
        }
    );
    assert_eq!(
        send_dispatch.max_lxmf_basic_content_bytes(),
        if cfg!(feature = "lxmf") {
            MAX_LXMF_BASIC_CONTENT_BYTES as u16
        } else {
            0
        }
    );
    assert_eq!(
        send_dispatch.lxmf_peer_discovery(),
        CapabilityAvailability::Unavailable
    );

    let peer_dispatch = CapabilitySnapshot::for_dispatch_with_lxmf_basic_send_and_peer_discovery(
        true,
        CapabilityAvailability::Disabled,
        CapabilityAvailability::Disabled,
        CapabilityAvailability::Disabled,
        128,
    );
    assert_eq!(
        peer_dispatch.lxmf_peer_discovery(),
        if cfg!(feature = "lxmf") {
            CapabilityAvailability::Disabled
        } else {
            CapabilityAvailability::Unavailable
        }
    );
    assert_eq!(
        peer_dispatch.max_lxmf_peer_app_data_bytes(),
        if cfg!(feature = "lxmf") { 128 } else { 0 }
    );

    let composed_dispatch = peer_dispatch.with_dispatch_nomad(CapabilityAvailability::Disabled);
    assert_eq!(
        composed_dispatch.lxmf_peer_discovery(),
        peer_dispatch.lxmf_peer_discovery()
    );
    assert_eq!(
        composed_dispatch.nomad(),
        if cfg!(feature = "nomad") {
            CapabilityAvailability::Disabled
        } else {
            CapabilityAvailability::Unavailable
        }
    );
    assert_eq!(
        composed_dispatch.max_nomad_page_path_bytes(),
        if cfg!(feature = "nomad") {
            MAX_NOMAD_PAGE_PATH_BYTES as u16
        } else {
            0
        }
    );
    assert_eq!(
        composed_dispatch.max_nomad_page_bytes(),
        if cfg!(feature = "nomad") {
            MAX_NOMAD_PAGE_BYTES as u16
        } else {
            0
        }
    );

    let network_dispatch =
        composed_dispatch.with_dispatch_network_config(CapabilityAvailability::Disabled);
    assert_eq!(
        network_dispatch.network_config(),
        if cfg!(feature = "network-config") {
            CapabilityAvailability::Disabled
        } else {
            CapabilityAvailability::Unavailable
        }
    );
}

#[cfg(feature = "rns-data")]
fn submit_request(payload: &'static [u8]) -> RequestEnvelope<'static> {
    RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(9),
        request: DeviceRequest::SubmitRnsData {
            destination: DestinationHash([
                0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
                0x0e, 0x0f,
            ]),
            payload,
            idempotency_key: IdempotencyKey([
                0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc, 0xfd,
                0xfe, 0xff,
            ]),
        },
    }
}

#[cfg(feature = "rns-data")]
#[test]
fn exact_submit_golden_and_borrowed_payload() {
    const GOLDEN: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x01, 0x03, 0xa3,
        0x00, 0x50, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
        0x0d, 0x0e, 0x0f, 0x01, 0x43, 0x61, 0x62, 0x63, 0x02, 0x50, 0xf0, 0xf1, 0xf2, 0xf3, 0xf4,
        0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc, 0xfd, 0xfe, 0xff,
    ];
    let expected = submit_request(b"abc");
    let mut output = [0u8; MAX_MESSAGE_BYTES];
    let written = encode_request(&expected, &mut output).unwrap();
    assert_eq!(&output[..written], GOLDEN);

    let decoded = decode_request(GOLDEN).unwrap();
    let DeviceRequest::SubmitRnsData { payload, .. } = decoded.request else {
        panic!("wrong decoded operation")
    };
    let offset = GOLDEN
        .windows(payload.len())
        .position(|w| w == payload)
        .unwrap();
    assert!(core::ptr::eq(payload.as_ptr(), &GOLDEN[offset]));
    assert_eq!(decoded, expected);
}

#[cfg(feature = "rns-data")]
#[test]
fn exact_submit_accepted_response_has_only_submission_id() {
    const GOLDEN: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x01, 0x03, 0xa1,
        0x00, 0x1b, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
    ];
    let envelope = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(9),
        response: DeviceResponse::SubmitRnsDataAccepted(SubmissionAccepted {
            id: SubmissionId(0x0102_0304_0506_0708),
        }),
    };
    assert_eq!(envelope.response.kind(), OP_SUBMIT_RNS_DATA);
    let mut output = [0u8; MAX_MESSAGE_BYTES];
    let written = encode_response(&envelope, &mut output).unwrap();
    assert_eq!(&output[..written], GOLDEN);
    assert_eq!(decode_response(GOLDEN).unwrap(), envelope);
}

#[cfg(feature = "rns-data")]
#[test]
fn submit_is_mutating_and_requires_auth_and_permission() {
    let request = submit_request(b"abc").request;
    assert!(request.is_mutating());
    assert_eq!(
        authorize_request(&DispatchContext::UNAUTHENTICATED, &request),
        Err(AuthorizationError::AuthenticationRequired)
    );
    let principal = PrincipalId([0x55; 16]);
    assert_eq!(
        authorize_request(
            &DispatchContext::authenticated(principal, Permissions::NONE, dispatch_provenance(),),
            &request,
        ),
        Err(AuthorizationError::PermissionDenied(
            RequiredPermission::SubmitRnsData
        ))
    );
    assert_eq!(
        authorize_request(
            &DispatchContext::authenticated(
                principal,
                Permissions::SUBMIT_RNS_DATA,
                dispatch_provenance(),
            ),
            &request,
        ),
        Ok(())
    );
}

#[cfg(feature = "rns-data")]
#[test]
fn submit_rejects_oversize_payload_on_encode_and_decode() {
    static PAYLOAD: [u8; MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES + 1] =
        [0x5a; MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES + 1];
    let mut output = [0u8; MAX_MESSAGE_BYTES];
    assert_eq!(
        encode_request(&submit_request(&PAYLOAD), &mut output),
        Err(EncodeError::PayloadTooLarge {
            actual: PAYLOAD.len(),
            max: MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES,
        })
    );

    let mut encoded = vec![
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x01, 0x03, 0xa3,
        0x00, 0x50,
    ];
    encoded.extend(0u8..16);
    encoded.extend([0x01, 0x59, 0x01, 0x80]);
    encoded.extend_from_slice(&PAYLOAD);
    encoded.extend([0x02, 0x50]);
    encoded.extend(0u8..16);
    assert!(encoded.len() <= MAX_MESSAGE_BYTES);
    assert_eq!(
        decode_request(&encoded),
        Err(DecodeError::PayloadTooLarge {
            actual: PAYLOAD.len(),
            max: MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES,
        })
    );
}

#[cfg(feature = "rns-data")]
#[test]
fn submit_rejects_duplicate_and_wrong_width_fields() {
    let duplicate_destination = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x01, 0x03, 0xa4,
        0x00, 0x50, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x00, 0x50, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0x40, 0x02, 0x50, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0,
    ];
    assert_eq!(
        decode_request(&duplicate_destination),
        Err(DecodeError::DuplicateField(
            RequiredField::SubmitDestination
        ))
    );

    let wrong_destination_width = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x01, 0x03, 0xa3,
        0x00, 0x4f, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0x40, 0x02, 0x50, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    assert_eq!(
        decode_request(&wrong_destination_width),
        Err(DecodeError::InvalidByteStringLength {
            field: RequiredField::SubmitDestination,
            expected: 16,
            actual: 15,
        })
    );
}

#[cfg(feature = "nomad")]
fn nomad_fetch_id() -> NomadFetchId {
    NomadFetchId::new(
        [0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7],
        0x0102_0304_0506_0708,
    )
    .unwrap()
}

#[cfg(feature = "nomad")]
fn nomad_fetch_start_wire(encoded_path: &[u8], encoded_timestamp: &[u8]) -> Vec<u8> {
    let mut wire = vec![
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x08, 0x03,
        0xa4, 0x00, 0x50, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b,
        0x0c, 0x0d, 0x0e, 0x0f, 0x01,
    ];
    wire.extend_from_slice(encoded_path);
    wire.push(0x02);
    wire.extend_from_slice(encoded_timestamp);
    wire.extend_from_slice(&[
        0x03, 0x50, 0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc,
        0xfd, 0xfe, 0xff,
    ]);
    wire
}

#[cfg(feature = "nomad")]
fn nomad_fetch_start_request() -> RequestEnvelope<'static> {
    RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        request: DeviceRequest::NomadFetchStart(NomadFetchStartRequest::new(
            DestinationHash([
                0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
                0x0e, 0x0f,
            ]),
            NomadPagePath::new("/page").unwrap(),
            NomadRequestTimestampUnixMs::new(1).unwrap(),
            IdempotencyKey([
                0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc, 0xfd,
                0xfe, 0xff,
            ]),
        )),
    }
}

#[cfg(feature = "nomad")]
#[test]
fn nomad_value_types_enforce_exact_logical_bounds() {
    assert_eq!(NomadPagePath::new("/").unwrap().as_str(), "/");
    assert!(NomadPagePath::new("").is_err());
    assert!(NomadPagePath::new("relative").is_err());
    assert!(NomadPagePath::new("/nul\0").is_err());

    let maximum_path = format!("/{}", "x".repeat(MAX_NOMAD_PAGE_PATH_BYTES - 1));
    assert_eq!(
        NomadPagePath::new(&maximum_path).unwrap().len(),
        MAX_NOMAD_PAGE_PATH_BYTES
    );
    let oversized_path = format!("/{}", "x".repeat(MAX_NOMAD_PAGE_PATH_BYTES));
    assert_eq!(
        NomadPagePath::new(&oversized_path).unwrap_err().maximum(),
        MAX_NOMAD_PAGE_PATH_BYTES
    );

    assert!(NomadRequestTimestampUnixMs::new(0).is_err());
    assert_eq!(
        NomadRequestTimestampUnixMs::new(MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS)
            .unwrap()
            .get(),
        MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS
    );
    assert_eq!(
        NomadRequestTimestampUnixMs::new(MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS + 1)
            .unwrap_err()
            .actual(),
        MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS + 1
    );

    let id = nomad_fetch_id();
    assert_eq!(
        id.incarnation(),
        [0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7]
    );
    assert_eq!(id.sequence(), 0x0102_0304_0506_0708);
    assert_eq!(NomadFetchId::from_bytes(*id.as_bytes()).unwrap(), id);
    assert!(NomadFetchId::new([0x55; 8], 0).is_err());
    assert!(NomadFetchId::from_bytes([0x55; 16]).is_ok());
    assert!(NomadFetchId::from_bytes([0; 16]).is_err());

    assert_eq!(NomadPage::new(b"").unwrap().as_str(), "");
    assert_eq!(NomadPage::new(b"hello").unwrap().as_str(), "hello");
    assert!(NomadPage::new(&[0xff]).is_err());
    assert!(NomadPage::new(&[b'x'; MAX_NOMAD_PAGE_BYTES + 1]).is_err());
}

#[cfg(feature = "nomad")]
#[test]
fn exact_nomad_fetch_start_request_golden_and_authorization() {
    const GOLDEN: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x08, 0x03,
        0xa4, 0x00, 0x50, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b,
        0x0c, 0x0d, 0x0e, 0x0f, 0x01, 0x65, 0x2f, 0x70, 0x61, 0x67, 0x65, 0x02, 0x01, 0x03, 0x50,
        0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc, 0xfd, 0xfe,
        0xff,
    ];
    let expected = nomad_fetch_start_request();
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_request(&expected, &mut output).unwrap();
    assert_eq!(&output[..written], GOLDEN);
    assert_eq!(decode_request(GOLDEN).unwrap(), expected);
    assert_eq!(expected.request.operation(), OP_NOMAD_FETCH_START);
    assert!(expected.request.is_mutating());
    assert_eq!(
        authorize_request(&DispatchContext::UNAUTHENTICATED, &expected.request),
        Err(AuthorizationError::AuthenticationRequired)
    );
    assert_eq!(
        authorize_request(
            &DispatchContext::authenticated(
                PrincipalId([0x31; 16]),
                Permissions::NONE,
                dispatch_provenance(),
            ),
            &expected.request,
        ),
        Ok(())
    );
}

#[cfg(feature = "nomad")]
#[test]
fn exact_nomad_fetch_start_response_distinguishes_fresh_and_replayed() {
    const ACCEPTED: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x08, 0x03,
        0xa2, 0x00, 0x50, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0x01, 0x02, 0x03, 0x04,
        0x05, 0x06, 0x07, 0x08, 0x01, 0x00,
    ];
    const REPLAYED: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x08, 0x03,
        0xa2, 0x00, 0x50, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0x01, 0x02, 0x03, 0x04,
        0x05, 0x06, 0x07, 0x08, 0x01, 0x01,
    ];
    for (outcome, golden) in [
        (NomadFetchStartOutcome::Accepted, ACCEPTED),
        (NomadFetchStartOutcome::Replayed, REPLAYED),
    ] {
        let expected = ResponseEnvelope {
            version: ApiVersion::CURRENT,
            request_id: RequestId(42),
            response: DeviceResponse::NomadFetchStartAccepted(NomadFetchStartAccepted {
                id: nomad_fetch_id(),
                outcome,
            }),
        };
        let mut output = [0_u8; MAX_MESSAGE_BYTES];
        let written = encode_response(&expected, &mut output).unwrap();
        assert_eq!(&output[..written], golden);
        assert_eq!(decode_response(golden).unwrap(), expected);
    }
}

#[cfg(feature = "nomad")]
#[test]
fn exact_nomad_fetch_poll_request_and_states_round_trip() {
    const REQUEST: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x09, 0x03,
        0xa1, 0x00, 0x50, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0x01, 0x02, 0x03, 0x04,
        0x05, 0x06, 0x07, 0x08,
    ];
    const PENDING: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x09, 0x03,
        0xa2, 0x00, 0x00, 0x01, 0x04,
    ];
    const READY: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x09, 0x03,
        0xa2, 0x00, 0x01, 0x01, 0x45, 0x68, 0x65, 0x6c, 0x6c, 0x6f,
    ];
    const FAILED: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x09, 0x03,
        0xa2, 0x00, 0x02, 0x01, 0x03,
    ];
    let request = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        request: DeviceRequest::NomadFetchPoll(NomadFetchPollRequest {
            id: nomad_fetch_id(),
        }),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_request(&request, &mut output).unwrap();
    assert_eq!(&output[..written], REQUEST);
    assert_eq!(decode_request(REQUEST).unwrap(), request);
    assert_eq!(request.request.operation(), OP_NOMAD_FETCH_POLL);
    assert!(!request.request.is_mutating());
    assert_eq!(
        authorize_request(&DispatchContext::UNAUTHENTICATED, &request.request),
        Err(AuthorizationError::AuthenticationRequired)
    );

    for (response, golden) in [
        (
            NomadFetchPollResponse::Pending(NomadFetchPhase::AwaitingResponse),
            PENDING,
        ),
        (
            NomadFetchPollResponse::Ready(NomadPage::new(b"hello").unwrap()),
            READY,
        ),
        (
            NomadFetchPollResponse::Failed(NomadFetchFailure::Timeout),
            FAILED,
        ),
    ] {
        let expected = ResponseEnvelope {
            version: ApiVersion::CURRENT,
            request_id: RequestId(42),
            response: DeviceResponse::NomadFetchPoll(response),
        };
        let written = encode_response(&expected, &mut output).unwrap();
        assert_eq!(&output[..written], golden);
        assert_eq!(decode_response(golden).unwrap(), expected);
    }
}

#[cfg(feature = "nomad")]
#[test]
fn maximum_nomad_page_has_exact_bounded_body_and_message_sizes() {
    const ENVELOPE_PREFIX: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x1b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0x02, 0x19, 0xf0, 0x09, 0x03,
    ];
    let page = NomadPage::new(&[b'x'; MAX_NOMAD_PAGE_BYTES]).unwrap();
    let expected = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(u64::MAX),
        response: DeviceResponse::NomadFetchPoll(NomadFetchPollResponse::Ready(page)),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_response(&expected, &mut output).unwrap();
    assert_eq!(&output[..ENVELOPE_PREFIX.len()], ENVELOPE_PREFIX);
    let body_len = written - ENVELOPE_PREFIX.len();
    assert_eq!(body_len, 407);
    assert_eq!(written, 429);
    assert!(body_len <= MAX_BODY_BYTES);
    assert!(written <= MAX_MESSAGE_BYTES);
    assert_eq!(decode_response(&output[..written]).unwrap(), expected);
}

#[cfg(feature = "nomad")]
#[test]
fn nomad_fetch_decoder_rejects_zero_sequence_and_invalid_state_values() {
    let zero_sequence = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x09, 0x03,
        0xa1, 0x00, 0x50, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00,
    ];
    assert_eq!(
        decode_request(&zero_sequence),
        Err(DecodeError::InvalidNomadFetchId)
    );

    let invalid_state = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x09, 0x03,
        0xa2, 0x00, 0x03, 0x01, 0x00,
    ];
    assert_eq!(
        decode_response(&invalid_state),
        Err(DecodeError::InvalidValue {
            field: RequiredField::NomadFetchState,
            value: 3,
        })
    );
}

#[cfg(feature = "nomad")]
#[test]
fn nomad_fetch_decoder_rejects_unknown_closed_outcome_phase_and_failure_values() {
    let unknown_start_outcome = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x08, 0x03,
        0xa2, 0x00, 0x50, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0x01, 0x02, 0x03, 0x04,
        0x05, 0x06, 0x07, 0x08, 0x01, 0x02,
    ];
    assert_eq!(
        decode_response(&unknown_start_outcome),
        Err(DecodeError::InvalidValue {
            field: RequiredField::NomadFetchStartOutcome,
            value: 2,
        })
    );

    let unknown_pending_phase = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x09, 0x03,
        0xa2, 0x00, 0x00, 0x01, 0x05,
    ];
    assert_eq!(
        decode_response(&unknown_pending_phase),
        Err(DecodeError::InvalidValue {
            field: RequiredField::NomadFetchPhase,
            value: 5,
        })
    );

    let unknown_terminal_failure = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x09, 0x03,
        0xa2, 0x00, 0x02, 0x01, 0x07,
    ];
    assert_eq!(
        decode_response(&unknown_terminal_failure),
        Err(DecodeError::InvalidValue {
            field: RequiredField::NomadFetchFailure,
            value: 7,
        })
    );
}

#[cfg(feature = "nomad")]
#[test]
fn nomad_fetch_start_decoder_enforces_path_and_timestamp_semantics() {
    let relative_path = nomad_fetch_start_wire(&[0x64, b'p', b'a', b'g', b'e'], &[0x01]);
    assert_eq!(
        decode_request(&relative_path),
        Err(DecodeError::InvalidNomadPagePath)
    );

    let nul_path = nomad_fetch_start_wire(&[0x62, b'/', 0x00], &[0x01]);
    assert_eq!(
        decode_request(&nul_path),
        Err(DecodeError::InvalidNomadPagePath)
    );

    let invalid_utf8_path = nomad_fetch_start_wire(&[0x61, 0xff], &[0x01]);
    assert_eq!(
        decode_request(&invalid_utf8_path),
        Err(DecodeError::Malformed)
    );

    let mut oversized_path = vec![0x78, 0x81, b'/'];
    oversized_path.extend(core::iter::repeat_n(b'x', MAX_NOMAD_PAGE_PATH_BYTES));
    assert_eq!(
        decode_request(&nomad_fetch_start_wire(&oversized_path, &[0x01])),
        Err(DecodeError::NomadPagePathTooLarge {
            actual: MAX_NOMAD_PAGE_PATH_BYTES + 1,
            max: MAX_NOMAD_PAGE_PATH_BYTES,
        })
    );

    assert_eq!(
        decode_request(&nomad_fetch_start_wire(&[0x61, b'/'], &[0x00],)),
        Err(DecodeError::InvalidValue {
            field: RequiredField::NomadFetchTimestampUnixMs,
            value: 0,
        })
    );
    let timestamp_above_limit = MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS + 1;
    let mut encoded_timestamp_above_limit = vec![0x1b];
    encoded_timestamp_above_limit.extend_from_slice(&timestamp_above_limit.to_be_bytes());
    assert_eq!(
        decode_request(&nomad_fetch_start_wire(
            &[0x61, b'/'],
            &encoded_timestamp_above_limit,
        )),
        Err(DecodeError::InvalidValue {
            field: RequiredField::NomadFetchTimestampUnixMs,
            value: timestamp_above_limit,
        })
    );
}

#[cfg(feature = "nomad")]
#[test]
fn nomad_ready_page_decoder_rejects_invalid_utf8_and_oversize_values() {
    let invalid_utf8 = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x09, 0x03,
        0xa2, 0x00, 0x01, 0x01, 0x41, 0xff,
    ];
    assert_eq!(
        decode_response(&invalid_utf8),
        Err(DecodeError::InvalidNomadPageUtf8)
    );

    let mut oversized_page = vec![
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x09, 0x03,
        0xa2, 0x00, 0x01, 0x01, 0x59, 0x01, 0x91,
    ];
    oversized_page.extend(core::iter::repeat_n(b'x', MAX_NOMAD_PAGE_BYTES + 1));
    assert_eq!(
        decode_response(&oversized_page),
        Err(DecodeError::NomadPageTooLarge {
            actual: MAX_NOMAD_PAGE_BYTES + 1,
            max: MAX_NOMAD_PAGE_BYTES,
        })
    );
}

#[cfg(feature = "network-config")]
fn network_upsert_request() -> RequestEnvelope<'static> {
    let wifi = WifiNetworkUpdate::new(
        true,
        10,
        reticulum_device_api::WifiSsid::new(b"mesh").unwrap(),
        WifiCredentialUpdate::replace(b"password").unwrap(),
    );
    RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(9),
        request: DeviceRequest::NetworkConfigMutate(NetworkConfigMutationRequest::new(
            NetworkConfigMutation::UpsertWifi {
                profile_id: WifiNetworkProfileId::new([0x22; 16]).unwrap(),
                network: wifi,
            },
            7,
            IdempotencyKey([0xa5; 16]),
        )),
    }
}

#[cfg(feature = "network-config")]
#[test]
fn network_wifi_upsert_is_borrowed_redacted_bounded_and_authorized() {
    let request = network_upsert_request();
    let expected = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x0b, 0x03, 0xa4,
        0x00, 0x00, 0x01, 0xa2, 0x00, 0x50, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22,
        0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x01, 0xa4, 0x00, 0xf5, 0x01, 0x44, b'm', b'e',
        b's', b'h', 0x02, 0xa2, 0x00, 0x01, 0x01, 0x48, b'p', b'a', b's', b's', b'w', b'o', b'r',
        b'd', 0x03, 0x0a, 0x02, 0x07, 0x03, 0x50, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5,
        0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5,
    ];
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_request(&request, &mut output).unwrap();
    assert_eq!(&output[..written], &expected);
    assert_eq!(decode_request(&output[..written]).unwrap(), request);
    assert!(written <= MAX_MESSAGE_BYTES);

    let debug = format!("{request:?}");
    assert!(!debug.contains("password"));
    assert!(debug.contains("<redacted>"));

    assert!(request.request.is_mutating());
    assert_eq!(
        authorize_request(&DispatchContext::UNAUTHENTICATED, &request.request),
        Err(AuthorizationError::AuthenticationRequired)
    );
    let principal = PrincipalId([0x11; 16]);
    assert_eq!(
        authorize_request(
            &DispatchContext::authenticated(principal, Permissions::NONE, dispatch_provenance()),
            &request.request,
        ),
        Err(AuthorizationError::PermissionDenied(
            RequiredPermission::ManageNetworkConfig
        ))
    );
    assert_eq!(
        authorize_request(
            &DispatchContext::authenticated(
                principal,
                Permissions::MANAGE_NETWORK_CONFIG,
                dispatch_provenance(),
            ),
            &request.request,
        ),
        Ok(())
    );
    assert_eq!(Permissions::MANAGE_NETWORK_CONFIG.bits(), 1 << 2);
    assert!(Permissions::from_bits(0b111).is_ok());
    assert_eq!(
        Permissions::from_bits(0b1000).unwrap_err().unknown(),
        0b1000
    );
}

#[cfg(feature = "network-config")]
#[test]
fn network_config_read_owns_four_redacted_profiles_and_one_tcp_peer() {
    let profiles = [
        Some(
            WifiNetworkConfigSummary::new(
                WifiNetworkProfileId::new([1; 16]).unwrap(),
                true,
                0,
                b"first",
                true,
            )
            .unwrap(),
        ),
        Some(
            WifiNetworkConfigSummary::new(
                WifiNetworkProfileId::new([2; 16]).unwrap(),
                false,
                10,
                b"\xffopaque",
                true,
            )
            .unwrap(),
        ),
        Some(
            WifiNetworkConfigSummary::new(
                WifiNetworkProfileId::new([3; 16]).unwrap(),
                true,
                20,
                &[b'x'; MAX_WIFI_SSID_BYTES],
                true,
            )
            .unwrap(),
        ),
        Some(
            WifiNetworkConfigSummary::new(
                WifiNetworkProfileId::new([4; 16]).unwrap(),
                true,
                u8::MAX,
                b"fourth",
                true,
            )
            .unwrap(),
        ),
    ];
    let tcp_peer = ReticulumTcpPeerConfigSummary::new(
        true,
        ReticulumTcpPeerIpv4Address::new([192, 0, 2, 1]).unwrap(),
        DEFAULT_RETICULUM_TCP_PORT,
    )
    .unwrap();
    let expected = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(10),
        response: DeviceResponse::NetworkConfig(
            NetworkConfigSnapshot::with_defaults(12, profiles, Some(tcp_peer)).unwrap(),
        ),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_response(&expected, &mut output).unwrap();
    assert!(written <= MAX_MESSAGE_BYTES);
    assert_eq!(decode_response(&output[..written]).unwrap(), expected);
    assert!(
        !output[..written]
            .windows(b"password".len())
            .any(|window| window == b"password")
    );

    let DeviceResponse::NetworkConfig(decoded) =
        decode_response(&output[..written]).unwrap().response
    else {
        panic!("unexpected response");
    };
    assert_eq!(
        decoded
            .wifi_profile(WifiNetworkProfileId::new([4; 16]).unwrap())
            .unwrap()
            .ssid()
            .as_bytes(),
        b"fourth"
    );
    assert_eq!(
        decoded.tcp_peer().unwrap().ipv4_address().octets(),
        [192, 0, 2, 1]
    );

    let mut missing_lora_profile = output[..written].to_vec();
    let body_header = missing_lora_profile
        .windows(3)
        .position(|window| window == [0x03, 0xab, 0x00])
        .expect("network configuration body map")
        + 1;
    let profile_field = missing_lora_profile
        .windows(2)
        .rposition(|window| window == [0x09, 0xa5])
        .expect("complete LoRa profile field");
    missing_lora_profile[body_header] = 0xa9;
    missing_lora_profile.truncate(profile_field);
    assert_eq!(
        decode_response(&missing_lora_profile),
        Err(DecodeError::MissingField(
            RequiredField::NetworkConfigLoraProfile
        ))
    );

    let read = DeviceRequest::NetworkConfigGet;
    assert!(!read.is_mutating());
    assert_eq!(
        authorize_request(&DispatchContext::UNAUTHENTICATED, &read),
        Err(AuthorizationError::AuthenticationRequired)
    );
    assert_eq!(
        authorize_request(
            &DispatchContext::authenticated(
                PrincipalId([0x44; 16]),
                Permissions::NONE,
                dispatch_provenance(),
            ),
            &read,
        ),
        Ok(())
    );
}

#[cfg(feature = "network-config")]
#[test]
fn network_mutations_and_live_status_round_trip() {
    let principal = PrincipalId([0x33; 16]);
    let peer = ReticulumTcpPeerUpdate::new(
        true,
        ReticulumTcpPeerIpv4Address::new([192, 0, 2, 1]).unwrap(),
        DEFAULT_RETICULUM_TCP_PORT,
    )
    .unwrap();
    let host_peer = ReticulumTcpPeerHostUpdate::new(
        true,
        ReticulumTcpPeerHostname::new("rmap.world").unwrap(),
        DEFAULT_RETICULUM_TCP_PORT,
    )
    .unwrap();
    let location = RmapLocation::new(42_360_100, -71_058_900).unwrap();
    for mutation in [
        NetworkConfigMutation::RemoveWifi {
            profile_id: WifiNetworkProfileId::new([3; 16]).unwrap(),
        },
        NetworkConfigMutation::ReplaceTcpPeer(Some(peer)),
        NetworkConfigMutation::ReplaceTcpPeer(None),
        NetworkConfigMutation::ReplaceTcpHostPeer(Some(host_peer)),
        NetworkConfigMutation::ReplaceTcpHostPeer(None),
        NetworkConfigMutation::SetGatewayPolicy(GatewayPolicy::new(false, true)),
        NetworkConfigMutation::SetRmapConfig(RmapConfig::new(true, true, Some(location))),
        NetworkConfigMutation::SetLoraTxPower(LoraTransmitPowerDbm::DBM_22),
    ] {
        let expected = RequestEnvelope {
            version: ApiVersion::CURRENT,
            request_id: RequestId(11),
            request: DeviceRequest::NetworkConfigMutate(NetworkConfigMutationRequest::new(
                mutation,
                12,
                IdempotencyKey([0x5a; 16]),
            )),
        };
        let mut output = [0_u8; MAX_MESSAGE_BYTES];
        let written = encode_request(&expected, &mut output).unwrap();
        assert_eq!(decode_request(&output[..written]).unwrap(), expected);
        assert_eq!(
            authorize_request(
                &DispatchContext::authenticated(
                    principal,
                    Permissions::MANAGE_NETWORK_CONFIG,
                    dispatch_provenance(),
                ),
                &expected.request,
            ),
            Ok(())
        );
    }

    let applied = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(11),
        response: DeviceResponse::NetworkConfigMutation(NetworkConfigMutationOutcome::Applied {
            revision: 13,
            reboot_required: true,
        }),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_response(&applied, &mut output).unwrap();
    assert_eq!(decode_response(&output[..written]).unwrap(), applied);

    let conflict = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(11),
        response: DeviceResponse::NetworkConfigMutation(
            NetworkConfigMutationOutcome::RevisionConflict {
                current_revision: 14,
            },
        ),
    };
    let written = encode_response(&conflict, &mut output).unwrap();
    assert_eq!(decode_response(&output[..written]).unwrap(), conflict);

    let status = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(12),
        response: DeviceResponse::NetworkStatus(
            NetworkRuntimeStatus::new(
                13,
                12,
                WifiStationState::Connected,
                Some(WifiNetworkProfileId::new([3; 16]).unwrap()),
                Some(b"mesh"),
                Some([192, 0, 2, 42]),
                Some(-61),
                ReticulumTcpPeerState::Connected,
            )
            .unwrap(),
        ),
    };
    let written = encode_response(&status, &mut output).unwrap();
    assert_eq!(decode_response(&output[..written]).unwrap(), status);
    let faulted_status = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(13),
        response: DeviceResponse::NetworkStatus(
            NetworkRuntimeStatus::new(
                13,
                13,
                WifiStationState::Connected,
                Some(WifiNetworkProfileId::new([3; 16]).unwrap()),
                Some(b"mesh"),
                Some([192, 0, 2, 42]),
                Some(-61),
                ReticulumTcpPeerState::Faulted,
            )
            .unwrap(),
        ),
    };
    let written = encode_response(&faulted_status, &mut output).unwrap();
    assert_eq!(decode_response(&output[..written]).unwrap(), faulted_status);
    assert_eq!(
        authorize_request(
            &DispatchContext::authenticated(principal, Permissions::NONE, dispatch_provenance(),),
            &DeviceRequest::NetworkStatus,
        ),
        Ok(())
    );
}

#[cfg(feature = "network-config")]
#[test]
fn network_status_tcp_failures_have_exact_wire_codes() {
    let mut output = [0_u8; MAX_MESSAGE_BYTES];

    let failures = [
        (ReticulumTcpFailure::DnsTimeout, 0),
        (ReticulumTcpFailure::DnsLookupFailed, 1),
        (ReticulumTcpFailure::DnsNoIpv4Result, 2),
        (ReticulumTcpFailure::ConnectInvalidState, 3),
        (ReticulumTcpFailure::ConnectReset, 4),
        (ReticulumTcpFailure::ConnectTimeout, 5),
        (ReticulumTcpFailure::ConnectNoRoute, 6),
        (ReticulumTcpFailure::SocketClosed, 7),
        (ReticulumTcpFailure::TransmitFailed, 8),
    ];
    for (failure, wire_code) in failures {
        let expected = ResponseEnvelope {
            version: ApiVersion::CURRENT,
            request_id: RequestId(1),
            response: DeviceResponse::NetworkStatus(
                NetworkRuntimeStatus::new_with_tcp_failure(
                    1,
                    1,
                    WifiStationState::Disconnected,
                    None,
                    None,
                    None,
                    None,
                    ReticulumTcpPeerState::Backoff,
                    Some(failure),
                )
                .unwrap(),
            ),
        };
        let mut exact = [
            0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x0c, 0x03,
            0xa9, 0x00, 0x01, 0x01, 0x01, 0x02, 0x01, 0x03, 0xf6, 0x04, 0xf6, 0x05, 0xf6, 0x06,
            0xf6, 0x07, 0x05, 0x08, 0x00,
        ];
        *exact.last_mut().unwrap() = wire_code;

        let written = encode_response(&expected, &mut output).unwrap();
        assert_eq!(&output[..written], &exact);
        assert_eq!(decode_response(&exact).unwrap(), expected);
        assert_eq!(failure.wire_code(), wire_code);
    }
}

#[cfg(feature = "network-config")]
#[test]
fn network_status_roundtrips_compact_rmap_publication_state() {
    let rmap = RmapRuntimeStatus::new(
        true,
        RmapStampPhase::Ready,
        1_234,
        RmapInitialTcpGateState::Open,
        3,
        RmapQueueOutcome::Accepted,
        Some(42),
        RmapEgressConfirmation::NotObserved,
        Some(3_600),
        None,
    );
    let expected = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(14),
        response: DeviceResponse::NetworkStatus(
            NetworkRuntimeStatus::new(
                13,
                13,
                WifiStationState::Connected,
                None,
                None,
                None,
                None,
                ReticulumTcpPeerState::Connected,
            )
            .unwrap()
            .with_rmap_status(rmap),
        ),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_response(&expected, &mut output).unwrap();
    assert_eq!(decode_response(&output[..written]).unwrap(), expected);
    assert!(written < MAX_BODY_BYTES);

    assert_eq!(RmapStampPhase::Ready.wire_code(), 2);
    assert_eq!(RmapInitialTcpGateState::Open.wire_code(), 2);
    assert_eq!(RmapQueueOutcome::Accepted.wire_code(), 1);
    assert_eq!(RmapEgressConfirmation::NotObserved.wire_code(), 1);
    assert_eq!(RmapDeferredReason::OrdinaryQueueRejected.wire_code(), 9);
}

#[cfg(feature = "network-config")]
#[test]
fn network_status_rejects_duplicate_unknown_and_malformed_tcp_diagnostics() {
    const DUPLICATE_FAILURE: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x0c, 0x03, 0xaa,
        0x00, 0x01, 0x01, 0x01, 0x02, 0x01, 0x03, 0xf6, 0x04, 0xf6, 0x05, 0xf6, 0x06, 0xf6, 0x07,
        0x05, 0x08, 0x00, 0x08, 0x01,
    ];
    assert_eq!(
        decode_response(DUPLICATE_FAILURE),
        Err(DecodeError::DuplicateField(
            RequiredField::NetworkLastTcpFailure
        ))
    );

    let mut unknown_failure = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x0c, 0x03, 0xa9,
        0x00, 0x01, 0x01, 0x01, 0x02, 0x01, 0x03, 0xf6, 0x04, 0xf6, 0x05, 0xf6, 0x06, 0xf6, 0x07,
        0x05, 0x08, 0x09,
    ];
    assert_eq!(
        decode_response(&unknown_failure),
        Err(DecodeError::InvalidValue {
            field: RequiredField::NetworkLastTcpFailure,
            value: 9,
        })
    );

    *unknown_failure.last_mut().unwrap() = 0xf5;
    assert_eq!(
        decode_response(&unknown_failure),
        Err(DecodeError::Malformed)
    );

    let mut unknown_state = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x0c, 0x03, 0xa8,
        0x00, 0x01, 0x01, 0x01, 0x02, 0x01, 0x03, 0xf6, 0x04, 0xf6, 0x05, 0xf6, 0x06, 0xf6, 0x07,
        0x06,
    ];
    assert_eq!(
        decode_response(&unknown_state),
        Err(DecodeError::InvalidValue {
            field: RequiredField::NetworkTcpPeerState,
            value: 6,
        })
    );

    *unknown_state.last_mut().unwrap() = ReticulumTcpPeerState::Backoff.wire_code();
    let decoded = decode_response(&unknown_state).unwrap();
    let DeviceResponse::NetworkStatus(status) = decoded.response else {
        panic!("unexpected response kind");
    };
    assert_eq!(status.tcp_peer_state, ReticulumTcpPeerState::Backoff);
    assert_eq!(status.last_tcp_failure, None);
}

#[cfg(feature = "network-config")]
fn dns_diagnostics_fixture() -> ReticulumDnsDiagnostics {
    let response_code =
        ReticulumDnsRawOutcome::response_code_outcome(3).expect("nonzero DNS response code");
    ReticulumDnsDiagnostics::new(
        Some([192, 168, 50, 1]),
        [Some([192, 168, 50, 1]), Some([8, 8, 8, 8]), None],
        ReticulumDnsPrimaryOutcome::LookupFailed,
        ReticulumDnsRawSetupState::Ready,
        [
            Some(ReticulumDnsRawAttempt::new(
                ReticulumDnsRawSource::Dhcp,
                [192, 168, 50, 1],
                ReticulumDnsRawOutcome::AwaitingResponse,
            )),
            Some(ReticulumDnsRawAttempt::new(
                ReticulumDnsRawSource::Dhcp,
                [192, 168, 50, 2],
                ReticulumDnsRawOutcome::Timeout,
            )),
            Some(ReticulumDnsRawAttempt::new(
                ReticulumDnsRawSource::Public,
                [1, 1, 1, 1],
                response_code,
            )),
            Some(ReticulumDnsRawAttempt::new(
                ReticulumDnsRawSource::Public,
                [9, 9, 9, 9],
                ReticulumDnsRawOutcome::Resolved,
            )),
            None,
        ],
        Some(ReticulumDnsResolution::new(
            [217, 154, 9, 220],
            ReticulumDnsResolutionSource::RawPublic,
            Some([9, 9, 9, 9]),
        )),
    )
}

#[cfg(feature = "network-config")]
fn dns_diagnostic_status() -> ResponseEnvelope {
    ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(1),
        response: DeviceResponse::NetworkStatus(
            NetworkRuntimeStatus::new_with_tcp_diagnostics(
                1,
                1,
                WifiStationState::Connected,
                None,
                None,
                Some([192, 168, 50, 99]),
                Some(-60),
                ReticulumTcpPeerState::Backoff,
                Some(ReticulumTcpFailure::DnsLookupFailed),
                Some(dns_diagnostics_fixture()),
            )
            .unwrap(),
        ),
    }
}

#[cfg(feature = "network-config")]
#[test]
fn network_status_dns_diagnostics_have_exact_bounded_wire_shape() {
    const EXACT: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x0c, 0x03, 0xaa,
        0x00, 0x01, 0x01, 0x01, 0x02, 0x03, 0x03, 0xf6, 0x04, 0xf6, 0x05, 0x44, 0xc0, 0xa8, 0x32,
        0x63, 0x06, 0x38, 0x3b, 0x07, 0x05, 0x08, 0x01, 0x09, 0xa6, 0x00, 0x44, 0xc0, 0xa8, 0x32,
        0x01, 0x01, 0x83, 0x44, 0xc0, 0xa8, 0x32, 0x01, 0x44, 0x08, 0x08, 0x08, 0x08, 0xf6, 0x02,
        0x05, 0x03, 0x02, 0x04, 0x85, 0xa3, 0x00, 0x00, 0x01, 0x44, 0xc0, 0xa8, 0x32, 0x01, 0x02,
        0x04, 0xa3, 0x00, 0x00, 0x01, 0x44, 0xc0, 0xa8, 0x32, 0x02, 0x02, 0x07, 0xa4, 0x00, 0x01,
        0x01, 0x44, 0x01, 0x01, 0x01, 0x01, 0x02, 0x0a, 0x03, 0x03, 0xa3, 0x00, 0x01, 0x01, 0x44,
        0x09, 0x09, 0x09, 0x09, 0x02, 0x05, 0xf6, 0x05, 0xa3, 0x00, 0x44, 0xd9, 0x9a, 0x09, 0xdc,
        0x01, 0x02, 0x02, 0x44, 0x09, 0x09, 0x09, 0x09,
    ];
    let expected = dns_diagnostic_status();
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_response(&expected, &mut output).unwrap();
    assert_eq!(&output[..written], EXACT);
    assert_eq!(decode_response(EXACT).unwrap(), expected);
    assert!(written < MAX_BODY_BYTES);

    let DeviceResponse::NetworkStatus(status) = expected.response else {
        panic!("unexpected response kind");
    };
    let diagnostics = status.dns_diagnostics.unwrap();
    assert_eq!(
        diagnostics.dhcp_servers.len(),
        MAX_RETICULUM_DNS_DHCP_SERVERS
    );
    assert_eq!(
        diagnostics.raw_attempts.len(),
        MAX_RETICULUM_DNS_RAW_ATTEMPTS
    );
}

#[cfg(feature = "network-config")]
#[test]
fn network_status_dns_diagnostics_reject_bad_slot_and_response_code_shapes() {
    let mut encoded = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_response(&dns_diagnostic_status(), &mut encoded).unwrap();
    let exact = &encoded[..written];

    let mut wrong_dhcp_slots = exact.to_vec();
    let dhcp_array = wrong_dhcp_slots
        .windows(3)
        .position(|window| window == [0x01, 0x83, 0x44])
        .expect("diagnostic DHCP array");
    wrong_dhcp_slots[dhcp_array + 1] = 0x82;
    wrong_dhcp_slots.remove(dhcp_array + 12);
    assert_eq!(
        decode_response(&wrong_dhcp_slots),
        Err(DecodeError::InvalidArrayLength {
            field: RequiredField::ReticulumDnsDhcpServers,
            expected: MAX_RETICULUM_DNS_DHCP_SERVERS as u64,
            actual: 2,
        })
    );

    let response_code = exact
        .windows(4)
        .position(|window| window == [0x02, 0x0a, 0x03, 0x03])
        .expect("typed DNS response code");
    let mut zero_response_code = exact.to_vec();
    zero_response_code[response_code + 3] = 0;
    assert_eq!(
        decode_response(&zero_response_code),
        Err(DecodeError::InvalidReticulumDnsDiagnostics)
    );

    let mut contradictory_response_code = exact.to_vec();
    contradictory_response_code[response_code + 1] = ReticulumDnsRawOutcome::Timeout.wire_code();
    assert_eq!(
        decode_response(&contradictory_response_code),
        Err(DecodeError::InvalidReticulumDnsDiagnostics)
    );
}

#[cfg(feature = "network-config")]
#[test]
fn network_config_round_trips_gateway_rmap_and_hostname_peer() {
    let location = RmapLocation::new(42_360_100, -71_058_900).unwrap();
    let host_peer =
        ReticulumTcpPeerHostConfigSummary::new(true, "rmap.world", DEFAULT_RETICULUM_TCP_PORT)
            .unwrap();
    let snapshot = NetworkConfigSnapshot::new(
        9,
        [None; MAX_WIFI_NETWORK_PROFILES],
        None,
        Some(host_peer),
        GatewayPolicy::new(false, false),
        RmapConfig::new(true, true, Some(location)),
        LoraRadioProfile::DEFAULT,
        None,
    )
    .unwrap();
    let expected = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(80),
        response: DeviceResponse::NetworkConfig(snapshot),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_response(&expected, &mut output).unwrap();
    assert_eq!(decode_response(&output[..written]).unwrap(), expected);
    assert_eq!(snapshot.tcp_peer(), None);
    assert_eq!(
        snapshot.tcp_host_peer().unwrap().hostname().as_str(),
        "rmap.world"
    );
    assert!(!snapshot.wifi_transport_enabled());
    assert!(!snapshot.automatic_announces_enabled());
    assert!(snapshot.rmap_discovery_enabled());
    assert!(snapshot.rmap_share_location());
    assert_eq!(snapshot.rmap_phone_location(), Some(location));
    assert_eq!(snapshot.lora_tx_power_dbm(), LoraTransmitPowerDbm::DEFAULT);
}

#[cfg(feature = "network-config")]
#[test]
fn lora_transmit_power_is_validated_and_round_trips_in_the_atomic_profile() {
    for value in [14, 17, 20, 22] {
        let power = LoraTransmitPowerDbm::new(value).expect("qualified power");
        assert_eq!(power.get(), value);
    }
    for value in [0, 13, 15, 16, 18, 19, 21, 23, u8::MAX] {
        let error = LoraTransmitPowerDbm::new(value).expect_err("unqualified power");
        assert_eq!(error.actual(), value);
    }
    assert_eq!(
        LoraTransmitPowerDbm::default(),
        LoraTransmitPowerDbm::DBM_14
    );

    let snapshot = NetworkConfigSnapshot::new(
        17,
        [None; MAX_WIFI_NETWORK_PROFILES],
        None,
        None,
        GatewayPolicy::new(false, true),
        RmapConfig::new(false, false, None),
        LoraRadioProfile::DEFAULT.with_tx_power(LoraTransmitPowerDbm::DBM_20),
        None,
    )
    .unwrap();
    let expected = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(83),
        response: DeviceResponse::NetworkConfig(snapshot),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_response(&expected, &mut output).unwrap();
    assert_eq!(decode_response(&output[..written]).unwrap(), expected);
    assert_eq!(snapshot.lora_tx_power_dbm(), LoraTransmitPowerDbm::DBM_20);

    let power_field = output[..written]
        .windows(2)
        .rposition(|window| window == [0x04, 0x14])
        .expect("canonical profile power key and 20 dBm value");
    let mut invalid_power = output[..written].to_vec();
    invalid_power[power_field + 1] = 15;
    assert_eq!(
        decode_response(&invalid_power),
        Err(DecodeError::InvalidLoraTransmitPowerDbm)
    );

    assert!(
        NetworkConfigSnapshot::new(
            0,
            [None; MAX_WIFI_NETWORK_PROFILES],
            None,
            None,
            GatewayPolicy::new(true, true),
            RmapConfig::new(false, false, None),
            LoraRadioProfile::DEFAULT.with_tx_power(LoraTransmitPowerDbm::DBM_17),
            None,
        )
        .is_err()
    );
}

#[cfg(feature = "network-config")]
#[test]
fn lora_radio_profile_round_trips_atomically_and_uses_mutation_kind_seven() {
    let profile = LoraRadioProfile::new(914_875_000, 250_000, 9, 7, LoraTransmitPowerDbm::DBM_22)
        .expect("valid profile");
    let snapshot = NetworkConfigSnapshot::new(
        18,
        [None; MAX_WIFI_NETWORK_PROFILES],
        None,
        None,
        GatewayPolicy::new(true, true),
        RmapConfig::new(false, false, None),
        profile,
        None,
    )
    .expect("valid snapshot");
    let response = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(85),
        response: DeviceResponse::NetworkConfig(snapshot),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_response(&response, &mut output).unwrap();
    assert_eq!(decode_response(&output[..written]).unwrap(), response);
    assert_eq!(snapshot.lora_profile(), profile);
    assert_eq!(snapshot.lora_tx_power_dbm(), LoraTransmitPowerDbm::DBM_22);

    let request = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(86),
        request: DeviceRequest::NetworkConfigMutate(NetworkConfigMutationRequest::new(
            NetworkConfigMutation::SetLoraProfile(profile),
            18,
            IdempotencyKey([0x86; 16]),
        )),
    };
    let written = encode_request(&request, &mut output).unwrap();
    assert_eq!(decode_request(&output[..written]).unwrap(), request);
    assert!(
        output[..written]
            .windows(3)
            .any(|window| window == [0x00, 0x07, 0x01])
    );

    assert!(LoraRadioProfile::new(0, 125_000, 7, 5, LoraTransmitPowerDbm::DBM_14).is_err());
    assert!(
        LoraRadioProfile::new(915_000_000, 100_000, 7, 5, LoraTransmitPowerDbm::DBM_14,).is_err()
    );
}

#[cfg(feature = "network-config")]
#[test]
fn lora_transmit_power_mutation_uses_kind_six_and_rejects_unknown_power() {
    let request = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(84),
        request: DeviceRequest::NetworkConfigMutate(NetworkConfigMutationRequest::new(
            NetworkConfigMutation::SetLoraTxPower(LoraTransmitPowerDbm::DBM_22),
            7,
            IdempotencyKey([0x84; 16]),
        )),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_request(&request, &mut output).unwrap();
    assert_eq!(decode_request(&output[..written]).unwrap(), request);
    let mutation = output[..written]
        .windows(4)
        .position(|window| window == [0x00, 0x06, 0x01, 0x16])
        .expect("kind 6 and 22 dBm scalar value");
    let mut invalid = output[..written].to_vec();
    invalid[mutation + 3] = 21;
    assert_eq!(
        decode_request(&invalid),
        Err(DecodeError::InvalidLoraTransmitPowerDbm)
    );
}

#[cfg(feature = "network-config")]
#[test]
fn device_name_mutation_uses_kind_eight_and_round_trips_a_snapshot_name() {
    let snapshot = NetworkConfigSnapshot::new(
        21,
        [None; MAX_WIFI_NETWORK_PROFILES],
        None,
        None,
        GatewayPolicy::new(true, true),
        RmapConfig::new(false, false, None),
        LoraRadioProfile::DEFAULT,
        Some(DeviceNameSummary::new("Field node").unwrap()),
    )
    .unwrap();
    let response = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(88),
        response: DeviceResponse::NetworkConfig(snapshot),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_response(&response, &mut output).unwrap();
    assert_eq!(decode_response(&output[..written]).unwrap(), response);
    assert_eq!(snapshot.device_name().unwrap().as_str(), "Field node");

    let set = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(89),
        request: DeviceRequest::NetworkConfigMutate(NetworkConfigMutationRequest::new(
            NetworkConfigMutation::SetDeviceName(Some(DeviceName::new("Field node").unwrap())),
            21,
            IdempotencyKey([0x89; 16]),
        )),
    };
    let written = encode_request(&set, &mut output).unwrap();
    assert_eq!(decode_request(&output[..written]).unwrap(), set);
    assert!(
        output[..written]
            .windows(3)
            .any(|window| window == [0x00, 0x08, 0x01])
    );

    let clear = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(90),
        request: DeviceRequest::NetworkConfigMutate(NetworkConfigMutationRequest::new(
            NetworkConfigMutation::SetDeviceName(None),
            21,
            IdempotencyKey([0x90; 16]),
        )),
    };
    let written = encode_request(&clear, &mut output).unwrap();
    assert_eq!(decode_request(&output[..written]).unwrap(), clear);

    assert!(DeviceName::new("").is_err());
    assert!(DeviceName::new(&"x".repeat(MAX_DEVICE_NAME_BYTES + 1)).is_err());
    assert!(DeviceName::new("bad\nname").is_err());
}

#[cfg(feature = "network-config")]
#[test]
fn hostname_and_phone_location_are_strictly_bounded_on_model_and_wire() {
    assert_eq!(MAX_RETICULUM_TCP_PEER_HOSTNAME_BYTES, 96);
    assert!(ReticulumTcpPeerHostname::new("rmap.world").is_ok());
    for invalid in [
        "",
        "-rmap.world",
        "rmap-.world",
        "rmap..world",
        "rmap_world",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.example",
    ] {
        assert!(
            ReticulumTcpPeerHostname::new(invalid).is_err(),
            "{invalid:?} unexpectedly accepted"
        );
    }
    let maximum_hostname = format!("{}.{}", "a".repeat(63), "b".repeat(32));
    assert_eq!(
        maximum_hostname.len(),
        MAX_RETICULUM_TCP_PEER_HOSTNAME_BYTES
    );
    assert!(ReticulumTcpPeerHostname::new(&maximum_hostname).is_ok());
    let oversized_hostname = format!("{maximum_hostname}.c");
    assert!(ReticulumTcpPeerHostname::new(&oversized_hostname).is_err());
    assert!(RmapLocation::new(-90_000_000, -180_000_000).is_ok());
    assert!(RmapLocation::new(90_000_000, 180_000_000).is_ok());
    assert!(RmapLocation::new(90_000_001, 0).is_err());
    assert!(RmapLocation::new(0, -180_000_001).is_err());

    let ipv4 = ReticulumTcpPeerConfigSummary::new(
        true,
        ReticulumTcpPeerIpv4Address::new([192, 0, 2, 1]).unwrap(),
        DEFAULT_RETICULUM_TCP_PORT,
    )
    .unwrap();
    let hostname =
        ReticulumTcpPeerHostConfigSummary::new(true, "rmap.world", DEFAULT_RETICULUM_TCP_PORT)
            .unwrap();
    assert!(
        NetworkConfigSnapshot::new(
            1,
            [None; MAX_WIFI_NETWORK_PROFILES],
            Some(ipv4),
            Some(hostname),
            GatewayPolicy::new(true, true),
            RmapConfig::new(false, false, None),
            LoraRadioProfile::DEFAULT,
            None,
        )
        .is_err()
    );

    let host_request = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(81),
        request: DeviceRequest::NetworkConfigMutate(NetworkConfigMutationRequest::new(
            NetworkConfigMutation::ReplaceTcpHostPeer(Some(
                ReticulumTcpPeerHostUpdate::new(
                    true,
                    ReticulumTcpPeerHostname::new("rmap.world").unwrap(),
                    DEFAULT_RETICULUM_TCP_PORT,
                )
                .unwrap(),
            )),
            1,
            IdempotencyKey([0x81; 16]),
        )),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_request(&host_request, &mut output).unwrap();
    let mut invalid_host = output[..written].to_vec();
    let host_start = invalid_host
        .windows(b"rmap.world".len())
        .position(|window| window == b"rmap.world")
        .unwrap();
    invalid_host[host_start + 4] = b'_';
    assert_eq!(
        decode_request(&invalid_host),
        Err(DecodeError::InvalidReticulumTcpPeerHostname)
    );

    let location_request = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(82),
        request: DeviceRequest::NetworkConfigMutate(NetworkConfigMutationRequest::new(
            NetworkConfigMutation::SetRmapConfig(RmapConfig::new(
                true,
                true,
                Some(RmapLocation::new(90_000_000, 0).unwrap()),
            )),
            1,
            IdempotencyKey([0x82; 16]),
        )),
    };
    let written = encode_request(&location_request, &mut output).unwrap();
    let mut invalid_location = output[..written].to_vec();
    let latitude = 90_000_000_i32.to_be_bytes();
    let latitude_start = invalid_location
        .windows(latitude.len())
        .position(|window| window == latitude)
        .unwrap();
    invalid_location[latitude_start..latitude_start + latitude.len()]
        .copy_from_slice(&90_000_001_i32.to_be_bytes());
    assert_eq!(
        decode_request(&invalid_location),
        Err(DecodeError::InvalidRmapLocation)
    );

    let missing_gateway_field = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x0b, 0x03, 0xa4,
        0x00, 0x04, 0x01, 0xa1, 0x00, 0xf5, 0x02, 0x01, 0x03, 0x50, 0x11, 0x11, 0x11, 0x11, 0x11,
        0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
    ];
    assert_eq!(
        decode_request(&missing_gateway_field),
        Err(DecodeError::MissingField(
            RequiredField::GatewayPolicyAutomaticAnnouncesEnabled,
        ))
    );

    let missing_rmap_location_field = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x0b, 0x03, 0xa4,
        0x00, 0x05, 0x01, 0xa2, 0x00, 0xf5, 0x01, 0xf4, 0x02, 0x01, 0x03, 0x50, 0x22, 0x22, 0x22,
        0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22,
    ];
    assert_eq!(
        decode_request(&missing_rmap_location_field),
        Err(DecodeError::MissingField(RequiredField::RmapPhoneLocation))
    );
}

#[cfg(feature = "network-config")]
#[test]
fn network_value_bounds_reject_invalid_profiles_and_secrets() {
    assert_eq!(OP_NETWORK_CONFIG_GET, 0xf00a);
    assert_eq!(OP_NETWORK_CONFIG_MUTATE, 0xf00b);
    assert_eq!(OP_NETWORK_STATUS, 0xf00c);
    assert_eq!(MAX_WIFI_NETWORK_PROFILES, 4);
    assert_eq!(MAX_WIFI_SSID_BYTES, 32);
    assert_eq!(MIN_WIFI_PASSPHRASE_BYTES, 8);
    assert_eq!(MAX_WIFI_PASSPHRASE_BYTES, 63);
    assert!(WifiNetworkProfileId::new([1; 16]).is_ok());
    assert!(WifiNetworkProfileId::new([0; 16]).is_err());
    assert!(reticulum_device_api::WifiSsid::new(&[0xff; MAX_WIFI_SSID_BYTES]).is_ok());
    assert!(reticulum_device_api::WifiSsid::new(&[]).is_err());
    assert!(reticulum_device_api::WifiSsid::new(&[b'x'; MAX_WIFI_SSID_BYTES + 1]).is_err());
    assert!(WifiCredentialUpdate::replace(b"1234567").is_err());
    assert!(WifiCredentialUpdate::replace(&[b'x'; MAX_WIFI_PASSPHRASE_BYTES]).is_ok());
    assert!(WifiCredentialUpdate::replace(&[b'x'; MAX_WIFI_PASSPHRASE_BYTES + 1]).is_err());
    assert!(WifiCredentialUpdate::replace(b"valid\nbad").is_err());
    assert!(ReticulumTcpPeerIpv4Address::new([0, 1, 2, 3]).is_err());
    assert!(ReticulumTcpPeerIpv4Address::new([127, 0, 0, 1]).is_err());
    assert!(ReticulumTcpPeerIpv4Address::new([224, 0, 0, 1]).is_err());
    assert!(ReticulumTcpPeerIpv4Address::new([240, 0, 0, 1]).is_err());
    assert!(ReticulumTcpPeerIpv4Address::new([255, 255, 255, 255]).is_err());
    assert!(ReticulumTcpPeerIpv4Address::new([192, 0, 2, 1]).is_ok());

    let profile = WifiNetworkConfigSummary::new(
        WifiNetworkProfileId::new([1; 16]).unwrap(),
        true,
        0,
        b"mesh",
        true,
    )
    .unwrap();
    assert!(
        NetworkConfigSnapshot::with_defaults(0, [Some(profile), None, None, None], None).is_err()
    );
    assert!(
        NetworkConfigSnapshot::with_defaults(1, [None, Some(profile), None, None], None).is_err()
    );
    assert!(
        NetworkConfigSnapshot::with_defaults(1, [Some(profile), Some(profile), None, None], None,)
            .is_err()
    );

    let maximum = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(99),
        request: DeviceRequest::NetworkConfigMutate(NetworkConfigMutationRequest::new(
            NetworkConfigMutation::UpsertWifi {
                profile_id: WifiNetworkProfileId::new([0xff; 16]).unwrap(),
                network: WifiNetworkUpdate::new(
                    true,
                    u8::MAX,
                    reticulum_device_api::WifiSsid::new(&[0xff; MAX_WIFI_SSID_BYTES]).unwrap(),
                    WifiCredentialUpdate::replace(&[b'~'; MAX_WIFI_PASSPHRASE_BYTES]).unwrap(),
                ),
            },
            u64::MAX,
            IdempotencyKey([0xff; 16]),
        )),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_request(&maximum, &mut output).unwrap();
    assert!(written <= MAX_MESSAGE_BYTES);
    assert_eq!(decode_request(&output[..written]).unwrap(), maximum);
}

#[cfg(feature = "network-config")]
#[test]
fn network_wire_rejects_zero_ids_invalid_secrets_and_contradictory_results() {
    let request = network_upsert_request();
    let mut wire = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_request(&request, &mut wire).unwrap();

    let id_marker = [0x22_u8; 16];
    let id_start = wire[..written]
        .windows(id_marker.len())
        .position(|window| window == id_marker)
        .unwrap();
    let mut zero_id = wire[..written].to_vec();
    zero_id[id_start..id_start + id_marker.len()].fill(0);
    assert_eq!(
        decode_request(&zero_id),
        Err(DecodeError::InvalidWifiNetworkProfileId)
    );

    let secret_start = wire[..written]
        .windows(b"password".len())
        .position(|window| window == b"password")
        .unwrap();
    let mut invalid_secret = wire[..written].to_vec();
    invalid_secret[secret_start] = b'\n';
    assert_eq!(
        decode_request(&invalid_secret),
        Err(DecodeError::InvalidWifiPassphrase)
    );

    let contradictory_conflict = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x0b, 0x03, 0xa3,
        0x00, 0x01, 0x01, 0x0e, 0x02, 0xf5,
    ];
    assert_eq!(
        decode_response(&contradictory_conflict),
        Err(DecodeError::InvalidNetworkConfigMutationOutcome)
    );

    let too_many_profiles = [
        0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x0a, 0x03, 0xa3,
        0x00, 0x01, 0x01, 0x85, 0xa0, 0xa0, 0xa0, 0xa0, 0xa0, 0x02, 0xf6,
    ];
    assert_eq!(
        decode_response(&too_many_profiles),
        Err(DecodeError::TooManyWifiNetworkProfiles {
            actual: 5,
            max: MAX_WIFI_NETWORK_PROFILES as u64,
        })
    );
}

#[test]
fn operation_is_unavailable_without_feature() {
    #[cfg(not(feature = "rns-data"))]
    {
        let request = [
            0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x01, 0x03,
            0xa0,
        ];
        assert_eq!(
            decode_request(&request),
            Err(DecodeError::UnsupportedOperation(0xf001))
        );

        let response = [
            0xa4, 0x00, 0xa2, 0x00, 0x03, 0x01, 0x00, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x01, 0x03,
            0xa1, 0x00, 0x01,
        ];
        assert_eq!(
            decode_response(&response),
            Err(DecodeError::UnsupportedResponseKind(0xf001))
        );
    }

    #[cfg(not(feature = "lxmf"))]
    for operation in [0xf004_u16, 0xf005, 0xf006, 0xf007] {
        let encoded_operation = operation.to_be_bytes();
        let envelope = [
            0xa4,
            0x00,
            0xa2,
            0x00,
            0x03,
            0x01,
            0x00,
            0x01,
            0x09,
            0x02,
            0x19,
            encoded_operation[0],
            encoded_operation[1],
            0x03,
            0xa0,
        ];
        assert_eq!(
            decode_request(&envelope),
            Err(DecodeError::UnsupportedOperation(operation))
        );
        assert_eq!(
            decode_response(&envelope),
            Err(DecodeError::UnsupportedResponseKind(operation))
        );
    }

    #[cfg(not(feature = "nomad"))]
    for operation in [0xf008_u16, 0xf009] {
        let encoded_operation = operation.to_be_bytes();
        let envelope = [
            0xa4,
            0x00,
            0xa2,
            0x00,
            0x03,
            0x01,
            0x00,
            0x01,
            0x09,
            0x02,
            0x19,
            encoded_operation[0],
            encoded_operation[1],
            0x03,
            0xa0,
        ];
        assert_eq!(
            decode_request(&envelope),
            Err(DecodeError::UnsupportedOperation(operation))
        );
        assert_eq!(
            decode_response(&envelope),
            Err(DecodeError::UnsupportedResponseKind(operation))
        );
    }

    #[cfg(not(feature = "network-config"))]
    for operation in [0xf00a_u16, 0xf00b, 0xf00c] {
        let encoded_operation = operation.to_be_bytes();
        let envelope = [
            0xa4,
            0x00,
            0xa2,
            0x00,
            0x03,
            0x01,
            0x00,
            0x01,
            0x09,
            0x02,
            0x19,
            encoded_operation[0],
            encoded_operation[1],
            0x03,
            0xa0,
        ];
        assert_eq!(
            decode_request(&envelope),
            Err(DecodeError::UnsupportedOperation(operation))
        );
        assert_eq!(
            decode_response(&envelope),
            Err(DecodeError::UnsupportedResponseKind(operation))
        );
    }

    assert_eq!(OP_SUBMISSION_STATUS, 0x0002);
}
