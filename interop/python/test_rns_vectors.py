"""Released-Python RNS foundation and LRRTT vector checks."""

from __future__ import annotations

import json
from pathlib import Path
import unittest

import generate_rns_vectors as vectors


CORPUS = Path(__file__).parents[1] / "vectors" / "rns-1.3.8.json"


class ReleasedRnsVectorTests(unittest.TestCase):
    """Keep committed bytes and released u-msgpack behavior inseparable."""

    @classmethod
    def setUpClass(cls) -> None:
        cls.generated = vectors.build_vectors()
        cls.lrrtt = cls.generated["lrrtt_messagepack"]
        cls.canonical = {
            case["name"]: case for case in cls.lrrtt["canonical_float64"]
        }
        cls.cases = {case["name"]: case for case in cls.lrrtt["decode_cases"]}

    def test_committed_corpus_matches_pinned_generator(self) -> None:
        committed = json.loads(CORPUS.read_text(encoding="utf-8"))
        self.assertEqual(committed, self.generated)

    def test_vendored_umsgpack_identity_is_frozen(self) -> None:
        self.assertEqual(self.lrrtt["umsgpack_version"], "2.7.1")
        self.assertEqual(
            self.lrrtt["umsgpack_source_sha256"],
            "f3f78e1281e13f96089a6b2dbac6e6d927e48b2049461c82e4be4a6e591e8d45",
        )
        self.assertEqual(
            self.lrrtt["rns_rtt_formula"],
            "max(0.25, umsgpack.unpackb(plaintext))",
        )

    def test_canonical_lrrtt_floats_are_exact_float64_messages(self) -> None:
        expected = {
            "float64_0.001": "cb3f50624dd2f1a9fc",
            "float64_0.125": "cb3fc0000000000000",
            "float64_1.0": "cb3ff0000000000000",
        }
        self.assertEqual(set(self.canonical), set(expected))
        for name, wire_hex in expected.items():
            with self.subTest(name=name):
                case = self.canonical[name]
                self.assertEqual(case["wire_hex"], wire_hex)
                self.assertEqual(case["python_unpack"]["python_type"], "float")
                self.assertEqual(case["python_unpack"]["value_class"], "finite")
                self.assertEqual(
                    vectors.umsgpack.packb(float(case["input_decimal"])).hex(),
                    wire_hex,
                )

    def test_numeric_decode_families_preserve_python_types(self) -> None:
        integer_cases = {
            "positive_fixint_1": "1",
            "negative_fixint_minus_1": "-1",
            "uint8_1": "1",
            "uint16_1": "1",
            "uint32_1": "1",
            "uint64_1": "1",
            "int8_minus_1": "-1",
            "int16_minus_1": "-1",
            "int32_minus_1": "-1",
            "int64_minus_1": "-1",
        }
        for name, decimal in integer_cases.items():
            with self.subTest(name=name):
                unpacked = self.cases[name]["python_unpack"]
                self.assertEqual(unpacked["python_type"], "int")
                self.assertEqual(unpacked["value_class"], "integer")
                self.assertEqual(unpacked["decimal"], decimal)

        float32 = self.cases["float32_0.125"]["python_unpack"]
        self.assertEqual(self.cases["float32_0.125"]["wire_hex"], "ca3e000000")
        self.assertEqual(float32["python_type"], "float")
        self.assertEqual(float32["f64_bits_hex"], "3fc0000000000000")

        self.assertEqual(
            self.cases["boolean_false"]["python_unpack"],
            {
                "result": "value",
                "python_type": "bool",
                "value_class": "boolean",
                "value": False,
            },
        )
        self.assertEqual(
            self.cases["boolean_true"]["python_unpack"],
            {
                "result": "value",
                "python_type": "bool",
                "value_class": "boolean",
                "value": True,
            },
        )

    def test_nonfinite_float_bits_and_rns_formula_results_are_stable(self) -> None:
        expected = {
            "float64_positive_infinity": (
                "positive_infinity",
                "7ff0000000000000",
                "positive_infinity",
                "7ff0000000000000",
            ),
            "float64_negative_infinity": (
                "negative_infinity",
                "fff0000000000000",
                "finite",
                "3fd0000000000000",
            ),
            "float64_nan_payload_1": (
                "nan",
                "7ff8000000000001",
                "finite",
                "3fd0000000000000",
            ),
        }
        for name, (
            unpack_class,
            unpack_bits,
            formula_class,
            formula_bits,
        ) in expected.items():
            with self.subTest(name=name):
                case = self.cases[name]
                self.assertEqual(case["python_unpack"]["value_class"], unpack_class)
                self.assertEqual(case["python_unpack"]["f64_bits_hex"], unpack_bits)
                self.assertEqual(
                    case["python_rns_rtt_formula"]["value_class"], formula_class
                )
                self.assertEqual(
                    case["python_rns_rtt_formula"]["f64_bits_hex"], formula_bits
                )

    def test_nonnumeric_and_malformed_outcomes_are_exact(self) -> None:
        nonnumeric = {
            "nil": "nil",
            "string_1.0": "string",
            "array_float64_1.0": "array",
            "map_rtt_float64_1.0": "map",
        }
        for name, value_class in nonnumeric.items():
            with self.subTest(name=name):
                case = self.cases[name]
                self.assertEqual(case["python_unpack"]["value_class"], value_class)
                self.assertEqual(
                    case["python_rns_rtt_formula"],
                    {"result": "exception", "exception_type": "TypeError"},
                )

        malformed = {
            "empty": "InsufficientDataException",
            "truncated_float64_1.0": "InsufficientDataException",
            "reserved_code": "ReservedCodeException",
        }
        for name, exception_type in malformed.items():
            with self.subTest(name=name):
                case = self.cases[name]
                self.assertEqual(
                    case["python_unpack"],
                    {"result": "exception", "exception_type": exception_type},
                )
                self.assertEqual(
                    case["python_rns_rtt_formula"], {"result": "not_run"}
                )

    def test_unpackb_uses_only_first_object_and_ignores_trailing_bytes(self) -> None:
        case = self.cases["float64_1.0_with_trailing_nil_and_reserved"]
        self.assertEqual(case["first_object_wire_hex"], "cb3ff0000000000000")
        self.assertEqual(case["trailing_hex"], "c0c1")
        first, _ = vectors.unpack_outcome(bytes.fromhex(case["first_object_wire_hex"]))
        complete, _ = vectors.unpack_outcome(bytes.fromhex(case["wire_hex"]))
        self.assertEqual(complete, first)
        self.assertEqual(complete, case["python_unpack"])

        legacy = self.cases["legacy_rete_raw_u32_timestamp"]
        self.assertEqual(legacy["wire_hex"], "6553f100")
        self.assertEqual(legacy["first_object_wire_hex"], "65")
        self.assertEqual(legacy["trailing_hex"], "53f100")
        self.assertEqual(
            legacy["python_unpack"],
            {
                "result": "value",
                "python_type": "int",
                "value_class": "integer",
                "decimal": "101",
            },
        )
        self.assertEqual(
            legacy["python_rns_rtt_formula"],
            {
                "result": "value",
                "python_type": "int",
                "value_class": "integer",
                "decimal": "101",
            },
        )

    def test_every_case_replays_and_json_contains_no_nonstandard_numbers(self) -> None:
        for case in [*self.canonical.values(), *self.cases.values()]:
            with self.subTest(name=case["name"]):
                unpacked, value = vectors.unpack_outcome(
                    bytes.fromhex(case["wire_hex"])
                )
                self.assertEqual(unpacked, case["python_unpack"])
                if unpacked["result"] == "value":
                    self.assertEqual(
                        vectors.rns_rtt_formula_outcome(value),
                        case["python_rns_rtt_formula"],
                    )
                else:
                    self.assertEqual(
                        case["python_rns_rtt_formula"], {"result": "not_run"}
                    )

        # Python's json module otherwise emits NaN and Infinity tokens by default.
        json.dumps(self.generated, allow_nan=False)


if __name__ == "__main__":
    unittest.main()
