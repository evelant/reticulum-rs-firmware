use reticulum_lxmf_wire::{
    BASIC_LXMF_SELECTION_OVERHEAD_BYTES, BufferError, EMPTY_LXMF_FIELDS_ENCODED_BYTES,
    LINK_PACKET_MAX_CONTENT, MAX_ENCODED_SIDEBAND_LOCATION_FIELDS_BYTES,
    MAX_ENCODED_SIDEBAND_LOCATION_TELEMETRY_BYTES, SidebandLocationElement,
    SidebandLocationTelemetry, SidebandLocationTelemetryDecodeError, WireLimits,
    basic_lxmf_payload_fits_content_limit, decode_sideband_location_fields,
    encode_sideband_location_fields, encode_sideband_location_telemetry,
    encoded_basic_lxmf_payload_len, encoded_messagepack_binary_len,
    encoded_sideband_location_fields_len, encoded_sideband_location_telemetry_len,
};

fn limits() -> WireLimits {
    WireLimits::new(512, 256, 32, 128, 512, 8)
}

#[test]
fn sideband_fields_encoding_is_exact_and_round_trips() {
    let telemetry =
        SidebandLocationTelemetry::new(48_534_200, 3_832_500, 7_700, 0, 0, 1, 1_781_467_569);
    let expected = hex::decode(
        "8102c4308201ce6a2f09b10297c40402e492b8c404003a7ab4c40400001e14\
         c40400000000c40400000000c4020001ce6a2f09b1",
    )
    .expect("fixture hex");
    let mut output = [0_u8; MAX_ENCODED_SIDEBAND_LOCATION_FIELDS_BYTES];
    let written = encode_sideband_location_fields(telemetry, &mut output).expect("encode");

    assert_eq!(written, encoded_sideband_location_fields_len(telemetry));
    assert_eq!(&output[..written], expected);
    let mut inner = [0_u8; MAX_ENCODED_SIDEBAND_LOCATION_TELEMETRY_BYTES];
    let inner_written =
        encode_sideband_location_telemetry(telemetry, &mut inner).expect("inner encode");
    assert_eq!(
        inner_written,
        encoded_sideband_location_telemetry_len(telemetry)
    );
    assert_eq!(&inner[..inner_written], &expected[4..]);
    assert_eq!(
        decode_sideband_location_fields(&output[..written], limits()).expect("decode"),
        Some(telemetry)
    );
}

#[test]
fn basic_payload_budget_matches_messagepack_width_boundaries_and_direct_limit() {
    assert_eq!(encoded_messagepack_binary_len(0), Some(2));
    assert_eq!(encoded_messagepack_binary_len(255), Some(257));
    assert_eq!(encoded_messagepack_binary_len(256), Some(259));
    assert_eq!(
        encoded_basic_lxmf_payload_len(0, 0, EMPTY_LXMF_FIELDS_ENCODED_BYTES),
        Some(15)
    );

    let current_location = SidebandLocationTelemetry::new(1, 2, 3, 4, 5, 6, 1_784_000_001);
    let fields_len = encoded_sideband_location_fields_len(current_location);
    assert_eq!(fields_len, MAX_ENCODED_SIDEBAND_LOCATION_FIELDS_BYTES);
    assert!(basic_lxmf_payload_fits_content_limit(
        0,
        268,
        fields_len,
        LINK_PACKET_MAX_CONTENT,
    ));
    assert!(!basic_lxmf_payload_fits_content_limit(
        0,
        269,
        fields_len,
        LINK_PACKET_MAX_CONTENT,
    ));
    assert_eq!(
        encoded_basic_lxmf_payload_len(0, 268, fields_len),
        Some(BASIC_LXMF_SELECTION_OVERHEAD_BYTES + LINK_PACKET_MAX_CONTENT)
    );
}

#[test]
fn live_sideband_handset_capture_decodes_and_ignores_unknown_sensor() {
    // Captured FIELD_TELEMETRY value from a live Sideband handset, wrapped in
    // an LXMF fields map. SID_TIME differs from the location update timestamp,
    // and sensor 0x04 is unrelated to location; both are valid Sideband data.
    let fields = hex::decode(
        "8102c4328301ce6a2f09bf04c00297c40402e492b8c404003a7ab4c40400001e14\
         c40400000000c40400000000c4020001ce6a2f09b1",
    )
    .expect("fixture hex");

    let decoded = decode_sideband_location_fields(&fields, limits())
        .expect("valid capture")
        .expect("location present");
    assert_eq!(decoded.latitude_e6(), 48_534_200);
    assert_eq!(decoded.longitude_e6(), 3_832_500);
    assert_eq!(decoded.altitude_cm(), 7_700);
    assert_eq!(decoded.speed_cm_per_second(), 0);
    assert_eq!(decoded.bearing_centidegrees(), 0);
    assert_eq!(decoded.accuracy_cm(), 1);
    assert_eq!(decoded.updated_at_unix_seconds(), 1_781_467_569);
}

