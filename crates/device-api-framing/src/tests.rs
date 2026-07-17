extern crate std;

use std::vec::Vec;

use crate::{
    AUTH_TAG_LENGTH, AUTHENTICATED_DATA_CAPACITY, DecodeEvent, FramedRecord, HEADER_LENGTH, MAGIC,
    PAYLOAD_CAPACITY, PROTOCOL_VERSION, PayloadLength, Record, RecordDecodeError, StreamDecoder,
    WIRE_RECORD_CAPACITY, cobs,
};

fn payload(seed: u8) -> [u8; PAYLOAD_CAPACITY] {
    let mut payload = [0_u8; PAYLOAD_CAPACITY];
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte = seed.wrapping_add((index as u8).wrapping_mul(37));
    }
    payload
}

fn tag(seed: u8) -> [u8; AUTH_TAG_LENGTH] {
    let mut tag = [0_u8; AUTH_TAG_LENGTH];
    for (index, byte) in tag.iter_mut().enumerate() {
        *byte = seed.wrapping_add(index as u8);
    }
    tag
}

fn record(seed: u8, length: usize) -> Record {
    Record::new(
        seed,
        [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ],
        0xf1f2_f3f4_f5f6_f7f8,
        PayloadLength::new(length).expect("test payload must fit"),
        payload(seed),
        tag(seed.wrapping_add(1)),
    )
}

fn decode_frame(bytes: &[u8]) -> Record {
    let mut decoder = StreamDecoder::new();
    let mut observed = None;
    for byte in bytes {
        match decoder.push(*byte) {
            DecodeEvent::Pending => {}
            DecodeEvent::Record(record) => {
                assert!(observed.is_none(), "one frame produced multiple records");
                observed = Some(record);
            }
            DecodeEvent::MalformedCobs => panic!("canonical frame had malformed COBS"),
            DecodeEvent::MalformedRecord(error) => panic!("canonical record failed: {error:?}"),
            DecodeEvent::Overflow => panic!("canonical frame overflowed"),
        }
    }
    observed.expect("frame produced no record")
}

fn assert_record(actual: &Record, seed: u8, length: usize) {
    let mut canonical_payload = [0_u8; PAYLOAD_CAPACITY];
    canonical_payload[..length].copy_from_slice(&payload(seed)[..length]);
    assert_eq!(actual.kind(), seed);
    assert_eq!(
        actual.session_id(),
        &[
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ]
    );
    assert_eq!(actual.sequence(), 0xf1f2_f3f4_f5f6_f7f8);
    assert_eq!(actual.payload_length().get(), length);
    assert_eq!(actual.payload(), &payload(seed)[..length]);
    assert_eq!(actual.full_payload(), &canonical_payload);
    assert_eq!(actual.authentication_tag(), &tag(seed.wrapping_add(1)));
}

#[test]
fn empty_and_maximum_records_round_trip_with_exact_owners() {
    for (seed, length) in [(0x00, 0), (0x31, 1), (0x42, 511), (0x53, 512)] {
        let input = record(seed, length);
        let framed = FramedRecord::encode(&input).expect("bounded record must encode");
        assert_eq!(framed.encoded().first(), Some(&0));
        assert_eq!(framed.encoded().last(), Some(&0));
        assert!(framed.length() <= WIRE_RECORD_CAPACITY);
        let output = decode_frame(framed.encoded());
        assert_record(&output, seed, length);
    }
}

#[test]
fn decoder_ignores_prefix_garbage_and_allows_shared_delimiters() {
    let first = FramedRecord::encode(&record(0x61, 19)).unwrap();
    let second = FramedRecord::encode(&record(0x62, 277)).unwrap();
    let mut wire = Vec::from([0x91, 0x92, 0x93]);
    wire.extend_from_slice(first.encoded());
    wire.extend_from_slice(&second.encoded()[1..]);

    let mut decoder = StreamDecoder::new();
    let mut records = Vec::new();
    for byte in wire {
        if let DecodeEvent::Record(record) = decoder.push(byte) {
            records.push(record);
        }
    }
    assert_eq!(records.len(), 2);
    assert_record(&records[0], 0x61, 19);
    assert_record(&records[1], 0x62, 277);
}

#[test]
fn overflow_discards_through_delimiter_then_recovers() {
    let valid = FramedRecord::encode(&record(0x71, 32)).unwrap();
    let mut decoder = StreamDecoder::new();
    assert!(matches!(decoder.push(0), DecodeEvent::Pending));
    for _ in 0..WIRE_RECORD_CAPACITY + 17 {
        assert!(matches!(decoder.push(1), DecodeEvent::Pending));
    }
    assert!(matches!(decoder.push(0), DecodeEvent::Overflow));

    let mut output = None;
    for byte in &valid.encoded()[1..] {
        if let DecodeEvent::Record(record) = decoder.push(*byte) {
            output = Some(record);
        }
    }
    assert_record(&output.expect("decoder did not recover"), 0x71, 32);
}

