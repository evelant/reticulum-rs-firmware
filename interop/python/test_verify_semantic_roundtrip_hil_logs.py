from __future__ import annotations

from contextlib import redirect_stderr, redirect_stdout
from io import StringIO
import json
from pathlib import Path
import tempfile
import unittest

import verify_semantic_roundtrip_hil_logs as verifier


INITIATOR_ANNOUNCE_HASH = "11" * 32
RESPONDER_ANNOUNCE_HASH = "22" * 32
DATA_HASH = "33" * 32
PROOF_HASH = "44" * 32


def packet(
    direction: str,
    step: str,
    sequence: int,
    packet_hash: str,
    *,
    rns_len: int,
    destination: str = "none",
    receipt: str = "none",
) -> str:
    if direction == "tx":
        status = "DRIVER_TX_DONE"
        signal = "false"
        rssi = 0
        snr = 0
    else:
        status = "RNODE_PACKET"
        signal = "true"
        rssi = -67
        snr = 8
    return (
        "tx-hil stage=semantic-roundtrip-packet "
        f"direction={direction} status={status} step={step} "
        f"rns_len={rns_len} physical_len={rns_len + 1} sequence={sequence} "
        f"packet_hash={packet_hash} destination_hash={destination} "
        f"data_receipt={receipt} signal_present={signal} "
        f"rssi_dbm={rssi} snr_db={snr}"
    )


class EventBuilder:
    def __init__(self, role: str) -> None:
        self.role = role
        self.events: list[str] = []
        self.heap_index = 0

    def add(self, event: str) -> None:
        self.events.append(event)

    def heap(self, checkpoint: str) -> None:
        self.heap_index += 1
        size = 65_536
        used = 1_000 + self.heap_index * 64
        self.add(
            "tx-hil stage=semantic-roundtrip-heap "
            f"checkpoint={checkpoint} role={self.role} heap_size={size} "
            f"heap_used={used} heap_free={size - used} heap_max_used={used}"
        )

    def common(
        self,
        *,
        base_mac: str,
        local_destination: str,
        peer_destination: str,
        initial_phase: str,
    ) -> None:
        self.add(
            "tx-hil stage=mac-gate "
            f"base_mac={base_mac} role={self.role} exact_match=true "
            "radio_constructed=false spi_constructed=false "
            "rf_state=reset_low_fem_low"
        )
        self.add(verifier.EXPECTED_PROFILE)
        self.add(
            "tx-hil stage=runtime-source "
            f"esp_rtos_source={verifier.EXPECTED_RUNTIME_SOURCE}"
        )
        self.add(
            "tx-hil stage=radio-init status=PASS "
            f"role={self.role} fem_state=powered_settled_ctx_rx "
            "sx1262_preamble_symbol_timeout=248 "
            "receive_whole_operation_outer_deadline_ms=1500 "
            "transmit_whole_operation_outer_deadline_ms=1500 "
            "tx_budget_frames=2"
        )
        self.heap("before-node-construction")
        self.heap("after-node-construction")
        self.add(
            "tx-hil stage=semantic-roundtrip-start status=ARMED "
            f"role={self.role} phase={initial_phase} "
            f"local_destination={local_destination} "
            f"peer_destination={peer_destination} payload_len=36 "
            "tx_budget=2 maximum_rx_windows=48"
        )

    def state(self, completed: str, next_phase: str) -> None:
        self.add(
            "tx-hil stage=semantic-roundtrip-state status=ADVANCED "
            f"completed={completed} next_phase={next_phase}"
        )

    def rx_armed(self, step: str, tx_done: int) -> None:
        self.add(
            "tx-hil stage=semantic-roundtrip-rx status=ARMED "
            f"role={self.role} step={step} maximum_windows=48 "
            f"tx_done={tx_done}"
        )


