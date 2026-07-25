extern crate std;

use reticulum_device_api_framing::{
    AUTH_TAG_LENGTH, DecodeEvent, FramedRecord, PAYLOAD_CAPACITY, PayloadLength, Record,
    SESSION_ID_LENGTH, StreamDecoder,
};

use crate::{
    ABORT_CURRENT_REQUEST_LENGTH, ACTIVATE_REQUEST_LENGTH, ACTIVATE_SUCCESS_LENGTH,
    AbortCurrentRequest, AbortCurrentResponse, AbortResult, ActivateFailure, ActivateRequest,
    ActivateResponse, ActivateResult, BEGIN_OFFER_LENGTH, BearerBinding, BeginOffer, BeginRequest,
    BeginResponse, BeginResult, ClientProof, ConnectionId, CredentialGeneration, CredentialId,
    DecodeError, DeviceChallenge, DeviceId, InvalidField, PROOF_CHALLENGE_LENGTH,
    PROOF_START_REQUEST_LENGTH, PROOF_SUITE, PROTOCOL_MAJOR, PROTOCOL_MINOR, PairingFailure,
    PairingPsk, PairingRequest, PairingResponse, PairingTranscript, ProofChallenge,
    ProofStartRequest, ProofStartResponse, ProofStartResult, RECORD_KIND_ABORT_CURRENT_REQUEST,
    RECORD_KIND_ABORT_CURRENT_RESPONSE, RECORD_KIND_ACTIVATE_REQUEST,
    RECORD_KIND_ACTIVATE_RESPONSE, RECORD_KIND_BEGIN_REQUEST, RECORD_KIND_BEGIN_RESPONSE,
    RECORD_KIND_PROOF_START_REQUEST, RECORD_KIND_PROOF_START_RESPONSE, WindowId,
    crypto::transcript_hash_for_test,
};

use core::mem::needs_drop;

const DEVICE_ID_BYTES: [u8; 16] = [
    0x65, 0x32, 0x39, 0x30, 0x2d, 0x61, 0x70, 0x69, 0x2d, 0x31, 0xac, 0xa7, 0x04, 0xe1, 0x3e, 0x88,
];
const CREDENTIAL_ID_BYTES: [u8; 16] = [
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
];
const GENERATION: u64 = 0x0102_0304_0506_0708;
const ACTIVATED_GENERATION: u64 = GENERATION + 1;
const CONNECTION_ID: u64 = 0x1112_1314_1516_1718;
const WINDOW_ID: u64 = 0x2122_2324_2526_2728;
const PSK_BYTES: [u8; 32] = ascending::<32>(0x20);
const CLIENT_NONCE: [u8; 32] = ascending::<32>(0x40);
const DEVICE_CHALLENGE: [u8; 32] = ascending::<32>(0x60);

const BEGIN_REQUEST_WIRE: &str = "0007524441310124010101010101010101010101010101010101010101010101010101010101010101010101010101010101010100";
const BEGIN_RESPONSE_WIRE: &str = "00075244413101250101010101010101010101010101010101010101010101010102580101010101010101020101010202020149653239302d6170692d31aca704e13e88101112131415161718191a1b1c1d1e1f0807060504030201202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f0101010101010101010101010101010100";
const PROOF_REQUEST_WIRE: &str = "0007524441310126010101010101010101010101010101010102010101010101010240020101010202020139101112131415161718191a1b1c1d1e1f0807060504030201404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f0101010101010101010101010101010100";
const PROOF_RESPONSE_WIRE: &str = "00075244413101270101010101010101010101010101010101020101010101010102680101010101010101020101010202020159653239302d6170692d31aca704e13e8818171615141312112827262524232221101112131415161718191a1b1c1d1e1f0807060504030201606162636465666768696a6b6c6d6e6f707172737475767778797a7b7c7d7e7f0101010101010101010101010101010100";
const ACTIVATE_REQUEST_WIRE: &str = "000752444131012801010101010101010101010101010101010202010101010101023839101112131415161718191a1b1c1d1e1f08070605040302016a3855c27a2d61fcd00e39a85c419fb52a09043d51f3333c7989ec832b51eb5d0101010101010101010101010101010100";
const ACTIVATE_RESPONSE_WIRE: &str = "0007524441310129010101010101010101010101010101010102020101010101010240010101010101010139101112131415161718191a1b1c1d1e1f090706050403020141a2f9c1ba3e8ab6d96d939ec1b6d6cd7f126ad9e9f3be275b3278a6031b88690101010101010101010101010101010100";
const ABORT_REQUEST_WIRE: &str = "000752444131012a010101010101010101010101010101010102030101010101010101010101010101010101010101010101010100";
const ABORT_RESPONSE_WIRE: &str = "000752444131012b01010101010101010101010101010101010203010101010101020101010101010101010101010101010101010100";
const TRANSCRIPT_HASH: [u8; 32] = [
    0xea, 0x2e, 0x28, 0xd9, 0xe1, 0x86, 0x96, 0xdf, 0x5b, 0x3f, 0xb8, 0xde, 0x34, 0x16, 0xcc, 0x6b,
    0x2e, 0x98, 0xff, 0xae, 0x1e, 0x8f, 0xcd, 0xa8, 0xc1, 0x50, 0x99, 0x0e, 0x60, 0x53, 0x51, 0xcb,
];
const CLIENT_PROOF: [u8; 32] = [
    0x6a, 0x38, 0x55, 0xc2, 0x7a, 0x2d, 0x61, 0xfc, 0xd0, 0x0e, 0x39, 0xa8, 0x5c, 0x41, 0x9f, 0xb5,
    0x2a, 0x09, 0x04, 0x3d, 0x51, 0xf3, 0x33, 0x3c, 0x79, 0x89, 0xec, 0x83, 0x2b, 0x51, 0xeb, 0x5d,
];
const ACTIVATION_CONFIRMATION: [u8; 32] = [
    0x41, 0xa2, 0xf9, 0xc1, 0xba, 0x3e, 0x8a, 0xb6, 0xd9, 0x6d, 0x93, 0x9e, 0xc1, 0xb6, 0xd6, 0xcd,
    0x7f, 0x12, 0x6a, 0xd9, 0xe9, 0xf3, 0xbe, 0x27, 0x5b, 0x32, 0x78, 0xa6, 0x03, 0x1b, 0x88, 0x69,
];

const fn ascending<const N: usize>(start: u8) -> [u8; N] {
    let mut bytes = [0_u8; N];
    let mut index = 0;
    while index < N {
        bytes[index] = start.wrapping_add(index as u8);
        index += 1;
    }
    bytes
}