#[test]
fn malformed_cobs_and_record_faults_are_bounded_and_recoverable() {
    let mut decoder = StreamDecoder::new();
    for byte in [0, 5, 1, 2, 0] {
        let event = decoder.push(byte);
        if byte == 0 && event_is_fault(&event) {
            assert!(matches!(event, DecodeEvent::MalformedCobs));
        }
    }

    let framed = FramedRecord::encode(&record(0x81, 8)).unwrap();
    let mut decoded = [0_u8; crate::DECODED_RECORD_CAPACITY];
    let decoded_length = cobs::decode(&framed.encoded()[1..framed.length() - 1], &mut decoded)
        .expect("test frame COBS");
    decoded[0] ^= 0xff;
    let mut corrupted = [0_u8; WIRE_RECORD_CAPACITY];
    corrupted[0] = 0;
    let encoded_length = cobs::encode(&decoded[..decoded_length], &mut corrupted[1..]).unwrap();
    corrupted[encoded_length + 1] = 0;
    let mut event = DecodeEvent::Pending;
    for byte in &corrupted[..encoded_length + 2] {
        event = decoder.push(*byte);
    }
    assert!(matches!(
        event,
        DecodeEvent::MalformedRecord(RecordDecodeError::BadMagic)
    ));

    let recovered = decode_frame(framed.encoded());
    assert_record(&recovered, 0x81, 8);
}

fn event_is_fault(event: &DecodeEvent) -> bool {
    matches!(
        event,
        DecodeEvent::MalformedCobs | DecodeEvent::MalformedRecord(_) | DecodeEvent::Overflow
    )
}

#[test]
fn transmit_cursor_advances_only_acknowledged_partial_writes() {
    let mut framed = FramedRecord::encode(&record(0x91, 512)).unwrap();
    let complete = Vec::from(framed.encoded());
    let mut replay = Vec::new();

    while !framed.is_complete() {
        let chunk = framed.next_chunk(64);
        let acknowledged = chunk.len().min(17);
        replay.extend_from_slice(&chunk[..acknowledged]);
        framed.advance(acknowledged).unwrap();
    }
    assert_eq!(replay, complete);
    assert_eq!(framed.acknowledged(), framed.length());
    let error = framed
        .advance(1)
        .expect_err("past-end acknowledgement must fail");
    assert_eq!(error.acknowledged(), 1);
    assert_eq!(error.remaining(), 0);
}

#[test]
fn reset_requires_a_fresh_delimiter() {
    let framed = FramedRecord::encode(&record(0xa1, 40)).unwrap();
    let mut decoder = StreamDecoder::new();
    for byte in &framed.encoded()[..framed.length() / 2] {
        let _ = decoder.push(*byte);
    }
    assert!(decoder.is_synchronized());
    decoder.reset();
    assert!(!decoder.is_synchronized());

    let mut produced = false;
    for byte in &framed.encoded()[framed.length() / 2..] {
        produced |= matches!(decoder.push(*byte), DecodeEvent::Record(_));
    }
    assert!(!produced);
    assert!(decoder.is_synchronized());

    let recovered = decode_frame(framed.encoded());
    assert_record(&recovered, 0xa1, 40);
}

#[test]
fn cobs_codec_round_trips_all_single_bytes_and_dense_zero_patterns() {
    let mut encoded = [0_u8; 520];
    let mut decoded = [0_u8; 512];
    for value in u8::MIN..=u8::MAX {
        let encoded_length = cobs::encode(&[value], &mut encoded).unwrap();
        let decoded_length = cobs::decode(&encoded[..encoded_length], &mut decoded).unwrap();
        assert_eq!(&decoded[..decoded_length], &[value]);
    }

    for length in 0..=512 {
        let source = [0_u8; 512];
        let encoded_length = cobs::encode(&source[..length], &mut encoded).unwrap();
        let decoded_length = cobs::decode(&encoded[..encoded_length], &mut decoded).unwrap();
        assert_eq!(&decoded[..decoded_length], &source[..length]);
    }
}

#[test]
fn payload_and_header_validation_fail_closed() {
    assert_eq!(PayloadLength::new(513).unwrap_err().length(), 513);
    assert_eq!(MAGIC, *b"RDA1");
    assert_eq!(PROTOCOL_VERSION, 1);

    let input = record(0xb1, 12);
    let mut body = [0_u8; crate::DECODED_RECORD_CAPACITY];
    let length = input.write_decoded(&mut body);
    body[4] = 2;
    assert!(matches!(
        Record::decode(&body[..length]),
        Err(RecordDecodeError::UnsupportedVersion { observed: 2 })
    ));
    body[4] = PROTOCOL_VERSION;
    body[6] = 1;
    assert!(matches!(
        Record::decode(&body[..length]),
        Err(RecordDecodeError::ReservedBitsSet)
    ));
    body[6] = 0;
    body[32..34].copy_from_slice(&13_u16.to_le_bytes());
    assert!(matches!(
        Record::decode(&body[..length]),
        Err(RecordDecodeError::LengthMismatch { .. })
    ));
}

#[test]
fn canonical_authenticated_bytes_cover_the_complete_semantic_header_and_payload() {
    let input = record(0xc1, 27);
    let mut authenticated = [0_u8; AUTHENTICATED_DATA_CAPACITY];
    let length = input.write_authenticated_data(&mut authenticated);

    assert_eq!(length, HEADER_LENGTH + 27);
    assert_eq!(&authenticated[..4], &MAGIC);
    assert_eq!(authenticated[4], PROTOCOL_VERSION);
    assert_eq!(authenticated[5], 0xc1);
    assert_eq!(&authenticated[6..8], &[0, 0]);
    assert_eq!(
        &authenticated[8..24],
        &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
    );
    assert_eq!(
        &authenticated[24..32],
        &0xf1f2_f3f4_f5f6_f7f8_u64.to_le_bytes()
    );
    assert_eq!(&authenticated[32..34], &27_u16.to_le_bytes());
    assert_eq!(&authenticated[HEADER_LENGTH..length], &payload(0xc1)[..27]);
}
