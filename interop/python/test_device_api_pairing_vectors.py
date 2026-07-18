"""Independent wired-pairing vector checks."""

from __future__ import annotations

import hashlib
import hmac
import json
import unittest

import generate_device_api_pairing_vectors as vectors


def cobs_decode(encoded: bytes) -> bytes:
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


class DeviceApiPairingVectorTests(unittest.TestCase):
    def test_committed_vectors_match_independent_derivation(self) -> None:
        committed = json.loads(vectors.DEFAULT_OUTPUT.read_text(encoding="utf-8"))
        self.assertEqual(committed, vectors.build_vectors())

    def test_known_answer_material_is_frozen(self) -> None:
        generated = vectors.build_vectors()["proof"]
        self.assertEqual(
            generated["transcript_hash_hex"],
            "bffdaf52b55fe93afc49c893e16d2c82943e042cf0087fb66f6df99e3398cf6a",
        )
        self.assertEqual(
            generated["client_proof_hex"],
            "b8d1cd5617191b45c59c1e8ca75ae9d317e36f94be7740c3fb9da1467cd2abba",
        )
        self.assertEqual(
            generated["activation_confirmation_hex"],
            "a7d3810f6f0a30b71cb614c164eee53705f0b6d13170c2b419b6f9b1bc456803",
        )

    def test_every_proof_payload_byte_and_role_is_transcript_bound(self) -> None:
        request = vectors.proof_request_payload()
        challenge = vectors.proof_challenge_payload()
        expected = vectors.transcript_hash(request, challenge)

        for index in range(len(request)):
            mutated = bytearray(request)
            mutated[index] ^= 1
            self.assertNotEqual(
                vectors.transcript_hash(bytes(mutated), challenge),
                expected,
                f"ProofStart byte {index} was not bound",
            )
        for index in range(len(challenge)):
            mutated = bytearray(challenge)
            mutated[index] ^= 1
            self.assertNotEqual(
                vectors.transcript_hash(request, bytes(mutated)),
                expected,
                f"challenge byte {index} was not bound",
            )

        wrong_request_role = vectors.KIND_PROOF_REQUEST ^ 1
        wrong_response_role = vectors.KIND_PROOF_RESPONSE ^ 1
        role_mutated = b"".join(
            (
                vectors.TRANSCRIPT_DOMAIN,
                bytes((wrong_request_role,)),
                vectors.u16(len(request)),
                request,
                bytes((vectors.KIND_PROOF_RESPONSE,)),
                vectors.u16(len(challenge)),
                challenge,
            )
        )
        self.assertNotEqual(hashlib.sha256(role_mutated).digest(), expected)
        role_mutated = b"".join(
            (
                vectors.TRANSCRIPT_DOMAIN,
                bytes((vectors.KIND_PROOF_REQUEST,)),
                vectors.u16(len(request)),
                request,
                bytes((wrong_response_role,)),
                vectors.u16(len(challenge)),
                challenge,
            )
        )
        self.assertNotEqual(hashlib.sha256(role_mutated).digest(), expected)

    def test_all_records_have_canonical_zero_session_tag_framing_and_cobs(self) -> None:
        generated = vectors.build_vectors()["records"]
        for name, material in generated.items():
            with self.subTest(name=name):
                payload = bytes.fromhex(material["payload_hex"])
                decoded = bytes.fromhex(material["decoded_record_hex"])
                wire = bytes.fromhex(material["wire_hex"])
                self.assertEqual(
                    decoded,
                    vectors.decoded_record(
                        material["kind"], material["sequence"], payload
                    ),
                )
                self.assertEqual(decoded[8:24], bytes(16))
                self.assertEqual(decoded[-16:], bytes(16))
                self.assertEqual(wire[:1], b"\x00")
                self.assertEqual(wire[-1:], b"\x00")
                self.assertNotIn(0, wire[1:-1])
                self.assertEqual(cobs_decode(wire[1:-1]), decoded)

    def test_proof_and_activation_domains_are_independent(self) -> None:
        transcript = vectors.transcript_hash(
            vectors.proof_request_payload(), vectors.proof_challenge_payload()
        )
        proof = vectors.client_proof(transcript)
        confirmation = vectors.activation_confirmation(transcript, proof)
        reflected = hmac.new(
            vectors.PSK,
            vectors.CLIENT_PROOF_DOMAIN + transcript + proof,
            hashlib.sha256,
        ).digest()
        self.assertNotEqual(proof, confirmation)
        self.assertNotEqual(confirmation, reflected)


if __name__ == "__main__":
    unittest.main()
