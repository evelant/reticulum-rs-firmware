#[cfg(any(
    feature = "experimental-rns-data",
    feature = "experimental-lxmf",
    feature = "experimental-nomad"
))]
use reticulum_device_api::IdempotencyKey;
use reticulum_device_api::{
    API_VERSION_MAJOR, API_VERSION_MINOR, ApiErrorCode, ApiErrorResponse, ApiVersion,
    AuthorizationError, CapabilityAvailability, CapabilitySnapshot, DecodeError, DestinationHash,
    DeviceRequest, DeviceResponse, DispatchContext, DispatchProvenance, DispatchProvenanceError,
    EncodeError, EncodedPacketSha256, IdentitySummary, MAX_BODY_BYTES, MAX_CBOR_NESTING_DEPTH,
    MAX_LXMF_BASIC_CONTENT_BYTES, MAX_LXMF_BASIC_TITLE_BYTES, MAX_LXMF_PEER_APP_DATA_BYTES,
    MAX_MESSAGE_BYTES, MAX_NOMAD_PAGE_BYTES, MAX_NOMAD_PAGE_PATH_BYTES,
    MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES, OP_IDENTITY_SUMMARY, OP_SUBMISSION_STATUS, Permissions,
    PreparedPacketDetails, PrincipalId, RequestEnvelope, RequestId, RequiredField,
    RequiredPermission, ResponseEnvelope, SubmissionFailure, SubmissionId, SubmissionState,
    SubmissionStatus, authorize_request, decode_request, decode_response, encode_request,
    encode_response,
};
#[cfg(feature = "experimental-lxmf")]
use reticulum_device_api::{
    IdentityHash, LxmfBasicSendAccepted, LxmfDiscoveredPeer, LxmfMessageHandle, LxmfMessageSummary,
    LxmfPeerDiscoveryCursor, LxmfPeerDiscoveryIncarnation, LxmfPeerDiscoveryPage,
    LxmfPeerGeneration, LxmfReadChunk, LxmfReadLength, MAX_LXMF_READ_CHUNK_BYTES,
    OP_EXPERIMENTAL_LXMF_BASIC_SEND, OP_EXPERIMENTAL_LXMF_NEXT, OP_EXPERIMENTAL_LXMF_PEER_NEXT,
    OP_EXPERIMENTAL_LXMF_READ,
};
#[cfg(feature = "experimental-nomad")]
use reticulum_device_api::{
    MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS, NomadFetchFailure, NomadFetchId, NomadFetchPhase,
    NomadFetchPollRequest, NomadFetchPollResponse, NomadFetchStartAccepted, NomadFetchStartOutcome,
    NomadFetchStartRequest, NomadPage, NomadPagePath, NomadRequestTimestampUnixMs,
    OP_EXPERIMENTAL_NOMAD_FETCH_POLL, OP_EXPERIMENTAL_NOMAD_FETCH_START,
};
#[cfg(feature = "experimental-rns-inbox")]
use reticulum_device_api::{
    MAX_RNS_INBOX_PAYLOAD_BYTES, OP_EXPERIMENTAL_RNS_INBOX_PEEK, OP_EXPERIMENTAL_RNS_INBOX_STATUS,
    RnsInboxItem, RnsInboxStatus,
};
#[cfg(feature = "experimental-rns-data")]
use reticulum_device_api::{OP_EXPERIMENTAL_SUBMIT_RNS_DATA, SubmissionAccepted};

const GOLDEN_CAPABILITIES_REQUEST: &[u8] = &[
    0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x01, 0x03, 0xa0,
];

