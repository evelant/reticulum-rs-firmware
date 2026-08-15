"""Independent session vector checks."""

from __future__ import annotations

import json
import unittest

import generate_device_api_session_vectors as vectors


def cobs_decode(encoded: bytes) -> bytes:
    """Decode one delimiter-free COBS frame for wire-vector assertions."""
    decoded = bytearray()
    index = 0
    while index < len(encoded):
        code = encoded[index]
        if code == 0:
            raise ValueError("zero byte inside COBS frame")
        index += 1
        block_end = index + code - 1
        if block_end > len(encoded):
            raise ValueError("COBS block exceeds frame")
        decoded.extend(encoded[index:block_end])
        index = block_end
        if code != 0xFF and index < len(encoded):
            decoded.append(0)
    return bytes(decoded)


class DeviceApiSessionVectorTests(unittest.TestCase):
    def test_committed_vectors_match_independent_derivation(self) -> None:
        committed = json.loads(vectors.DEFAULT_OUTPUT.read_text(encoding="utf-8"))
        self.assertEqual(committed, vectors.build_vectors())

    def test_known_answer_material_is_frozen(self) -> None:
        generated = vectors.build_vectors()
        self.assertEqual(
            generated["handshake"]["transcript_hash_hex"],
            "d5ba810802c893d9e0fb48c8c39ca93105739e2c3db7cad46391b36c09e0b994",
        )
        self.assertEqual(
            generated["key_schedule"]["session_id_hex"],
            "8ab0532b2da56a401b3847a26d7b06cd",
        )
        self.assertEqual(
            generated["handshake"]["server_proof_hex"],
            "e78f45ba3c2d5dc97adbce04ce69eaab2ee9715bb421d577de91309dbe9f83c5",
        )
        self.assertEqual(
            generated["handshake"]["client_proof_hex"],
            "e75a98367a87d0905ea1dc7d3112644d8a37db3dfa757ed43a655b7f0b4ad858",
        )
        self.assertEqual(
            generated["records"]["request_tag_hex"],
            "f8020c2c9ee75f5706744264dd7eb560",
        )

    def test_every_hello_byte_is_transcript_bound(self) -> None:
        client = vectors.client_hello()
        server = vectors.server_hello()
        expected = vectors.transcript_hash(client, server)
        for index in range(len(client)):
            mutated = bytearray(client)
            mutated[index] ^= 1
            self.assertNotEqual(
                vectors.transcript_hash(bytes(mutated), server),
                expected,
                f"client hello byte {index} was not bound",
            )
        for index in range(len(server)):
            mutated = bytearray(server)
            mutated[index] ^= 1
            self.assertNotEqual(
                vectors.transcript_hash(client, bytes(mutated)),
                expected,
                f"server hello byte {index} was not bound",
            )

    def test_all_handshake_records_have_frozen_framing_and_cobs_wires(self) -> None:
        generated = vectors.build_vectors()
        handshake = generated["handshake"]
        session_id = bytes.fromhex(generated["key_schedule"]["session_id_hex"])

        cases = (
            (
                "client_hello",
                vectors.KIND_CLIENT_HELLO,
                bytes(16),
                bytes.fromhex(handshake["client_hello_payload_hex"]),
            ),
            (
                "server_hello",
                vectors.KIND_SERVER_HELLO,
                bytes(16),
                bytes.fromhex(handshake["server_hello_payload_hex"]),
            ),
            (
                "server_proof",
                vectors.KIND_SERVER_PROOF,
                session_id,
                bytes.fromhex(handshake["server_proof_hex"]),
            ),
            (
                "client_proof",
                vectors.KIND_CLIENT_PROOF,
                session_id,
                bytes.fromhex(handshake["client_proof_hex"]),
            ),
        )

        for name, kind, record_session_id, payload in cases:
            with self.subTest(name=name):
                expected_decoded = vectors.handshake_record(
                    kind, record_session_id, payload
                )
                decoded = bytes.fromhex(
                    handshake[f"{name}_decoded_record_hex"]
                )
                wire = bytes.fromhex(handshake[f"{name}_wire_hex"])

                self.assertEqual(decoded, expected_decoded)
                self.assertEqual(decoded[-16:], bytes(16))
                self.assertEqual(wire[:1], b"\x00")
                self.assertEqual(wire[-1:], b"\x00")
                self.assertNotIn(0, wire[1:-1])
                self.assertEqual(cobs_decode(wire[1:-1]), decoded)

    def test_kdf_purposes_and_record_directions_do_not_alias(self) -> None:
        transcript = vectors.transcript_hash(
            vectors.client_hello(), vectors.server_hello()
        )
        _, keys = vectors.key_schedule(transcript)
        self.assertEqual(len(set(keys.values())), len(keys))

        authenticated = vectors.authenticated_record_data(
            vectors.KIND_REQUEST,
            keys["session_id"],
            0,
            vectors.REQUEST_PAYLOAD,
        )
        client_tag = vectors.record_tag(
            keys["client_record_key"], vectors.CLIENT_RECORD_DOMAIN, authenticated
        )
        reflected_key = vectors.record_tag(
            keys["server_record_key"], vectors.CLIENT_RECORD_DOMAIN, authenticated
        )
        reflected_domain = vectors.record_tag(
            keys["client_record_key"], vectors.SERVER_RECORD_DOMAIN, authenticated
        )
        self.assertNotEqual(client_tag, reflected_key)
        self.assertNotEqual(client_tag, reflected_domain)


if __name__ == "__main__":
    unittest.main()
