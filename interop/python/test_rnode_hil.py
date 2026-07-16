from __future__ import annotations

from contextlib import redirect_stdout
from io import BytesIO, StringIO
import hashlib
import importlib.util
import json
from pathlib import Path
from copy import deepcopy
import sys
import tempfile
from types import ModuleType, SimpleNamespace
import unittest
from unittest.mock import patch

import rnode_hil


CORPUS = Path(__file__).parents[1] / "vectors" / "rnode-hil-v1.json"
RNS_VECTORS = Path(__file__).parents[1] / "vectors" / "rns-1.3.8.json"
HAS_RNS = importlib.util.find_spec("RNS") is not None


def channel_stats_payload(
    *,
    airtime_short: int,
    airtime_long: int = 0,
    channel_load_short: int = 0,
    channel_load_long: int = 0,
    current_rssi_raw: int = 100,
    noise_floor_raw: int = 90,
    interference_raw: int = 0xFF,
) -> bytes:
    return b"".join(
        value.to_bytes(2, "big")
        for value in (
            airtime_short,
            airtime_long,
            channel_load_short,
            channel_load_long,
        )
    ) + bytes((current_rssi_raw, noise_floor_raw, interference_raw))


def listen_argv(
    output_dir: Path,
    *,
    sx1262_irq_diagnostics: bool = False,
    validate_rns_announce: bool = False,
    expected_payload_hex: str = "0102",
    expected_mode: str = "rnode_packet",
    expected_scenario: str | None = None,
) -> list[str]:
    argv = ["listen"]
    if expected_scenario is None:
        argv.extend(
            [
                "--expected-payload-hex",
                expected_payload_hex,
                "--expected-mode",
                expected_mode,
            ]
        )
    else:
        argv.extend(["--expected-scenario", expected_scenario])
    argv.extend(
        [
            "--port",
            "/dev/fake-rnode",
            "--output-dir",
            str(output_dir),
            "--frequency-hz",
            "915000000",
            "--bandwidth-hz",
            "125000",
            "--spreading-factor",
            "7",
            "--coding-rate-denominator",
            "5",
            "--tx-power-dbm",
            "14",
            "--expected-peer-preamble-symbols",
            "24",
            "--short-airtime-limit-basis-points",
            "500",
            "--long-airtime-limit-basis-points",
            "250",
            "--listen-duration-ms",
            "1",
            "--expected-firmware",
            "1.86",
            "--region-basis",
            "unit-test NA 915 MHz",
            "--antenna-or-load-attached",
            "--fresh-peer-reset-ack",
            rnode_hil.FRESH_PEER_RESET_ACK,
        ]
    )
    if sx1262_irq_diagnostics:
        argv.append("--sx1262-irq-diagnostics")
    if validate_rns_announce:
        argv.append("--validate-rns-announce")
    return argv


class FakeSerial:
    def __init__(
        self,
        *,
        airtime_echo_delta: int = 0,
        irq_diagnostic_payloads: list[bytes] | None = None,
    ) -> None:
        self.decoder = rnode_hil.KissDecoder()
        self.pending = bytearray()
        self.commands: list[tuple[int, bytes]] = []
        self.airtime_echo_delta = airtime_echo_delta
        self.irq_diagnostic_payloads = (
            None
            if irq_diagnostic_payloads is None
            else list(irq_diagnostic_payloads)
        )
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
        elif frame.command == rnode_hil.CMD_STAT_IRQ:
            if self.irq_diagnostic_payloads is None:
                response = b"\x01\x00\x00\x00\x00"
            elif self.irq_diagnostic_payloads:
                response = self.irq_diagnostic_payloads.pop(0)
            else:
                raise AssertionError("unexpected extra IRQ diagnostics query")
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

    def reset_input_buffer(self) -> None:
        self.pending.clear()

    def __enter__(self) -> FakeSerial:
        return self

    def __exit__(self, *_args: object) -> None:
        pass