fn golden_capabilities_response() -> Vec<u8> {
    let mut encoded = vec![
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x01, 0x03, 0xb3, 0x00,
        0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0xf4, 0x02, 0x00, 0x03,
    ];
    encoded.push(if cfg!(feature = "experimental-rns-data") {
        0xf5
    } else {
        0xf4
    });
    encoded.extend_from_slice(&[
        0x04, 0x19, 0x02, 0x00, 0x05, 0x19, 0x01, 0xc0, 0x06, 0x19, 0x01, 0x7f, 0x07,
    ]);
    encoded.push(if cfg!(feature = "experimental-rns-inbox") {
        0x02
    } else {
        0x00
    });
    encoded.push(0x08);
    if cfg!(feature = "experimental-rns-inbox") {
        encoded.extend_from_slice(&[0x19, 0x01, 0x7f]);
    } else {
        encoded.push(0x00);
    }
    encoded.extend_from_slice(&[
        0x09,
        if cfg!(feature = "experimental-lxmf") {
            0x02
        } else {
            0x00
        },
        0x0a,
    ]);
    if cfg!(feature = "experimental-lxmf") {
        encoded.extend_from_slice(&[0x19, 0x01, 0xa0]);
    } else {
        encoded.push(0x00);
    }
    encoded.extend_from_slice(&[
        0x0b,
        if cfg!(feature = "experimental-lxmf") {
            0x02
        } else {
            0x00
        },
        0x0c,
    ]);
    if cfg!(feature = "experimental-lxmf") {
        encoded.extend_from_slice(&[0x19, 0x01, 0x27]);
    } else {
        encoded.push(0x00);
    }
    encoded.push(0x0d);
    if cfg!(feature = "experimental-lxmf") {
        encoded.extend_from_slice(&[0x19, 0x01, 0x27]);
    } else {
        encoded.push(0x00);
    }
    encoded.extend_from_slice(&[
        0x0e,
        if cfg!(feature = "experimental-lxmf") {
            0x02
        } else {
            0x00
        },
        0x0f,
    ]);
    if cfg!(feature = "experimental-lxmf") {
        encoded.extend_from_slice(&[0x19, 0x01, 0x00]);
    } else {
        encoded.push(0x00);
    }
    encoded.extend_from_slice(&[
        0x10,
        if cfg!(feature = "experimental-nomad") {
            0x02
        } else {
            0x00
        },
        0x11,
    ]);
    if cfg!(feature = "experimental-nomad") {
        encoded.extend_from_slice(&[0x18, 0x80]);
    } else {
        encoded.push(0x00);
    }
    encoded.push(0x12);
    if cfg!(feature = "experimental-nomad") {
        encoded.extend_from_slice(&[0x19, 0x01, 0x90]);
    } else {
        encoded.push(0x00);
    }
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
fn legacy_capabilities_default_absent_inbox_and_lxmf_fields() {
    const LEGACY: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x01, 0x03, 0xa7, 0x00,
        0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0xf4, 0x02, 0x00, 0x03, 0xf4, 0x04, 0x19, 0x02, 0x00,
        0x05, 0x19, 0x01, 0xc0, 0x06, 0x19, 0x01, 0x7f,
    ];

    for minor in [0_u8, 1] {
        let mut encoded = LEGACY.to_vec();
        encoded[6] = minor;
        encoded[19] = minor;
        let decoded = decode_response(&encoded).unwrap();
        assert_eq!(decoded.version.minor, u16::from(minor));
        let DeviceResponse::SystemCapabilities(capabilities) = decoded.response else {
            panic!("expected capabilities response")
        };
        assert_eq!(capabilities.api_version().minor, u16::from(minor));
        assert_eq!(
            capabilities.experimental_rns_inbox(),
            CapabilityAvailability::Unavailable
        );
        assert_eq!(capabilities.max_rns_inbox_payload_bytes(), 0);
        assert_eq!(
            capabilities.experimental_lxmf(),
            CapabilityAvailability::Unavailable
        );
        assert_eq!(capabilities.max_lxmf_read_chunk_bytes(), 0);
        assert_eq!(
            capabilities.experimental_lxmf_basic_send(),
            CapabilityAvailability::Unavailable
        );
        assert_eq!(capabilities.max_lxmf_basic_title_bytes(), 0);
        assert_eq!(capabilities.max_lxmf_basic_content_bytes(), 0);
        assert_eq!(
            capabilities.experimental_lxmf_peer_discovery(),
            CapabilityAvailability::Unavailable
        );
        assert_eq!(capabilities.max_lxmf_peer_app_data_bytes(), 0);
    }
}

#[test]
fn inbox_capability_availability_is_a_closed_wire_vocabulary() {
    let mut encoded = golden_capabilities_response();
    let key = encoded
        .windows(2)
        .rposition(|window| {
            window
                == [
                    0x07,
                    CapabilitySnapshot::current()
                        .experimental_rns_inbox()
                        .wire_code(),
                ]
        })
        .expect("capability key 7");
    encoded.splice(key + 1..=key + 1, [0x18, 99]);
    assert_eq!(
        decode_response(&encoded),
        Err(DecodeError::InvalidValue {
            field: RequiredField::CapabilityExperimentalRnsInbox,
            value: 99,
        })
    );
}

#[test]
fn lxmf_capability_availability_is_a_closed_wire_vocabulary() {
    let mut encoded = golden_capabilities_response();
    let key = encoded
        .windows(2)
        .rposition(|window| {
            window
                == [
                    0x09,
                    CapabilitySnapshot::current()
                        .experimental_lxmf()
                        .wire_code(),
                ]
        })
        .expect("capability key 9");
    encoded.splice(key + 1..=key + 1, [0x18, 99]);
    assert_eq!(
        decode_response(&encoded),
        Err(DecodeError::InvalidValue {
            field: RequiredField::CapabilityExperimentalLxmf,
            value: 99,
        })
    );
}

#[test]
fn exact_identity_summary_goldens_round_trip() {
    const REQUEST: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x03, 0x03, 0xa0,
    ];
    const RESPONSE: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x03, 0x03, 0xa1, 0x00,
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x03, 0x03, 0xa2, 0x00,
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
    assert_eq!(API_VERSION_MINOR, 6);
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
fn identity_summary_unknown_fields_are_skipped_but_required_field_is_strict() {
    const UNKNOWN_REQUEST_FIELD: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x03, 0x03, 0xa1, 0x18,
        0x63, 0x82, 0x01, 0x02,
    ];
    const UNKNOWN_RESPONSE_FIELD: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x03, 0x03, 0xa2, 0x00,
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x01, 0x01, 0x18, 0x2a, 0x02, 0x03, 0x03, 0xa0,
    ];
    assert_eq!(
        decode_response(&missing),
        Err(DecodeError::MissingField(
            RequiredField::IdentityPrimaryDestination,
        ))
    );

    let duplicate = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x01, 0x01, 0x18, 0x2a, 0x02, 0x03, 0x03, 0xa2, 0x00,
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x01, 0x01, 0x18, 0x2a, 0x02, 0x03, 0x03, 0xa1, 0x00,
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x03, 0x01, 0x18, 0x2a, 0x02, 0x03, 0x03, 0xa2, 0x00,
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

#[cfg(feature = "experimental-rns-inbox")]
#[test]
fn exact_experimental_inbox_status_goldens_round_trip() {
    const REQUEST: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x02, 0x03,
        0xa0,
    ];
    const RESPONSE: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x02, 0x03,
        0xa5, 0x00, 0x03, 0x01, 0x18, 0x20, 0x02, 0x1b, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
        0x08, 0x03, 0x19, 0x01, 0x7f, 0x04, 0xf5,
    ];
    let request = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        request: DeviceRequest::RnsInboxStatus,
    };
    let status = RnsInboxStatus {
        depth: 3,
        capacity: 32,
        dropped_since_boot: 0x0102_0304_0506_0708,
        max_payload_bytes: MAX_RNS_INBOX_PAYLOAD_BYTES as u16,
        durable: true,
    };
    let response = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        response: DeviceResponse::RnsInboxStatus(status),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let request_len = encode_request(&request, &mut output).unwrap();
    assert_eq!(&output[..request_len], REQUEST);
    assert_eq!(decode_request(REQUEST).unwrap(), request);
    let response_len = encode_response(&response, &mut output).unwrap();
    assert_eq!(&output[..response_len], RESPONSE);
    assert_eq!(decode_response(RESPONSE).unwrap(), response);
    assert_eq!(response.response.kind(), OP_EXPERIMENTAL_RNS_INBOX_STATUS);
}

