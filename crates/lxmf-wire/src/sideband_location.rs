use core::fmt;

use crate::{
    BufferError, MessagePackKind, WireError, WireLimits, msgpack::Scanner,
    validate_messagepack_value,
};

/// LXMF core field allocated to one telemetry snapshot.
pub const FIELD_TELEMETRY: u8 = 0x02;
/// Sideband telemetry sensor identifier for the snapshot timestamp.
pub const SIDEBAND_SENSOR_TIME: u8 = 0x01;
/// Sideband telemetry sensor identifier for a location observation.
pub const SIDEBAND_SENSOR_LOCATION: u8 = 0x02;
/// Largest Sideband sensor-map payload emitted as the value of `FIELD_TELEMETRY`.
///
/// This excludes the surrounding LXMF fields map and its MessagePack binary
/// wrapper. The encoded length varies with the canonical MessagePack width of
/// `updated_at_unix_seconds`; this bound covers the complete `u32` range.
pub const MAX_ENCODED_SIDEBAND_LOCATION_TELEMETRY_BYTES: usize = 48;
/// Largest complete LXMF fields map emitted by [`encode_sideband_location_fields`].
///
/// The encoded length varies with the canonical MessagePack width of
/// `updated_at_unix_seconds`. This bound covers the complete `u32` range.
pub const MAX_ENCODED_SIDEBAND_LOCATION_FIELDS_BYTES: usize = 52;

/// Sideband-compatible fixed-point location carried in LXMF `FIELD_TELEMETRY`.
///
/// Sideband stores the first five values as four-byte MessagePack binary
/// scalars and accuracy as a two-byte binary scalar. The integer values here
/// retain those exact wire units without a floating-point conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidebandLocationTelemetry {
    latitude_e6: i32,
    longitude_e6: i32,
    altitude_cm: i32,
    speed_cm_per_second: u32,
    bearing_centidegrees: i32,
    accuracy_cm: u16,
    updated_at_unix_seconds: u32,
}

impl SidebandLocationTelemetry {
    /// Construct one exact fixed-point location observation.
    pub const fn new(
        latitude_e6: i32,
        longitude_e6: i32,
        altitude_cm: i32,
        speed_cm_per_second: u32,
        bearing_centidegrees: i32,
        accuracy_cm: u16,
        updated_at_unix_seconds: u32,
    ) -> Self {
        Self {
            latitude_e6,
            longitude_e6,
            altitude_cm,
            speed_cm_per_second,
            bearing_centidegrees,
            accuracy_cm,
            updated_at_unix_seconds,
        }
    }

    /// Latitude in signed millionths of one degree.
    pub const fn latitude_e6(self) -> i32 {
        self.latitude_e6
    }

    /// Longitude in signed millionths of one degree.
    pub const fn longitude_e6(self) -> i32 {
        self.longitude_e6
    }

    /// Altitude in signed centimetres.
    pub const fn altitude_cm(self) -> i32 {
        self.altitude_cm
    }

    /// Ground speed in centimetres per second.
    pub const fn speed_cm_per_second(self) -> u32 {
        self.speed_cm_per_second
    }

    /// Bearing in signed hundredths of one degree.
    pub const fn bearing_centidegrees(self) -> i32 {
        self.bearing_centidegrees
    }

    /// Horizontal accuracy in centimetres.
    pub const fn accuracy_cm(self) -> u16 {
        self.accuracy_cm
    }

    /// Unix timestamp of the location update in whole seconds.
    pub const fn updated_at_unix_seconds(self) -> u32 {
        self.updated_at_unix_seconds
    }
}

/// Semantic element whose Sideband binary representation was malformed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebandLocationElement {
    /// Signed four-byte latitude in degrees times one million.
    Latitude,
    /// Signed four-byte longitude in degrees times one million.
    Longitude,
    /// Signed four-byte altitude in centimetres.
    Altitude,
    /// Unsigned four-byte speed in centimetres per second.
    Speed,
    /// Signed four-byte bearing in hundredths of one degree.
    Bearing,
    /// Unsigned two-byte horizontal accuracy in centimetres.
    Accuracy,
    /// Unsigned MessagePack integer containing the location update time.
    UpdatedAt,
}