fn device_id() -> DeviceId {
    DeviceId::new(DEVICE_ID_BYTES).unwrap()
}

fn credential_id() -> CredentialId {
    CredentialId::new(CREDENTIAL_ID_BYTES)
}

fn generation() -> CredentialGeneration {
    CredentialGeneration::new(GENERATION)
}

fn activated_generation() -> CredentialGeneration {
    CredentialGeneration::new(ACTIVATED_GENERATION)
}

fn proof_request() -> ProofStartRequest {
    ProofStartRequest::new(
        BearerBinding::UsbSerialJtag,
        1,
        credential_id(),
        generation(),
        CLIENT_NONCE,
    )
    .unwrap()
}

fn proof_challenge() -> ProofChallenge {
    ProofChallenge::new(
        BearerBinding::UsbSerialJtag,
        device_id(),
        ConnectionId::new(CONNECTION_ID).unwrap(),
        WindowId::new(WINDOW_ID).unwrap(),
        credential_id(),
        generation(),
        DeviceChallenge::new(DEVICE_CHALLENGE).unwrap(),
    )
    .unwrap()
}

fn transcript() -> PairingTranscript {
    PairingTranscript::new(&proof_request(), &proof_challenge()).unwrap()
}

fn psk() -> PairingPsk {
    PairingPsk::new(PSK_BYTES).unwrap()
}

fn record(
    kind: u8,
    session_id: [u8; SESSION_ID_LENGTH],
    sequence: u64,
    payload: &[u8],
    tag: [u8; AUTH_TAG_LENGTH],
) -> Record {
    let mut owner = [0_u8; PAYLOAD_CAPACITY];
    owner[..payload.len()].copy_from_slice(payload);
    Record::new(
        kind,
        session_id,
        sequence,
        PayloadLength::new(payload.len()).unwrap(),
        owner,
        tag,
    )
}

fn decode_wire(bytes: &[u8]) -> Record {
    let mut decoder = StreamDecoder::new();
    let mut decoded = None;
    for byte in bytes {
        match decoder.push(*byte) {
            DecodeEvent::Pending => {}
            DecodeEvent::Record(record) => {
                assert!(decoded.is_none());
                decoded = Some(record);
            }
            _ => panic!("canonical wire rejected"),
        }
    }
    decoded.expect("complete frame produced no record")
}

fn assert_wire(record: &Record, expected_hex: &str) {
    let frame = FramedRecord::encode(record).unwrap();
    assert_eq!(frame.encoded(), hex::decode(expected_hex).unwrap());
    let decoded = decode_wire(frame.encoded());
    assert_eq!(decoded.kind(), record.kind());
    assert_eq!(decoded.sequence(), record.sequence());
    assert_eq!(decoded.session_id(), record.session_id());
    assert_eq!(decoded.payload(), record.payload());
    assert_eq!(decoded.authentication_tag(), record.authentication_tag());
}

#[test]
fn constants_and_complete_result_vocabularies_are_frozen() {
    assert_eq!(PROTOCOL_MAJOR, 1);
    assert_eq!(PROTOCOL_MINOR, 0);
    assert_eq!(PROOF_SUITE, 2);
    assert_eq!(BearerBinding::UsbSerialJtag.code(), 1);
    assert_eq!(BearerBinding::BleGatt.code(), 2);
    assert_eq!(
        BearerBinding::from_code(1),
        Some(BearerBinding::UsbSerialJtag)
    );
    assert_eq!(BearerBinding::from_code(2), Some(BearerBinding::BleGatt));
    assert_eq!(BearerBinding::from_code(0), None);
    assert_eq!(
        [
            RECORD_KIND_BEGIN_REQUEST,
            RECORD_KIND_BEGIN_RESPONSE,
            RECORD_KIND_PROOF_START_REQUEST,
            RECORD_KIND_PROOF_START_RESPONSE,
            RECORD_KIND_ACTIVATE_REQUEST,
            RECORD_KIND_ACTIVATE_RESPONSE,
            RECORD_KIND_ABORT_CURRENT_REQUEST,
            RECORD_KIND_ABORT_CURRENT_RESPONSE,
        ],
        [0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b]
    );
    assert_eq!(BEGIN_OFFER_LENGTH, 88);
    assert_eq!(PROOF_START_REQUEST_LENGTH, 64);
    assert_eq!(PROOF_CHALLENGE_LENGTH, 104);
    assert_eq!(ACTIVATE_REQUEST_LENGTH, 56);
    assert_eq!(ACTIVATE_SUCCESS_LENGTH, 64);
    assert_eq!(ABORT_CURRENT_REQUEST_LENGTH, 0);

    let pairing = [
        PairingFailure::PhysicalPresenceRequired,
        PairingFailure::Refused,
        PairingFailure::Blocked,
        PairingFailure::Unavailable,
    ];
    for (index, value) in pairing.into_iter().enumerate() {
        assert_eq!(value.code(), index as u8 + 1);
        assert_eq!(PairingFailure::from_code(value.code()), Some(value));
    }
    assert_eq!(PairingFailure::from_code(0), None);
    assert_eq!(PairingFailure::from_code(5), None);

    let activate = [
        ActivateFailure::ProofRejected,
        ActivateFailure::Refused,
        ActivateFailure::Blocked,
        ActivateFailure::Unavailable,
    ];
    for (index, value) in activate.into_iter().enumerate() {
        assert_eq!(value.code(), index as u8 + 1);
        assert_eq!(ActivateFailure::from_code(value.code()), Some(value));
    }
    assert_eq!(ActivateFailure::from_code(0), None);
    assert_eq!(ActivateFailure::from_code(5), None);

    let abort = [
        AbortResult::Aborted,
        AbortResult::PhysicalPresenceRequired,
        AbortResult::Refused,
        AbortResult::Blocked,
        AbortResult::Unavailable,
    ];
    for (index, value) in abort.into_iter().enumerate() {
        assert_eq!(value.code(), index as u8);
        assert_eq!(AbortResult::from_code(value.code()), Some(value));
    }
    assert_eq!(AbortResult::from_code(5), None);
}

