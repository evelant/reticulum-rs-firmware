//! Qualification-suite transcript, key schedule, proofs, and record tags.

use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use reticulum_device_api_framing::{AUTH_TAG_LENGTH, AUTHENTICATED_DATA_CAPACITY, Record};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::protocol::{
    CLIENT_HELLO_LENGTH, ClientHello, RECORD_KIND_CLIENT_HELLO, RECORD_KIND_SERVER_HELLO,
    SERVER_HELLO_LENGTH, ServerHello, SessionId,
};

type HmacSha256 = Hmac<Sha256>;

const TRANSCRIPT_DOMAIN: &[u8] = b"reticulum-rs-firmware/device-api/session/transcript/v1\0";
const HKDF_SALT_DOMAIN: &[u8] = b"reticulum-rs-firmware/device-api/session/hkdf-salt/v1\0";
const HKDF_EXPAND_DOMAIN: &[u8] = b"reticulum-rs-firmware/device-api/session/hkdf-expand/v1\0";
const SERVER_PROOF_DOMAIN: &[u8] = b"reticulum-rs-firmware/device-api/session/server-proof/v1\0";
const CLIENT_PROOF_DOMAIN: &[u8] = b"reticulum-rs-firmware/device-api/session/client-proof/v1\0";
const CLIENT_RECORD_DOMAIN: &[u8] =
    b"reticulum-rs-firmware/device-api/session/client-to-device-record/v1\0";
const SERVER_RECORD_DOMAIN: &[u8] =
    b"reticulum-rs-firmware/device-api/session/device-to-client-record/v1\0";

const PURPOSE_SERVER_PROOF: u8 = 1;
const PURPOSE_CLIENT_PROOF: u8 = 2;
const PURPOSE_CLIENT_RECORD: u8 = 3;
const PURPOSE_SERVER_RECORD: u8 = 4;
const PURPOSE_SESSION_ID: u8 = 5;
const EXPAND_INFO_LENGTH: usize = HKDF_EXPAND_DOMAIN.len() + 1 + 32;

pub(crate) struct KeySchedule {
    pub(crate) transcript_hash: [u8; 32],
    pub(crate) server_proof_key: Zeroizing<[u8; 32]>,
    pub(crate) client_proof_key: Zeroizing<[u8; 32]>,
    pub(crate) client_record_key: Zeroizing<[u8; 32]>,
    pub(crate) server_record_key: Zeroizing<[u8; 32]>,
    pub(crate) session_id: SessionId,
}

pub(crate) fn derive(
    psk: &[u8; 32],
    client_hello: &ClientHello,
    server_hello: &ServerHello,
) -> KeySchedule {
    let transcript_hash = transcript_hash(client_hello, server_hello);

    let mut salt_hasher = Sha256::new();
    salt_hasher.update(HKDF_SALT_DOMAIN);
    salt_hasher.update(transcript_hash);
    let salt: [u8; 32] = salt_hasher.finalize().into();
    let hkdf = Hkdf::<Sha256>::new(Some(&salt), psk);

    let server_proof_key =
        Zeroizing::new(expand::<32>(&hkdf, PURPOSE_SERVER_PROOF, &transcript_hash));
    let client_proof_key =
        Zeroizing::new(expand::<32>(&hkdf, PURPOSE_CLIENT_PROOF, &transcript_hash));
    let client_record_key =
        Zeroizing::new(expand::<32>(&hkdf, PURPOSE_CLIENT_RECORD, &transcript_hash));
    let server_record_key =
        Zeroizing::new(expand::<32>(&hkdf, PURPOSE_SERVER_RECORD, &transcript_hash));
    let session_id = SessionId(expand::<16>(&hkdf, PURPOSE_SESSION_ID, &transcript_hash));

    KeySchedule {
        transcript_hash,
        server_proof_key,
        client_proof_key,
        client_record_key,
        server_record_key,
        session_id,
    }
}

pub(crate) fn transcript_hash(client_hello: &ClientHello, server_hello: &ServerHello) -> [u8; 32] {
    let client = client_hello.encode();
    let server = server_hello.encode();
    let mut hasher = Sha256::new();
    hasher.update(TRANSCRIPT_DOMAIN);
    hasher.update([RECORD_KIND_CLIENT_HELLO]);
    hasher.update((CLIENT_HELLO_LENGTH as u16).to_le_bytes());
    hasher.update(client);
    hasher.update([RECORD_KIND_SERVER_HELLO]);
    hasher.update((SERVER_HELLO_LENGTH as u16).to_le_bytes());
    hasher.update(server);
    hasher.finalize().into()
}

