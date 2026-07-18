#!/usr/bin/env python3
"""Generate independent device-API wired-pairing known-answer vectors.

This implementation uses only Python's standard-library SHA-256 and HMAC and
an independent RDA1/COBS encoder. It does not call Rust or import project code.
All fixed secrets are public test material and must never be used as credentials.
"""

from __future__ import annotations

import argparse
import hashlib
import hmac
import json
from pathlib import Path


ROOT = Path(__file__).parents[2]
DEFAULT_OUTPUT = ROOT / "interop" / "vectors" / "device-api-pairing-v1.json"

PAIRING_MAJOR = 1
PAIRING_MINOR = 0
PAIRING_SUITE = 2
BEARER_USB_SERIAL_JTAG = 1
FRAMING_VERSION = 1

KIND_BEGIN_REQUEST = 0x24
KIND_BEGIN_RESPONSE = 0x25
KIND_PROOF_REQUEST = 0x26
KIND_PROOF_RESPONSE = 0x27
KIND_ACTIVATE_REQUEST = 0x28
KIND_ACTIVATE_RESPONSE = 0x29
KIND_ABORT_CURRENT_REQUEST = 0x2A
KIND_ABORT_CURRENT_RESPONSE = 0x2B

TRANSCRIPT_DOMAIN = b"reticulum-rs-firmware/device-api/pairing/transcript/v1\0"
CLIENT_PROOF_DOMAIN = b"reticulum-rs-firmware/device-api/pairing/client-proof/v2\0"
ACTIVATION_PROOF_DOMAIN = (
    b"reticulum-rs-firmware/device-api/pairing/activation-proof/v2\0"
)

DEVICE_ID = b"e290-api-1" + bytes.fromhex("aca704e13e88")
CREDENTIAL_ID = bytes(range(0x10, 0x20))
CREDENTIAL_GENERATION = 0x0102_0304_0506_0708
ACTIVATED_CREDENTIAL_GENERATION = CREDENTIAL_GENERATION + 1
PSK = bytes(range(0x20, 0x40))
CLIENT_NONCE = bytes(range(0x40, 0x60))
DEVICE_CHALLENGE = bytes(range(0x60, 0x80))
CONNECTION_ID = 0x1112_1314_1516_1718
WINDOW_ID = 0x2122_2324_2526_2728


def u16(value: int) -> bytes:
    return value.to_bytes(2, "little")


def u64(value: int) -> bytes:
    return value.to_bytes(8, "little")


def begin_request_payload() -> bytes:
    return b""


def begin_offer_payload() -> bytes:
    payload = b"".join(
        (
            bytes((0,)),
            bytes(7),
            u16(PAIRING_MAJOR),
            u16(PAIRING_MINOR),
            u16(PAIRING_SUITE),
            bytes((BEARER_USB_SERIAL_JTAG, 0)),
            DEVICE_ID,
            CREDENTIAL_ID,
            u64(CREDENTIAL_GENERATION),
            PSK,
        )
    )
    assert len(payload) == 88
    return payload


def proof_request_payload() -> bytes:
    payload = b"".join(
        (
            u16(PAIRING_MAJOR),
            u16(PAIRING_MINOR),
            u16(PAIRING_SUITE),
            bytes((BEARER_USB_SERIAL_JTAG, 0)),
            CREDENTIAL_ID,
            u64(CREDENTIAL_GENERATION),
            CLIENT_NONCE,
        )
    )
    assert len(payload) == 64
    return payload


def proof_challenge_payload() -> bytes:
    payload = b"".join(
        (
            bytes((0,)),
            bytes(7),
            u16(PAIRING_MAJOR),
            u16(PAIRING_MINOR),
            u16(PAIRING_SUITE),
            bytes((BEARER_USB_SERIAL_JTAG, 0)),
            DEVICE_ID,
            u64(CONNECTION_ID),
            u64(WINDOW_ID),
            CREDENTIAL_ID,
            u64(CREDENTIAL_GENERATION),
            DEVICE_CHALLENGE,
        )
    )
    assert len(payload) == 104
    return payload


def transcript_hash(request: bytes, challenge: bytes) -> bytes:
    transcript = b"".join(
        (
            TRANSCRIPT_DOMAIN,
            bytes((KIND_PROOF_REQUEST,)),
            u16(len(request)),
            request,
            bytes((KIND_PROOF_RESPONSE,)),
            u16(len(challenge)),
            challenge,
        )
    )
    return hashlib.sha256(transcript).digest()