def initiator_events() -> list[str]:
    builder = EventBuilder("initiator")
    builder.common(
        base_mac=verifier.INITIATOR_BASE_MAC,
        local_destination=verifier.INITIATOR_DESTINATION,
        peer_destination=verifier.RESPONDER_DESTINATION,
        initial_phase="InitiatorSendAnnounce",
    )
    builder.add(
        "tx-hil stage=semantic-roundtrip-delay role=initiator "
        "purpose=responder-startup delay_driver=blocking-esp-hal "
        "delay_ms=3000 tx_done=0"
    )
    builder.heap("before-initiator-announce-sign")
    builder.heap("after-initiator-announce-sign")
    builder.add(
        packet(
            "tx",
            "InitiatorAnnounce",
            9,
            INITIATOR_ANNOUNCE_HASH,
            rns_len=167,
            destination=verifier.INITIATOR_DESTINATION,
        )
    )
    builder.state(
        "Transmit(InitiatorAnnounce)", "InitiatorAwaitResponderAnnounce"
    )
    builder.rx_armed("ResponderAnnounce", 1)
    builder.add(
        packet(
            "rx",
            "ResponderAnnounce",
            10,
            RESPONDER_ANNOUNCE_HASH,
            rns_len=167,
            destination=verifier.RESPONDER_DESTINATION,
        )
    )
    builder.heap("before-announce-validation")
    builder.heap("after-announce-validation")
    builder.add(
        "tx-hil stage=semantic-roundtrip-announce-ingress "
        "status=SEMANTIC_VALIDATED role=initiator step=ResponderAnnounce "
        f"peer_destination={verifier.RESPONDER_DESTINATION} "
        "route_learned=true extra_actions=0"
    )
    builder.state("Receive(ResponderAnnounce)", "InitiatorSendData")
    builder.add(
        "tx-hil stage=semantic-roundtrip-delay role=initiator "
        "purpose=responder-rx-rearm delay_driver=blocking-esp-hal "
        "delay_ms=250 tx_done=1"
    )
    builder.heap("before-data-encrypt")
    builder.heap("after-data-encrypt")
    builder.add(
        packet(
            "tx",
            "EncryptedData",
            11,
            DATA_HASH,
            rns_len=147,
            receipt=DATA_HASH,
        )
    )
    builder.state("Transmit(EncryptedData)", "InitiatorAwaitProof")
    builder.rx_armed("DeliveryProof", 2)
    builder.add(
        packet(
            "rx",
            "DeliveryProof",
            12,
            PROOF_HASH,
            rns_len=115,
            receipt=DATA_HASH,
        )
    )
    builder.heap("before-proof-validation")
    builder.heap("after-proof-validation")
    builder.add(
        "tx-hil stage=semantic-roundtrip-proof-ingress "
        "status=SEMANTIC_VALIDATED role=initiator receipt_kind=Data "
        f"candidate={DATA_HASH} terminal=Delivered receipt_slots_used=0 "
        f"extra_actions=0 proof_packet_hash={PROOF_HASH}"
    )
    builder.state("Receive(DeliveryProof)", "Complete")
    builder.heap("terminal")
    builder.add(
        "tx-hil stage=semantic-roundtrip-terminal status=PASS "
        "role=initiator tx_done=2 "
        f"local_destination={verifier.INITIATOR_DESTINATION} "
        f"peer_destination={verifier.RESPONDER_DESTINATION} "
        f"data_receipt={DATA_HASH} radio_shutdown=next"
    )
    builder.add(
        "tx-hil stage=complete role=initiator radio_active=false "
        "action=permanent-rf-inert-hold"
    )
    return builder.events


