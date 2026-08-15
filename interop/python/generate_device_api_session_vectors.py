#!/usr/bin/env python3
"""Generate independent local device-API session vectors.

This implementation uses only Python's standard-library SHA-256 and HMAC. It
does not call Rust or import Reticulum/Rete code. The fixed inputs are public
test material and are not usable credentials.
"""

from __future__ import annotations

import argparse
import hashlib
import hmac
import json
from pathlib import Path


ROOT = Path(__file__).parents[2]
DEFAULT_OUTPUT = ROOT / "interop" / "vectors" / "device-api-session-v1.json"

PROTOCOL_MAJOR = 1
PROTOCOL_MINOR = 0
SUITE = 3
BEARER_BLE_GATT = 3
KIND_CLIENT_HELLO = 0x01
KIND_SERVER_HELLO = 0x02
KIND_SERVER_PROOF = 0x03
KIND_CLIENT_PROOF = 0x04
KIND_REQUEST = 0x10
KIND_RESPONSE = 0x11
FRAMING_VERSION = 1
SERVER_FLAGS = 0x0000_0007
MAX_PAYLOAD = 512
MAX_MESSAGE = 512
MAX_IN_FLIGHT = 1

TRANSCRIPT_DOMAIN = b"reticulum-rs-firmware/device-api/session/transcript/v1\0"
HKDF_SALT_DOMAIN = b"reticulum-rs-firmware/device-api/session/hkdf-salt/v1\0"
HKDF_EXPAND_DOMAIN = b"reticulum-rs-firmware/device-api/session/hkdf-expand/v1\0"
SERVER_PROOF_DOMAIN = b"reticulum-rs-firmware/device-api/session/server-proof/v1\0"
CLIENT_PROOF_DOMAIN = b"reticulum-rs-firmware/device-api/session/client-proof/v1\0"
CLIENT_RECORD_DOMAIN = (
    b"reticulum-rs-firmware/device-api/session/client-to-device-record/v1\0"
)
SERVER_RECORD_DOMAIN = (
    b"reticulum-rs-firmware/device-api/session/device-to-client-record/v1\0"
)

PURPOSES = {
    "server_proof_key": 1,
    "client_proof_key": 2,
    "client_record_key": 3,
    "server_record_key": 4,
    "session_id": 5,
}

PSK = bytes(range(0x00, 0x20))
CREDENTIAL_ID = bytes(range(0x10, 0x20))
DEVICE_ID = bytes(range(0x20, 0x30))
CLIENT_NONCE = bytes(range(0x40, 0x60))
SERVER_NONCE = bytes(range(0x60, 0x80))
CREDENTIAL_GENERATION = 0x0102_0304_0506_0708
REQUEST_PAYLOAD = b"vector-request"
RESPONSE_PAYLOAD = b"vector-response"


def u16(value: int) -> bytes:
    return value.to_bytes(2, "little")


def u32(value: int) -> bytes:
    return value.to_bytes(4, "little")


def u64(value: int) -> bytes:
    return value.to_bytes(8, "little")


def client_hello() -> bytes:
    encoded = b"".join(
        (
            u16(PROTOCOL_MAJOR),
            u16(PROTOCOL_MINOR),
            u16(SUITE),
            bytes((BEARER_BLE_GATT, 0)),
            CREDENTIAL_ID,
            CLIENT_NONCE,
        )
    )
    assert len(encoded) == 56
    return encoded


def server_hello() -> bytes:
    encoded = b"".join(
        (
            u16(PROTOCOL_MAJOR),
            u16(PROTOCOL_MINOR),
            u16(SUITE),
            bytes((BEARER_BLE_GATT, 0)),
            DEVICE_ID,
            SERVER_NONCE,
            u64(CREDENTIAL_GENERATION),
            u16(MAX_PAYLOAD),
            u16(MAX_MESSAGE),
            bytes((MAX_IN_FLIGHT, 0, 0, 0)),
            u32(SERVER_FLAGS),
        )
    )
    assert len(encoded) == 76
    return encoded


