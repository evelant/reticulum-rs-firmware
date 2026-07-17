use reticulum_device_api::{
    API_VERSION_MAJOR, ApiErrorCode, ApiErrorResponse, ApiVersion, AuthorizationError,
    CapabilityAvailability, CapabilitySnapshot, DecodeError, DeviceRequest, DeviceResponse,
    DispatchContext, EncodeError, EncodedPacketSha256, MAX_BODY_BYTES, MAX_CBOR_NESTING_DEPTH,
    MAX_MESSAGE_BYTES, MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES, OP_SUBMISSION_STATUS, Permissions,
    PreparedPacketDetails, PrincipalId, RequestEnvelope, RequestId, RequiredField,
    RequiredPermission, ResponseEnvelope, SubmissionFailure, SubmissionId, SubmissionState,
    SubmissionStatus, authorize_request, decode_request, decode_response, encode_request,
    encode_response,
};
#[cfg(feature = "experimental-rns-data")]
use reticulum_device_api::{
    DestinationHash, IdempotencyKey, OP_EXPERIMENTAL_SUBMIT_RNS_DATA, SubmissionAccepted,
};

const GOLDEN_CAPABILITIES_REQUEST: &[u8] = &[
    0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x01, 0x03, 0xa0,
];

#[cfg(not(feature = "experimental-rns-data"))]
const GOLDEN_CAPABILITIES_RESPONSE: &[u8] = &[
    0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x01, 0x03, 0xa7, 0x00, 0xa2,
    0x00, 0x01, 0x01, 0x00, 0x01, 0xf4, 0x02, 0x00, 0x03, 0xf4, 0x04, 0x19, 0x02, 0x00, 0x05, 0x19,
    0x01, 0xc0, 0x06, 0x19, 0x01, 0x7f,
];

#[cfg(feature = "experimental-rns-data")]
const GOLDEN_CAPABILITIES_RESPONSE: &[u8] = &[
    0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x01, 0x03, 0xa7, 0x00, 0xa2,
    0x00, 0x01, 0x01, 0x00, 0x01, 0xf4, 0x02, 0x00, 0x03, 0xf5, 0x04, 0x19, 0x02, 0x00, 0x05, 0x19,
    0x01, 0xc0, 0x06, 0x19, 0x01, 0x7f,
];

fn capabilities_request() -> RequestEnvelope<'static> {
    RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(42),
        request: DeviceRequest::SystemCapabilities,
    }
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
    assert_eq!(&output[..written], GOLDEN_CAPABILITIES_RESPONSE);
    assert_eq!(
        decode_response(GOLDEN_CAPABILITIES_RESPONSE).unwrap(),
        envelope
    );
}

#[test]
fn exact_typed_error_response_golden_round_trip() {
    const GOLDEN: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x00, 0x03, 0xa2, 0x00,
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x00, 0x03, 0xa2, 0x00,
        0x09, 0x01, 0x19, 0xf0, 0x01,
    ];
    const IDEMPOTENCY_GOLDEN: &[u8] = &[
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x00, 0x03, 0xa2, 0x00,
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x07, 0x02, 0x02, 0x03, 0xa3, 0x00, 0x01,
        0x01, 0x00, 0x02, 0x18, 0x61,
    ];
    let awaiting_delivery_without_hash = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x07, 0x02, 0x02, 0x03, 0xa3, 0x00, 0x01,
        0x01, 0x02, 0x02, 0x18, 0x61,
    ];
    let failed_without_failure = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x07, 0x02, 0x02, 0x03, 0xa2, 0x00, 0x01,
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
        0xa5, 0x00, 0xa3, 0x00, 0x01, 0x01, 0x00, 0x07, 0x82, 0x01, 0x02, 0x01, 0x18, 0x2a, 0x02,
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x07, 0x02, 0x02, 0x03, 0xa2, 0x00, 0x18,
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x19, 0x12, 0x34, 0x03,
        0xa0,
    ];
    assert_eq!(
        decode_request(&bytes),
        Err(DecodeError::UnsupportedOperation(0x1234))
    );
}

#[test]
fn incompatible_major_version_is_typed_but_new_minor_is_accepted() {
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
        0xa3, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x01,
    ];
    assert_eq!(
        decode_request(&missing_body),
        Err(DecodeError::MissingField(RequiredField::EnvelopeBody))
    );

    let missing_submission_id = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x07, 0x02, 0x02, 0x03, 0xa0,
    ];
    assert_eq!(
        decode_request(&missing_submission_id),
        Err(DecodeError::MissingField(RequiredField::SubmissionId))
    );
}