def responder_events() -> list[str]:
    builder = EventBuilder("responder")
    builder.common(
        base_mac=verifier.RESPONDER_BASE_MAC,
        local_destination=verifier.RESPONDER_DESTINATION,
        peer_destination=verifier.INITIATOR_DESTINATION,
        initial_phase="ResponderAwaitInitiatorAnnounce",
    )
    builder.rx_armed("InitiatorAnnounce", 0)
    builder.add(
        packet(
            "rx",
            "InitiatorAnnounce",
            9,
            INITIATOR_ANNOUNCE_HASH,
            rns_len=167,
            destination=verifier.INITIATOR_DESTINATION,
        )
    )
    builder.heap("before-announce-validation")
    builder.heap("after-announce-validation")
    builder.add(
        "tx-hil stage=semantic-roundtrip-announce-ingress "
        "status=SEMANTIC_VALIDATED role=responder step=InitiatorAnnounce "
        f"peer_destination={verifier.INITIATOR_DESTINATION} "
        "route_learned=true extra_actions=0"
    )
    builder.state("Receive(InitiatorAnnounce)", "ResponderSendAnnounce")
    builder.heap("before-responder-announce-sign")
    builder.heap("after-responder-announce-sign")
    builder.add(
        packet(
            "tx",
            "ResponderAnnounce",
            10,
            RESPONDER_ANNOUNCE_HASH,
            rns_len=167,
            destination=verifier.RESPONDER_DESTINATION,
        )
    )
    builder.state("Transmit(ResponderAnnounce)", "ResponderAwaitData")
    builder.rx_armed("EncryptedData", 1)
    builder.add(
        packet(
            "rx",
            "EncryptedData",
            11,
            DATA_HASH,
            rns_len=147,
            receipt=DATA_HASH,
        )
    )
    builder.heap("before-data-decrypt-and-proof")
    builder.heap("after-data-decrypt-and-proof")
    builder.add(
        "tx-hil stage=semantic-roundtrip-data-ingress "
        "status=SEMANTIC_VALIDATED role=responder payload_len=36 "
        f"destination={verifier.RESPONDER_DESTINATION} "
        f"data_receipt={DATA_HASH} proof_actions=1 extra_actions=0"
    )
    builder.state("Receive(EncryptedData)", "ResponderSendProof")
    builder.add(
        packet(
            "tx",
            "DeliveryProof",
            12,
            PROOF_HASH,
            rns_len=115,
            receipt=DATA_HASH,
        )
    )
    builder.state("Transmit(DeliveryProof)", "Complete")
    builder.heap("terminal")
    builder.add(
        "tx-hil stage=semantic-roundtrip-terminal status=PASS "
        "role=responder tx_done=2 "
        f"local_destination={verifier.RESPONDER_DESTINATION} "
        f"peer_destination={verifier.INITIATOR_DESTINATION} "
        f"data_receipt={DATA_HASH} radio_shutdown=next"
    )
    builder.add(
        "tx-hil stage=complete role=responder radio_active=false "
        "action=permanent-rf-inert-hold"
    )
    return builder.events


def render(events: list[str], *, ansi: bool = False) -> bytes:
    lines = [
        verifier.EXPECTED_ROM_BOOT,
        "Build:Mar 27 2021",
        verifier.EXPECTED_COUNTED_RESET,
    ]
    prefix = "\x1b[0;32mINFO\x1b[0m - " if ansi else "INFO - "
    lines.extend(prefix + event for event in events)
    return ("\r\n".join(lines) + "\r\n").encode()


def event_index(events: list[str], fragment: str) -> int:
    return next(index for index, event in enumerate(events) if fragment in event)