#[test]
fn independent_record_vectors_match_every_successful_flight() {
    let begin_request = BeginRequest::new(0).into_record();
    assert_wire(&begin_request, BEGIN_REQUEST_WIRE);
    assert!(matches!(
        PairingRequest::from_record(BearerBinding::UsbSerialJtag, decode_wire(&hex::decode(BEGIN_REQUEST_WIRE).unwrap())),
        Ok(PairingRequest::Begin(request)) if request.sequence() == 0
    ));

    let offer = BeginOffer::after_pending_commit(
        BearerBinding::UsbSerialJtag,
        device_id(),
        credential_id(),
        generation(),
        psk(),
    )
    .unwrap();
    let begin_response = BeginResponse::offered(0, offer).into_record();
    assert_wire(&begin_response, BEGIN_RESPONSE_WIRE);
    let decoded = PairingResponse::from_record(
        BearerBinding::UsbSerialJtag,
        decode_wire(&hex::decode(BEGIN_RESPONSE_WIRE).unwrap()),
    )
    .unwrap();
    match decoded {
        PairingResponse::Begin(response) => {
            assert_eq!(response.sequence(), 0);
            assert_eq!(response.result(), BeginResult::Offered);
            let offer = response.offer().unwrap();
            assert_eq!(offer.device_id(), device_id());
            assert_eq!(offer.credential_id(), credential_id());
            assert_eq!(offer.generation(), generation());
            assert_eq!(offer.psk().as_bytes(), &PSK_BYTES);
        }
        _ => panic!("wrong response union variant"),
    }

    let proof_request_record = proof_request().into_record();
    assert_wire(&proof_request_record, PROOF_REQUEST_WIRE);
    let decoded = PairingRequest::from_record(
        BearerBinding::UsbSerialJtag,
        decode_wire(&hex::decode(PROOF_REQUEST_WIRE).unwrap()),
    )
    .unwrap();
    match decoded {
        PairingRequest::ProofStart(request) => {
            assert_eq!(request.sequence(), 1);
            assert_eq!(request.credential_id(), credential_id());
            assert_eq!(request.generation(), generation());
            assert_eq!(request.client_nonce(), &CLIENT_NONCE);
        }
        _ => panic!("wrong request union variant"),
    }

    let proof_response = ProofStartResponse::challenge(1, proof_challenge()).into_record();
    assert_wire(&proof_response, PROOF_RESPONSE_WIRE);
    let decoded = PairingResponse::from_record(
        BearerBinding::UsbSerialJtag,
        decode_wire(&hex::decode(PROOF_RESPONSE_WIRE).unwrap()),
    )
    .unwrap();
    match decoded {
        PairingResponse::ProofStart(response) => {
            assert_eq!(response.sequence(), 1);
            assert_eq!(response.result(), ProofStartResult::Challenge);
            let challenge = response.proof_challenge().unwrap();
            assert_eq!(challenge.device_id(), device_id());
            assert_eq!(challenge.connection_id().get(), CONNECTION_ID);
            assert_eq!(challenge.window_id().get(), WINDOW_ID);
            assert_eq!(challenge.credential_id(), credential_id());
            assert_eq!(challenge.generation(), generation());
            assert_eq!(challenge.challenge().as_bytes(), &DEVICE_CHALLENGE);
        }
        _ => panic!("wrong response union variant"),
    }

    let activate_request = ActivateRequest::new(
        2,
        credential_id(),
        generation(),
        ClientProof::from_bytes(CLIENT_PROOF),
    )
    .unwrap()
    .into_record();
    assert_wire(&activate_request, ACTIVATE_REQUEST_WIRE);
    let decoded = PairingRequest::from_record(
        BearerBinding::UsbSerialJtag,
        decode_wire(&hex::decode(ACTIVATE_REQUEST_WIRE).unwrap()),
    )
    .unwrap();
    match decoded {
        PairingRequest::Activate(request) => {
            assert_eq!(request.sequence(), 2);
            assert_eq!(request.credential_id(), credential_id());
            assert_eq!(request.generation(), generation());
            assert_eq!(request.proof().as_bytes(), &CLIENT_PROOF);
        }
        _ => panic!("wrong request union variant"),
    }

    let activate_response = ActivateResponse::activated_after_durable_commit(
        2,
        crate::ActivationConfirmation::from_bytes(
            credential_id(),
            activated_generation(),
            ACTIVATION_CONFIRMATION,
        )
        .unwrap(),
    )
    .into_record();
    assert_wire(&activate_response, ACTIVATE_RESPONSE_WIRE);
    let decoded = PairingResponse::from_record(
        BearerBinding::UsbSerialJtag,
        decode_wire(&hex::decode(ACTIVATE_RESPONSE_WIRE).unwrap()),
    )
    .unwrap();
    match decoded {
        PairingResponse::Activate(response) => {
            assert_eq!(response.sequence(), 2);
            assert_eq!(response.result(), ActivateResult::Activated);
            assert_eq!(response.credential_id(), Some(credential_id()));
            assert_eq!(response.generation(), Some(activated_generation()));
            assert_eq!(
                response.confirmation().unwrap().as_bytes(),
                &ACTIVATION_CONFIRMATION
            );
            assert!(response.verify_confirmation(&psk(), &transcript()));
        }
        _ => panic!("wrong response union variant"),
    }

    let abort_request = AbortCurrentRequest::new(3).into_record();
    assert_wire(&abort_request, ABORT_REQUEST_WIRE);
    assert!(matches!(
        PairingRequest::from_record(BearerBinding::UsbSerialJtag, decode_wire(&hex::decode(ABORT_REQUEST_WIRE).unwrap())),
        Ok(PairingRequest::AbortCurrent(request)) if request.sequence() == 3
    ));

    let abort_response = AbortCurrentResponse::new(3, AbortResult::Aborted).into_record();
    assert_wire(&abort_response, ABORT_RESPONSE_WIRE);
    assert!(matches!(
        PairingResponse::from_record(BearerBinding::UsbSerialJtag, decode_wire(&hex::decode(ABORT_RESPONSE_WIRE).unwrap())),
        Ok(PairingResponse::AbortCurrent(response))
            if response.sequence() == 3 && response.result() == AbortResult::Aborted
    ));
}