#[cfg(feature = "experimental-rns-inbox")]
#[test]
fn exact_experimental_inbox_peek_goldens_round_trip() {
    const REQUEST: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x03, 0x03,
        0xa0,
    ];
    const RESPONSE: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x03, 0x03,
        0xa3, 0x00, 0x1b, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x01, 0x50, 0x10, 0x11,
        0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x02,
        0x43, 0x61, 0x62, 0x63,
    ];
    let request = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        request: DeviceRequest::RnsInboxPeek,
    };
    let item = RnsInboxItem::new(
        core::num::NonZeroU64::new(0x0102_0304_0506_0708).unwrap(),
        PRIMARY_DESTINATION,
        b"abc",
    )
    .unwrap();
    let response = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        response: DeviceResponse::RnsInboxPeek(item),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let request_len = encode_request(&request, &mut output).unwrap();
    assert_eq!(&output[..request_len], REQUEST);
    assert_eq!(decode_request(REQUEST).unwrap(), request);
    let response_len = encode_response(&response, &mut output).unwrap();
    assert_eq!(&output[..response_len], RESPONSE);
    assert_eq!(decode_response(RESPONSE).unwrap(), response);
    assert_eq!(response.response.kind(), OP_EXPERIMENTAL_RNS_INBOX_PEEK);
}

#[cfg(feature = "experimental-rns-inbox")]
#[test]
fn inbox_item_is_owned_bounded_and_redacts_payload_from_debug() {
    let mut source = [0x5a_u8; MAX_RNS_INBOX_PAYLOAD_BYTES];
    let item = RnsInboxItem::new(
        core::num::NonZeroU64::new(7).unwrap(),
        PRIMARY_DESTINATION,
        &source,
    )
    .unwrap();
    source.fill(0);
    assert_eq!(item.id(), 7);
    assert_eq!(item.destination(), PRIMARY_DESTINATION);
    assert_eq!(item.payload_len() as usize, MAX_RNS_INBOX_PAYLOAD_BYTES);
    assert_eq!(item.payload(), &[0x5a; MAX_RNS_INBOX_PAYLOAD_BYTES]);
    let debug = std::format!("{item:?}");
    assert!(debug.contains("payload_len: 383"));
    assert!(!debug.contains("90, 90"));

    let oversized = [0_u8; MAX_RNS_INBOX_PAYLOAD_BYTES + 1];
    let error = RnsInboxItem::new(
        core::num::NonZeroU64::new(8).unwrap(),
        PRIMARY_DESTINATION,
        &oversized,
    )
    .unwrap_err();
    assert_eq!(error.actual(), MAX_RNS_INBOX_PAYLOAD_BYTES + 1);
    assert_eq!(error.maximum(), MAX_RNS_INBOX_PAYLOAD_BYTES);
}

#[cfg(feature = "experimental-rns-inbox")]
#[test]
fn maximum_inbox_payload_fits_the_frozen_message_and_body_limits() {
    let payload = [0x6b_u8; MAX_RNS_INBOX_PAYLOAD_BYTES];
    let envelope = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(u64::MAX),
        response: DeviceResponse::RnsInboxPeek(
            RnsInboxItem::new(
                core::num::NonZeroU64::new(u64::MAX).unwrap(),
                DestinationHash([0xff; 16]),
                &payload,
            )
            .unwrap(),
        ),
    };
    let mut output = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_response(&envelope, &mut output).unwrap();
    assert!(written <= MAX_MESSAGE_BYTES);
    assert_eq!(decode_response(&output[..written]).unwrap(), envelope);
}

#[cfg(feature = "experimental-rns-inbox")]
#[test]
fn inbox_wire_fields_are_required_unique_and_bounded() {
    let missing_status_depth = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x02, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x02, 0x03, 0xa4,
        0x01, 0x01, 0x02, 0x00, 0x03, 0x19, 0x01, 0x7f, 0x04, 0xf4,
    ];
    assert_eq!(
        decode_response(&missing_status_depth),
        Err(DecodeError::MissingField(RequiredField::RnsInboxDepth))
    );

    let duplicate_item_id = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x02, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x03, 0x03, 0xa4,
        0x00, 0x01, 0x00, 0x02, 0x01, 0x50, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x02,
        0x40,
    ];
    assert_eq!(
        decode_response(&duplicate_item_id),
        Err(DecodeError::DuplicateField(RequiredField::RnsInboxItemId))
    );

    let wrong_destination_width = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x02, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x03, 0x03, 0xa3,
        0x00, 0x01, 0x01, 0x4f, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x02, 0x40,
    ];
    assert_eq!(
        decode_response(&wrong_destination_width),
        Err(DecodeError::InvalidByteStringLength {
            field: RequiredField::RnsInboxDestination,
            expected: 16,
            actual: 15,
        })
    );

    let zero_item_id = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x02, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x03, 0x03, 0xa3,
        0x00, 0x00, 0x01, 0x50, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x02, 0x40,
    ];
    assert_eq!(
        decode_response(&zero_item_id),
        Err(DecodeError::InvalidValue {
            field: RequiredField::RnsInboxItemId,
            value: 0,
        })
    );

    let oversized = [0x5a_u8; MAX_RNS_INBOX_PAYLOAD_BYTES + 1];
    let mut encoded = vec![
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x02, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x03, 0x03, 0xa3,
        0x00, 0x01, 0x01, 0x50,
    ];
    encoded.extend_from_slice(&[0; 16]);
    encoded.extend([0x02, 0x59, 0x01, 0x80]);
    encoded.extend_from_slice(&oversized);
    assert_eq!(
        decode_response(&encoded),
        Err(DecodeError::InboxPayloadTooLarge {
            actual: MAX_RNS_INBOX_PAYLOAD_BYTES + 1,
            max: MAX_RNS_INBOX_PAYLOAD_BYTES,
        })
    );
}

