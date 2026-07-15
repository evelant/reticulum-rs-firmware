from __future__ import annotations

from io import BytesIO
import hashlib
import json
from pathlib import Path
from copy import deepcopy
import unittest

import rnode_hil


CORPUS = Path(__file__).parents[1] / "vectors" / "rnode-hil-v1.json"


class FakeSerial:
    def __init__(self, *, airtime_echo_delta: int = 0) -> None:
        self.decoder = rnode_hil.KissDecoder()
        self.pending = bytearray()
        self.commands: list[tuple[int, bytes]] = []
        self.airtime_echo_delta = airtime_echo_delta
        self.settings: dict[int, bytes] = {}

    @property
    def in_waiting(self) -> int:
        return len(self.pending)

    def write(self, wire: bytes) -> int:
        frames = self.decoder.feed(wire)
        if len(frames) != 1:
            raise AssertionError(f"expected one host frame, got {frames!r}")
        frame = frames[0]
        self.commands.append((frame.command, frame.payload))

        response: bytes | None
        if frame.command == rnode_hil.CMD_DETECT:
            response = bytes([rnode_hil.DETECT_RESPONSE])
        elif frame.command == rnode_hil.CMD_FW_VERSION:
            response = bytes([1, 86])
        elif frame.command == rnode_hil.CMD_BOARD:
            response = bytes([0x37])
        elif frame.command == rnode_hil.CMD_PLATFORM:
            response = bytes([0x80])
        elif frame.command == rnode_hil.CMD_MCU:
            response = bytes([0x81])
        elif frame.command == rnode_hil.CMD_READY:
            response = bytes([1])
        elif frame.command == rnode_hil.CMD_DATA:
            response = None
        elif frame.command in {rnode_hil.CMD_FREQUENCY, rnode_hil.CMD_BANDWIDTH}:
            if frame.payload == b"\x00" * 4:
                response = self.settings[frame.command]
            else:
                self.settings[frame.command] = frame.payload
                response = frame.payload
        elif frame.command in {
            rnode_hil.CMD_TXPOWER,
            rnode_hil.CMD_SF,
            rnode_hil.CMD_CR,
            rnode_hil.CMD_RADIO_STATE,
        }:
            if frame.payload == b"\xff":
                response = self.settings[frame.command]
            else:
                self.settings[frame.command] = frame.payload
                response = frame.payload
        elif frame.command in {rnode_hil.CMD_ST_ALOCK, rnode_hil.CMD_LT_ALOCK}:
            requested = int.from_bytes(frame.payload, "big")
            response = (requested - self.airtime_echo_delta).to_bytes(2, "big")
        else:
            response = frame.payload
        if (
            frame.command == rnode_hil.CMD_RADIO_STATE
            and frame.payload == bytes([rnode_hil.RADIO_STATE_ON])
        ):
            # Pinned RNode emits this unsolicited timing record while applying
            # the modem profile, before acknowledging radio state.
            physical = b"\x04\x00\x03\xd0\x00\x18\x00\x19\x00\x18\x00\x30"
            self.pending.extend(rnode_hil.kiss_frame(rnode_hil.CMD_STAT_PHYPRM, physical))
        if response is not None:
            self.pending.extend(rnode_hil.kiss_frame(frame.command, response))
        return len(wire)

    def read(self, length: int) -> bytes:
        if not self.pending:
            return b""
        count = min(length, 3, len(self.pending))
        chunk = bytes(self.pending[:count])
        del self.pending[:count]
        return chunk

    def flush(self) -> None:
        pass


class BatchReadSerial:
    def __init__(self, payload: bytes) -> None:
        self.pending = payload

    @property
    def in_waiting(self) -> int:
        return len(self.pending)

    def read(self, length: int) -> bytes:
        payload = self.pending[:length]
        self.pending = self.pending[length:]
        return payload