#[test]
fn ble_profile_round_trips_only_under_ble_and_changes_the_proof_transcript() {
    let offer = BeginOffer::after_pending_commit(
        BearerBinding::BleGatt,
        device_id(),
        credential_id(),
        generation(),
        psk(),
    )
    .unwrap();
    assert_eq!(offer.bearer(), BearerBinding::BleGatt);
    let begin = BeginResponse::offered(0, offer).into_record();
    assert_eq!(begin.payload()[14], BearerBinding::BleGatt.code());
    let decoded = PairingResponse::from_record(BearerBinding::BleGatt, begin).unwrap();
    assert!(matches!(
        decoded,
        PairingResponse::Begin(response)
            if response.offer().map(BeginOffer::bearer) == Some(BearerBinding::BleGatt)
    ));

    let wrong_bearer_begin = BeginResponse::offered(
        0,
        BeginOffer::after_pending_commit(
            BearerBinding::BleGatt,
            device_id(),
            credential_id(),
            generation(),
            psk(),
        )
        .unwrap(),
    )
    .into_record();
    assert_eq!(
        PairingResponse::from_record(BearerBinding::UsbSerialJtag, wrong_bearer_begin).err(),
        Some(DecodeError::UnsupportedBearer {
            observed: BearerBinding::BleGatt.code()
        })
    );

    let ble_request = ProofStartRequest::new(
        BearerBinding::BleGatt,
        1,
        credential_id(),
        generation(),
        CLIENT_NONCE,
    )
    .unwrap();
    let ble_challenge = ProofChallenge::new(
        BearerBinding::BleGatt,
        device_id(),
        ConnectionId::new(CONNECTION_ID).unwrap(),
        WindowId::new(WINDOW_ID).unwrap(),
        credential_id(),
        generation(),
        DeviceChallenge::new(DEVICE_CHALLENGE).unwrap(),
    )
    .unwrap();
    assert_eq!(ble_request.bearer(), BearerBinding::BleGatt);
    assert_eq!(ble_challenge.bearer(), BearerBinding::BleGatt);
    assert_eq!(
        ble_request.encode_payload()[6],
        BearerBinding::BleGatt.code()
    );
    assert_eq!(
        ble_challenge.encode_payload()[14],
        BearerBinding::BleGatt.code()
    );

    let ble_transcript = PairingTranscript::new(&ble_request, &ble_challenge).unwrap();
    assert_eq!(ble_transcript.bearer(), BearerBinding::BleGatt);
    let usb_transcript = transcript();
    assert_ne!(ble_transcript.as_bytes(), usb_transcript.as_bytes());
    assert_ne!(
        ClientProof::calculate(&psk(), &ble_transcript).as_bytes(),
        ClientProof::calculate(&psk(), &usb_transcript).as_bytes()
    );
    assert!(PairingTranscript::new(&ble_request, &proof_challenge()).is_err());
}

#[test]
fn independent_crypto_vectors_match_and_verify_both_directions() {
    let transcript = transcript();
    assert_eq!(transcript.as_bytes(), &TRANSCRIPT_HASH);
    let psk = psk();
    let proof = ClientProof::calculate(&psk, &transcript);
    assert_eq!(proof.as_bytes(), &CLIENT_PROOF);

    let expected_confirmation =
        crate::ActivationConfirmation::calculate(&psk, &transcript, &proof, activated_generation())
            .unwrap();
    assert_eq!(expected_confirmation.as_bytes(), &ACTIVATION_CONFIRMATION);
    assert!(expected_confirmation.verify(&psk, &transcript, &proof));

    let verified = ClientProof::from_bytes(CLIENT_PROOF)
        .verify(&psk, &transcript)
        .unwrap();
    let server_confirmation = verified
        .into_activation_confirmation(activated_generation(), &psk)
        .unwrap();
    assert_eq!(server_confirmation.as_bytes(), &ACTIVATION_CONFIRMATION);
    assert!(server_confirmation.verify(&psk, &transcript, &proof));

    let mut bad_proof = CLIENT_PROOF;
    bad_proof[31] ^= 1;
    assert!(
        ClientProof::from_bytes(bad_proof)
            .verify(&psk, &transcript)
            .is_err()
    );
    let mut bad_confirmation = ACTIVATION_CONFIRMATION;
    bad_confirmation[0] ^= 1;
    assert!(
        !crate::ActivationConfirmation::from_bytes(
            credential_id(),
            activated_generation(),
            bad_confirmation,
        )
        .unwrap()
        .verify(&psk, &transcript, &proof)
    );
}

#[test]
fn every_coarse_failure_round_trips_with_echoed_arbitrary_sequences() {
    let pairing = [
        PairingFailure::PhysicalPresenceRequired,
        PairingFailure::Refused,
        PairingFailure::Blocked,
        PairingFailure::Unavailable,
    ];
    for (index, failure) in pairing.into_iter().enumerate() {
        let sequence = u64::MAX - index as u64;
        let begin = BeginResponse::failure(sequence, failure).into_record();
        assert_eq!(begin.payload(), &[failure.code()]);
        match PairingResponse::from_record(
            BearerBinding::UsbSerialJtag,
            decode_wire(FramedRecord::encode(&begin).unwrap().encoded()),
        )
        .unwrap()
        {
            PairingResponse::Begin(response) => {
                assert_eq!(response.sequence(), sequence);
                assert_eq!(response.failure_kind(), Some(failure));
                assert_eq!(response.result().code(), failure.code());
            }
            _ => panic!("wrong union variant"),
        }

        let proof = ProofStartResponse::failure(sequence, failure).into_record();
        match PairingResponse::from_record(
            BearerBinding::UsbSerialJtag,
            decode_wire(FramedRecord::encode(&proof).unwrap().encoded()),
        )
        .unwrap()
        {
            PairingResponse::ProofStart(response) => {
                assert_eq!(response.sequence(), sequence);
                assert_eq!(response.failure_kind(), Some(failure));
                assert_eq!(response.result().code(), failure.code());
            }
            _ => panic!("wrong union variant"),
        }
    }

    let activation = [
        ActivateFailure::ProofRejected,
        ActivateFailure::Refused,
        ActivateFailure::Blocked,
        ActivateFailure::Unavailable,
    ];
    for (index, failure) in activation.into_iter().enumerate() {
        let sequence = 0x8000 + index as u64;
        let record = ActivateResponse::failure(sequence, failure).into_record();
        match PairingResponse::from_record(
            BearerBinding::UsbSerialJtag,
            decode_wire(FramedRecord::encode(&record).unwrap().encoded()),
        )
        .unwrap()
        {
            PairingResponse::Activate(response) => {
                assert_eq!(response.sequence(), sequence);
                assert_eq!(response.failure_kind(), Some(failure));
                assert_eq!(response.result().code(), failure.code());
            }
            _ => panic!("wrong union variant"),
        }
    }

    for code in 0_u8..=4 {
        let result = AbortResult::from_code(code).unwrap();
        let record = AbortCurrentResponse::new(code as u64, result).into_record();
        assert!(matches!(
            PairingResponse::from_record(BearerBinding::UsbSerialJtag, decode_wire(
                FramedRecord::encode(&record).unwrap().encoded()
            )),
            Ok(PairingResponse::AbortCurrent(response))
                if response.sequence() == code as u64 && response.result() == result
        ));
    }
}