#[cfg(feature = "experimental-rns-inbox")]
#[test]
fn inbox_reads_require_authentication_but_no_persisted_permission_bit() {
    let authenticated = DispatchContext::authenticated(
        PrincipalId([0x72; 16]),
        Permissions::NONE,
        dispatch_provenance(),
    );
    for request in [DeviceRequest::RnsInboxStatus, DeviceRequest::RnsInboxPeek] {
        assert!(!request.is_mutating());
        assert_eq!(
            authorize_request(&DispatchContext::UNAUTHENTICATED, &request),
            Err(AuthorizationError::AuthenticationRequired)
        );
        assert_eq!(authorize_request(&authenticated, &request), Ok(()));
    }
}

#[cfg(feature = "experimental-lxmf")]
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

#[cfg(feature = "experimental-lxmf")]
#[test]
fn exact_experimental_lxmf_next_goldens_round_trip() {
    const REQUEST: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x04, 0x03,
        0xa1, 0x00, 0x07,
    ];
    const RESPONSE: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x04, 0x03,
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
    assert_eq!(request.request.operation(), OP_EXPERIMENTAL_LXMF_NEXT);
    assert_eq!(response.response.kind(), OP_EXPERIMENTAL_LXMF_NEXT);
}

#[cfg(feature = "experimental-lxmf")]
#[test]
fn exact_experimental_lxmf_read_goldens_round_trip() {
    const REQUEST: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x05, 0x03,
        0xa3, 0x00, 0x07, 0x01, 0x19, 0x01, 0x00, 0x02, 0x19, 0x01, 0xa0,
    ];
    const RESPONSE: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x05, 0x03,
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
    assert_eq!(request.request.operation(), OP_EXPERIMENTAL_LXMF_READ);
    assert_eq!(response.response.kind(), OP_EXPERIMENTAL_LXMF_READ);
}

#[cfg(feature = "experimental-lxmf")]
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
            idempotency_key: IdempotencyKey([
                0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc, 0xfd,
                0xfe, 0xff,
            ]),
        },
    }
}

#[cfg(feature = "experimental-lxmf")]
#[test]
fn exact_basic_lxmf_send_goldens_are_source_free_and_borrowed() {
    const REQUEST: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x06, 0x03, 0xa5,
        0x00, 0x50, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
        0x0d, 0x0e, 0x0f, 0x01, 0x1b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02, 0x43,
        0x74, 0x74, 0x6c, 0x03, 0x47, 0x63, 0x6f, 0x6e, 0x74, 0x65, 0x6e, 0x74, 0x04, 0x50, 0xf0,
        0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc, 0xfd, 0xfe, 0xff,
    ];
    const RESPONSE: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x06, 0x03, 0xa2,
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
    assert_eq!(request.request.operation(), OP_EXPERIMENTAL_LXMF_BASIC_SEND);

    let response_len = encode_response(&response, &mut output).unwrap();
    assert_eq!(&output[..response_len], RESPONSE);
    assert_eq!(decode_response(RESPONSE).unwrap(), response);
    assert_eq!(response.response.kind(), OP_EXPERIMENTAL_LXMF_BASIC_SEND);
}

#[cfg(feature = "experimental-lxmf")]
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

#[cfg(feature = "experimental-lxmf")]
#[test]
fn exact_nearby_lxmf_peer_goldens_use_complete_boot_scoped_cursors() {
    const FIRST_REQUEST: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x07, 0x03,
        0xa0,
    ];
    const NEXT_REQUEST: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x07, 0x03,
        0xa2, 0x00, 0x48, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0x01, 0x09,
    ];
    const RESPONSE: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x07, 0x03,
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
    assert_eq!(
        first_request.request.operation(),
        OP_EXPERIMENTAL_LXMF_PEER_NEXT
    );
    assert_eq!(response.response.kind(), OP_EXPERIMENTAL_LXMF_PEER_NEXT);
}

#[cfg(feature = "experimental-lxmf")]
#[test]
fn nearby_lxmf_peer_cursor_and_response_shapes_are_strict() {
    const ONLY_INCARNATION: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x07, 0x03, 0xa1,
        0x00, 0x48, 0, 1, 2, 3, 4, 5, 6, 7,
    ];
    const ONLY_GENERATION: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x07, 0x03, 0xa1,
        0x01, 0x00,
    ];
    const SHORT_INCARNATION: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x07, 0x03, 0xa2,
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

#[cfg(feature = "experimental-lxmf")]
fn append_cbor_bytes(encoded: &mut Vec<u8>, bytes: &[u8]) {
    match bytes.len() {
        0..=23 => encoded.push(0x40 + bytes.len() as u8),
        24..=255 => encoded.extend([0x58, bytes.len() as u8]),
        _ => encoded.extend([0x59, (bytes.len() >> 8) as u8, bytes.len() as u8]),
    }
    encoded.extend_from_slice(bytes);
}