def transcript_hash(client: bytes, server: bytes) -> bytes:
    transcript = b"".join(
        (
            TRANSCRIPT_DOMAIN,
            bytes((KIND_CLIENT_HELLO,)),
            u16(len(client)),
            client,
            bytes((KIND_SERVER_HELLO,)),
            u16(len(server)),
            server,
        )
    )
    return hashlib.sha256(transcript).digest()


def hkdf_expand(prk: bytes, info: bytes, length: int) -> bytes:
    output = bytearray()
    previous = b""
    counter = 1
    while len(output) < length:
        previous = hmac.new(
            prk,
            previous + info + bytes((counter,)),
            hashlib.sha256,
        ).digest()
        output.extend(previous)
        counter += 1
    return bytes(output[:length])


def key_schedule(transcript: bytes) -> tuple[bytes, dict[str, bytes]]:
    salt = hashlib.sha256(HKDF_SALT_DOMAIN + transcript).digest()
    prk = hmac.new(salt, PSK, hashlib.sha256).digest()
    keys: dict[str, bytes] = {}
    for name, purpose in PURPOSES.items():
        length = 16 if name == "session_id" else 32
        info = HKDF_EXPAND_DOMAIN + bytes((purpose,)) + transcript
        keys[name] = hkdf_expand(prk, info, length)
    return salt, keys


def authenticated_record_data(
    kind: int,
    session_id: bytes,
    sequence: int,
    payload: bytes,
) -> bytes:
    assert len(session_id) == 16
    assert len(payload) <= MAX_PAYLOAD
    return b"".join(
        (
            b"RDA1",
            bytes((FRAMING_VERSION, kind)),
            b"\x00\x00",
            session_id,
            u64(sequence),
            u16(len(payload)),
            payload,
        )
    )


def record_tag(key: bytes, domain: bytes, authenticated: bytes) -> bytes:
    return hmac.new(key, domain + authenticated, hashlib.sha256).digest()[:16]


def cobs_encode(decoded: bytes) -> bytes:
    encoded = bytearray((0,))
    code_index = 0
    code = 1
    for byte in decoded:
        if byte == 0:
            encoded[code_index] = code
            code_index = len(encoded)
            encoded.append(0)
            code = 1
        else:
            encoded.append(byte)
            code += 1
            if code == 0xFF:
                encoded[code_index] = code
                code_index = len(encoded)
                encoded.append(0)
                code = 1
    encoded[code_index] = code
    return bytes(encoded)


def wire_record(decoded: bytes) -> bytes:
    return b"\x00" + cobs_encode(decoded) + b"\x00"


def handshake_record(kind: int, session_id: bytes, payload: bytes) -> bytes:
    """Build the decoded framing record used for an unauthenticated flight."""
    assert kind in {
        KIND_CLIENT_HELLO,
        KIND_SERVER_HELLO,
        KIND_SERVER_PROOF,
        KIND_CLIENT_PROOF,
    }
    return authenticated_record_data(kind, session_id, 0, payload) + bytes(16)


