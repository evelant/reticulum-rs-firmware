extern crate std;

use reticulum_device_api_framing::{
    AUTH_TAG_LENGTH, DecodeEvent, FramedRecord, PAYLOAD_CAPACITY, PayloadLength, Record,
    SESSION_ID_LENGTH, StreamDecoder,
};

use crate::{
    ControlRequest, ControlResponse, DecodeError, InitializationStatus, InitializeResult,
    RECORD_KIND_INITIALIZE_REQUEST, RECORD_KIND_INITIALIZE_RESPONSE, RECORD_KIND_STATUS_REQUEST,
    RECORD_KIND_STATUS_RESPONSE,
};

const STATUSES: [InitializationStatus; 5] = [
    InitializationStatus::Unavailable,
    InitializationStatus::InitializationRequired,
    InitializationStatus::InFlight,
    InitializationStatus::Completed,
    InitializationStatus::Blocked,
];

const RESULTS: [InitializeResult; 6] = [
    InitializeResult::Completed,
    InitializeResult::Retrying,
    InitializeResult::PhysicalPresenceRequired,
    InitializeResult::Refused,
    InitializeResult::Blocked,
    InitializeResult::Unavailable,
];

fn record(
    kind: u8,
    session_id: [u8; SESSION_ID_LENGTH],
    sequence: u64,
    payload: &[u8],
    authentication_tag: [u8; AUTH_TAG_LENGTH],
) -> Record {
    let mut owned_payload = [0_u8; PAYLOAD_CAPACITY];
    owned_payload[..payload.len()].copy_from_slice(payload);
    Record::new(
        kind,
        session_id,
        sequence,
        PayloadLength::new(payload.len()).expect("test payload must fit"),
        owned_payload,
        authentication_tag,
    )
}

fn decode_wire(frame: &FramedRecord) -> Record {
    let mut decoder = StreamDecoder::new();
    let mut decoded = None;
    for byte in frame.encoded() {
        match decoder.push(*byte) {
            DecodeEvent::Pending => {}
            DecodeEvent::Record(record) => {
                assert!(decoded.is_none(), "one frame produced multiple records");
                decoded = Some(record);
            }
            DecodeEvent::MalformedCobs => panic!("canonical frame produced malformed COBS"),
            DecodeEvent::MalformedRecord(error) => {
                panic!("canonical frame produced malformed record: {error:?}")
            }
            DecodeEvent::Overflow => panic!("canonical bounded frame overflowed"),
        }
    }
    decoded.expect("complete frame produced no record")
}

#[test]
fn record_kinds_and_all_public_codes_are_frozen() {
    assert_eq!(RECORD_KIND_STATUS_REQUEST, 0x20);
    assert_eq!(RECORD_KIND_STATUS_RESPONSE, 0x21);
    assert_eq!(RECORD_KIND_INITIALIZE_REQUEST, 0x22);
    assert_eq!(RECORD_KIND_INITIALIZE_RESPONSE, 0x23);

    for (code, status) in STATUSES.into_iter().enumerate() {
        assert_eq!(status.code(), code as u8);
        assert_eq!(InitializationStatus::from_code(code as u8), Some(status));
    }
    for code in 5_u8..=u8::MAX {
        assert_eq!(InitializationStatus::from_code(code), None);
    }

    for (code, result) in RESULTS.into_iter().enumerate() {
        assert_eq!(result.code(), code as u8);
        assert_eq!(InitializeResult::from_code(code as u8), Some(result));
    }
    for code in 6_u8..=u8::MAX {
        assert_eq!(InitializeResult::from_code(code), None);
    }
}

#[test]
fn both_request_types_round_trip_and_preserve_arbitrary_sequences() {
    let requests = [
        ControlRequest::status(0),
        ControlRequest::status(u64::MAX),
        ControlRequest::initialize(0x8877_6655_4433_2211),
    ];

    for request in requests {
        let expected = request;
        let expected_sequence = request.sequence();
        let record = request.into_record();
        assert_eq!(record.sequence(), expected_sequence);
        assert_eq!(record.session_id(), &[0; SESSION_ID_LENGTH]);
        assert_eq!(record.authentication_tag(), &[0; AUTH_TAG_LENGTH]);
        assert_eq!(record.payload(), &[]);
        let framed = FramedRecord::encode(&record).expect("bounded request must frame");
        assert_eq!(
            ControlRequest::from_record(decode_wire(&framed)),
            Ok(expected)
        );
    }
}

#[test]
fn every_response_code_round_trips_through_exact_stream_framing() {
    for (index, status) in STATUSES.into_iter().enumerate() {
        let response = ControlResponse::status(0x1000 + index as u64, status);
        let record = response.into_record();
        assert_eq!(record.kind(), RECORD_KIND_STATUS_RESPONSE);
        assert_eq!(record.payload(), &[status.code()]);
        assert_eq!(record.session_id(), &[0; SESSION_ID_LENGTH]);
        assert_eq!(record.authentication_tag(), &[0; AUTH_TAG_LENGTH]);
        let framed = FramedRecord::encode(&record).expect("bounded response must frame");
        assert_eq!(
            ControlResponse::from_record(decode_wire(&framed)),
            Ok(response)
        );
    }

    for (index, result) in RESULTS.into_iter().enumerate() {
        let response = ControlResponse::initialize(u64::MAX - index as u64, result);
        let record = response.into_record();
        assert_eq!(record.kind(), RECORD_KIND_INITIALIZE_RESPONSE);
        assert_eq!(record.payload(), &[result.code()]);
        let framed = FramedRecord::encode(&record).expect("bounded response must frame");
        assert_eq!(
            ControlResponse::from_record(decode_wire(&framed)),
            Ok(response)
        );
    }
}