#[cfg(feature = "experimental-lxmf")]
fn raw_peer_response(app_data: &[u8], peer_generation: u8) -> Vec<u8> {
    let mut encoded = vec![
        0xa4,
        0x00,
        0xa2,
        0x00,
        0x01,
        0x01,
        0x05,
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

#[cfg(feature = "experimental-lxmf")]
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

#[cfg(feature = "experimental-lxmf")]
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

#[cfg(feature = "experimental-lxmf")]
fn raw_basic_lxmf_send(title: &[u8], content: &[u8]) -> Vec<u8> {
    let mut encoded = vec![
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x06, 0x03, 0xa5,
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

#[cfg(feature = "experimental-lxmf")]
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

#[cfg(feature = "experimental-lxmf")]
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
            RequiredPermission::ExperimentalSubmitRnsData
        ))
    );
    assert_eq!(
        authorize_request(
            &DispatchContext::authenticated(
                principal,
                Permissions::EXPERIMENTAL_SUBMIT_RNS_DATA,
                dispatch_provenance(),
            ),
            &request,
        ),
        Ok(())
    );
}

#[cfg(feature = "experimental-lxmf")]
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

#[cfg(feature = "experimental-lxmf")]
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

#[cfg(feature = "experimental-lxmf")]
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

#[cfg(feature = "experimental-lxmf")]
#[test]
fn lxmf_wire_fields_are_required_unique_and_strictly_bounded() {
    let missing_read_max = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x03, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x05, 0x03, 0xa2,
        0x00, 0x01, 0x01, 0x00,
    ];
    assert_eq!(
        decode_request(&missing_read_max),
        Err(DecodeError::MissingField(RequiredField::LxmfReadMaxBytes))
    );

    let duplicate_handle = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x03, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x05, 0x03, 0xa4,
        0x00, 0x01, 0x00, 0x02, 0x01, 0x00, 0x02, 0x01,
    ];
    assert_eq!(
        decode_request(&duplicate_handle),
        Err(DecodeError::DuplicateField(RequiredField::LxmfHandle))
    );

    let zero_after = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x03, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x04, 0x03, 0xa1,
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
            0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x03, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x05, 0x03,
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x03, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x05, 0x03, 0xa4,
        0x00, 0x01, 0x01, 0x00, 0x02, 0x01, 0x03, 0x40,
    ];
    assert_eq!(
        decode_response(&empty_chunk),
        Err(DecodeError::InvalidLxmfReadChunk)
    );

    let outside_chunk = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x03, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x05, 0x03, 0xa4,
        0x00, 0x01, 0x01, 0x01, 0x02, 0x01, 0x03, 0x41, 0x61,
    ];
    assert_eq!(
        decode_response(&outside_chunk),
        Err(DecodeError::InvalidLxmfReadChunk)
    );

    let mut oversized_chunk = vec![
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x03, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x05, 0x03, 0xa4,
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x03, 0x01, 0x01, 0x02, 0x19, 0xf0, 0x04, 0x03, 0xa1,
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x00, 0x03, 0xa2, 0x00,
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x00, 0x03, 0xa2, 0x00,
        0x09, 0x01, 0x19, 0xf0, 0x01,
    ];
    const IDEMPOTENCY_GOLDEN: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x00, 0x03, 0xa2, 0x00,
        0x0a, 0x01, 0x19, 0xf0, 0x01,
    ];
    for (code, golden) in [
        (ApiErrorCode::CapacityExhausted, CAPACITY_GOLDEN),
        (ApiErrorCode::IdempotencyConflict, IDEMPOTENCY_GOLDEN),
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x01, 0x01, 0x07, 0x02, 0x02, 0x03, 0xa3, 0x00, 0x01,
        0x01, 0x00, 0x02, 0x18, 0x61,
    ];
    let awaiting_delivery_without_hash = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x01, 0x01, 0x07, 0x02, 0x02, 0x03, 0xa3, 0x00, 0x01,
        0x01, 0x02, 0x02, 0x18, 0x61,
    ];
    let failed_without_failure = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x01, 0x01, 0x07, 0x02, 0x02, 0x03, 0xa2, 0x00, 0x01,
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
        0xa5, 0x00, 0xa3, 0x00, 0x01, 0x01, 0x06, 0x07, 0x82, 0x01, 0x02, 0x01, 0x18, 0x2a, 0x02,
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x01, 0x01, 0x07, 0x02, 0x02, 0x03, 0xa2, 0x00, 0x18,
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x01, 0x01, 0x18, 0x2a, 0x02, 0x19, 0x12, 0x34, 0x03,
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
        0xa4, 0x00, 0xa2, 0x00, 0x02, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x01, 0x03, 0xa0,
    ];
    assert_eq!(
        decode_request(&incompatible),
        Err(DecodeError::UnsupportedVersion(ApiVersion {
            major: API_VERSION_MAJOR + 1,
            minor: 0,
        }))
    );

    let newer_minor = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x09, 0x01, 0x18, 0x2a, 0x02, 0x01, 0x03, 0xa0,
    ];
    assert_eq!(decode_request(&newer_minor).unwrap().version.minor, 9);

    let previous_minor = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x01, 0x03, 0xa0,
    ];
    assert_eq!(decode_request(&previous_minor).unwrap().version.minor, 0);
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
        0xa3, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x01, 0x01, 0x18, 0x2a, 0x02, 0x01,
    ];
    assert_eq!(
        decode_request(&missing_body),
        Err(DecodeError::MissingField(RequiredField::EnvelopeBody))
    );

    let missing_submission_id = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x01, 0x01, 0x07, 0x02, 0x02, 0x03, 0xa0,
    ];
    assert_eq!(
        decode_request(&missing_submission_id),
        Err(DecodeError::MissingField(RequiredField::SubmissionId))
    );
}