def build_vectors() -> dict[str, object]:
    client = client_hello()
    server = server_hello()
    transcript = transcript_hash(client, server)
    salt, keys = key_schedule(transcript)
    server_proof = hmac.new(
        keys["server_proof_key"],
        SERVER_PROOF_DOMAIN + transcript,
        hashlib.sha256,
    ).digest()
    client_proof = hmac.new(
        keys["client_proof_key"],
        CLIENT_PROOF_DOMAIN + transcript + server_proof,
        hashlib.sha256,
    ).digest()

    client_hello_decoded = handshake_record(
        KIND_CLIENT_HELLO, bytes(16), client
    )
    server_hello_decoded = handshake_record(
        KIND_SERVER_HELLO, bytes(16), server
    )
    server_proof_decoded = handshake_record(
        KIND_SERVER_PROOF, keys["session_id"], server_proof
    )
    client_proof_decoded = handshake_record(
        KIND_CLIENT_PROOF, keys["session_id"], client_proof
    )

    request_authenticated = authenticated_record_data(
        KIND_REQUEST,
        keys["session_id"],
        0,
        REQUEST_PAYLOAD,
    )
    request_tag = record_tag(
        keys["client_record_key"], CLIENT_RECORD_DOMAIN, request_authenticated
    )
    request_decoded = request_authenticated + request_tag

    response_authenticated = authenticated_record_data(
        KIND_RESPONSE,
        keys["session_id"],
        0,
        RESPONSE_PAYLOAD,
    )
    response_tag = record_tag(
        keys["server_record_key"], SERVER_RECORD_DOMAIN, response_authenticated
    )
    response_decoded = response_authenticated + response_tag

    return {
        "schema": 1,
        "protocol": "reticulum-device-api-session",
        "profile": "ble-gatt-authenticated-integrity-only",
        "generator": "interop/python/generate_device_api_session_vectors.py",
        "command": "python3 interop/python/generate_device_api_session_vectors.py",
        "inputs": {
            "protocol_major": PROTOCOL_MAJOR,
            "protocol_minor": PROTOCOL_MINOR,
            "suite": SUITE,
            "bearer": BEARER_BLE_GATT,
            "credential_id_hex": CREDENTIAL_ID.hex(),
            "credential_generation": CREDENTIAL_GENERATION,
            "device_id_hex": DEVICE_ID.hex(),
            "psk_hex": PSK.hex(),
            "client_nonce_hex": CLIENT_NONCE.hex(),
            "server_nonce_hex": SERVER_NONCE.hex(),
            "server_flags": SERVER_FLAGS,
            "max_record_payload": MAX_PAYLOAD,
            "max_message": MAX_MESSAGE,
            "max_in_flight": MAX_IN_FLIGHT,
        },
        "handshake": {
            "client_hello_payload_hex": client.hex(),
            "client_hello_decoded_record_hex": client_hello_decoded.hex(),
            "client_hello_wire_hex": wire_record(client_hello_decoded).hex(),
            "server_hello_payload_hex": server.hex(),
            "server_hello_decoded_record_hex": server_hello_decoded.hex(),
            "server_hello_wire_hex": wire_record(server_hello_decoded).hex(),
            "transcript_hash_hex": transcript.hex(),
            "hkdf_salt_hex": salt.hex(),
            "server_proof_hex": server_proof.hex(),
            "server_proof_decoded_record_hex": server_proof_decoded.hex(),
            "server_proof_wire_hex": wire_record(server_proof_decoded).hex(),
            "client_proof_hex": client_proof.hex(),
            "client_proof_decoded_record_hex": client_proof_decoded.hex(),
            "client_proof_wire_hex": wire_record(client_proof_decoded).hex(),
        },
        "key_schedule": {name + "_hex": value.hex() for name, value in keys.items()},
        "records": {
            "request_payload_hex": REQUEST_PAYLOAD.hex(),
            "request_authenticated_data_hex": request_authenticated.hex(),
            "request_tag_hex": request_tag.hex(),
            "request_decoded_hex": request_decoded.hex(),
            "request_wire_hex": wire_record(request_decoded).hex(),
            "response_payload_hex": RESPONSE_PAYLOAD.hex(),
            "response_authenticated_data_hex": response_authenticated.hex(),
            "response_tag_hex": response_tag.hex(),
            "response_decoded_hex": response_decoded.hex(),
            "response_wire_hex": wire_record(response_decoded).hex(),
        },
    }


def encoded_vectors() -> str:
    return json.dumps(build_vectors(), indent=2, sort_keys=True) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument(
        "--check",
        action="store_true",
        help="fail if the committed corpus differs instead of rewriting it",
    )
    args = parser.parse_args()

    generated = encoded_vectors()
    if args.check:
        try:
            committed = args.output.read_text(encoding="utf-8")
        except FileNotFoundError:
            parser.error(f"missing vector file: {args.output}")
        if committed != generated:
            parser.error(
                f"{args.output} is stale; regenerate it with this script"
            )
        print(f"ok: {args.output} matches independent session derivation")
        return 0

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(generated, encoding="utf-8")
    print(f"wrote {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