class ListeningFakeSerial(FakeSerial):
    def __init__(
        self,
        payloads: list[bytes],
        *,
        irq_diagnostic_payloads: list[bytes] | None = None,
    ) -> None:
        super().__init__(irq_diagnostic_payloads=irq_diagnostic_payloads)
        self.payloads = payloads
        self.injected = False

    def write(self, wire: bytes) -> int:
        written = super().write(wire)
        command, payload = self.commands[-1]
        if (
            not self.injected
            and command == rnode_hil.CMD_RADIO_STATE
            and payload == bytes([rnode_hil.RADIO_STATE_ON])
        ):
            self.injected = True
            for received in self.payloads:
                self.pending.extend(
                    rnode_hil.kiss_frame(rnode_hil.CMD_STAT_RSSI, b"\x64")
                )
                self.pending.extend(
                    rnode_hil.kiss_frame(rnode_hil.CMD_STAT_SNR, b"\xf0")
                )
                self.pending.extend(
                    rnode_hil.kiss_frame(
                        rnode_hil.CMD_STAT_CHTM,
                        channel_stats_payload(
                            airtime_short=0,
                            channel_load_short=15,
                        ),
                    )
                )
                self.pending.extend(
                    rnode_hil.kiss_frame(rnode_hil.CMD_DATA, received)
                )
        return written