#[test]
fn duplicate_required_fields_are_rejected_at_each_level() {
    let duplicate_envelope_id = [
        0xa5, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x01, 0x01, 0x18, 0x2a, 0x01, 0x18, 0x2b, 0x02, 0x01,
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x01, 0x01, 0x07, 0x02, 0x02, 0x03, 0xa2, 0x00, 0x01,
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x01, 0x01, 0x01, 0x02, 0x18, 0x63, 0x03, 0xa0,
    ];
    assert_eq!(
        decode_response(&unknown_kind),
        Err(DecodeError::UnsupportedResponseKind(99))
    );

    let duplicate_error_code = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x01, 0x01, 0x01, 0x02, 0x00, 0x03, 0xa2, 0x00, 0x01,
        0x00, 0x02,
    ];
    assert_eq!(
        decode_response(&duplicate_error_code),
        Err(DecodeError::DuplicateField(RequiredField::ErrorCode))
    );

    let invalid_error_code = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x01, 0x01, 0x01, 0x02, 0x00, 0x03, 0xa1, 0x00, 0x18,
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
fn api_v1_numeric_enum_vocabularies_are_closed_and_frozen() {
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
        ],
        [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
    );

    let unknown_state = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x01, 0x01, 0x01, 0x02, 0x02, 0x03, 0xa2, 0x00, 0x01,
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x01, 0x01, 0x01, 0x02, 0x02, 0x03, 0xa3, 0x00, 0x01,
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
        0x01,
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x01, 0x01, 0x18, 0x2a, 0x02, 0x01, 0x03, 0xa1, 0x18,
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
    assert_eq!(
        capabilities.experimental_submit_rns_data(),
        cfg!(feature = "experimental-rns-data")
    );
    assert_eq!(
        capabilities.experimental_rns_inbox(),
        if cfg!(feature = "experimental-rns-inbox") {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Unavailable
        }
    );
    assert_eq!(
        capabilities.max_rns_inbox_payload_bytes(),
        if cfg!(feature = "experimental-rns-inbox") {
            383
        } else {
            0
        }
    );
    assert_eq!(
        capabilities.experimental_lxmf(),
        if cfg!(feature = "experimental-lxmf") {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Unavailable
        }
    );
    assert_eq!(
        capabilities.max_lxmf_read_chunk_bytes(),
        if cfg!(feature = "experimental-lxmf") {
            416
        } else {
            0
        }
    );
    assert_eq!(
        capabilities.experimental_lxmf_basic_send(),
        if cfg!(feature = "experimental-lxmf") {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Unavailable
        }
    );
    assert_eq!(
        capabilities.max_lxmf_basic_title_bytes(),
        if cfg!(feature = "experimental-lxmf") {
            MAX_LXMF_BASIC_TITLE_BYTES as u16
        } else {
            0
        }
    );
    assert_eq!(
        capabilities.max_lxmf_basic_content_bytes(),
        if cfg!(feature = "experimental-lxmf") {
            MAX_LXMF_BASIC_CONTENT_BYTES as u16
        } else {
            0
        }
    );
    assert_eq!(
        capabilities.experimental_lxmf_peer_discovery(),
        if cfg!(feature = "experimental-lxmf") {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Unavailable
        }
    );
    assert_eq!(
        capabilities.max_lxmf_peer_app_data_bytes(),
        if cfg!(feature = "experimental-lxmf") {
            MAX_LXMF_PEER_APP_DATA_BYTES as u16
        } else {
            0
        }
    );
    assert_eq!(
        capabilities.experimental_nomad(),
        if cfg!(feature = "experimental-nomad") {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Unavailable
        }
    );
    assert_eq!(
        capabilities.max_nomad_page_path_bytes(),
        if cfg!(feature = "experimental-nomad") {
            MAX_NOMAD_PAGE_PATH_BYTES as u16
        } else {
            0
        }
    );
    assert_eq!(
        capabilities.max_nomad_page_bytes(),
        if cfg!(feature = "experimental-nomad") {
            MAX_NOMAD_PAGE_BYTES as u16
        } else {
            0
        }
    );

    let legacy_dispatch = CapabilitySnapshot::for_dispatch(true);
    assert_eq!(
        legacy_dispatch.experimental_submit_rns_data(),
        cfg!(feature = "experimental-rns-data")
    );
    assert_eq!(
        legacy_dispatch.experimental_rns_inbox(),
        CapabilityAvailability::Unavailable
    );
    assert_eq!(legacy_dispatch.max_rns_inbox_payload_bytes(), 0);
    assert_eq!(
        legacy_dispatch.experimental_lxmf(),
        CapabilityAvailability::Unavailable
    );
    assert_eq!(legacy_dispatch.max_lxmf_read_chunk_bytes(), 0);
    assert_eq!(
        legacy_dispatch.experimental_lxmf_basic_send(),
        CapabilityAvailability::Unavailable
    );
    assert_eq!(legacy_dispatch.max_lxmf_basic_title_bytes(), 0);
    assert_eq!(legacy_dispatch.max_lxmf_basic_content_bytes(), 0);
    assert_eq!(
        legacy_dispatch.experimental_lxmf_peer_discovery(),
        CapabilityAvailability::Unavailable
    );
    assert_eq!(legacy_dispatch.max_lxmf_peer_app_data_bytes(), 0);
    assert_eq!(
        legacy_dispatch.experimental_nomad(),
        CapabilityAvailability::Unavailable
    );
    assert_eq!(legacy_dispatch.max_nomad_page_path_bytes(), 0);
    assert_eq!(legacy_dispatch.max_nomad_page_bytes(), 0);
    assert!(!CapabilitySnapshot::for_dispatch(false).experimental_submit_rns_data());

    let inbox_dispatch =
        CapabilitySnapshot::for_dispatch_with_inbox(true, CapabilityAvailability::Disabled);
    assert_eq!(
        inbox_dispatch.experimental_rns_inbox(),
        if cfg!(feature = "experimental-rns-inbox") {
            CapabilityAvailability::Disabled
        } else {
            CapabilityAvailability::Unavailable
        }
    );
    assert_eq!(
        inbox_dispatch.max_rns_inbox_payload_bytes(),
        if cfg!(feature = "experimental-rns-inbox") {
            383
        } else {
            0
        }
    );
    assert_eq!(
        inbox_dispatch.experimental_lxmf(),
        CapabilityAvailability::Unavailable
    );
    assert_eq!(inbox_dispatch.max_lxmf_read_chunk_bytes(), 0);
    assert_eq!(
        inbox_dispatch.experimental_lxmf_basic_send(),
        CapabilityAvailability::Unavailable
    );

    let lxmf_dispatch = CapabilitySnapshot::for_dispatch_with_inbox_and_lxmf(
        true,
        CapabilityAvailability::Disabled,
        CapabilityAvailability::Disabled,
    );
    assert_eq!(
        lxmf_dispatch.experimental_lxmf(),
        if cfg!(feature = "experimental-lxmf") {
            CapabilityAvailability::Disabled
        } else {
            CapabilityAvailability::Unavailable
        }
    );
    assert_eq!(
        lxmf_dispatch.max_lxmf_read_chunk_bytes(),
        if cfg!(feature = "experimental-lxmf") {
            416
        } else {
            0
        }
    );
    assert_eq!(
        lxmf_dispatch.experimental_lxmf_basic_send(),
        CapabilityAvailability::Unavailable
    );

    let send_dispatch = CapabilitySnapshot::for_dispatch_with_inbox_lxmf_and_basic_send(
        true,
        CapabilityAvailability::Disabled,
        CapabilityAvailability::Disabled,
        CapabilityAvailability::Disabled,
    );
    assert_eq!(
        send_dispatch.experimental_lxmf_basic_send(),
        if cfg!(feature = "experimental-lxmf") {
            CapabilityAvailability::Disabled
        } else {
            CapabilityAvailability::Unavailable
        }
    );
    assert_eq!(
        send_dispatch.max_lxmf_basic_title_bytes(),
        if cfg!(feature = "experimental-lxmf") {
            MAX_LXMF_BASIC_TITLE_BYTES as u16
        } else {
            0
        }
    );
    assert_eq!(
        send_dispatch.max_lxmf_basic_content_bytes(),
        if cfg!(feature = "experimental-lxmf") {
            MAX_LXMF_BASIC_CONTENT_BYTES as u16
        } else {
            0
        }
    );
    assert_eq!(
        send_dispatch.experimental_lxmf_peer_discovery(),
        CapabilityAvailability::Unavailable
    );

    let peer_dispatch =
        CapabilitySnapshot::for_dispatch_with_inbox_lxmf_basic_send_and_peer_discovery(
            true,
            CapabilityAvailability::Disabled,
            CapabilityAvailability::Disabled,
            CapabilityAvailability::Disabled,
            CapabilityAvailability::Disabled,
            128,
        );
    assert_eq!(
        peer_dispatch.experimental_lxmf_peer_discovery(),
        if cfg!(feature = "experimental-lxmf") {
            CapabilityAvailability::Disabled
        } else {
            CapabilityAvailability::Unavailable
        }
    );
    assert_eq!(
        peer_dispatch.max_lxmf_peer_app_data_bytes(),
        if cfg!(feature = "experimental-lxmf") {
            128
        } else {
            0
        }
    );

    let composed_dispatch = peer_dispatch.with_dispatch_nomad(CapabilityAvailability::Disabled);
    assert_eq!(
        composed_dispatch.experimental_lxmf_peer_discovery(),
        peer_dispatch.experimental_lxmf_peer_discovery()
    );
    assert_eq!(
        composed_dispatch.experimental_nomad(),
        if cfg!(feature = "experimental-nomad") {
            CapabilityAvailability::Disabled
        } else {
            CapabilityAvailability::Unavailable
        }
    );
    assert_eq!(
        composed_dispatch.max_nomad_page_path_bytes(),
        if cfg!(feature = "experimental-nomad") {
            MAX_NOMAD_PAGE_PATH_BYTES as u16
        } else {
            0
        }
    );
    assert_eq!(
        composed_dispatch.max_nomad_page_bytes(),
        if cfg!(feature = "experimental-nomad") {
            MAX_NOMAD_PAGE_BYTES as u16
        } else {
            0
        }
    );
}