#[test]
fn request_union_rejects_nonzero_pre_authentication_fields_kinds_and_lengths() {
    let cases = [
        (
            record(
                RECORD_KIND_BEGIN_REQUEST,
                [1; SESSION_ID_LENGTH],
                7,
                &[],
                [0; AUTH_TAG_LENGTH],
            ),
            DecodeError::NonzeroSessionId,
        ),
        (
            record(
                RECORD_KIND_BEGIN_REQUEST,
                [0; SESSION_ID_LENGTH],
                7,
                &[],
                [1; AUTH_TAG_LENGTH],
            ),
            DecodeError::NonzeroAuthenticationTag,
        ),
        (
            record(0xff, [0; 16], 7, &[], [0; 16]),
            DecodeError::UnknownKind,
        ),
        (
            record(RECORD_KIND_BEGIN_RESPONSE, [0; 16], 7, &[1], [0; 16]),
            DecodeError::UnknownKind,
        ),
        (
            record(RECORD_KIND_BEGIN_REQUEST, [0; 16], 7, &[0], [0; 16]),
            DecodeError::WrongPayloadLength {
                expected: 0,
                observed: 1,
            },
        ),
        (
            record(
                RECORD_KIND_PROOF_START_REQUEST,
                [0; 16],
                7,
                &[0; PROOF_START_REQUEST_LENGTH - 1],
                [0; 16],
            ),
            DecodeError::WrongPayloadLength {
                expected: PROOF_START_REQUEST_LENGTH,
                observed: PROOF_START_REQUEST_LENGTH - 1,
            },
        ),
        (
            record(
                RECORD_KIND_ACTIVATE_REQUEST,
                [0; 16],
                7,
                &[0; ACTIVATE_REQUEST_LENGTH + 1],
                [0; 16],
            ),
            DecodeError::WrongPayloadLength {
                expected: ACTIVATE_REQUEST_LENGTH,
                observed: ACTIVATE_REQUEST_LENGTH + 1,
            },
        ),
        (
            record(RECORD_KIND_ABORT_CURRENT_REQUEST, [0; 16], 7, &[0], [0; 16]),
            DecodeError::WrongPayloadLength {
                expected: 0,
                observed: 1,
            },
        ),
    ];
    for (record, expected) in cases {
        assert_eq!(
            PairingRequest::from_record(BearerBinding::UsbSerialJtag, record).err(),
            Some(expected)
        );
    }
}

#[test]
fn response_union_rejects_nonzero_pre_authentication_fields_shapes_and_codes() {
    let cases = [
        (
            record(RECORD_KIND_BEGIN_RESPONSE, [1; 16], 9, &[1], [0; 16]),
            DecodeError::NonzeroSessionId,
        ),
        (
            record(RECORD_KIND_BEGIN_RESPONSE, [0; 16], 9, &[1], [1; 16]),
            DecodeError::NonzeroAuthenticationTag,
        ),
        (
            record(0xff, [0; 16], 9, &[1], [0; 16]),
            DecodeError::UnknownKind,
        ),
        (
            record(RECORD_KIND_BEGIN_REQUEST, [0; 16], 9, &[], [0; 16]),
            DecodeError::UnknownKind,
        ),
        (
            record(RECORD_KIND_BEGIN_RESPONSE, [0; 16], 9, &[], [0; 16]),
            DecodeError::WrongPayloadLength {
                expected: 1,
                observed: 0,
            },
        ),
        (
            record(RECORD_KIND_BEGIN_RESPONSE, [0; 16], 9, &[0], [0; 16]),
            DecodeError::WrongResultShape,
        ),
        (
            record(RECORD_KIND_PROOF_START_RESPONSE, [0; 16], 9, &[0], [0; 16]),
            DecodeError::WrongResultShape,
        ),
        (
            record(RECORD_KIND_ACTIVATE_RESPONSE, [0; 16], 9, &[0], [0; 16]),
            DecodeError::WrongResultShape,
        ),
        (
            record(RECORD_KIND_ABORT_CURRENT_RESPONSE, [0; 16], 9, &[], [0; 16]),
            DecodeError::WrongPayloadLength {
                expected: 1,
                observed: 0,
            },
        ),
    ];
    for (record, expected) in cases {
        assert_eq!(
            PairingResponse::from_record(BearerBinding::UsbSerialJtag, record).err(),
            Some(expected)
        );
    }

    for kind in [
        RECORD_KIND_BEGIN_RESPONSE,
        RECORD_KIND_PROOF_START_RESPONSE,
        RECORD_KIND_ACTIVATE_RESPONSE,
        RECORD_KIND_ABORT_CURRENT_RESPONSE,
    ] {
        for code in 5_u8..=u8::MAX {
            assert_eq!(
                PairingResponse::from_record(
                    BearerBinding::UsbSerialJtag,
                    record(kind, [0; 16], 0, &[code], [0; 16])
                )
                .err(),
                Some(DecodeError::UnknownResult { observed: code })
            );
        }
    }
}