def serial_module_for(
    payloads: list[bytes],
    instances: list[ListeningFakeSerial],
    *,
    irq_diagnostic_payloads: list[bytes] | None = None,
) -> ModuleType:
    module = ModuleType("serial")
    module.__version__ = rnode_hil.EXPECTED_PYSERIAL
    module.EIGHTBITS = 8
    module.PARITY_NONE = "N"
    module.STOPBITS_ONE = 1

    def open_serial(*_args: object, **_kwargs: object) -> ListeningFakeSerial:
        instance = ListeningFakeSerial(
            payloads,
            irq_diagnostic_payloads=irq_diagnostic_payloads,
        )
        instances.append(instance)
        return instance

    module.Serial = open_serial
    return module


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

    def test_channel_stats_decode_exact_single_interface_payload(self) -> None:
        stats = rnode_hil.decode_channel_stats(
            channel_stats_payload(
                airtime_short=123,
                airtime_long=45,
                channel_load_short=678,
                channel_load_long=90,
                current_rssi_raw=100,
                noise_floor_raw=80,
                interference_raw=110,
            )
        )
        self.assertEqual(stats["airtime_short_basis_points"], 123)
        self.assertEqual(stats["airtime_long_basis_points"], 45)
        self.assertEqual(stats["channel_load_short_basis_points"], 678)
        self.assertEqual(stats["channel_load_long_basis_points"], 90)
        self.assertEqual(stats["current_rssi_dbm"], -57)
        self.assertEqual(stats["noise_floor_dbm"], -77)
        self.assertEqual(stats["interference_dbm"], -47)

        no_interference = rnode_hil.decode_channel_stats(
            channel_stats_payload(airtime_short=0)
        )
        self.assertIsNone(no_interference["interference_dbm"])

    def test_channel_stats_reject_malformed_or_wrapped_gauges(self) -> None:
        with self.assertRaisesRegex(ValueError, "exactly 11 bytes"):
            rnode_hil.decode_channel_stats(b"\x00" * 8)
        with self.assertRaisesRegex(ValueError, "outside the 0..10000 gauge domain"):
            rnode_hil.decode_channel_stats(
                channel_stats_payload(airtime_short=0xFFFF)
            )

    def test_packet_rssi_and_snr_decode_exact_wire_units(self) -> None:
        self.assertEqual(rnode_hil.decode_packet_rssi(b"\x64"), -57)
        self.assertEqual(rnode_hil.decode_packet_snr(b"\xf0"), -4.0)
        with self.assertRaisesRegex(ValueError, "exactly one byte"):
            rnode_hil.decode_packet_rssi(b"")
        with self.assertRaisesRegex(ValueError, "exactly one byte"):
            rnode_hil.decode_packet_snr(b"\x00\x01")

    def test_sx1262_irq_diagnostics_decode_is_strict_and_names_flags(self) -> None:
        decoded = rnode_hil.decode_sx1262_irq_diagnostics(
            b"\x01\x00\x04\x00\x72"
        )
        self.assertEqual(decoded["dcd_irq_mask"], 0x0004)
        self.assertEqual(decoded["dcd_irq_mask_hex"], "0x0004")
        self.assertEqual(decoded["dio1_irq_mask"], 0x0072)
        self.assertEqual(decoded["dio1_irq_mask_hex"], "0x0072")
        self.assertTrue(decoded["flags"]["dcd"]["PreambleDetected"])
        self.assertFalse(decoded["flags"]["dcd"]["RxDone"])
        self.assertTrue(decoded["flags"]["dio1"]["RxDone"])
        self.assertTrue(decoded["flags"]["dio1"]["HeaderValid"])
        self.assertTrue(decoded["flags"]["dio1"]["HeaderError"])
        self.assertTrue(decoded["flags"]["dio1"]["CrcError"])

        for malformed in (b"\x01\x00\x00\x00", b"\x01\x00\x00\x00\x00\x00"):
            with self.subTest(malformed=malformed.hex()):
                with self.assertRaisesRegex(ValueError, "exactly 5 bytes"):
                    rnode_hil.decode_sx1262_irq_diagnostics(malformed)
        with self.assertRaisesRegex(ValueError, "unsupported.*schema 2"):
            rnode_hil.decode_sx1262_irq_diagnostics(b"\x02\x00\x00\x00\x00")

    def test_sx1262_irq_query_rejects_malformed_peer_response_immediately(self) -> None:
        serial = FakeSerial(irq_diagnostic_payloads=[b"\x01\x00"])
        peer = rnode_hil.RNodePeer(serial, rnode_hil.Transcript(BytesIO()))
        with self.assertRaisesRegex(ValueError, "exactly 5 bytes"):
            peer.query_sx1262_irq_diagnostics()
        self.assertEqual(serial.commands, [(rnode_hil.CMD_STAT_IRQ, b"\x00")])

    def test_receive_evidence_requires_one_exact_and_no_extra_data(self) -> None:
        expected = b"\x01\x02"
        exact = {
            "payload_hex": expected.hex(),
            "payload_len": len(expected),
        }
        mismatch = {"payload_hex": "03", "payload_len": 1}

        evidence = rnode_hil.receive_evidence(expected, [exact])
        self.assertTrue(evidence["unambiguous_exact_payload_received"])
        self.assertEqual(evidence["result"], "exact_payload_received_once")
        self.assertFalse(evidence["rns_semantic_validity_checked"])

        missing = rnode_hil.receive_evidence(expected, [])
        self.assertFalse(missing["unambiguous_exact_payload_received"])
        self.assertEqual(missing["result"], "no_cmd_data_received")

        wrong = rnode_hil.receive_evidence(expected, [mismatch])
        self.assertEqual(wrong["result"], "expected_payload_not_received")
        duplicate = rnode_hil.receive_evidence(expected, [exact, exact])
        self.assertEqual(duplicate["result"], "ambiguous_extra_cmd_data")
        mixed = rnode_hil.receive_evidence(expected, [exact, mismatch])
        self.assertEqual(mixed["result"], "ambiguous_extra_cmd_data")

    def test_driver_tx_evidence_retains_increase_across_later_decay(self) -> None:
        baseline = rnode_hil.decode_channel_stats(
            channel_stats_payload(airtime_short=0)
        )
        increased = rnode_hil.decode_channel_stats(
            channel_stats_payload(airtime_short=27, airtime_long=1)
        )
        decayed = rnode_hil.decode_channel_stats(
            channel_stats_payload(airtime_short=0, airtime_long=1)
        )
        evidence = rnode_hil.driver_tx_evidence(
            baseline,
            [increased, decayed],
        )
        self.assertTrue(evidence["peer_driver_tx_observed"])
        self.assertEqual(
            evidence["maximum_post_enqueue_airtime_short_basis_points"], 27
        )
        self.assertEqual(evidence["airtime_short_increase_basis_points"], 27)
        self.assertEqual(evidence["post_enqueue_observation_count"], 2)
        self.assertFalse(evidence["u16_wrap_arithmetic_used"])
        self.assertFalse(evidence["rf_verified"])

    def test_nonzero_baseline_refuses_decay_ambiguous_inference(self) -> None:
        baseline = rnode_hil.decode_channel_stats(
            channel_stats_payload(airtime_short=12)
        )
        observation = rnode_hil.decode_channel_stats(
            channel_stats_payload(airtime_short=13)
        )
        with self.assertRaisesRegex(ValueError, "baseline is not zero"):
            rnode_hil.driver_tx_evidence(baseline, [observation])

    def test_peer_records_every_channel_stat_for_bounded_evidence(self) -> None:
        wire = b"".join(
            rnode_hil.kiss_frame(rnode_hil.CMD_STAT_CHTM, payload)
            for payload in (
                channel_stats_payload(airtime_short=25),
                channel_stats_payload(airtime_short=0),
            )
        )
        peer = rnode_hil.RNodePeer(
            BatchReadSerial(wire),
            rnode_hil.Transcript(BytesIO()),
        )
        baseline = rnode_hil.decode_channel_stats(
            channel_stats_payload(airtime_short=0)
        )
        evidence = peer.wait_for_driver_tx_evidence(
            baseline,
            observation_index=0,
            timeout_seconds=0.1,
        )
        self.assertTrue(evidence["peer_driver_tx_observed"])
        self.assertEqual(len(evidence["post_enqueue_observations"]), 2)
        self.assertEqual(
            evidence["maximum_post_enqueue_airtime_short_basis_points"], 25
        )

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