/// Failure decoding a Sideband location from an LXMF fields MessagePack map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebandLocationTelemetryDecodeError {
    /// The complete LXMF fields value was not structurally valid MessagePack.
    InvalidFields(WireError),
    /// The supplied LXMF fields value was structurally valid but not a map.
    FieldsNotMap,
    /// `FIELD_TELEMETRY` was present but did not contain MessagePack binary.
    TelemetryFieldNotBinary,
    /// The binary telemetry payload was not structurally valid MessagePack.
    InvalidTelemetry(WireError),
    /// The binary telemetry payload was structurally valid but not a sensor map.
    TelemetryNotMap,
    /// The Sideband time sensor was absent.
    MissingTimeSensor,
    /// The Sideband time sensor was not an unsigned integer fitting `u32`.
    InvalidTimeSensor,
    /// The Sideband location sensor was absent.
    MissingLocationSensor,
    /// The Sideband location sensor was not an array.
    LocationNotArray,
    /// The Sideband location sensor did not contain its seven required values.
    LocationTooShort {
        /// Number of values present in the location array.
        actual: usize,
    },
    /// One required location value had the wrong MessagePack representation.
    InvalidLocationElement {
        /// Element whose representation was invalid.
        element: SidebandLocationElement,
    },
}

impl fmt::Display for SidebandLocationTelemetryDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFields(error) => write!(formatter, "invalid LXMF fields: {error}"),
            Self::FieldsNotMap => formatter.write_str("LXMF fields value is not a map"),
            Self::TelemetryFieldNotBinary => {
                formatter.write_str("LXMF telemetry field is not MessagePack binary")
            }
            Self::InvalidTelemetry(error) => {
                write!(formatter, "invalid Sideband telemetry payload: {error}")
            }
            Self::TelemetryNotMap => {
                formatter.write_str("Sideband telemetry payload is not a sensor map")
            }
            Self::MissingTimeSensor => formatter.write_str("Sideband time sensor is missing"),
            Self::InvalidTimeSensor => formatter.write_str("Sideband time sensor is invalid"),
            Self::MissingLocationSensor => {
                formatter.write_str("Sideband location sensor is missing")
            }
            Self::LocationNotArray => {
                formatter.write_str("Sideband location sensor is not an array")
            }
            Self::LocationTooShort { actual } => write!(
                formatter,
                "Sideband location has {actual} values; at least 7 are required"
            ),
            Self::InvalidLocationElement { element } => {
                write!(formatter, "invalid Sideband location element {element:?}")
            }
        }
    }
}

/// Return the exact bytes required by [`encode_sideband_location_fields`].
pub const fn encoded_sideband_location_fields_len(telemetry: SidebandLocationTelemetry) -> usize {
    // Outer fixmap, field id and bin8 header.
    4 + encoded_sideband_location_telemetry_len(telemetry)
}

/// Return the exact Sideband sensor-map bytes required for `FIELD_TELEMETRY`.
pub const fn encoded_sideband_location_telemetry_len(
    telemetry: SidebandLocationTelemetry,
) -> usize {
    // Inner fixmap, two sensor ids, fixarray, five bin4 scalars and one bin2
    // scalar. The timestamp occurs in both SID_TIME and the location array's
    // final element.
    38 + messagepack_u32_len(telemetry.updated_at_unix_seconds) * 2
}

/// Encode one complete LXMF fields map containing Sideband location telemetry.
///
/// The result is `{ FIELD_TELEMETRY: binary(sensor_map) }`. The inner sensor map
/// contains `SID_TIME` and `SID_LOCATION`; all fixed-point location scalars use
/// Sideband's exact big-endian MessagePack-binary representation. No bytes are
/// written when `output` is too small.
pub fn encode_sideband_location_fields(
    telemetry: SidebandLocationTelemetry,
    output: &mut [u8],
) -> Result<usize, BufferError> {
    let required = encoded_sideband_location_fields_len(telemetry);
    if output.len() < required {
        return Err(BufferError {
            required,
            available: output.len(),
        });
    }

    let inner_len = required - 4;
    let target = &mut output[..required];
    let mut cursor = 0;

    put_byte(target, &mut cursor, 0x81); // fields fixmap(1)
    put_byte(target, &mut cursor, FIELD_TELEMETRY);
    put_byte(target, &mut cursor, 0xc4); // bin8
    put_byte(
        target,
        &mut cursor,
        u8::try_from(inner_len).expect("bounded Sideband telemetry fits bin8"),
    );

    let inner_written = encode_sideband_location_telemetry(
        telemetry,
        target
            .get_mut(cursor..)
            .expect("preflighted output contains inner telemetry"),
    )
    .expect("preflighted output contains complete inner telemetry");
    cursor += inner_written;

    debug_assert_eq!(cursor, required);
    Ok(required)
}