#[test]
fn duplicate_required_fields_are_rejected_at_each_level() {
    let duplicate_envelope_id = [
        0xa5, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x01, 0x18, 0x2b, 0x02, 0x01,
        0x03, 0xa0,
    ];
    assert_eq!(
        decode_request(&duplicate_envelope_id),
        Err(DecodeError::DuplicateField(
            RequiredField::EnvelopeRequestId
        ))
    );

    let duplicate_version_major = [
        0xa4, 0x00, 0xa3, 0x00, 0x01, 0x00, 0x01, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x01, 0x03,
        0xa0,
    ];
    assert_eq!(
        decode_request(&duplicate_version_major),
        Err(DecodeError::DuplicateField(RequiredField::VersionMajor))
    );

    let duplicate_submission_id = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x07, 0x02, 0x02, 0x03, 0xa2, 0x00, 0x01,
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
    for end in 0..GOLDEN_CAPABILITIES_RESPONSE.len() {
        assert!(
            decode_response(&GOLDEN_CAPABILITIES_RESPONSE[..end]).is_err(),
            "response prefix of length {end} unexpectedly decoded"
        );
    }
}

#[test]
fn malformed_and_unknown_responses_are_typed() {
    let unknown_kind = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x01, 0x02, 0x18, 0x63, 0x03, 0xa0,
    ];
    assert_eq!(
        decode_response(&unknown_kind),
        Err(DecodeError::UnsupportedResponseKind(99))
    );

    let duplicate_error_code = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x01, 0x02, 0x00, 0x03, 0xa2, 0x00, 0x01,
        0x00, 0x02,
    ];
    assert_eq!(
        decode_response(&duplicate_error_code),
        Err(DecodeError::DuplicateField(RequiredField::ErrorCode))
    );

    let invalid_error_code = [
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x01, 0x02, 0x00, 0x03, 0xa1, 0x00, 0x18,
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x01, 0x02, 0x02, 0x03, 0xa2, 0x00, 0x01,
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x01, 0x02, 0x02, 0x03, 0xa3, 0x00, 0x01,
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x18, 0x2a, 0x02, 0x01, 0x03, 0xa1, 0x18,
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
        authorize_request(DispatchContext::UNAUTHENTICATED, &capabilities),
        Ok(())
    );

    let status = DeviceRequest::SubmissionStatus {
        id: SubmissionId(1),
    };
    assert_eq!(
        authorize_request(DispatchContext::UNAUTHENTICATED, &status),
        Err(AuthorizationError::AuthenticationRequired)
    );
    let principal = PrincipalId([0x44; 16]);
    assert_eq!(
        authorize_request(
            DispatchContext::authenticated(principal, Permissions::NONE),
            &status,
        ),
        Err(AuthorizationError::PermissionDenied(
            RequiredPermission::ReadSubmissionStatus
        ))
    );
    assert_eq!(
        authorize_request(
            DispatchContext::authenticated(principal, Permissions::READ_SUBMISSION_STATUS),
            &status,
        ),
        Ok(())
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
    assert!(!CapabilitySnapshot::for_dispatch(false).experimental_submit_rns_data());
    assert_eq!(CapabilitySnapshot::for_dispatch(true), capabilities);
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x01, 0x03, 0xa3,
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x01, 0x03, 0xa1,
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
        authorize_request(DispatchContext::UNAUTHENTICATED, &request),
        Err(AuthorizationError::AuthenticationRequired)
    );
    let principal = PrincipalId([0x55; 16]);
    assert_eq!(
        authorize_request(
            DispatchContext::authenticated(principal, Permissions::NONE),
            &request,
        ),
        Err(AuthorizationError::PermissionDenied(
            RequiredPermission::ExperimentalSubmitRnsData
        ))
    );
    assert_eq!(
        authorize_request(
            DispatchContext::authenticated(principal, Permissions::EXPERIMENTAL_SUBMIT_RNS_DATA,),
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x01, 0x03, 0xa3,
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x01, 0x03, 0xa4,
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
        0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x01, 0x03, 0xa3,
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

#[test]
fn experimental_operation_is_unavailable_without_feature() {
    #[cfg(not(feature = "experimental-rns-data"))]
    {
        let request = [
            0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x01, 0x03,
            0xa0,
        ];
        assert_eq!(
            decode_request(&request),
            Err(DecodeError::UnsupportedOperation(0xf001))
        );

        let response = [
            0xa4, 0x00, 0xa2, 0x00, 0x01, 0x01, 0x00, 0x01, 0x09, 0x02, 0x19, 0xf0, 0x01, 0x03,
            0xa1, 0x00, 0x01,
        ];
        assert_eq!(
            decode_response(&response),
            Err(DecodeError::UnsupportedResponseKind(0xf001))
        );
    }

    assert_eq!(OP_SUBMISSION_STATUS, 0x0002);
}