@unittest.skipUnless(
    HAS_RNS,
    "pinned Python-RNS environment is required for semantic integration tests",
)
class RnsSemanticTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        vectors = json.loads(RNS_VECTORS.read_text(encoding="utf-8"))
        cls.vector = vectors
        cls.raw = bytes.fromhex(vectors["announce"]["raw_hex"])
        cls.rns, cls.peer = rnode_hil.load_pinned_rns_validation_peer()

    def test_committed_announce_is_cryptographically_and_semantically_valid(self) -> None:
        evidence = rnode_hil.validate_rns_announce(
            self.raw,
            self.rns,
            self.peer,
        )
        self.assertTrue(evidence["evaluated"])
        self.assertTrue(evidence["valid"])
        self.assertEqual(
            evidence["result"],
            "valid_expected_first_hop_announce",
        )
        self.assertIsNone(evidence["failure"])
        self.assertEqual(evidence["packet"]["flags_hex"], "0x01")
        self.assertEqual(
            evidence["packet"]["packet_hash_hex"],
            self.vector["announce"]["packet_hash_hex"],
        )
        self.assertEqual(
            evidence["packet"]["destination_hash_hex"],
            self.vector["announce"]["destination_hash_hex"],
        )
        self.assertEqual(
            evidence["announce"]["identity_hash_hex"],
            self.vector["identity"]["identity_hash_hex"],
        )
        self.assertEqual(
            evidence["announce"]["public_key_hex"],
            self.vector["identity"]["public_key_hex"],
        )
        self.assertEqual(
            evidence["announce"]["name_hash_hex"],
            self.vector["announce"]["name_hash_hex"],
        )
        self.assertIsNone(evidence["announce"]["app_data_hex"])
        self.assertEqual(evidence["peer"]["version"], "1.3.8")
        self.assertEqual(
            evidence["peer"]["revision"],
            "dca2a9282935d3dd251f2a1588daa41b5deee8c7",
        )
        self.assertFalse(evidence["peer"]["full_reticulum_instance_started"])

    def test_signature_and_destination_tampering_are_rejected(self) -> None:
        signature_tampered = bytearray(self.raw)
        signature_tampered[-1] ^= 1
        destination_tampered = bytearray(self.raw)
        destination_tampered[2] ^= 1

        for label, tampered in (
            ("signature", signature_tampered),
            ("destination", destination_tampered),
        ):
            with self.subTest(label=label):
                evidence = rnode_hil.validate_rns_announce(
                    bytes(tampered),
                    self.rns,
                    self.peer,
                )
                self.assertFalse(evidence["valid"])
                self.assertEqual(
                    evidence["failure"]["reason"],
                    "announce_cryptographic_validation_failed",
                )

    def test_first_hop_policy_rejects_other_valid_wire_shapes(self) -> None:
        mutations: tuple[tuple[str, int, int], ...] = (
            ("unexpected_ifac_flag", 0, self.raw[0] | 0x80),
            ("unexpected_header_type", 0, self.raw[0] | 0x40),
            ("unexpected_transport_type", 0, self.raw[0] | 0x10),
            ("unexpected_destination_type", 0, self.raw[0] | 0x04),
            ("unexpected_packet_type", 0, self.raw[0] & 0xFC),
            ("unexpected_context_flag", 0, self.raw[0] | 0x20),
            ("unexpected_hops", 1, 1),
            ("unexpected_context", 18, 1),
        )
        for expected_reason, index, value in mutations:
            with self.subTest(expected_reason=expected_reason):
                changed = bytearray(self.raw)
                changed[index] = value
                evidence = rnode_hil.validate_rns_announce(
                    bytes(changed),
                    self.rns,
                    self.peer,
                )
                self.assertFalse(evidence["valid"])
                self.assertEqual(
                    evidence["failure"]["reason"],
                    expected_reason,
                )

    def test_base_packet_bounds_are_enforced_before_python_unpack(self) -> None:
        too_short = rnode_hil.validate_rns_announce(
            self.raw[: rnode_hil.RNS_MINIMUM_PACKET_LEN - 1],
            self.rns,
            self.peer,
        )
        self.assertEqual(too_short["failure"]["reason"], "packet_too_short")

        oversized = self.raw + bytes(int(self.rns.Reticulum.MTU) + 1 - len(self.raw))
        too_long = rnode_hil.validate_rns_announce(
            oversized,
            self.rns,
            self.peer,
        )
        self.assertEqual(too_long["failure"]["reason"], "packet_too_long")