/// Encode the Sideband sensor-map bytes used as `FIELD_TELEMETRY`'s value.
///
/// This output intentionally excludes the outer LXMF fields map and MessagePack
/// binary wrapper. It can therefore be passed directly to an LXMF composer that
/// already represents field values as binary byte vectors. No bytes are written
/// when `output` is too small.
pub fn encode_sideband_location_telemetry(
    telemetry: SidebandLocationTelemetry,
    output: &mut [u8],
) -> Result<usize, BufferError> {
    let required = encoded_sideband_location_telemetry_len(telemetry);
    if output.len() < required {
        return Err(BufferError {
            required,
            available: output.len(),
        });
    }

    let target = &mut output[..required];
    let mut cursor = 0;
    put_byte(target, &mut cursor, 0x82); // sensor fixmap(2)
    put_byte(target, &mut cursor, SIDEBAND_SENSOR_TIME);
    put_u32_integer(target, &mut cursor, telemetry.updated_at_unix_seconds);
    put_byte(target, &mut cursor, SIDEBAND_SENSOR_LOCATION);
    put_byte(target, &mut cursor, 0x97); // location fixarray(7)
    put_bin4(target, &mut cursor, telemetry.latitude_e6.to_be_bytes());
    put_bin4(target, &mut cursor, telemetry.longitude_e6.to_be_bytes());
    put_bin4(target, &mut cursor, telemetry.altitude_cm.to_be_bytes());
    put_bin4(
        target,
        &mut cursor,
        telemetry.speed_cm_per_second.to_be_bytes(),
    );
    put_bin4(
        target,
        &mut cursor,
        telemetry.bearing_centidegrees.to_be_bytes(),
    );
    put_bin2(target, &mut cursor, telemetry.accuracy_cm.to_be_bytes());
    put_u32_integer(target, &mut cursor, telemetry.updated_at_unix_seconds);

    debug_assert_eq!(cursor, required);
    Ok(required)
}

/// Decode optional Sideband location telemetry from one complete LXMF fields map.
///
/// Unknown LXMF fields and unknown Sideband sensors are ignored. Absence of
/// `FIELD_TELEMETRY` returns `Ok(None)`. Once that field is present, malformed
/// binary, sensor-map or location representations return a typed error.
pub fn decode_sideband_location_fields(
    fields: &[u8],
    limits: WireLimits,
) -> Result<Option<SidebandLocationTelemetry>, SidebandLocationTelemetryDecodeError> {
    let view = validate_messagepack_value(fields, limits)
        .map_err(SidebandLocationTelemetryDecodeError::InvalidFields)?;
    if view.kind() != MessagePackKind::Map {
        return Err(SidebandLocationTelemetryDecodeError::FieldsNotMap);
    }

    let (count, mut cursor) = map_header(fields).expect("validated map has a map header");
    let mut scanner = Scanner::new(fields, limits);
    for _ in 0..count {
        let key_start = cursor;
        let key = scanner
            .scan_value(cursor, 2)
            .map_err(SidebandLocationTelemetryDecodeError::InvalidFields)?;
        cursor = key.end;
        let value_start = cursor;
        let value = scanner
            .scan_value(cursor, 2)
            .map_err(SidebandLocationTelemetryDecodeError::InvalidFields)?;
        cursor = value.end;

        if decode_integer(&fields[key_start..key.end]) == Some(i128::from(FIELD_TELEMETRY)) {
            if value.kind != MessagePackKind::Binary {
                return Err(SidebandLocationTelemetryDecodeError::TelemetryFieldNotBinary);
            }
            let binary = binary_payload(&fields[value_start..value.end])
                .expect("validated binary has a binary header");
            return decode_telemetry(binary, limits).map(Some);
        }
    }
    Ok(None)
}