#[test]
fn profile_reserved_and_every_required_nonzero_field_are_validated() {
    let mut request_payload = proof_request().encode_payload();
    let profile_cases = [
        (0, 2, DecodeError::UnsupportedVersion { major: 2, minor: 0 }),
        (2, 1, DecodeError::UnsupportedVersion { major: 1, minor: 1 }),
        (4, 3, DecodeError::UnsupportedSuite { observed: 3 }),
        (6, 2, DecodeError::UnsupportedBearer { observed: 2 }),
        (7, 1, DecodeError::ReservedBitsSet),
    ];
    for (index, value, expected) in profile_cases {
        let old = request_payload[index];
        request_payload[index] = value;
        assert_eq!(
            PairingRequest::from_record(
                BearerBinding::UsbSerialJtag,
                record(
                    RECORD_KIND_PROOF_START_REQUEST,
                    [0; 16],
                    1,
                    &request_payload,
                    [0; 16]
                )
            )
            .err(),
            Some(expected)
        );
        request_payload[index] = old;
    }

    let zero_cases = [
        (8, 24, InvalidField::ZeroCredentialId),
        (24, 32, InvalidField::ZeroCredentialGeneration),
        (32, 64, InvalidField::ZeroClientNonce),
    ];
    for (start, end, field) in zero_cases {
        let mut payload = proof_request().encode_payload();
        payload[start..end].fill(0);
        assert_eq!(
            PairingRequest::from_record(
                BearerBinding::UsbSerialJtag,
                record(
                    RECORD_KIND_PROOF_START_REQUEST,
                    [0; 16],
                    1,
                    &payload,
                    [0; 16]
                )
            )
            .err(),
            Some(DecodeError::InvalidRequiredField(field))
        );
    }

    let mut activate = [0_u8; ACTIVATE_REQUEST_LENGTH];
    activate[..16].copy_from_slice(&CREDENTIAL_ID_BYTES);
    activate[16..24].copy_from_slice(&GENERATION.to_le_bytes());
    activate[24..].copy_from_slice(&CLIENT_PROOF);
    for (start, end, field) in [
        (0, 16, InvalidField::ZeroCredentialId),
        (16, 24, InvalidField::ZeroCredentialGeneration),
    ] {
        let mut payload = activate;
        payload[start..end].fill(0);
        assert_eq!(
            PairingRequest::from_record(
                BearerBinding::UsbSerialJtag,
                record(RECORD_KIND_ACTIVATE_REQUEST, [0; 16], 2, &payload, [0; 16])
            )
            .err(),
            Some(DecodeError::InvalidRequiredField(field))
        );
    }

    let mut begin = BeginOffer::after_pending_commit(
        BearerBinding::UsbSerialJtag,
        device_id(),
        credential_id(),
        generation(),
        psk(),
    )
    .unwrap()
    .encode_payload();
    for index in 1..8 {
        begin[index] = 1;
        assert_eq!(
            PairingResponse::from_record(
                BearerBinding::UsbSerialJtag,
                record(RECORD_KIND_BEGIN_RESPONSE, [0; 16], 0, &begin[..], [0; 16])
            )
            .err(),
            Some(DecodeError::ReservedBitsSet)
        );
        begin[index] = 0;
    }
    begin[15] = 1;
    assert_eq!(
        PairingResponse::from_record(
            BearerBinding::UsbSerialJtag,
            record(RECORD_KIND_BEGIN_RESPONSE, [0; 16], 0, &begin[..], [0; 16])
        )
        .err(),
        Some(DecodeError::ReservedBitsSet)
    );

    for (start, end, field) in [
        (16, 32, InvalidField::ZeroDeviceId),
        (32, 48, InvalidField::ZeroCredentialId),
        (48, 56, InvalidField::ZeroCredentialGeneration),
        (56, 88, InvalidField::ZeroPairingPsk),
    ] {
        let mut payload = BeginOffer::after_pending_commit(
            BearerBinding::UsbSerialJtag,
            device_id(),
            credential_id(),
            generation(),
            psk(),
        )
        .unwrap()
        .encode_payload();
        payload[start..end].fill(0);
        assert_eq!(
            PairingResponse::from_record(
                BearerBinding::UsbSerialJtag,
                record(
                    RECORD_KIND_BEGIN_RESPONSE,
                    [0; 16],
                    0,
                    &payload[..],
                    [0; 16]
                )
            )
            .err(),
            Some(DecodeError::InvalidRequiredField(field))
        );
    }

    for (start, end, field) in [
        (16, 32, InvalidField::ZeroDeviceId),
        (32, 40, InvalidField::ZeroConnectionId),
        (40, 48, InvalidField::ZeroWindowId),
        (48, 64, InvalidField::ZeroCredentialId),
        (64, 72, InvalidField::ZeroCredentialGeneration),
        (72, 104, InvalidField::ZeroDeviceChallenge),
    ] {
        let mut payload = proof_challenge().encode_payload();
        payload[start..end].fill(0);
        assert_eq!(
            PairingResponse::from_record(
                BearerBinding::UsbSerialJtag,
                record(
                    RECORD_KIND_PROOF_START_RESPONSE,
                    [0; 16],
                    1,
                    &payload[..],
                    [0; 16]
                )
            )
            .err(),
            Some(DecodeError::InvalidRequiredField(field))
        );
    }
}

#[test]
fn client_proof_changes_for_every_transcript_byte_role_and_length() {
    let request = proof_request();
    let challenge = proof_challenge();
    let request_payload = request.encode_payload();
    let challenge_payload = challenge.encode_payload();
    let canonical = PairingTranscript::new(&request, &challenge).unwrap();
    let psk = psk();
    let canonical_proof = ClientProof::calculate(&psk, &canonical);

    for index in 0..request_payload.len() {
        let mut changed = request_payload;
        changed[index] ^= 0x80;
        let transcript = transcript_hash_for_test(
            RECORD_KIND_PROOF_START_REQUEST,
            PROOF_START_REQUEST_LENGTH as u16,
            &changed,
            RECORD_KIND_PROOF_START_RESPONSE,
            PROOF_CHALLENGE_LENGTH as u16,
            &challenge_payload[..],
        );
        assert!(
            ClientProof::from_bytes(*canonical_proof.as_bytes())
                .verify(&psk, &transcript)
                .is_err(),
            "request payload byte {index} was not bound"
        );
    }

    for index in 0..challenge_payload.len() {
        let mut changed = proof_challenge().encode_payload();
        changed[index] ^= 0x80;
        let transcript = transcript_hash_for_test(
            RECORD_KIND_PROOF_START_REQUEST,
            PROOF_START_REQUEST_LENGTH as u16,
            &request_payload,
            RECORD_KIND_PROOF_START_RESPONSE,
            PROOF_CHALLENGE_LENGTH as u16,
            &changed[..],
        );
        assert!(
            ClientProof::from_bytes(*canonical_proof.as_bytes())
                .verify(&psk, &transcript)
                .is_err(),
            "challenge payload byte {index} was not bound"
        );
    }

    let metadata_mutations = [
        (
            RECORD_KIND_PROOF_START_REQUEST ^ 1,
            PROOF_START_REQUEST_LENGTH as u16,
            RECORD_KIND_PROOF_START_RESPONSE,
            PROOF_CHALLENGE_LENGTH as u16,
        ),
        (
            RECORD_KIND_PROOF_START_REQUEST,
            PROOF_START_REQUEST_LENGTH as u16 + 1,
            RECORD_KIND_PROOF_START_RESPONSE,
            PROOF_CHALLENGE_LENGTH as u16,
        ),
        (
            RECORD_KIND_PROOF_START_REQUEST,
            PROOF_START_REQUEST_LENGTH as u16,
            RECORD_KIND_PROOF_START_RESPONSE ^ 1,
            PROOF_CHALLENGE_LENGTH as u16,
        ),
        (
            RECORD_KIND_PROOF_START_REQUEST,
            PROOF_START_REQUEST_LENGTH as u16,
            RECORD_KIND_PROOF_START_RESPONSE,
            PROOF_CHALLENGE_LENGTH as u16 + 1,
        ),
    ];
    for (request_kind, request_length, response_kind, response_length) in metadata_mutations {
        let transcript = transcript_hash_for_test(
            request_kind,
            request_length,
            &request_payload,
            response_kind,
            response_length,
            &challenge_payload[..],
        );
        assert!(
            ClientProof::from_bytes(*canonical_proof.as_bytes())
                .verify(&psk, &transcript)
                .is_err()
        );
    }
}