class VerificationTests(unittest.TestCase):
    def test_accepts_exact_cross_bound_exchange_with_ansi(self) -> None:
        result = verifier.verify_segments(
            render(initiator_events(), ansi=True),
            render(responder_events(), ansi=True),
        )
        self.assertEqual(result.status, "PASS")
        self.assertEqual(result.packet_hashes["encrypted_data"], DATA_HASH)
        self.assertEqual(result.data_receipt, DATA_HASH)
        self.assertEqual(result.initiator.tx_done, 2)
        self.assertEqual(result.responder.tx_done, 2)
        self.assertGreater(result.initiator.heap.max_heap_used, 0)
        self.assertGreater(result.responder.heap.min_heap_free, 0)

    def test_independent_offsets_exclude_uncounted_runs(self) -> None:
        initiator_prefix = render(initiator_events())
        responder_prefix = b"uncounted responder noise\n"
        initiator = initiator_prefix + render(initiator_events())
        responder = responder_prefix + render(responder_events())
        result = verifier.verify_captures(
            initiator,
            responder,
            len(initiator_prefix),
            len(responder_prefix),
        )
        self.assertEqual(result.status, "PASS")

    def test_exact_post_completion_inert_heartbeats_are_allowed(self) -> None:
        initiator = initiator_events() + [verifier.EXPECTED_INERT_HEARTBEAT] * 2
        responder = responder_events() + [verifier.EXPECTED_INERT_HEARTBEAT]
        result = verifier.verify_segments(render(initiator), render(responder))
        self.assertEqual(result.status, "PASS")

        initiator[-1] += " corrupted=true"
        with self.assertRaisesRegex(verifier.VerificationError, "unexpected"):
            verifier.verify_segments(render(initiator), render(responder))

    def test_each_segment_requires_exactly_one_counted_boot(self) -> None:
        initiator = render(initiator_events())
        duplicate = (verifier.EXPECTED_ROM_BOOT + "\r\n").encode() + initiator
        with self.assertRaisesRegex(verifier.VerificationError, "exactly one counted"):
            verifier.verify_segments(duplicate, render(responder_events()))

        missing_reset = initiator.replace(
            (verifier.EXPECTED_COUNTED_RESET + "\r\n").encode(), b"", 1
        )
        with self.assertRaisesRegex(verifier.VerificationError, "exactly one counted"):
            verifier.verify_segments(missing_reset, render(responder_events()))

    def test_runtime_source_identity_is_exact(self) -> None:
        initiator = initiator_events()
        source = event_index(initiator, "stage=runtime-source")
        initiator[source] = initiator[source].replace(
            verifier.EXPECTED_RUNTIME_SOURCE, "unexpected-source"
        )
        with self.assertRaisesRegex(verifier.VerificationError, "runtime source identity"):
            verifier.verify_segments(render(initiator), render(responder_events()))

    def test_cross_board_packet_hash_mismatch_fails(self) -> None:
        responder = responder_events()
        index = event_index(responder, "direction=rx status=RNODE_PACKET step=InitiatorAnnounce")
        responder[index] = responder[index].replace(
            f"packet_hash={INITIATOR_ANNOUNCE_HASH}", f"packet_hash={'55' * 32}"
        )
        with self.assertRaisesRegex(verifier.VerificationError, "TX/RX.*packet_hash"):
            verifier.verify_segments(render(initiator_events()), render(responder))

    def test_packet_lengths_are_exact_for_this_fixture(self) -> None:
        initiator = initiator_events()
        data = event_index(
            initiator, "direction=tx status=DRIVER_TX_DONE step=EncryptedData"
        )
        initiator[data] = initiator[data].replace(
            "rns_len=147 physical_len=148", "rns_len=146 physical_len=147"
        )
        with self.assertRaisesRegex(verifier.VerificationError, "wrong fixture length"):
            verifier.verify_segments(render(initiator), render(responder_events()))

    def test_destination_mismatch_fails(self) -> None:
        initiator = initiator_events()
        start = event_index(initiator, "stage=semantic-roundtrip-start")
        initiator[start] = initiator[start].replace(
            verifier.RESPONDER_DESTINATION, "66" * 16
        )
        with self.assertRaisesRegex(verifier.VerificationError, "readiness"):
            verifier.verify_segments(render(initiator), render(responder_events()))

    def test_data_receipt_mismatch_fails(self) -> None:
        initiator = initiator_events()
        proof = event_index(
            initiator, "direction=rx status=RNODE_PACKET step=DeliveryProof"
        )
        initiator[proof] = initiator[proof].replace(
            f"data_receipt={DATA_HASH}", f"data_receipt={'77' * 32}"
        )
        with self.assertRaisesRegex(verifier.VerificationError, "wrong DATA receipt"):
            verifier.verify_segments(render(initiator), render(responder_events()))

    def test_four_packet_hashes_must_be_distinct(self) -> None:
        initiator = initiator_events()
        responder = responder_events()
        for events in (initiator, responder):
            for index, event in enumerate(events):
                if "step=ResponderAnnounce" in event:
                    events[index] = event.replace(
                        RESPONDER_ANNOUNCE_HASH, INITIATOR_ANNOUNCE_HASH
                    )
        with self.assertRaisesRegex(verifier.VerificationError, "distinct and nonzero"):
            verifier.verify_segments(render(initiator), render(responder))

    def test_missing_transmission_fails_closed(self) -> None:
        responder = responder_events()
        tx = event_index(
            responder, "direction=tx status=DRIVER_TX_DONE step=ResponderAnnounce"
        )
        responder.pop(tx)
        with self.assertRaisesRegex(verifier.VerificationError, "tx packet ResponderAnnounce"):
            verifier.verify_segments(render(initiator_events()), render(responder))

    def test_extra_transmission_fails_closed(self) -> None:
        initiator = initiator_events()
        tx = event_index(
            initiator, "direction=tx status=DRIVER_TX_DONE step=EncryptedData"
        )
        initiator.insert(tx + 1, initiator[tx])
        with self.assertRaises(verifier.VerificationError):
            verifier.verify_segments(render(initiator), render(responder_events()))

    def test_wrong_action_order_fails_closed(self) -> None:
        initiator = initiator_events()
        before = event_index(initiator, "checkpoint=before-data-encrypt")
        after = event_index(initiator, "checkpoint=after-data-encrypt")
        initiator[before], initiator[after] = initiator[after], initiator[before]
        with self.assertRaisesRegex(verifier.VerificationError, "wrong heap checkpoint"):
            verifier.verify_segments(render(initiator), render(responder_events()))

    def test_any_firmware_failure_is_never_ignored(self) -> None:
        initiator = initiator_events()
        initiator.append(
            "tx-hil stage=semantic-roundtrip-terminal status=FAIL reason=test"
        )
        with self.assertRaisesRegex(verifier.VerificationError, "reported failure"):
            verifier.verify_segments(render(initiator), render(responder_events()))

    def test_fatal_runtime_output_is_never_ignored(self) -> None:
        valid = render(initiator_events())
        for marker in (
            "panicked at src/main.rs",
            "Guru Meditation Error",
            "abort()",
            "Stack overflow",
        ):
            with self.subTest(marker=marker), self.assertRaisesRegex(
                verifier.VerificationError, "fatal runtime output"
            ):
                verifier.verify_segments(
                    valid + f"ERROR - {marker}\r\n".encode(),
                    render(responder_events()),
                )

    def test_offset_beyond_each_capture_fails_closed(self) -> None:
        with self.assertRaisesRegex(verifier.VerificationError, "initiator.*exceeds"):
            verifier.verify_captures(b"short", b"long enough", 6, 0)
        with self.assertRaisesRegex(verifier.VerificationError, "responder.*exceeds"):
            verifier.verify_captures(b"long enough", b"short", 0, 6)

    def test_heap_accounting_inconsistency_fails(self) -> None:
        responder = responder_events()
        heap = event_index(responder, "checkpoint=after-node-construction")
        fields = responder[heap].split()
        free_index = next(i for i, field in enumerate(fields) if field.startswith("heap_free="))
        fields[free_index] = "heap_free=1"
        responder[heap] = " ".join(fields)
        with self.assertRaisesRegex(verifier.VerificationError, "heap checkpoint.*inconsistent"):
            verifier.verify_segments(render(initiator_events()), render(responder))

    def test_terminal_destinations_must_be_reversed(self) -> None:
        responder = responder_events()
        terminal = event_index(responder, "stage=semantic-roundtrip-terminal")
        responder[terminal] = responder[terminal].replace(
            f"peer_destination={verifier.INITIATOR_DESTINATION}",
            f"peer_destination={verifier.RESPONDER_DESTINATION}",
        )
        with self.assertRaisesRegex(verifier.VerificationError, "semantic terminal"):
            verifier.verify_segments(render(initiator_events()), render(responder))