fn decode_telemetry(
    encoded: &[u8],
    limits: WireLimits,
) -> Result<SidebandLocationTelemetry, SidebandLocationTelemetryDecodeError> {
    let view = validate_messagepack_value(encoded, limits)
        .map_err(SidebandLocationTelemetryDecodeError::InvalidTelemetry)?;
    if view.kind() != MessagePackKind::Map {
        return Err(SidebandLocationTelemetryDecodeError::TelemetryNotMap);
    }

    let (count, mut cursor) = map_header(encoded).expect("validated map has a map header");
    let mut scanner = Scanner::new(encoded, limits);
    let mut time = None;
    let mut location = None;
    for _ in 0..count {
        let key_start = cursor;
        let key = scanner
            .scan_value(cursor, 2)
            .map_err(SidebandLocationTelemetryDecodeError::InvalidTelemetry)?;
        cursor = key.end;
        let value_start = cursor;
        let scanned_value = scanner
            .scan_value(cursor, 2)
            .map_err(SidebandLocationTelemetryDecodeError::InvalidTelemetry)?;
        cursor = scanned_value.end;
        match decode_integer(&encoded[key_start..key.end]) {
            Some(value) if value == i128::from(SIDEBAND_SENSOR_TIME) => {
                time = Some(&encoded[value_start..scanned_value.end]);
            }
            Some(value) if value == i128::from(SIDEBAND_SENSOR_LOCATION) => {
                location = Some((&encoded[value_start..scanned_value.end], scanned_value.kind));
            }
            _ => {}
        }
    }

    let time = time.ok_or(SidebandLocationTelemetryDecodeError::MissingTimeSensor)?;
    decode_u32_integer(time).ok_or(SidebandLocationTelemetryDecodeError::InvalidTimeSensor)?;
    let (location, kind) =
        location.ok_or(SidebandLocationTelemetryDecodeError::MissingLocationSensor)?;
    if kind != MessagePackKind::Array {
        return Err(SidebandLocationTelemetryDecodeError::LocationNotArray);
    }
    decode_location(location, limits)
}

fn decode_location(
    encoded: &[u8],
    limits: WireLimits,
) -> Result<SidebandLocationTelemetry, SidebandLocationTelemetryDecodeError> {
    let (count, mut cursor) = array_header(encoded).expect("validated array has an array header");
    if count < 7 {
        return Err(SidebandLocationTelemetryDecodeError::LocationTooShort { actual: count });
    }
    let mut values = [&[][..]; 7];
    let mut scanner = Scanner::new(encoded, limits);
    for slot in &mut values {
        let start = cursor;
        let value = scanner
            .scan_value(cursor, 2)
            .map_err(SidebandLocationTelemetryDecodeError::InvalidTelemetry)?;
        cursor = value.end;
        *slot = &encoded[start..value.end];
    }

    let latitude_e6 = decode_i32_binary(values[0]).ok_or(
        SidebandLocationTelemetryDecodeError::InvalidLocationElement {
            element: SidebandLocationElement::Latitude,
        },
    )?;
    let longitude_e6 = decode_i32_binary(values[1]).ok_or(
        SidebandLocationTelemetryDecodeError::InvalidLocationElement {
            element: SidebandLocationElement::Longitude,
        },
    )?;
    let altitude_cm = decode_i32_binary(values[2]).ok_or(
        SidebandLocationTelemetryDecodeError::InvalidLocationElement {
            element: SidebandLocationElement::Altitude,
        },
    )?;
    let speed_cm_per_second = decode_u32_binary(values[3]).ok_or(
        SidebandLocationTelemetryDecodeError::InvalidLocationElement {
            element: SidebandLocationElement::Speed,
        },
    )?;
    let bearing_centidegrees = decode_i32_binary(values[4]).ok_or(
        SidebandLocationTelemetryDecodeError::InvalidLocationElement {
            element: SidebandLocationElement::Bearing,
        },
    )?;
    let accuracy_cm = decode_u16_binary(values[5]).ok_or(
        SidebandLocationTelemetryDecodeError::InvalidLocationElement {
            element: SidebandLocationElement::Accuracy,
        },
    )?;
    let updated_at_unix_seconds = decode_u32_integer(values[6]).ok_or(
        SidebandLocationTelemetryDecodeError::InvalidLocationElement {
            element: SidebandLocationElement::UpdatedAt,
        },
    )?;

    Ok(SidebandLocationTelemetry::new(
        latitude_e6,
        longitude_e6,
        altitude_cm,
        speed_cm_per_second,
        bearing_centidegrees,
        accuracy_cm,
        updated_at_unix_seconds,
    ))
}

const fn messagepack_u32_len(value: u32) -> usize {
    if value <= 0x7f {
        1
    } else if value <= u8::MAX as u32 {
        2
    } else if value <= u16::MAX as u32 {
        3
    } else {
        5
    }
}

fn put_byte(output: &mut [u8], cursor: &mut usize, value: u8) {
    output[*cursor] = value;
    *cursor += 1;
}