#[test]
fn empty_status_request_has_the_exact_cobs_wire_form() {
    let record = ControlRequest::status(0).into_record();
    let framed = FramedRecord::encode(&record).expect("bounded request must frame");

    assert_eq!(framed.length(), 53);
    assert_eq!(
        &framed.encoded()[..8],
        &[0, 7, b'R', b'D', b'A', b'1', 1, RECORD_KIND_STATUS_REQUEST]
    );
    assert_eq!(&framed.encoded()[8..52], &[1; 44]);
    assert_eq!(framed.encoded()[52], 0);
    assert_eq!(
        ControlRequest::from_record(decode_wire(&framed)),
        Ok(ControlRequest::status(0))
    );
}

#[test]
fn request_decode_rejects_nonzero_pre_auth_fields_and_wrong_shapes() {
    let cases = [
        (
            record(
                RECORD_KIND_STATUS_REQUEST,
                [1; SESSION_ID_LENGTH],
                7,
                &[],
                [0; AUTH_TAG_LENGTH],
            ),
            DecodeError::NonzeroSessionId,
        ),
        (
            record(
                RECORD_KIND_INITIALIZE_REQUEST,
                [0; SESSION_ID_LENGTH],
                7,
                &[],
                [1; AUTH_TAG_LENGTH],
            ),
            DecodeError::NonzeroAuthenticationTag,
        ),
        (
            record(
                RECORD_KIND_STATUS_REQUEST,
                [0; SESSION_ID_LENGTH],
                7,
                &[0],
                [0; AUTH_TAG_LENGTH],
            ),
            DecodeError::WrongPayloadShape,
        ),
        (
            record(0xff, [0; SESSION_ID_LENGTH], 7, &[], [0; AUTH_TAG_LENGTH]),
            DecodeError::UnknownKind,
        ),
        (
            record(
                RECORD_KIND_STATUS_RESPONSE,
                [0; SESSION_ID_LENGTH],
                7,
                &[0],
                [0; AUTH_TAG_LENGTH],
            ),
            DecodeError::UnknownKind,
        ),
    ];

    for (record, expected) in cases {
        assert_eq!(ControlRequest::from_record(record), Err(expected));
    }
}

#[test]
fn response_decode_rejects_nonzero_pre_auth_fields_and_wrong_shapes() {
    let cases = [
        (
            record(
                RECORD_KIND_STATUS_RESPONSE,
                [1; SESSION_ID_LENGTH],
                9,
                &[0],
                [0; AUTH_TAG_LENGTH],
            ),
            DecodeError::NonzeroSessionId,
        ),
        (
            record(
                RECORD_KIND_INITIALIZE_RESPONSE,
                [0; SESSION_ID_LENGTH],
                9,
                &[0],
                [1; AUTH_TAG_LENGTH],
            ),
            DecodeError::NonzeroAuthenticationTag,
        ),
        (
            record(
                RECORD_KIND_STATUS_RESPONSE,
                [0; SESSION_ID_LENGTH],
                9,
                &[],
                [0; AUTH_TAG_LENGTH],
            ),
            DecodeError::WrongPayloadShape,
        ),
        (
            record(
                RECORD_KIND_INITIALIZE_RESPONSE,
                [0; SESSION_ID_LENGTH],
                9,
                &[0, 1],
                [0; AUTH_TAG_LENGTH],
            ),
            DecodeError::WrongPayloadShape,
        ),
        (
            record(0xfe, [0; SESSION_ID_LENGTH], 9, &[0], [0; AUTH_TAG_LENGTH]),
            DecodeError::UnknownKind,
        ),
        (
            record(
                RECORD_KIND_STATUS_REQUEST,
                [0; SESSION_ID_LENGTH],
                9,
                &[],
                [0; AUTH_TAG_LENGTH],
            ),
            DecodeError::UnknownKind,
        ),
    ];

    for (record, expected) in cases {
        assert_eq!(ControlResponse::from_record(record), Err(expected));
    }
}

#[test]
fn response_decode_rejects_every_unassigned_status_and_result_code() {
    for code in 5_u8..=u8::MAX {
        let record = record(
            RECORD_KIND_STATUS_RESPONSE,
            [0; SESSION_ID_LENGTH],
            11,
            &[code],
            [0; AUTH_TAG_LENGTH],
        );
        assert_eq!(
            ControlResponse::from_record(record),
            Err(DecodeError::UnknownCode)
        );
    }

    for code in 6_u8..=u8::MAX {
        let record = record(
            RECORD_KIND_INITIALIZE_RESPONSE,
            [0; SESSION_ID_LENGTH],
            12,
            &[code],
            [0; AUTH_TAG_LENGTH],
        );
        assert_eq!(
            ControlResponse::from_record(record),
            Err(DecodeError::UnknownCode)
        );
    }
}

#[test]
fn framing_faults_remain_owned_by_the_framing_decoder() {
    let record = ControlResponse::initialize(77, InitializeResult::Retrying).into_record();
    let framed = FramedRecord::encode(&record).expect("bounded response must frame");
    let mut corrupted = std::vec::Vec::from(framed.encoded());
    corrupted[1] = 0;

    let mut decoder = StreamDecoder::new();
    let mut observed_fault = false;
    for byte in corrupted {
        observed_fault |= matches!(
            decoder.push(byte),
            DecodeEvent::MalformedCobs | DecodeEvent::MalformedRecord(_)
        );
    }
    assert!(observed_fault);
}