class KissTests(unittest.TestCase):
    def test_escape_round_trip_across_fragmented_reads(self) -> None:
        payload = bytes([0x00, rnode_hil.FEND, 0x42, rnode_hil.FESC, 0xFF])
        wire = rnode_hil.kiss_frame(rnode_hil.CMD_DATA, payload)
        decoder = rnode_hil.KissDecoder()
        decoded = []
        for byte in wire:
            decoded.extend(decoder.feed(bytes([byte])))
        self.assertEqual(
            decoded,
            [rnode_hil.DecodedFrame(rnode_hil.CMD_DATA, payload, wire)],
        )

    def test_invalid_escape_is_rejected(self) -> None:
        decoder = rnode_hil.KissDecoder()
        with self.assertRaisesRegex(ValueError, "invalid KISS escape"):
            decoder.feed(bytes([rnode_hil.FEND, 0x00, rnode_hil.FESC, 0x01]))

    def test_truncated_escape_is_rejected(self) -> None:
        decoder = rnode_hil.KissDecoder()
        with self.assertRaisesRegex(ValueError, "truncated KISS escape"):
            decoder.feed(bytes([rnode_hil.FEND, 0x00, rnode_hil.FESC, rnode_hil.FEND]))

    def test_error_does_not_hide_later_frames_from_same_read(self) -> None:
        wire = rnode_hil.kiss_frame(rnode_hil.CMD_ERROR, b"\x01") + rnode_hil.kiss_frame(
            rnode_hil.CMD_BOARD, b"\x37"
        )
        transcript_file = BytesIO()
        peer = rnode_hil.RNodePeer(
            BatchReadSerial(wire),
            rnode_hil.Transcript(transcript_file),
        )
        with self.assertRaisesRegex(OSError, "error code 1"):
            peer._read_frames()
        records = transcript_file.getvalue().decode("utf-8").splitlines()
        self.assertEqual(len(records), 2)
        self.assertEqual(peer.latest_frames[rnode_hil.CMD_BOARD].payload, b"\x37")

    def test_peer_configuration_and_transcript_are_exact(self) -> None:
        serial = FakeSerial()
        transcript_file = BytesIO()
        peer = rnode_hil.RNodePeer(serial, rnode_hil.Transcript(transcript_file))
        self.assertEqual(peer.inspect_device()["firmware_version"], "1.86")
        configuration_start = len(serial.commands)
        physical = peer.configure(
            {
                "frequency_hz": 915_000_000,
                "bandwidth_hz": 125_000,
                "spreading_factor": 7,
                "coding_rate_denominator": 5,
                "tx_power_dbm": 2,
                "expected_peer_preamble_symbols": 24,
                "short_airtime_limit_basis_points": 500,
                "long_airtime_limit_basis_points": 250,
            }
        )
        peer.set_promiscuous(True)
        peer.transmit(bytes([0xC0, 0xDB, 0x42]))

        self.assertIn((rnode_hil.CMD_PROMISC, b"\x01"), serial.commands)
        configuration = serial.commands[configuration_start:]
        self.assertEqual(
            configuration[0],
            (rnode_hil.CMD_RADIO_STATE, bytes([rnode_hil.RADIO_STATE_OFF])),
        )
        self.assertLess(
            configuration.index((rnode_hil.CMD_IMPLICIT, b"\x00")),
            configuration.index(
                (rnode_hil.CMD_RADIO_STATE, bytes([rnode_hil.RADIO_STATE_ON]))
            ),
        )
        self.assertIn((rnode_hil.CMD_ST_ALOCK, b"\x01\xf4"), serial.commands)
        self.assertIn((rnode_hil.CMD_LT_ALOCK, b"\x00\xfa"), serial.commands)
        self.assertIn((rnode_hil.CMD_DATA, bytes([0xC0, 0xDB, 0x42])), serial.commands)
        self.assertEqual(physical["preamble_symbols"], 24)
        self.assertEqual(physical["symbol_time_us"], 1_024)
        records = [
            json.loads(line)
            for line in transcript_file.getvalue().decode("utf-8").splitlines()
        ]
        self.assertTrue(records)
        self.assertEqual(
            [record["sequence"] for record in records], list(range(len(records)))
        )

    def test_one_basis_point_airtime_float_rounding_is_recorded(self) -> None:
        peer = rnode_hil.RNodePeer(
            FakeSerial(airtime_echo_delta=1),
            rnode_hil.Transcript(BytesIO()),
        )
        physical = peer.configure(
            {
                "frequency_hz": 915_000_000,
                "bandwidth_hz": 125_000,
                "spreading_factor": 7,
                "coding_rate_denominator": 5,
                "tx_power_dbm": 2,
                "expected_peer_preamble_symbols": 24,
                "short_airtime_limit_basis_points": 420,
                "long_airtime_limit_basis_points": 250,
            }
        )
        self.assertEqual(
            physical["effective_short_airtime_limit_basis_points"], 419
        )
        self.assertEqual(
            physical["effective_long_airtime_limit_basis_points"], 249
        )

    def test_empty_payload_cannot_rely_on_rnode_queue_bug(self) -> None:
        peer = rnode_hil.RNodePeer(FakeSerial(), rnode_hil.Transcript(BytesIO()))
        with self.assertRaisesRegex(ValueError, "empty payload"):
            peer.transmit(b"")

    def test_profile_rejects_peer_preamble_mismatch(self) -> None:
        peer = rnode_hil.RNodePeer(FakeSerial(), rnode_hil.Transcript(BytesIO()))
        profile = {
            "frequency_hz": 915_000_000,
            "bandwidth_hz": 125_000,
            "spreading_factor": 7,
            "coding_rate_denominator": 5,
            "tx_power_dbm": 2,
            "expected_peer_preamble_symbols": 25,
            "short_airtime_limit_basis_points": 500,
            "long_airtime_limit_basis_points": 250,
        }
        with self.assertRaisesRegex(ValueError, "reported preamble 24"):
            peer.configure(profile)


