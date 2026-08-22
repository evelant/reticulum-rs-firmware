//! Minimal, allocation-free DNS A-query wire support for the TCP bootstrap fallback.
//!
//! Embassy's built-in resolver remains the primary path because it follows the
//! DHCP-provided resolver list. This module only supplies the public-resolver
//! fallback with a host-testable encoder and a deliberately narrow response
//! parser.

use smoltcp::wire::{
    DnsFlags, DnsOpcode, DnsPacket, DnsQueryType, DnsQuestion, DnsRcode, DnsRecord, DnsRecordData,
};

const DNS_HEADER_BYTES: usize = 12;
const DNS_QUESTION_SUFFIX_BYTES: usize = 4;
const DNS_TYPE_A: u16 = 1;
const DNS_CLASS_IN: u16 = 1;

/// Maximum UDP DNS message accepted by the fallback.
///
/// The fallback does not advertise EDNS, so a conforming resolver either fits
/// its response in 512 bytes or sets the DNS truncation flag.
pub const MAX_DNS_MESSAGE_BYTES: usize = 512;

/// Return whether a hostname may be sent to the configured public fallback resolvers.
///
/// Single-label and common private/local suffixes remain DHCP-resolver-only so
/// local service names are neither leaked nor accidentally resolved publicly.
pub fn allows_public_fallback(hostname: &str) -> bool {
    let hostname = hostname.strip_suffix('.').unwrap_or(hostname);
    hostname.as_bytes().contains(&b'.')
        && ![
            "local",
            "localhost",
            "home.arpa",
            "internal",
            "lan",
            "localdomain",
        ]
        .iter()
        .any(|suffix| hostname_has_suffix(hostname, suffix))
}

/// Failure while constructing a DNS A query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DnsQueryEncodeError {
    /// The hostname is empty, has an empty label, or has a label over 63 bytes.
    InvalidHostname,
    /// The complete query does not fit in the caller-provided buffer.
    BufferTooSmall,
}

/// Failure while validating a DNS A response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DnsResponseError {
    /// The DNS transaction identifier does not match the outstanding query.
    TransactionMismatch,
    /// The packet is not a standard DNS response.
    NotAResponse,
    /// The resolver reported that its UDP response was truncated.
    Truncated,
    /// The resolver returned a non-zero DNS response code.
    ResponseCode(u8),
    /// The echoed question does not match the requested A record.
    QuestionMismatch,
    /// The response is structurally malformed or incomplete.
    Malformed,
    /// The response contains no IPv4 answer.
    NoIpv4Address,
}

/// Encode one recursion-desired IN/A query and return its used byte length.
pub fn encode_a_query(
    hostname: &str,
    transaction_id: u16,
    output: &mut [u8],
) -> Result<usize, DnsQueryEncodeError> {
    let hostname = hostname.strip_suffix('.').unwrap_or(hostname);
    if hostname.is_empty() {
        return Err(DnsQueryEncodeError::InvalidHostname);
    }

    let mut required = DNS_HEADER_BYTES + DNS_QUESTION_SUFFIX_BYTES + 1;
    for label in hostname.as_bytes().split(|byte| *byte == b'.') {
        if label.is_empty() || label.len() > 63 {
            return Err(DnsQueryEncodeError::InvalidHostname);
        }
        required = required
            .checked_add(1 + label.len())
            .ok_or(DnsQueryEncodeError::BufferTooSmall)?;
    }
    if required > output.len() {
        return Err(DnsQueryEncodeError::BufferTooSmall);
    }

    output[..required].fill(0);
    output[0..2].copy_from_slice(&transaction_id.to_be_bytes());
    output[2..4].copy_from_slice(&0x0100_u16.to_be_bytes());
    output[4..6].copy_from_slice(&1_u16.to_be_bytes());

    let mut cursor = DNS_HEADER_BYTES;
    for label in hostname.as_bytes().split(|byte| *byte == b'.') {
        output[cursor] = label.len() as u8;
        cursor += 1;
        output[cursor..cursor + label.len()].copy_from_slice(label);
        cursor += label.len();
    }
    output[cursor] = 0;
    cursor += 1;
    output[cursor..cursor + 2].copy_from_slice(&DNS_TYPE_A.to_be_bytes());
    cursor += 2;
    output[cursor..cursor + 2].copy_from_slice(&DNS_CLASS_IN.to_be_bytes());
    cursor += 2;

    Ok(cursor)
}