pub(crate) fn server_proof(schedule: &KeySchedule) -> [u8; 32] {
    full_mac(
        &schedule.server_proof_key,
        SERVER_PROOF_DOMAIN,
        &[&schedule.transcript_hash],
    )
}

#[cfg(test)]
pub(crate) fn client_proof(schedule: &KeySchedule, server_proof: &[u8; 32]) -> [u8; 32] {
    full_mac(
        &schedule.client_proof_key,
        CLIENT_PROOF_DOMAIN,
        &[&schedule.transcript_hash, server_proof],
    )
}

pub(crate) fn verify_client_proof(
    schedule: &KeySchedule,
    server_proof: &[u8; 32],
    observed: &[u8; 32],
) -> bool {
    verify_full_mac(
        &schedule.client_proof_key,
        CLIENT_PROOF_DOMAIN,
        &[&schedule.transcript_hash, server_proof],
        observed,
    )
}

#[cfg(test)]
pub(crate) fn client_record_tag(key: &[u8; 32], record: &Record) -> [u8; AUTH_TAG_LENGTH] {
    record_tag(key, CLIENT_RECORD_DOMAIN, record)
}

pub(crate) fn verify_client_record_tag(key: &[u8; 32], record: &Record) -> bool {
    verify_record_tag(key, CLIENT_RECORD_DOMAIN, record)
}

pub(crate) fn server_record_tag(key: &[u8; 32], record: &Record) -> [u8; AUTH_TAG_LENGTH] {
    record_tag(key, SERVER_RECORD_DOMAIN, record)
}

#[cfg(test)]
pub(crate) fn verify_server_record_tag(key: &[u8; 32], record: &Record) -> bool {
    verify_record_tag(key, SERVER_RECORD_DOMAIN, record)
}

fn expand<const N: usize>(hkdf: &Hkdf<Sha256>, purpose: u8, transcript_hash: &[u8; 32]) -> [u8; N] {
    let mut info = [0_u8; EXPAND_INFO_LENGTH];
    let domain_end = HKDF_EXPAND_DOMAIN.len();
    info[..domain_end].copy_from_slice(HKDF_EXPAND_DOMAIN);
    info[domain_end] = purpose;
    info[domain_end + 1..].copy_from_slice(transcript_hash);
    let mut output = [0_u8; N];
    hkdf.expand(&info, &mut output)
        .expect("fixed qualification key expansion is within HKDF limits");
    output
}

fn full_mac(key: &[u8; 32], domain: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key)
        .expect("HMAC-SHA256 accepts the fixed qualification key length");
    mac.update(domain);
    for part in parts {
        mac.update(part);
    }
    mac.finalize().into_bytes().into()
}

fn verify_full_mac(key: &[u8; 32], domain: &[u8], parts: &[&[u8]], observed: &[u8; 32]) -> bool {
    let mut mac = HmacSha256::new_from_slice(key)
        .expect("HMAC-SHA256 accepts the fixed qualification key length");
    mac.update(domain);
    for part in parts {
        mac.update(part);
    }
    mac.verify_slice(observed).is_ok()
}

fn record_tag(key: &[u8; 32], domain: &[u8], record: &Record) -> [u8; AUTH_TAG_LENGTH] {
    let mut authenticated = [0_u8; AUTHENTICATED_DATA_CAPACITY];
    let length = record.write_authenticated_data(&mut authenticated);
    let full = full_mac(key, domain, &[&authenticated[..length]]);
    let mut tag = [0_u8; AUTH_TAG_LENGTH];
    tag.copy_from_slice(&full[..AUTH_TAG_LENGTH]);
    tag
}

fn verify_record_tag(key: &[u8; 32], domain: &[u8], record: &Record) -> bool {
    let mut authenticated = [0_u8; AUTHENTICATED_DATA_CAPACITY];
    let length = record.write_authenticated_data(&mut authenticated);
    let mut mac = HmacSha256::new_from_slice(key)
        .expect("HMAC-SHA256 accepts the fixed qualification key length");
    mac.update(domain);
    mac.update(&authenticated[..length]);
    mac.verify_truncated_left(record.authentication_tag())
        .is_ok()
}