#[cfg(feature = "experimental-rns-data")]
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

#[cfg(feature = "experimental-rns-data")]
#[test]
fn exact_experimental_submit_golden_and_borrowed_payload() {
    const GOLDEN: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x01, 0x03, 0xa3,
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

#[cfg(feature = "experimental-rns-data")]
#[test]
fn exact_experimental_submit_accepted_response_has_only_submission_id() {
    const GOLDEN: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x01, 0x03, 0xa1,
        0x00, 0x1b, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
    ];
    let envelope = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(9),
        response: DeviceResponse::SubmitRnsDataAccepted(SubmissionAccepted {
            id: SubmissionId(0x0102_0304_0506_0708),
        }),
    };
    assert_eq!(envelope.response.kind(), OP_EXPERIMENTAL_SUBMIT_RNS_DATA);
    let mut output = [0u8; MAX_MESSAGE_BYTES];
    let written = encode_response(&envelope, &mut output).unwrap();
    assert_eq!(&output[..written], GOLDEN);
    assert_eq!(decode_response(GOLDEN).unwrap(), envelope);
}

#[cfg(feature = "experimental-rns-data")]
#[test]
fn experimental_submit_is_mutating_and_requires_auth_and_permission() {
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
            RequiredPermission::ExperimentalSubmitRnsData
        ))
    );
    assert_eq!(
        authorize_request(
            &DispatchContext::authenticated(
                principal,
                Permissions::EXPERIMENTAL_SUBMIT_RNS_DATA,
                dispatch_provenance(),
            ),
            &request,
        ),
        Ok(())
    );
}