/// Validate one response and return its first IN/A answer.
///
/// Compressed owner names and compressed CNAME data are accepted. A matching
/// transaction, source endpoint, and echoed question are all required before
/// the caller uses the returned address.
pub fn parse_a_response(
    packet: &[u8],
    transaction_id: u16,
    hostname: &str,
) -> Result<[u8; 4], DnsResponseError> {
    let packet = DnsPacket::new_checked(packet).map_err(|_| DnsResponseError::Malformed)?;
    if packet.transaction_id() != transaction_id {
        return Err(DnsResponseError::TransactionMismatch);
    }
    if packet.opcode() != DnsOpcode::Query || !packet.flags().contains(DnsFlags::RESPONSE) {
        return Err(DnsResponseError::NotAResponse);
    }
    if packet.flags().contains(DnsFlags::TRUNCATED) {
        return Err(DnsResponseError::Truncated);
    }
    if packet.rcode() != DnsRcode::NoError {
        return Err(DnsResponseError::ResponseCode(packet.rcode().into()));
    }

    if packet.question_count() != 1 {
        return Err(DnsResponseError::QuestionMismatch);
    }

    let hostname = hostname.strip_suffix('.').unwrap_or(hostname);
    let (mut payload, question) =
        DnsQuestion::parse(packet.payload()).map_err(|_| DnsResponseError::Malformed)?;
    if question.type_ != DnsQueryType::A || !parsed_name_matches(&packet, question.name, hostname)?
    {
        return Err(DnsResponseError::QuestionMismatch);
    }

    for _ in 0..packet.answer_record_count() {
        let (remaining, record) =
            DnsRecord::parse(payload).map_err(|_| DnsResponseError::Malformed)?;
        payload = remaining;
        match record.data {
            DnsRecordData::A(address) => return Ok(address.octets()),
            DnsRecordData::Cname(name) => {
                for label in packet.parse_name(name) {
                    label.map_err(|_| DnsResponseError::Malformed)?;
                }
            }
            DnsRecordData::Aaaa(_) | DnsRecordData::Other(_, _) => {}
        }
    }

    Err(DnsResponseError::NoIpv4Address)
}

fn parsed_name_matches<T: AsRef<[u8]>>(
    packet: &DnsPacket<T>,
    encoded_name: &[u8],
    hostname: &str,
) -> Result<bool, DnsResponseError> {
    let mut expected_labels = hostname.as_bytes().split(|byte| *byte == b'.');
    for parsed_label in packet.parse_name(encoded_name) {
        let parsed_label = parsed_label.map_err(|_| DnsResponseError::Malformed)?;
        let Some(expected_label) = expected_labels.next() else {
            return Ok(false);
        };
        if parsed_label.len() != expected_label.len()
            || !parsed_label
                .iter()
                .zip(expected_label)
                .all(|(actual, expected)| actual.eq_ignore_ascii_case(expected))
        {
            return Ok(false);
        }
    }
    Ok(expected_labels.next().is_none())
}