#[test]
fn activation_confirmation_binds_transcript_and_every_client_proof_byte() {
    let transcript = transcript();
    let psk = psk();
    let proof = ClientProof::from_bytes(CLIENT_PROOF);
    let confirmation = crate::ActivationConfirmation::from_bytes(
        credential_id(),
        activated_generation(),
        ACTIVATION_CONFIRMATION,
    )
    .unwrap();
    assert!(confirmation.verify(&psk, &transcript, &proof));

    let request_payload = proof_request().encode_payload();
    let mut challenge_payload = proof_challenge().encode_payload();
    challenge_payload[72] ^= 1;
    let changed_transcript = transcript_hash_for_test(
        RECORD_KIND_PROOF_START_REQUEST,
        PROOF_START_REQUEST_LENGTH as u16,
        &request_payload,
        RECORD_KIND_PROOF_START_RESPONSE,
        PROOF_CHALLENGE_LENGTH as u16,
        &challenge_payload[..],
    );
    assert!(!confirmation.verify(&psk, &changed_transcript, &proof));

    for index in 0..CLIENT_PROOF.len() {
        let mut changed = CLIENT_PROOF;
        changed[index] ^= 1;
        assert!(!confirmation.verify(&psk, &transcript, &ClientProof::from_bytes(changed)));
    }
    for index in 0..ACTIVATION_CONFIRMATION.len() {
        let mut changed = ACTIVATION_CONFIRMATION;
        changed[index] ^= 1;
        assert!(
            !crate::ActivationConfirmation::from_bytes(
                credential_id(),
                activated_generation(),
                changed,
            )
            .unwrap()
            .verify(&psk, &transcript, &proof)
        );
    }
}

#[test]
fn activation_confirmation_requires_and_binds_the_actual_advanced_generation() {
    let transcript = transcript();
    let psk = psk();
    let proof = ClientProof::calculate(&psk, &transcript);
    assert!(
        crate::ActivationConfirmation::calculate(&psk, &transcript, &proof, generation()).is_err()
    );
    let verified = ClientProof::from_bytes(CLIENT_PROOF)
        .verify(&psk, &transcript)
        .unwrap();
    assert!(
        verified
            .into_activation_confirmation(generation(), &psk)
            .is_err()
    );

    let later_generation = CredentialGeneration::new(ACTIVATED_GENERATION + 9);
    let confirmation =
        crate::ActivationConfirmation::calculate(&psk, &transcript, &proof, later_generation)
            .unwrap();
    assert_eq!(confirmation.generation(), later_generation);
    assert!(confirmation.verify(&psk, &transcript, &proof));
    let substituted = crate::ActivationConfirmation::from_bytes(
        credential_id(),
        CredentialGeneration::new(later_generation.get() + 1),
        *confirmation.as_bytes(),
    )
    .unwrap();
    assert!(!substituted.verify(&psk, &transcript, &proof));
}

#[test]
fn typed_constructors_reject_zero_values_and_transcript_reference_mismatch() {
    assert_eq!(DeviceId::new([0; 16]), Err(InvalidField::ZeroDeviceId));
    assert_eq!(ConnectionId::new(0), None);
    assert_eq!(WindowId::new(0), None);
    assert_eq!(
        DeviceChallenge::new([0; 32]).err(),
        Some(InvalidField::ZeroDeviceChallenge)
    );
    assert_eq!(
        PairingPsk::new([0; 32]).err(),
        Some(InvalidField::ZeroPairingPsk)
    );
    assert_eq!(
        ProofStartRequest::new(
            BearerBinding::UsbSerialJtag,
            0,
            CredentialId::new([0; 16]),
            generation(),
            CLIENT_NONCE
        ),
        Err(InvalidField::ZeroCredentialId)
    );
    assert_eq!(
        ProofStartRequest::new(
            BearerBinding::UsbSerialJtag,
            0,
            credential_id(),
            CredentialGeneration::new(0),
            CLIENT_NONCE
        ),
        Err(InvalidField::ZeroCredentialGeneration)
    );
    assert_eq!(
        ProofStartRequest::new(
            BearerBinding::UsbSerialJtag,
            0,
            credential_id(),
            generation(),
            [0; 32]
        ),
        Err(InvalidField::ZeroClientNonce)
    );

    let other = ProofStartRequest::new(
        BearerBinding::UsbSerialJtag,
        1,
        CredentialId::new([0xaa; 16]),
        generation(),
        CLIENT_NONCE,
    )
    .unwrap();
    assert!(PairingTranscript::new(&other, &proof_challenge()).is_err());
}

#[test]
fn every_secret_bearing_public_owner_requires_drop() {
    assert!(needs_drop::<PairingPsk>());
    assert!(needs_drop::<ClientProof>());
    assert!(needs_drop::<DeviceChallenge>());
    assert!(needs_drop::<BeginOffer>());
    assert!(needs_drop::<ProofChallenge>());
    assert!(needs_drop::<ActivateRequest>());
    assert!(needs_drop::<crate::ActivationConfirmation>());
    assert!(needs_drop::<crate::VerifiedClientProof>());
}