#[cfg(feature = "experimental-rns-data")]
#[test]
fn experimental_submit_rejects_oversize_payload_on_encode_and_decode() {
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x01, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x01, 0x03, 0xa3,
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

#[cfg(feature = "experimental-rns-data")]
#[test]
fn experimental_submit_rejects_duplicate_and_wrong_width_fields() {
    let duplicate_destination = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x01, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x01, 0x03, 0xa4,
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x01, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x01, 0x03, 0xa3,
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

#[cfg(feature = "experimental-nomad")]
fn nomad_fetch_id() -> NomadFetchId {
    NomadFetchId::new(
        [0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7],
        0x0102_0304_0506_0708,
    )
    .unwrap()
}

#[cfg(feature = "experimental-nomad")]
fn nomad_fetch_start_wire(encoded_path: &[u8], encoded_timestamp: &[u8]) -> Vec<u8> {
    let mut wire = vec![
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x08, 0x03,
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

#[cfg(feature = "experimental-nomad")]
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

#[cfg(feature = "experimental-nomad")]
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

#[cfg(feature = "experimental-nomad")]
#[test]
fn exact_nomad_fetch_start_request_golden_and_authorization() {
    const GOLDEN: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x08, 0x03,
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
    assert_eq!(
        expected.request.operation(),
        OP_EXPERIMENTAL_NOMAD_FETCH_START
    );
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

#[cfg(feature = "experimental-nomad")]
#[test]
fn exact_nomad_fetch_start_response_distinguishes_fresh_and_replayed() {
    const ACCEPTED: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x08, 0x03,
        0xa2, 0x00, 0x50, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0x01, 0x02, 0x03, 0x04,
        0x05, 0x06, 0x07, 0x08, 0x01, 0x00,
    ];
    const REPLAYED: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x08, 0x03,
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

#[cfg(feature = "experimental-nomad")]
#[test]
fn exact_nomad_fetch_poll_request_and_states_round_trip() {
    const REQUEST: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x09, 0x03,
        0xa1, 0x00, 0x50, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0x01, 0x02, 0x03, 0x04,
        0x05, 0x06, 0x07, 0x08,
    ];
    const PENDING: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x09, 0x03,
        0xa2, 0x00, 0x00, 0x01, 0x04,
    ];
    const READY: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x09, 0x03,
        0xa2, 0x00, 0x01, 0x01, 0x45, 0x68, 0x65, 0x6c, 0x6c, 0x6f,
    ];
    const FAILED: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x09, 0x03,
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
    assert_eq!(
        request.request.operation(),
        OP_EXPERIMENTAL_NOMAD_FETCH_POLL
    );
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

#[cfg(feature = "experimental-nomad")]
#[test]
fn maximum_nomad_page_has_exact_bounded_body_and_message_sizes() {
    const ENVELOPE_PREFIX: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x1b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
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

#[cfg(feature = "experimental-nomad")]
#[test]
fn nomad_fetch_decoder_rejects_zero_sequence_and_invalid_state_values() {
    let zero_sequence = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x09, 0x03,
        0xa1, 0x00, 0x50, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00,
    ];
    assert_eq!(
        decode_request(&zero_sequence),
        Err(DecodeError::InvalidNomadFetchId)
    );

    let invalid_state = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x09, 0x03,
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

#[cfg(feature = "experimental-nomad")]
#[test]
fn nomad_fetch_decoder_rejects_unknown_closed_outcome_phase_and_failure_values() {
    let unknown_start_outcome = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x08, 0x03,
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x09, 0x03,
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x09, 0x03,
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

#[cfg(feature = "experimental-nomad")]
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

#[cfg(feature = "experimental-nomad")]
#[test]
fn nomad_ready_page_decoder_rejects_invalid_utf8_and_oversize_values() {
    let invalid_utf8 = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x09, 0x03,
        0xa2, 0x00, 0x01, 0x01, 0x41, 0xff,
    ];
    assert_eq!(
        decode_response(&invalid_utf8),
        Err(DecodeError::InvalidNomadPageUtf8)
    );

    let mut oversized_page = vec![
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x06, 0x01, 0x18, 0x2a, 0x02, 0x19, 0xf0, 0x09, 0x03,
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

#[test]
fn experimental_operation_is_unavailable_without_feature() {
    #[cfg(not(feature = "experimental-rns-data"))]
    {
        let request = [
            0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x01, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x01, 0x03,
            0xa0,
        ];
        assert_eq!(
            decode_request(&request),
            Err(DecodeError::UnsupportedOperation(0xf001))
        );

        let response = [
            0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x01, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x01, 0x03,
            0xa1, 0x00, 0x01,
        ];
        assert_eq!(
            decode_response(&response),
            Err(DecodeError::UnsupportedResponseKind(0xf001))
        );
    }

    #[cfg(not(feature = "experimental-rns-inbox"))]
    for operation in [0xf002_u16, 0xf003] {
        let encoded_operation = operation.to_be_bytes();
        let request = [
            0xa4,
            0x00,
            0xa2,
            0x00,
            0x01,
            0x01,
            0x02,
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
            decode_request(&request),
            Err(DecodeError::UnsupportedOperation(operation))
        );
        assert_eq!(
            decode_response(&request),
            Err(DecodeError::UnsupportedResponseKind(operation))
        );
    }

    #[cfg(not(feature = "experimental-lxmf"))]
    for operation in [0xf004_u16, 0xf005, 0xf006, 0xf007] {
        let encoded_operation = operation.to_be_bytes();
        let envelope = [
            0xa4,
            0x00,
            0xa2,
            0x00,
            0x01,
            0x01,
            0x03,
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

    #[cfg(not(feature = "experimental-nomad"))]
    for operation in [0xf008_u16, 0xf009] {
        let encoded_operation = operation.to_be_bytes();
        let envelope = [
            0xa4,
            0x00,
            0xa2,
            0x00,
            0x01,
            0x01,
            0x06,
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