class CommandLineTests(unittest.TestCase):
    def test_success_report_binds_both_captures_and_segments(self) -> None:
        initiator_prefix = b"uncounted E9\n"
        responder_prefix = b"uncounted E0\n"
        initiator_payload = initiator_prefix + render(initiator_events())
        responder_payload = responder_prefix + render(responder_events())
        with tempfile.TemporaryDirectory() as directory:
            initiator_log = Path(directory) / "e9.log"
            responder_log = Path(directory) / "e0.log"
            initiator_log.write_bytes(initiator_payload)
            responder_log.write_bytes(responder_payload)
            stdout = StringIO()
            with redirect_stdout(stdout):
                status = verifier.main(
                    [
                        str(initiator_log),
                        str(responder_log),
                        "--e9-byte-offset",
                        hex(len(initiator_prefix)),
                        "--e0-byte-offset",
                        str(len(responder_prefix)),
                    ]
                )
        self.assertEqual(status, 0)
        report = json.loads(stdout.getvalue())
        self.assertEqual(report["schema"], 1)
        self.assertEqual(report["status"], "PASS")
        self.assertEqual(
            report["identities"]["initiator"]["destination_hash"],
            verifier.INITIATOR_DESTINATION,
        )
        self.assertEqual(report["packet_hashes"]["delivery_proof"], PROOF_HASH)
        self.assertEqual(report["data_receipt"], DATA_HASH)
        for role in ("initiator", "responder"):
            capture = report["captures"][role]
            self.assertEqual(len(capture["capture_sha256"]), 64)
            self.assertEqual(len(capture["segment_sha256"]), 64)
            self.assertGreater(report["heap"][role]["max_heap_used"], 0)
            self.assertGreater(report["heap"][role]["min_heap_free"], 0)

    def test_failure_returns_nonzero_without_pass_report(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            initiator_log = Path(directory) / "e9.log"
            responder_log = Path(directory) / "e0.log"
            initiator_log.write_bytes(b"not evidence")
            responder_log.write_bytes(render(responder_events()))
            stdout = StringIO()
            stderr = StringIO()
            with redirect_stdout(stdout), redirect_stderr(stderr):
                status = verifier.main([str(initiator_log), str(responder_log)])
        self.assertEqual(status, 1)
        self.assertEqual(stdout.getvalue(), "")
        self.assertIn("status=FAIL", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