#[test]
fn every_success_response_rejects_noncanonical_profile_reference_and_shape() {
    let profile_cases = [
        (0, 2, DecodeError::UnsupportedVersion { major: 2, minor: 0 }),
        (2, 1, DecodeError::UnsupportedVersion { major: 1, minor: 1 }),
        (4, 3, DecodeError::UnsupportedSuite { observed: 3 }),
        (6, 2, DecodeError::UnsupportedBearer { observed: 2 }),
        (7, 1, DecodeError::ReservedBitsSet),
    ];

    for (profile_index, value, expected) in profile_cases {
        let mut begin = BeginOffer::after_pending_commit(
            BearerBinding::UsbSerialJtag,
            device_id(),
            credential_id(),
            generation(),
            psk(),
        )
        .unwrap()
        .encode_payload();
        begin[8 + profile_index] = value;
        assert_eq!(
            PairingResponse::from_record(
                BearerBinding::UsbSerialJtag,
                record(RECORD_KIND_BEGIN_RESPONSE, [0; 16], 0, &begin[..], [0; 16])
            )
            .err(),
            Some(expected)
        );

        let mut challenge = proof_challenge().encode_payload();
        challenge[8 + profile_index] = value;
        assert_eq!(
            PairingResponse::from_record(
                BearerBinding::UsbSerialJtag,
                record(
                    RECORD_KIND_PROOF_START_RESPONSE,
                    [0; 16],
                    1,
                    &challenge[..],
                    [0; 16]
                )
            )
            .err(),
            Some(expected)
        );
    }

    for index in 1..8 {
        let mut challenge = proof_challenge().encode_payload();
        challenge[index] = 1;
        assert_eq!(
            PairingResponse::from_record(
                BearerBinding::UsbSerialJtag,
                record(
                    RECORD_KIND_PROOF_START_RESPONSE,
                    [0; 16],
                    1,
                    &challenge[..],
                    [0; 16]
                )
            )
            .err(),
            Some(DecodeError::ReservedBitsSet)
        );
    }

    let confirmation = crate::ActivationConfirmation::from_bytes(
        credential_id(),
        activated_generation(),
        ACTIVATION_CONFIRMATION,
    )
    .unwrap();
    let success = ActivateResponse::activated_after_durable_commit(2, confirmation).into_record();
    let mut activate = [0_u8; ACTIVATE_SUCCESS_LENGTH];
    activate.copy_from_slice(success.payload());
    for (start, end, field) in [
        (8, 24, InvalidField::ZeroCredentialId),
        (24, 32, InvalidField::ZeroCredentialGeneration),
    ] {
        let mut payload = activate;
        payload[start..end].fill(0);
        assert_eq!(
            PairingResponse::from_record(
                BearerBinding::UsbSerialJtag,
                record(RECORD_KIND_ACTIVATE_RESPONSE, [0; 16], 2, &payload, [0; 16])
            )
            .err(),
            Some(DecodeError::InvalidRequiredField(field))
        );
    }
    for index in 1..8 {
        let mut payload = activate;
        payload[index] = 1;
        assert_eq!(
            PairingResponse::from_record(
                BearerBinding::UsbSerialJtag,
                record(RECORD_KIND_ACTIVATE_RESPONSE, [0; 16], 2, &payload, [0; 16])
            )
            .err(),
            Some(DecodeError::ReservedBitsSet)
        );
    }

    let begin = BeginOffer::after_pending_commit(
        BearerBinding::UsbSerialJtag,
        device_id(),
        credential_id(),
        generation(),
        psk(),
    )
    .unwrap()
    .encode_payload();
    assert_eq!(
        PairingResponse::from_record(
            BearerBinding::UsbSerialJtag,
            record(
                RECORD_KIND_BEGIN_RESPONSE,
                [0; 16],
                0,
                &begin[..BEGIN_OFFER_LENGTH - 1],
                [0; 16]
            )
        )
        .err(),
        Some(DecodeError::WrongPayloadLength {
            expected: BEGIN_OFFER_LENGTH,
            observed: BEGIN_OFFER_LENGTH - 1,
        })
    );

    let challenge = proof_challenge().encode_payload();
    assert_eq!(
        PairingResponse::from_record(
            BearerBinding::UsbSerialJtag,
            record(
                RECORD_KIND_PROOF_START_RESPONSE,
                [0; 16],
                1,
                &challenge[..PROOF_CHALLENGE_LENGTH - 1],
                [0; 16]
            )
        )
        .err(),
        Some(DecodeError::WrongPayloadLength {
            expected: PROOF_CHALLENGE_LENGTH,
            observed: PROOF_CHALLENGE_LENGTH - 1,
        })
    );

    assert_eq!(
        PairingResponse::from_record(
            BearerBinding::UsbSerialJtag,
            record(
                RECORD_KIND_ACTIVATE_RESPONSE,
                [0; 16],
                2,
                &activate[..ACTIVATE_SUCCESS_LENGTH - 1],
                [0; 16]
            )
        )
        .err(),
        Some(DecodeError::WrongPayloadLength {
            expected: ACTIVATE_SUCCESS_LENGTH,
            observed: ACTIVATE_SUCCESS_LENGTH - 1,
        })
    );
}

#[test]
fn activate_continuation_and_confirmation_reject_substituted_references() {
    let transcript = transcript();
    let psk = psk();

    let canonical = ActivateRequest::new(
        2,
        credential_id(),
        generation(),
        ClientProof::calculate(&psk, &transcript),
    )
    .unwrap();
    let confirmation = canonical
        .verify_continuation(&psk, &transcript)
        .unwrap()
        .into_activation_confirmation(activated_generation(), &psk)
        .unwrap();
    assert_eq!(confirmation.credential_id(), credential_id());
    assert_eq!(confirmation.generation(), activated_generation());
    assert_eq!(confirmation.as_bytes(), &ACTIVATION_CONFIRMATION);

    let wrong_id = ActivateRequest::new(
        2,
        CredentialId::new([0xaa; 16]),
        generation(),
        ClientProof::calculate(&psk, &transcript),
    )
    .unwrap();
    assert!(wrong_id.verify_continuation(&psk, &transcript).is_err());

    let wrong_generation = ActivateRequest::new(
        2,
        credential_id(),
        CredentialGeneration::new(GENERATION + 1),
        ClientProof::calculate(&psk, &transcript),
    )
    .unwrap();
    assert!(
        wrong_generation
            .verify_continuation(&psk, &transcript)
            .is_err()
    );

    let confirmation = crate::ActivationConfirmation::from_bytes(
        credential_id(),
        activated_generation(),
        ACTIVATION_CONFIRMATION,
    )
    .unwrap();
    let response = ActivateResponse::activated_after_durable_commit(2, confirmation).into_record();
    let mut payload = [0_u8; ACTIVATE_SUCCESS_LENGTH];
    payload.copy_from_slice(response.payload());

    payload[8..24].fill(0xaa);
    let substituted_id = PairingResponse::from_record(
        BearerBinding::UsbSerialJtag,
        record(RECORD_KIND_ACTIVATE_RESPONSE, [0; 16], 2, &payload, [0; 16]),
    )
    .unwrap();
    match substituted_id {
        PairingResponse::Activate(response) => {
            assert!(!response.verify_confirmation(&psk, &transcript))
        }
        _ => panic!("wrong response union variant"),
    }

    payload[8..24].copy_from_slice(&CREDENTIAL_ID_BYTES);
    payload[24..32].copy_from_slice(&(ACTIVATED_GENERATION + 1).to_le_bytes());
    let substituted_generation = PairingResponse::from_record(
        BearerBinding::UsbSerialJtag,
        record(RECORD_KIND_ACTIVATE_RESPONSE, [0; 16], 2, &payload, [0; 16]),
    )
    .unwrap();
    match substituted_generation {
        PairingResponse::Activate(response) => {
            assert!(!response.verify_confirmation(&psk, &transcript))
        }
        _ => panic!("wrong response union variant"),
    }
}