fn hostname_has_suffix(hostname: &str, suffix: &str) -> bool {
    if hostname.len() < suffix.len() {
        return false;
    }
    let start = hostname.len() - suffix.len();
    (start == 0 || hostname.as_bytes().get(start - 1) == Some(&b'.'))
        && hostname.as_bytes()[start..].eq_ignore_ascii_case(suffix.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TXID: u16 = 0x1234;

    fn query(hostname: &str) -> ([u8; 96], usize) {
        let mut query = [0; 96];
        let length = encode_a_query(hostname, TXID, &mut query).expect("valid fixture hostname");
        (query, length)
    }

    fn response_header(answer_count: u16, flags: u16) -> [u8; DNS_HEADER_BYTES] {
        let mut header = [0; DNS_HEADER_BYTES];
        header[0..2].copy_from_slice(&TXID.to_be_bytes());
        header[2..4].copy_from_slice(&flags.to_be_bytes());
        header[4..6].copy_from_slice(&1_u16.to_be_bytes());
        header[6..8].copy_from_slice(&answer_count.to_be_bytes());
        header
    }

    fn append(output: &mut [u8], cursor: &mut usize, bytes: &[u8]) {
        output[*cursor..*cursor + bytes.len()].copy_from_slice(bytes);
        *cursor += bytes.len();
    }

    #[test]
    fn encodes_recursion_desired_a_query() {
        let (query, length) = query("rmap.world");
        assert_eq!(
            &query[..length],
            &[
                0x12, 0x34, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, b'r',
                b'm', b'a', b'p', 0x05, b'w', b'o', b'r', b'l', b'd', 0x00, 0x00, 0x01, 0x00, 0x01,
            ]
        );
    }

    #[test]
    fn parses_compressed_cname_followed_by_a_answer() {
        let (query, query_length) = query("alias.example");
        let mut response = [0; 128];
        response[..DNS_HEADER_BYTES].copy_from_slice(&response_header(2, 0x8180));
        response[DNS_HEADER_BYTES..query_length]
            .copy_from_slice(&query[DNS_HEADER_BYTES..query_length]);
        let mut cursor = query_length;

        append(
            &mut response,
            &mut cursor,
            &[
                0xc0, 0x0c, 0x00, 0x05, 0x00, 0x01, 0x00, 0x00, 0x00, 0x3c, 0x00, 0x09,
            ],
        );
        append(
            &mut response,
            &mut cursor,
            &[0x06, b't', b'a', b'r', b'g', b'e', b't', 0xc0, 0x12],
        );
        append(
            &mut response,
            &mut cursor,
            &[
                0x06, b't', b'a', b'r', b'g', b'e', b't', 0xc0, 0x12, 0x00, 0x01, 0x00, 0x01, 0x00,
                0x00, 0x00, 0x3c, 0x00, 0x04, 203, 0, 113, 7,
            ],
        );

        assert_eq!(
            parse_a_response(&response[..cursor], TXID, "alias.example"),
            Ok([203, 0, 113, 7])
        );
    }

    #[test]
    fn parses_observed_router_response_for_rmap_world() {
        let response = [
            0x12, 0x34, 0x81, 0x80, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x04, b'r',
            b'm', b'a', b'p', 0x05, b'w', b'o', b'r', b'l', b'd', 0x00, 0x00, 0x01, 0x00, 0x01,
            0xc0, 0x0c, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x0b, 0x05, 0x00, 0x04, 217, 154, 9,
            220,
        ];
        assert_eq!(
            parse_a_response(&response, TXID, "rmap.world"),
            Ok([217, 154, 9, 220])
        );
    }

    #[test]
    fn rejects_mismatched_transaction_and_question() {
        let (query, query_length) = query("rmap.world");
        let mut response = [0; 96];
        response[..DNS_HEADER_BYTES].copy_from_slice(&response_header(0, 0x8180));
        response[DNS_HEADER_BYTES..query_length]
            .copy_from_slice(&query[DNS_HEADER_BYTES..query_length]);

        assert_eq!(
            parse_a_response(&response[..query_length], TXID ^ 1, "rmap.world"),
            Err(DnsResponseError::TransactionMismatch)
        );
        assert_eq!(
            parse_a_response(&response[..query_length], TXID, "other.world"),
            Err(DnsResponseError::QuestionMismatch)
        );
    }

    #[test]
    fn reports_nxdomain_and_truncation() {
        let (query, query_length) = query("missing.example");
        let mut response = [0; 96];
        response[..DNS_HEADER_BYTES].copy_from_slice(&response_header(0, 0x8183));
        response[DNS_HEADER_BYTES..query_length]
            .copy_from_slice(&query[DNS_HEADER_BYTES..query_length]);
        assert_eq!(
            parse_a_response(&response[..query_length], TXID, "missing.example"),
            Err(DnsResponseError::ResponseCode(3))
        );

        response[2..4].copy_from_slice(&0x8380_u16.to_be_bytes());
        assert_eq!(
            parse_a_response(&response[..query_length], TXID, "missing.example"),
            Err(DnsResponseError::Truncated)
        );
    }

    #[test]
    fn rejects_malformed_packets_and_records() {
        assert_eq!(
            parse_a_response(&[0; 4], TXID, "rmap.world"),
            Err(DnsResponseError::Malformed)
        );

        let (query, query_length) = query("rmap.world");
        let mut response = [0; 96];
        response[..DNS_HEADER_BYTES].copy_from_slice(&response_header(1, 0x8180));
        response[DNS_HEADER_BYTES..query_length]
            .copy_from_slice(&query[DNS_HEADER_BYTES..query_length]);
        response[query_length..query_length + 2].copy_from_slice(&[0xc0, 0xff]);
        assert_eq!(
            parse_a_response(&response[..query_length + 2], TXID, "rmap.world"),
            Err(DnsResponseError::Malformed)
        );
    }

    #[test]
    fn public_fallback_excludes_local_names_and_accepts_public_dns_names() {
        for local in [
            "printer",
            "printer.local",
            "router.lan.",
            "service.home.arpa",
            "THING.INTERNAL",
            "host.localdomain",
            "localhost",
        ] {
            assert!(!allows_public_fallback(local), "{local}");
        }
        assert!(allows_public_fallback("rmap.world"));
        assert!(allows_public_fallback("relay.example.com."));
    }
}