#[test]
fn signed_and_unsigned_extremes_round_trip_without_float_conversion() {
    let telemetry = SidebandLocationTelemetry::new(
        i32::MIN,
        i32::MAX,
        -123_456,
        u32::MAX,
        -18_000,
        u16::MAX,
        u32::MAX,
    );
    let mut output = [0_u8; MAX_ENCODED_SIDEBAND_LOCATION_FIELDS_BYTES];
    let written = encode_sideband_location_fields(telemetry, &mut output).expect("encode");
    assert_eq!(written, MAX_ENCODED_SIDEBAND_LOCATION_FIELDS_BYTES);
    assert_eq!(
        decode_sideband_location_fields(&output[..written], limits()).expect("decode"),
        Some(telemetry)
    );
}

#[test]
fn timestamp_integer_width_boundaries_have_exact_lengths() {
    for (timestamp, expected_inner_len) in [
        (0, 40),
        (0x7f, 40),
        (0x80, 42),
        (u32::from(u8::MAX), 42),
        (u32::from(u8::MAX) + 1, 44),
        (u32::from(u16::MAX), 44),
        (u32::from(u16::MAX) + 1, 48),
        (u32::MAX, 48),
    ] {
        let telemetry = SidebandLocationTelemetry::new(1, -2, 3, 4, -5, 6, timestamp);
        assert_eq!(
            encoded_sideband_location_telemetry_len(telemetry),
            expected_inner_len
        );
        assert_eq!(
            encoded_sideband_location_fields_len(telemetry),
            expected_inner_len + 4
        );
        let mut fields = [0_u8; MAX_ENCODED_SIDEBAND_LOCATION_FIELDS_BYTES];
        let written = encode_sideband_location_fields(telemetry, &mut fields).expect("encode");
        assert_eq!(
            decode_sideband_location_fields(&fields[..written], limits()).expect("decode"),
            Some(telemetry)
        );
    }
}

#[test]
fn absent_telemetry_is_none_and_unknown_lxmf_fields_are_ignored() {
    assert_eq!(
        decode_sideband_location_fields(&[0x81, 0x04, 0xc0], limits()).expect("fields map"),
        None
    );
}

#[test]
fn present_malformed_telemetry_returns_typed_errors() {
    assert_eq!(
        decode_sideband_location_fields(&[0x81, 0x02, 0xc0], limits()),
        Err(SidebandLocationTelemetryDecodeError::TelemetryFieldNotBinary)
    );

    // Inner payload is a valid integer, but not a Sideband sensor map.
    assert_eq!(
        decode_sideband_location_fields(&[0x81, 0x02, 0xc4, 0x01, 0x01], limits()),
        Err(SidebandLocationTelemetryDecodeError::TelemetryNotMap)
    );

    // Location latitude is a bare MessagePack integer instead of Sideband's
    // exact four-byte big-endian binary scalar.
    let malformed = hex::decode(
        "8102c42b8201ce6a2f09b1029700c404003a7ab4c40400001e14c40400000000\
         c40400000000c4020001ce6a2f09b1",
    )
    .expect("fixture hex");
    assert_eq!(
        decode_sideband_location_fields(&malformed, limits()),
        Err(
            SidebandLocationTelemetryDecodeError::InvalidLocationElement {
                element: SidebandLocationElement::Latitude,
            }
        )
    );
}

#[test]
fn short_output_is_rejected_without_mutation() {
    let telemetry = SidebandLocationTelemetry::new(1, 2, 3, 4, 5, 6, u32::MAX);
    let required = encoded_sideband_location_fields_len(telemetry);
    let mut output = [0xa5_u8; MAX_ENCODED_SIDEBAND_LOCATION_FIELDS_BYTES - 1];
    assert_eq!(
        encode_sideband_location_fields(telemetry, &mut output),
        Err(BufferError {
            required,
            available: output.len(),
        })
    );
    assert!(output.iter().all(|byte| *byte == 0xa5));

    let inner_required = encoded_sideband_location_telemetry_len(telemetry);
    let mut inner = [0x5a_u8; MAX_ENCODED_SIDEBAND_LOCATION_TELEMETRY_BYTES - 1];
    assert_eq!(
        encode_sideband_location_telemetry(telemetry, &mut inner),
        Err(BufferError {
            required: inner_required,
            available: inner.len(),
        })
    );
    assert!(inner.iter().all(|byte| *byte == 0x5a));
}