fn put_bytes(output: &mut [u8], cursor: &mut usize, value: &[u8]) {
    let end = *cursor + value.len();
    output[*cursor..end].copy_from_slice(value);
    *cursor = end;
}

fn put_bin4(output: &mut [u8], cursor: &mut usize, value: [u8; 4]) {
    put_bytes(output, cursor, &[0xc4, 0x04]);
    put_bytes(output, cursor, &value);
}

fn put_bin2(output: &mut [u8], cursor: &mut usize, value: [u8; 2]) {
    put_bytes(output, cursor, &[0xc4, 0x02]);
    put_bytes(output, cursor, &value);
}

fn put_u32_integer(output: &mut [u8], cursor: &mut usize, value: u32) {
    if value <= 0x7f {
        put_byte(output, cursor, value as u8);
    } else if let Ok(value) = u8::try_from(value) {
        put_bytes(output, cursor, &[0xcc, value]);
    } else if let Ok(value) = u16::try_from(value) {
        put_byte(output, cursor, 0xcd);
        put_bytes(output, cursor, &value.to_be_bytes());
    } else {
        put_byte(output, cursor, 0xce);
        put_bytes(output, cursor, &value.to_be_bytes());
    }
}

fn map_header(raw: &[u8]) -> Option<(usize, usize)> {
    match *raw.first()? {
        marker @ 0x80..=0x8f => Some((usize::from(marker & 0x0f), 1)),
        0xde => Some((
            usize::from(u16::from_be_bytes(raw.get(1..3)?.try_into().ok()?)),
            3,
        )),
        0xdf => Some((
            usize::try_from(u32::from_be_bytes(raw.get(1..5)?.try_into().ok()?)).ok()?,
            5,
        )),
        _ => None,
    }
}

fn array_header(raw: &[u8]) -> Option<(usize, usize)> {
    match *raw.first()? {
        marker @ 0x90..=0x9f => Some((usize::from(marker & 0x0f), 1)),
        0xdc => Some((
            usize::from(u16::from_be_bytes(raw.get(1..3)?.try_into().ok()?)),
            3,
        )),
        0xdd => Some((
            usize::try_from(u32::from_be_bytes(raw.get(1..5)?.try_into().ok()?)).ok()?,
            5,
        )),
        _ => None,
    }
}

fn binary_payload(raw: &[u8]) -> Option<&[u8]> {
    let header = match *raw.first()? {
        0xc4 => 2,
        0xc5 => 3,
        0xc6 => 5,
        _ => return None,
    };
    raw.get(header..)
}

fn decode_integer(raw: &[u8]) -> Option<i128> {
    match *raw.first()? {
        value @ 0x00..=0x7f => Some(i128::from(value)),
        value @ 0xe0..=0xff => Some(i128::from(value as i8)),
        0xcc => Some(i128::from(*raw.get(1)?)),
        0xcd => Some(i128::from(u16::from_be_bytes(
            raw.get(1..3)?.try_into().ok()?,
        ))),
        0xce => Some(i128::from(u32::from_be_bytes(
            raw.get(1..5)?.try_into().ok()?,
        ))),
        0xcf => Some(i128::from(u64::from_be_bytes(
            raw.get(1..9)?.try_into().ok()?,
        ))),
        0xd0 => Some(i128::from(*raw.get(1)? as i8)),
        0xd1 => Some(i128::from(i16::from_be_bytes(
            raw.get(1..3)?.try_into().ok()?,
        ))),
        0xd2 => Some(i128::from(i32::from_be_bytes(
            raw.get(1..5)?.try_into().ok()?,
        ))),
        0xd3 => Some(i128::from(i64::from_be_bytes(
            raw.get(1..9)?.try_into().ok()?,
        ))),
        _ => None,
    }
}

fn decode_u32_integer(raw: &[u8]) -> Option<u32> {
    u32::try_from(decode_integer(raw)?).ok()
}

fn decode_i32_binary(raw: &[u8]) -> Option<i32> {
    let bytes: [u8; 4] = binary_payload(raw)?.try_into().ok()?;
    Some(i32::from_be_bytes(bytes))
}

fn decode_u32_binary(raw: &[u8]) -> Option<u32> {
    let bytes: [u8; 4] = binary_payload(raw)?.try_into().ok()?;
    Some(u32::from_be_bytes(bytes))
}

fn decode_u16_binary(raw: &[u8]) -> Option<u16> {
    let bytes: [u8; 2] = binary_payload(raw)?.try_into().ok()?;
    Some(u16::from_be_bytes(bytes))
}