class CorpusTests(unittest.TestCase):
    def test_custom_corpus_is_not_overwritten_by_send_subparser_defaults(self) -> None:
        parser = rnode_hil.build_parser()
        args = parser.parse_args(
            [
                "--corpus",
                "/tmp/custom-corpus.json",
                "send",
                "boot-local-data",
                "--port",
                "/dev/null",
                "--target-artifact-mode",
                "lab-rx",
                "--output-dir",
                "/tmp/output",
                "--frequency-hz",
                "915000000",
                "--bandwidth-hz",
                "125000",
                "--spreading-factor",
                "7",
                "--coding-rate-denominator",
                "5",
                "--tx-power-dbm",
                "2",
                "--expected-peer-preamble-symbols",
                "24",
                "--receiver-preamble-symbols",
                "18",
                "--short-airtime-limit-basis-points",
                "500",
                "--long-airtime-limit-basis-points",
                "250",
                "--post-enqueue-observation-ms",
                "2000",
                "--expected-firmware",
                "1.86",
                "--region-basis",
                "test only",
                "--antenna-or-load-attached",
                "--fresh-peer-reset-ack",
                rnode_hil.FRESH_PEER_RESET_ACK,
                "--fresh-tracker-boot-ack",
                rnode_hil.FRESH_TRACKER_BOOT_ACK,
                "--transmit-ack",
                rnode_hil.TRANSMIT_ACK,
            ]
        )
        self.assertEqual(args.corpus, Path("/tmp/custom-corpus.json"))

    def test_committed_corpus_has_safe_physical_bounds_and_exact_hashes(self) -> None:
        corpus = rnode_hil.load_corpus(CORPUS)
        self.assertEqual(corpus["schema"], 3)
        self.assertEqual(len(corpus["scenarios"]), 19)
        names = set()
        for scenario in corpus["scenarios"]:
            self.assertNotIn(scenario["name"], names)
            names.add(scenario["name"])
            self.assertEqual(len(rnode_hil.scenario_modes(scenario)), 1)
            for step in scenario["steps"]:
                payload = bytes.fromhex(step["payload_hex"])
                self.assertEqual(len(payload), step["payload_len"])
                self.assertEqual(hashlib.sha256(payload).hexdigest(), step["payload_sha256"])
                self.assertGreater(len(payload), 0)
                maximum = 255 if step["mode"] == "raw_lora_frame" else 508
                self.assertLessEqual(len(payload), maximum)
            rnode_hil.validate_scenario_for_send(
                corpus,
                scenario,
                receiver_fragment_timeout_us=5_246_912,
                receiver_maximum_frame_airtime_us=123_456,
                peer_preamble_extension_us=6_144,
            )

    def test_fragment_timeout_wait_has_no_hidden_default(self) -> None:
        wait = {"kind": "receiver_fragment_timeout", "margin_ms": 1_000}
        with self.assertRaisesRegex(ValueError, "requires --receiver"):
            rnode_hil.resolve_wait_seconds(
                wait,
                receiver_fragment_timeout_us=None,
                receiver_maximum_frame_airtime_us=None,
                peer_preamble_extension_us=None,
            )
        self.assertAlmostEqual(
            rnode_hil.resolve_wait_seconds(
                wait,
                receiver_fragment_timeout_us=5_500_000,
                receiver_maximum_frame_airtime_us=250_000,
                peer_preamble_extension_us=6_144,
            ),
            6.756144,
        )

    def test_peer_preamble_extension_covers_rnode_dynamic_preamble(self) -> None:
        self.assertEqual(
            rnode_hil.peer_preamble_extension_us(
                receiver_preamble_symbols=18,
                peer_preamble_symbols=24,
                spreading_factor=7,
                bandwidth_hz=125_000,
            ),
            6_144,
        )

    def test_target_artifact_mode_is_bound_to_scenario(self) -> None:
        corpus = rnode_hil.load_corpus(CORPUS)
        ordinary = next(
            item for item in corpus["scenarios"] if item["name"] == "raw-single-1"
        )
        pressure = next(
            item
            for item in corpus["scenarios"]
            if item["name"] == "raw-backpressure-four-frame"
        )
        returned_fault = next(
            item
            for item in corpus["scenarios"]
            if item["name"] == "raw-returned-fault-trigger"
        )
        returned_fault_repeat = next(
            item
            for item in corpus["scenarios"]
            if item["name"] == "raw-returned-fault-repeat-until-quarantine"
        )
        rnode_hil.validate_target_artifact_mode(ordinary, "lab-rx")
        rnode_hil.validate_target_artifact_mode(
            pressure, "lab-rx-backpressure-hil"
        )
        rnode_hil.validate_target_artifact_mode(
            returned_fault, rnode_hil.RETURNED_FAULT_ARTIFACT_MODE
        )
        rnode_hil.validate_target_artifact_mode(
            returned_fault_repeat,
            rnode_hil.RETURNED_FAULT_REPEAT_ARTIFACT_MODE,
        )
        self.assertEqual(
            pressure["target_expectations"],
            rnode_hil.BACKPRESSURE_TARGET_EXPECTATIONS,
        )
        self.assertEqual(
            len(pressure["unstalled_reference_deltas"]["completed_packets"]), 3
        )
        self.assertEqual(
            returned_fault["target_expectations"],
            rnode_hil.RETURNED_FAULT_TARGET_EXPECTATIONS,
        )
        self.assertEqual(
            returned_fault_repeat["target_expectations"],
            rnode_hil.RETURNED_FAULT_REPEAT_TARGET_EXPECTATIONS,
        )
        self.assertEqual(
            returned_fault["unstalled_reference_deltas"]["completed_packets"],
            [
                {
                    "packet_len": 167,
                    "packet_sha256": "74dd63d749a9df03f2d315d3bf8ee5568d13a1ebbbd55f380392e3eff9b93080",
                    "rete_disposition": "processed",
                    "rns_admitted": True,
                }
            ],
        )
        self.assertEqual(
            returned_fault_repeat["unstalled_reference_deltas"],
            returned_fault["unstalled_reference_deltas"],
        )
        with self.assertRaisesRegex(ValueError, "requires --target-artifact-mode"):
            rnode_hil.validate_target_artifact_mode(
                pressure, "lab-rx"
            )
        changed_pressure = deepcopy(pressure)
        changed_pressure["steps"][1]["payload_hex"] = (
            "00" + changed_pressure["steps"][1]["payload_hex"][2:]
        )
        with self.assertRaisesRegex(ValueError, "exact committed pressure stimulus"):
            rnode_hil.validate_target_artifact_mode(
                changed_pressure, "lab-rx-backpressure-hil"
            )
        with self.assertRaisesRegex(ValueError, "requires --target-artifact-mode"):
            rnode_hil.validate_target_artifact_mode(
                returned_fault, "lab-rx"
            )
        with self.assertRaisesRegex(ValueError, "requires --target-artifact-mode"):
            rnode_hil.validate_target_artifact_mode(
                returned_fault,
                rnode_hil.RETURNED_FAULT_REPEAT_ARTIFACT_MODE,
            )
        with self.assertRaisesRegex(ValueError, "requires --target-artifact-mode"):
            rnode_hil.validate_target_artifact_mode(
                returned_fault_repeat,
                rnode_hil.RETURNED_FAULT_ARTIFACT_MODE,
            )
        changed_returned_fault = deepcopy(returned_fault)
        changed_returned_fault["steps"][0]["payload_hex"] = (
            "00" + changed_returned_fault["steps"][0]["payload_hex"][2:]
        )
        with self.assertRaisesRegex(ValueError, "exact committed trigger stimulus"):
            rnode_hil.validate_target_artifact_mode(
                changed_returned_fault, rnode_hil.RETURNED_FAULT_ARTIFACT_MODE
            )
        changed_returned_fault_repeat = deepcopy(returned_fault_repeat)
        changed_returned_fault_repeat["steps"][0]["payload_hex"] = (
            "00" + changed_returned_fault_repeat["steps"][0]["payload_hex"][2:]
        )
        with self.assertRaisesRegex(ValueError, "exact committed trigger stimulus"):
            rnode_hil.validate_target_artifact_mode(
                changed_returned_fault_repeat,
                rnode_hil.RETURNED_FAULT_REPEAT_ARTIFACT_MODE,
            )

    def test_fragment_timeout_wait_rejects_zero_margin(self) -> None:
        with self.assertRaisesRegex(ValueError, "1000 through"):
            rnode_hil.resolve_wait_seconds(
                {"kind": "receiver_fragment_timeout", "margin_ms": 0},
                receiver_fragment_timeout_us=5_500_000,
                receiver_maximum_frame_airtime_us=250_000,
                peer_preamble_extension_us=6_144,
            )

    def test_custom_scenario_is_fully_validated_before_send(self) -> None:
        corpus = rnode_hil.load_corpus(CORPUS)
        scenario = deepcopy(corpus["scenarios"][0])
        scenario["steps"][0]["mode"] = "unknown"
        with self.assertRaisesRegex(ValueError, "unsupported mode"):
            rnode_hil.validate_scenario_for_send(
                corpus,
                scenario,
                receiver_fragment_timeout_us=None,
                receiver_maximum_frame_airtime_us=None,
                peer_preamble_extension_us=0,
            )

        scenario = deepcopy(corpus["scenarios"][0])
        scenario["steps"][0]["payload_sha256"] = "00" * 32
        with self.assertRaisesRegex(ValueError, "payload_sha256"):
            rnode_hil.validate_scenario_for_send(
                corpus,
                scenario,
                receiver_fragment_timeout_us=None,
                receiver_maximum_frame_airtime_us=None,
                peer_preamble_extension_us=0,
            )

        scenario = deepcopy(corpus["scenarios"][0])
        scenario["steps"][0]["wait_after"] = {
            "kind": "fixed",
            "milliseconds": -1,
        }
        with self.assertRaisesRegex(ValueError, "fixed wait"):
            rnode_hil.validate_scenario_for_send(
                corpus,
                scenario,
                receiver_fragment_timeout_us=None,
                receiver_maximum_frame_airtime_us=None,
                peer_preamble_extension_us=0,
            )

        scenario = deepcopy(
            next(item for item in corpus["scenarios"] if item["name"] == "rnode-split-256")
        )
        scenario["steps"][0]["mode"] = "raw_lora_frame"
        with self.assertRaisesRegex(ValueError, "255-byte raw_lora_frame limit"):
            rnode_hil.validate_scenario_for_send(
                corpus,
                scenario,
                receiver_fragment_timeout_us=None,
                receiver_maximum_frame_airtime_us=None,
                peer_preamble_extension_us=0,
            )

        scenario = deepcopy(
            next(
                item
                for item in corpus["scenarios"]
                if item["name"] == "rnode-501-through-508"
            )
        )
        scenario["steps"].extend(deepcopy(scenario["steps"][-1]) for _ in range(3))
        with self.assertRaisesRegex(ValueError, "5119-byte cumulative limit"):
            rnode_hil.validate_scenario_for_send(
                corpus,
                scenario,
                receiver_fragment_timeout_us=None,
                receiver_maximum_frame_airtime_us=None,
                peer_preamble_extension_us=0,
            )
        full_step = deepcopy(scenario["steps"][-1])
        forty = b"x" * 40
        boundary = {
            "name": "queue-boundary",
            "description": "exact smallest RNode queue boundary",
            "steps": [deepcopy(full_step) for _ in range(10)]
            + [
                {
                    "mode": "rnode_packet",
                    "payload_hex": forty.hex(),
                    "payload_len": len(forty),
                    "payload_sha256": hashlib.sha256(forty).hexdigest(),
                    "wait_after": {"kind": "fixed", "milliseconds": 0},
                }
            ],
        }
        with self.assertRaisesRegex(ValueError, "5119-byte cumulative limit"):
            rnode_hil.validate_scenario_for_send(
                corpus,
                boundary,
                receiver_fragment_timeout_us=None,
                receiver_maximum_frame_airtime_us=None,
                peer_preamble_extension_us=0,
            )
        thirty_nine = b"x" * 39
        boundary["steps"][-1].update(
            payload_hex=thirty_nine.hex(),
            payload_len=len(thirty_nine),
            payload_sha256=hashlib.sha256(thirty_nine).hexdigest(),
        )
        rnode_hil.validate_scenario_for_send(
            corpus,
            boundary,
            receiver_fragment_timeout_us=None,
            receiver_maximum_frame_airtime_us=None,
            peer_preamble_extension_us=0,
        )


if __name__ == "__main__":
    unittest.main()