def client_proof(transcript: bytes) -> bytes:
    return hmac.new(
        PSK,
        CLIENT_PROOF_DOMAIN + transcript,
        hashlib.sha256,
    ).digest()


def activation_confirmation(
    transcript: bytes, proof: bytes, activated_generation: int
) -> bytes:
    return hmac.new(
        PSK,
        ACTIVATION_PROOF_DOMAIN + transcript + proof + u64(activated_generation),
        hashlib.sha256,
    ).digest()


def activate_request_payload(proof: bytes) -> bytes:
    payload = CREDENTIAL_ID + u64(CREDENTIAL_GENERATION) + proof
    assert len(payload) == 56
    return payload


def activate_response_payload(confirmation: bytes) -> bytes:
    payload = b"".join(
        (
            bytes((0,)),
            bytes(7),
            CREDENTIAL_ID,
            u64(ACTIVATED_CREDENTIAL_GENERATION),
            confirmation,
        )
    )
    assert len(payload) == 64
    return payload


def abort_current_request_payload() -> bytes:
    return b""


def abort_current_response_payload() -> bytes:
    return bytes((0,))


def decoded_record(kind: int, sequence: int, payload: bytes) -> bytes:
    assert len(payload) <= 512
    return b"".join(
        (
            b"RDA1",
            bytes((FRAMING_VERSION, kind)),
            b"\x00\x00",
            bytes(16),
            u64(sequence),
            u16(len(payload)),
            payload,
            bytes(16),
        )
    )


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


def record_vector(kind: int, sequence: int, payload: bytes) -> dict[str, object]:
    decoded = decoded_record(kind, sequence, payload)
    return {
        "kind": kind,
        "sequence": sequence,
        "payload_hex": payload.hex(),
        "decoded_record_hex": decoded.hex(),
        "wire_hex": wire_record(decoded).hex(),
    }


def build_vectors() -> dict[str, object]:
    proof_request = proof_request_payload()
    challenge = proof_challenge_payload()
    transcript = transcript_hash(proof_request, challenge)
    proof = client_proof(transcript)
    confirmation = activation_confirmation(
        transcript, proof, ACTIVATED_CREDENTIAL_GENERATION
    )

    return {
        "profile": "reticulum-rs-firmware device API wired pairing v1",
        "generator": "interop/python/generate_device_api_pairing_vectors.py",
        "command": "python3 interop/python/generate_device_api_pairing_vectors.py",
        "inputs": {
            "device_id_hex": DEVICE_ID.hex(),
            "credential_id_hex": CREDENTIAL_ID.hex(),
            "credential_generation": CREDENTIAL_GENERATION,
            "activated_credential_generation": ACTIVATED_CREDENTIAL_GENERATION,
            "psk_hex": PSK.hex(),
            "client_nonce_hex": CLIENT_NONCE.hex(),
            "device_challenge_hex": DEVICE_CHALLENGE.hex(),
            "connection_id": CONNECTION_ID,
            "window_id": WINDOW_ID,
        },
        "proof": {
            "transcript_domain_hex": TRANSCRIPT_DOMAIN.hex(),
            "client_proof_domain_hex": CLIENT_PROOF_DOMAIN.hex(),
            "activation_proof_domain_hex": ACTIVATION_PROOF_DOMAIN.hex(),
            "transcript_hash_hex": transcript.hex(),
            "client_proof_hex": proof.hex(),
            "activation_confirmation_hex": confirmation.hex(),
        },
        "records": {
            "begin_request": record_vector(
                KIND_BEGIN_REQUEST, 0, begin_request_payload()
            ),
            "begin_response": record_vector(
                KIND_BEGIN_RESPONSE, 0, begin_offer_payload()
            ),
            "proof_request": record_vector(KIND_PROOF_REQUEST, 1, proof_request),
            "proof_response": record_vector(KIND_PROOF_RESPONSE, 1, challenge),
            "activate_request": record_vector(
                KIND_ACTIVATE_REQUEST, 2, activate_request_payload(proof)
            ),
            "activate_response": record_vector(
                KIND_ACTIVATE_RESPONSE,
                2,
                activate_response_payload(confirmation),
            ),
            "abort_current_request": record_vector(
                KIND_ABORT_CURRENT_REQUEST, 3, abort_current_request_payload()
            ),
            "abort_current_response": record_vector(
                KIND_ABORT_CURRENT_RESPONSE, 3, abort_current_response_payload()
            ),
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
        print(f"ok: {args.output} matches independent pairing derivation")
        return 0

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(generated, encoding="utf-8")
    print(f"wrote {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