class ListenTests(unittest.TestCase):
    def test_listen_expectation_resolves_cli_or_one_step_scenario(self) -> None:
        corpus = rnode_hil.load_corpus(CORPUS)
        cli = SimpleNamespace(
            expected_scenario=None,
            expected_payload_hex="0102",
            expected_mode="rnode_packet",
        )
        raw_mode, payload, source = rnode_hil.resolve_listen_expectation(
            cli,
            corpus,
        )
        self.assertFalse(raw_mode)
        self.assertEqual(payload, b"\x01\x02")
        self.assertEqual(source["kind"], "cli_hex")
        self.assertFalse(source["rns_semantic_validity_checked_during_listen"])

        scenario = SimpleNamespace(
            expected_scenario="raw-single-1",
            expected_payload_hex=None,
            expected_mode=None,
        )
        raw_mode, payload, source = rnode_hil.resolve_listen_expectation(
            scenario,
            corpus,
        )
        self.assertTrue(raw_mode)
        self.assertEqual(payload, bytes.fromhex("b042"))
        self.assertEqual(source["scenario"], "raw-single-1")

        multiple = SimpleNamespace(
            expected_scenario="released-python-announce-duplicate",
            expected_payload_hex=None,
            expected_mode=None,
        )
        with self.assertRaisesRegex(ValueError, "exactly one step"):
            rnode_hil.resolve_listen_expectation(multiple, corpus)

    def test_listen_expectation_rejects_ambiguous_cli_hex(self) -> None:
        corpus = rnode_hil.load_corpus(CORPUS)
        missing_mode = SimpleNamespace(
            expected_scenario=None,
            expected_payload_hex="0102",
            expected_mode=None,
        )
        with self.assertRaisesRegex(ValueError, "--expected-mode is required"):
            rnode_hil.resolve_listen_expectation(missing_mode, corpus)

        uppercase = SimpleNamespace(
            expected_scenario=None,
            expected_payload_hex="A0",
            expected_mode="raw_lora_frame",
        )
        with self.assertRaisesRegex(ValueError, "canonical lowercase hex"):
            rnode_hil.resolve_listen_expectation(uppercase, corpus)

    def test_listen_parser_and_profile_require_explicit_inputs(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "listen"
            parser = rnode_hil.build_parser()
            args = parser.parse_args(listen_argv(output))
            self.assertEqual(args.command, "listen")
            self.assertEqual(args.expected_mode, "rnode_packet")
            self.assertEqual(args.listen_duration_ms, 1)
            self.assertFalse(args.sx1262_irq_diagnostics)
            self.assertFalse(args.validate_rns_announce)
            corpus = rnode_hil.load_corpus(CORPUS)
            rnode_hil.validate_listen_args(args, corpus)
            args.listen_duration_ms = 0
            with self.assertRaisesRegex(ValueError, "1 through 300000"):
                rnode_hil.validate_listen_args(args, corpus)

    def test_semantic_validation_is_explicit_and_forbids_raw_mode(self) -> None:
        corpus = rnode_hil.load_corpus(CORPUS)
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "listen"
            ordinary = rnode_hil.build_parser().parse_args(
                listen_argv(output, validate_rns_announce=True)
            )
            self.assertTrue(ordinary.validate_rns_announce)
            rnode_hil.validate_listen_args(ordinary, corpus)

            raw = rnode_hil.build_parser().parse_args(
                listen_argv(
                    output,
                    validate_rns_announce=True,
                    expected_payload_hex="a0",
                    expected_mode="raw_lora_frame",
                )
            )
            with self.assertRaisesRegex(
                ValueError,
                "requires ordinary rnode_packet mode",
            ):
                rnode_hil.validate_listen_args(raw, corpus)

    def test_listen_run_records_exact_delivery_and_radio_telemetry(self) -> None:
        corpus, corpus_sha256 = rnode_hil.load_corpus_snapshot(CORPUS)
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "listen"
            args = rnode_hil.build_parser().parse_args(listen_argv(output))
            instances: list[ListeningFakeSerial] = []
            serial_module = serial_module_for([b"\x01\x02"], instances)
            with patch.dict(sys.modules, {"serial": serial_module}):
                with redirect_stdout(StringIO()):
                    self.assertEqual(
                        rnode_hil.run_listen(args, corpus, corpus_sha256),
                        0,
                    )

            manifest = json.loads((output / "peer-manifest.json").read_text())
            self.assertEqual(
                manifest["status"],
                "expected_payload_received_via_rnode_serial_"
                "rns_semantics_not_verified",
            )
            evidence = manifest["serial_delivery_evidence"]
            self.assertTrue(evidence["unambiguous_exact_payload_received"])
            self.assertEqual(evidence["cmd_data_observation_count"], 1)
            self.assertFalse(manifest["rns_semantic_validity_checked"])
            self.assertEqual(
                manifest["radio_telemetry"]["packet_rssi"][0]["rssi_dbm"],
                -57,
            )
            self.assertEqual(
                manifest["radio_telemetry"]["packet_snr"][0]["snr_db"],
                -4.0,
            )
            self.assertEqual(
                manifest["radio_telemetry"]["channel_stats"][0][
                    "channel_load_short_basis_points"
                ],
                15,
            )
            self.assertIn("transcript_sha256", manifest)
            self.assertFalse(manifest["sx1262_irq_diagnostics"]["requested"])
            self.assertIsNone(
                manifest["sx1262_irq_diagnostics"]["baseline_after_configure"]
            )
            commands = instances[0].commands
            self.assertLess(
                commands.index((rnode_hil.CMD_PROMISC, b"\x00")),
                commands.index(
                    (rnode_hil.CMD_RADIO_STATE, bytes([rnode_hil.RADIO_STATE_ON]))
                ),
            )

    @unittest.skipUnless(
        HAS_RNS,
        "pinned Python-RNS environment is required for semantic integration tests",
    )
    def test_listen_run_records_committed_announce_semantic_success(self) -> None:
        corpus, corpus_sha256 = rnode_hil.load_corpus_snapshot(CORPUS)
        vector = json.loads(RNS_VECTORS.read_text(encoding="utf-8"))
        announce = bytes.fromhex(vector["announce"]["raw_hex"])
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "listen"
            args = rnode_hil.build_parser().parse_args(
                listen_argv(
                    output,
                    validate_rns_announce=True,
                    expected_scenario="released-python-announce",
                )
            )
            serial_module = serial_module_for([announce], [])
            with patch.dict(sys.modules, {"serial": serial_module}):
                with redirect_stdout(StringIO()):
                    self.assertEqual(
                        rnode_hil.run_listen(args, corpus, corpus_sha256),
                        0,
                    )

            manifest = json.loads((output / "peer-manifest.json").read_text())
            self.assertEqual(
                manifest["status"],
                "expected_rns_announce_received_and_semantically_validated",
            )
            self.assertTrue(manifest["rns_semantic_validity_checked"])
            semantic = manifest["rns_semantic_evidence"]
            self.assertTrue(semantic["valid"])
            self.assertEqual(
                semantic["packet"]["packet_hash_hex"],
                vector["announce"]["packet_hash_hex"],
            )
            self.assertEqual(
                semantic["announce"]["identity_hash_hex"],
                vector["identity"]["identity_hash_hex"],
            )
            delivery = manifest["serial_delivery_evidence"]
            self.assertTrue(delivery["unambiguous_exact_payload_received"])
            self.assertTrue(delivery["rns_semantic_validity_checked"])
            self.assertTrue(delivery["rns_semantically_valid"])
            self.assertTrue(
                manifest["expected"][
                    "rns_semantic_validity_checked_during_listen"
                ]
            )
            self.assertIn("first-hop announce validation", manifest["evidence_scope"])

    @unittest.skipUnless(
        HAS_RNS,
        "pinned Python-RNS environment is required for semantic integration tests",
    )
    def test_semantic_failure_preserves_exact_delivery_evidence(self) -> None:
        corpus, corpus_sha256 = rnode_hil.load_corpus_snapshot(CORPUS)
        vector = json.loads(RNS_VECTORS.read_text(encoding="utf-8"))
        tampered = bytearray.fromhex(vector["announce"]["raw_hex"])
        tampered[-1] ^= 1
        payload = bytes(tampered)
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "listen"
            args = rnode_hil.build_parser().parse_args(
                listen_argv(
                    output,
                    validate_rns_announce=True,
                    expected_payload_hex=payload.hex(),
                )
            )
            serial_module = serial_module_for([payload], [])
            with patch.dict(sys.modules, {"serial": serial_module}):
                with redirect_stdout(StringIO()):
                    with self.assertRaisesRegex(
                        RuntimeError,
                        "announce_cryptographic_validation_failed",
                    ):
                        rnode_hil.run_listen(args, corpus, corpus_sha256)

            manifest = json.loads((output / "peer-manifest.json").read_text())
            self.assertEqual(manifest["status"], "failed_after_radio_activation")
            self.assertTrue(manifest["rns_semantic_validity_checked"])
            delivery = manifest["serial_delivery_evidence"]
            self.assertTrue(delivery["unambiguous_exact_payload_received"])
            self.assertTrue(delivery["rns_semantic_validity_checked"])
            self.assertFalse(delivery["rns_semantically_valid"])
            semantic = manifest["rns_semantic_evidence"]
            self.assertTrue(semantic["evaluated"])
            self.assertFalse(semantic["valid"])
            self.assertEqual(
                semantic["failure"]["reason"],
                "announce_cryptographic_validation_failed",
            )
            self.assertIn("transcript_sha256", manifest)

    def test_missing_pinned_rns_fails_before_opening_the_radio(self) -> None:
        corpus, corpus_sha256 = rnode_hil.load_corpus_snapshot(CORPUS)
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "listen"
            args = rnode_hil.build_parser().parse_args(
                listen_argv(output, validate_rns_announce=True)
            )
            with patch.object(
                rnode_hil,
                "load_pinned_rns_validation_peer",
                side_effect=RuntimeError("missing pinned RNS"),
            ):
                with self.assertRaisesRegex(RuntimeError, "missing pinned RNS"):
                    rnode_hil.run_listen(args, corpus, corpus_sha256)

            manifest = json.loads((output / "peer-manifest.json").read_text())
            self.assertEqual(manifest["status"], "failed_before_radio_activation")
            self.assertFalse(manifest["radio_activated"])
            self.assertFalse(manifest["rns_semantic_validity_checked"])
            self.assertTrue(manifest["rns_semantic_evidence"]["requested"])
            self.assertFalse(manifest["rns_semantic_evidence"]["evaluated"])
            self.assertFalse((output / "peer-transcript.jsonl").exists())

    def test_listen_run_records_irq_diagnostic_baseline_and_final(self) -> None:
        corpus, corpus_sha256 = rnode_hil.load_corpus_snapshot(CORPUS)
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "listen"
            args = rnode_hil.build_parser().parse_args(
                listen_argv(output, sx1262_irq_diagnostics=True)
            )
            instances: list[ListeningFakeSerial] = []
            serial_module = serial_module_for(
                [b"\x01\x02"],
                instances,
                irq_diagnostic_payloads=[
                    b"\x01\x00\x00\x00\x00",
                    b"\x01\x00\x04\x00\x12",
                ],
            )
            with patch.dict(sys.modules, {"serial": serial_module}):
                with redirect_stdout(StringIO()):
                    self.assertEqual(
                        rnode_hil.run_listen(args, corpus, corpus_sha256),
                        0,
                    )

            manifest = json.loads((output / "peer-manifest.json").read_text())
            diagnostics = manifest["sx1262_irq_diagnostics"]
            self.assertTrue(diagnostics["requested"])
            self.assertIn("GFSK-only", diagnostics["sync_word_valid_semantics"])
            self.assertEqual(
                diagnostics["baseline_after_configure"]["payload_hex"],
                "0100000000",
            )
            final = diagnostics["final_after_listen_window"]
            self.assertEqual(final["dcd_irq_mask"], 0x0004)
            self.assertTrue(final["flags"]["dcd"]["PreambleDetected"])
            self.assertEqual(final["dio1_irq_mask"], 0x0012)
            self.assertTrue(final["flags"]["dio1"]["RxDone"])
            self.assertTrue(final["flags"]["dio1"]["HeaderValid"])
            self.assertEqual(
                [
                    command
                    for command, payload in instances[0].commands
                    if command == rnode_hil.CMD_STAT_IRQ and payload == b"\x00"
                ],
                [rnode_hil.CMD_STAT_IRQ, rnode_hil.CMD_STAT_IRQ],
            )

    def test_listen_run_fails_on_ambiguous_extra_data(self) -> None:
        corpus, corpus_sha256 = rnode_hil.load_corpus_snapshot(CORPUS)
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "listen"
            args = rnode_hil.build_parser().parse_args(listen_argv(output))
            serial_module = serial_module_for(
                [b"\x01\x02", b"\x03"],
                [],
            )
            with patch.dict(sys.modules, {"serial": serial_module}):
                with redirect_stdout(StringIO()):
                    with self.assertRaisesRegex(RuntimeError, "ambiguous_extra_cmd_data"):
                        rnode_hil.run_listen(args, corpus, corpus_sha256)

            manifest = json.loads((output / "peer-manifest.json").read_text())
            self.assertEqual(manifest["status"], "failed_after_radio_activation")
            evidence = manifest["serial_delivery_evidence"]
            self.assertFalse(evidence["unambiguous_exact_payload_received"])
            self.assertEqual(evidence["cmd_data_observation_count"], 2)
            self.assertEqual(evidence["result"], "ambiguous_extra_cmd_data")

    def test_listen_failure_preserves_both_irq_diagnostic_snapshots(self) -> None:
        corpus, corpus_sha256 = rnode_hil.load_corpus_snapshot(CORPUS)
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "listen"
            args = rnode_hil.build_parser().parse_args(
                listen_argv(output, sx1262_irq_diagnostics=True)
            )
            serial_module = serial_module_for(
                [],
                [],
                irq_diagnostic_payloads=[
                    b"\x01\x00\x00\x00\x00",
                    b"\x01\x00\x04\x00\x20",
                ],
            )
            with patch.dict(sys.modules, {"serial": serial_module}):
                with redirect_stdout(StringIO()):
                    with self.assertRaisesRegex(RuntimeError, "no_cmd_data_received"):
                        rnode_hil.run_listen(args, corpus, corpus_sha256)

            manifest = json.loads((output / "peer-manifest.json").read_text())
            self.assertEqual(manifest["status"], "failed_after_radio_activation")
            self.assertEqual(
                manifest["serial_delivery_evidence"]["result"],
                "no_cmd_data_received",
            )
            diagnostics = manifest["sx1262_irq_diagnostics"]
            self.assertEqual(
                diagnostics["baseline_after_configure"]["dcd_irq_mask"],
                0,
            )
            final = diagnostics["final_after_listen_window"]
            self.assertEqual(final["dcd_irq_mask"], 0x0004)
            self.assertTrue(final["flags"]["dcd"]["PreambleDetected"])
            self.assertEqual(final["dio1_irq_mask"], 0x0020)
            self.assertTrue(final["flags"]["dio1"]["HeaderError"])


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
            ordinary,
            rnode_hil.RNODE_PEER_ISOLATION_ARTIFACT_MODE,
        )
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
        for feature_bound in (pressure, returned_fault, returned_fault_repeat):
            with self.assertRaisesRegex(
                ValueError,
                "requires --target-artifact-mode",
            ):
                rnode_hil.validate_target_artifact_mode(
                    feature_bound,
                    rnode_hil.RNODE_PEER_ISOLATION_ARTIFACT_MODE,
                )
        mislabeled_ordinary = deepcopy(ordinary)
        mislabeled_ordinary["target_expectations"] = {}
        with self.assertRaisesRegex(ValueError, "must not declare target-only"):
            rnode_hil.validate_target_artifact_mode(
                mislabeled_ordinary,
                rnode_hil.RNODE_PEER_ISOLATION_ARTIFACT_MODE,
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
