from __future__ import annotations

from contextlib import redirect_stderr, redirect_stdout
from io import StringIO
import json
from pathlib import Path
import tempfile
import unittest

import verify_e290_semantic_hil_logs as verifier


INITIATOR_ANNOUNCE_HASH = "11" * 32
RESPONDER_ANNOUNCE_HASH = "22" * 32
DATA_HASH = "33" * 32
PROOF_HASH = "44" * 32


def packet(
    direction: str,
    step: str,
    packet_hash: str,
    *,
    destination: str = "none",
    receipt: str = "none",
    rssi: int = -67,
    snr: int = 8,
) -> str:
    if direction == "tx":
        status = "DRIVER_TX_DONE"
        signal = "false"
        rssi = 0
        snr = 0
    else:
        status = "RNODE_PACKET"
        signal = "true"
    rns_len, physical_len = verifier.EXPECTED_PACKET_LENGTHS[step]
    return (
        "e290-semantic-hil stage=packet "
        f"direction={direction} status={status} step={step} "
        f"rns_len={rns_len} physical_len={physical_len} "
        f"sequence={verifier.EXPECTED_SEQUENCE[step]} packet_hash={packet_hash} "
        f"destination_hash={destination} data_receipt={receipt} "
        f"signal_present={signal} rssi_dbm={rssi} snr_db={snr}"
    )


def cad(step: str, observed_at_us: int) -> str:
    return (
        "e290-semantic-hil stage=cad status=CLEAR "
        f"step={step} activity_detected=false observed_at_us={observed_at_us}"
    )


def common(
    *,
    role: str,
    base_mac: str,
    phase: str,
    local_destination: str,
    peer_destination: str,
) -> list[str]:
    return [
        "e290-semantic-hil stage=mac-gate "
        f"base_mac={base_mac} role={role} exact_match=true "
        "radio_constructed=false spi_constructed=false "
        "rf_state=reset_low_nss_high",
        verifier.EXPECTED_PROFILE,
        "e290-semantic-hil stage=runtime-source "
        f"esp_rtos_source={verifier.EXPECTED_RUNTIME_SOURCE}",
        "e290-semantic-hil stage=radio-init status=PASS "
        f"role={role} regulator=dcdc rf_switch=dio2 tcxo=dio3_1v8 "
        "tx_budget_packets=2",
        "e290-semantic-hil stage=exchange status=ARMED "
        f"role={role} phase={phase} local_destination={local_destination} "
        f"peer_destination={peer_destination} payload_len=36 tx_budget=2 "
        "maximum_rx_windows=48",
    ]


def state(completed: str, next_phase: str) -> str:
    return (
        "e290-semantic-hil stage=state status=ADVANCED "
        f"completed={completed} next_phase={next_phase}"
    )


def rx_armed(role: str, step: str) -> str:
    return (
        "e290-semantic-hil stage=rx status=ARMED "
        f"role={role} step={step} maximum_windows=48"
    )


def terminal(
    role: str, local_destination: str, peer_destination: str, receipt: str
) -> list[str]:
    return [
        "e290-semantic-hil stage=terminal status=PASS "
        f"role={role} tx_done=2 local_destination={local_destination} "
        f"peer_destination={peer_destination} data_receipt={receipt} "
        "radio_shutdown=next",
        "e290-semantic-hil stage=complete "
        f"role={role} radio_active=false action=permanent-rf-inert",
    ]


def initiator_events() -> list[str]:
    events = common(
        role="initiator",
        base_mac=verifier.INITIATOR_BASE_MAC,
        phase="InitiatorSendAnnounce",
        local_destination=verifier.INITIATOR_DESTINATION,
        peer_destination=verifier.RESPONDER_DESTINATION,
    )
    events.extend(
        [
            cad("InitiatorAnnounce", 1_000),
            packet(
                "tx",
                "InitiatorAnnounce",
                INITIATOR_ANNOUNCE_HASH,
                destination=verifier.INITIATOR_DESTINATION,
            ),
            state(
                "Transmit(InitiatorAnnounce)",
                "InitiatorAwaitResponderAnnounce",
            ),
            rx_armed("initiator", "ResponderAnnounce"),
            packet(
                "rx",
                "ResponderAnnounce",
                RESPONDER_ANNOUNCE_HASH,
                destination=verifier.RESPONDER_DESTINATION,
            ),
            "e290-semantic-hil stage=announce-ingress "
            "status=SEMANTIC_VALIDATED "
            f"peer_destination={verifier.RESPONDER_DESTINATION} "
            "route_learned=true",
            state("Receive(ResponderAnnounce)", "InitiatorSendData"),
            cad("EncryptedData", 2_000),
            packet(
                "tx",
                "EncryptedData",
                DATA_HASH,
                receipt=DATA_HASH,
            ),
            state("Transmit(EncryptedData)", "InitiatorAwaitProof"),
            rx_armed("initiator", "DeliveryProof"),
            packet(
                "rx",
                "DeliveryProof",
                PROOF_HASH,
                receipt=DATA_HASH,
            ),
            "e290-semantic-hil stage=proof-ingress "
            "status=SEMANTIC_VALIDATED "
            f"receipt={DATA_HASH} terminal=Delivered receipt_slots_used=0 "
            f"proof_packet_hash={PROOF_HASH}",
            state("Receive(DeliveryProof)", "Complete"),
        ]
    )
    events.extend(
        terminal(
            "initiator",
            verifier.INITIATOR_DESTINATION,
            verifier.RESPONDER_DESTINATION,
            DATA_HASH,
        )
    )
    return events


def responder_events() -> list[str]:
    events = common(
        role="responder",
        base_mac=verifier.RESPONDER_BASE_MAC,
        phase="ResponderAwaitInitiatorAnnounce",
        local_destination=verifier.RESPONDER_DESTINATION,
        peer_destination=verifier.INITIATOR_DESTINATION,
    )
    events.extend(
        [
            rx_armed("responder", "InitiatorAnnounce"),
            packet(
                "rx",
                "InitiatorAnnounce",
                INITIATOR_ANNOUNCE_HASH,
                destination=verifier.INITIATOR_DESTINATION,
            ),
            "e290-semantic-hil stage=announce-ingress "
            "status=SEMANTIC_VALIDATED "
            f"peer_destination={verifier.INITIATOR_DESTINATION} "
            "route_learned=true",
            state("Receive(InitiatorAnnounce)", "ResponderSendAnnounce"),
            cad("ResponderAnnounce", 1_500),
            packet(
                "tx",
                "ResponderAnnounce",
                RESPONDER_ANNOUNCE_HASH,
                destination=verifier.RESPONDER_DESTINATION,
            ),
            state("Transmit(ResponderAnnounce)", "ResponderAwaitData"),
            rx_armed("responder", "EncryptedData"),
            packet(
                "rx",
                "EncryptedData",
                DATA_HASH,
                receipt=DATA_HASH,
            ),
            "e290-semantic-hil stage=data-ingress status=SEMANTIC_VALIDATED "
            "role=responder payload_len=36 "
            f"destination={verifier.RESPONDER_DESTINATION} "
            f"data_receipt={DATA_HASH} proof_actions=1 extra_actions=0",
            state("Receive(EncryptedData)", "ResponderSendProof"),
            cad("DeliveryProof", 2_500),
            packet(
                "tx",
                "DeliveryProof",
                PROOF_HASH,
                receipt=DATA_HASH,
            ),
            state("Transmit(DeliveryProof)", "Complete"),
        ]
    )
    events.extend(
        terminal(
            "responder",
            verifier.RESPONDER_DESTINATION,
            verifier.INITIATOR_DESTINATION,
            DATA_HASH,
        )
    )
    return events


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
    def test_accepts_exact_cross_bound_e290_exchange_with_ansi(self) -> None:
        result = verifier.verify_segments(
            render(initiator_events(), ansi=True),
            render(responder_events(), ansi=True),
        )
        self.assertEqual(result.schema, verifier.SCHEMA)
        self.assertEqual(result.status, "PASS")
        self.assertEqual(result.data_receipt, DATA_HASH)
        self.assertEqual(result.packet_hashes["delivery_proof"], PROOF_HASH)
        self.assertEqual(result.initiator.cad_timestamps_us, (1_000, 2_000))
        self.assertEqual(result.responder.cad_timestamps_us, (1_500, 2_500))

    def test_independent_offsets_exclude_uncounted_runs(self) -> None:
        initiator_prefix = render(initiator_events())
        responder_prefix = b"uncounted responder noise\n"
        result = verifier.verify_captures(
            initiator_prefix + render(initiator_events()),
            responder_prefix + render(responder_events()),
            len(initiator_prefix),
            len(responder_prefix),
        )
        self.assertEqual(result.status, "PASS")

    def test_exact_inert_heartbeats_after_completion_are_allowed(self) -> None:
        initiator = initiator_events() + [verifier.EXPECTED_INERT_HEARTBEAT] * 2
        responder = responder_events() + [verifier.EXPECTED_INERT_HEARTBEAT]
        self.assertEqual(
            verifier.verify_segments(render(initiator), render(responder)).status,
            "PASS",
        )
        responder[-1] += " corrupted=true"
        with self.assertRaisesRegex(verifier.VerificationError, "unexpected"):
            verifier.verify_segments(render(initiator), render(responder))

    def test_each_segment_requires_one_counted_boot_and_reset(self) -> None:
        initiator = render(initiator_events())
        duplicate = (verifier.EXPECTED_ROM_BOOT + "\r\n").encode() + initiator
        with self.assertRaisesRegex(verifier.VerificationError, "exactly one counted"):
            verifier.verify_segments(duplicate, render(responder_events()))
        missing = initiator.replace(
            (verifier.EXPECTED_COUNTED_RESET + "\r\n").encode(), b"", 1
        )
        with self.assertRaisesRegex(verifier.VerificationError, "exactly one counted"):
            verifier.verify_segments(missing, render(responder_events()))

    def test_physical_mac_and_role_are_exact(self) -> None:
        initiator = initiator_events()
        initiator[0] = initiator[0].replace(
            verifier.INITIATOR_BASE_MAC, verifier.RESPONDER_BASE_MAC
        )
        with self.assertRaisesRegex(verifier.VerificationError, "physical MAC role gate"):
            verifier.verify_segments(render(initiator), render(responder_events()))

    def test_radio_profile_is_exact(self) -> None:
        responder = responder_events()
        profile = event_index(responder, "stage=profile")
        responder[profile] = responder[profile].replace(
            "frequency_hz=915000000", "frequency_hz=914999999"
        )
        with self.assertRaisesRegex(verifier.VerificationError, "fixed NA915 profile"):
            verifier.verify_segments(render(initiator_events()), render(responder))

    def test_busy_cad_fails(self) -> None:
        initiator = initiator_events()
        index = event_index(initiator, "stage=cad")
        initiator[index] = initiator[index].replace(
            "status=CLEAR", "status=BUSY"
        ).replace("activity_detected=false", "activity_detected=true")
        with self.assertRaisesRegex(verifier.VerificationError, "clear CAD"):
            verifier.verify_segments(render(initiator), render(responder_events()))

    def test_cad_timestamps_must_advance(self) -> None:
        initiator = initiator_events()
        second = event_index(initiator, "step=EncryptedData activity_detected=false")
        initiator[second] = initiator[second].replace(
            "observed_at_us=2000", "observed_at_us=1000"
        )
        with self.assertRaisesRegex(verifier.VerificationError, "strictly increasing"):
            verifier.verify_segments(render(initiator), render(responder_events()))

    def test_cross_board_packet_hash_mismatch_fails(self) -> None:
        responder = responder_events()
        index = event_index(
            responder, "direction=rx status=RNODE_PACKET step=InitiatorAnnounce"
        )
        responder[index] = responder[index].replace(
            INITIATOR_ANNOUNCE_HASH, "55" * 32
        )
        with self.assertRaisesRegex(verifier.VerificationError, "TX/RX.*packet_hash"):
            verifier.verify_segments(render(initiator_events()), render(responder))

    def test_packet_lengths_are_exact(self) -> None:
        initiator = initiator_events()
        data = event_index(
            initiator, "direction=tx status=DRIVER_TX_DONE step=EncryptedData"
        )
        initiator[data] = initiator[data].replace(
            "rns_len=147 physical_len=148", "rns_len=146 physical_len=147"
        )
        with self.assertRaisesRegex(verifier.VerificationError, "wrong fixture length"):
            verifier.verify_segments(render(initiator), render(responder_events()))

    def test_data_receipt_mismatch_fails(self) -> None:
        initiator = initiator_events()
        proof = event_index(
            initiator, "direction=rx status=RNODE_PACKET step=DeliveryProof"
        )
        initiator[proof] = initiator[proof].replace(DATA_HASH, "66" * 32)
        with self.assertRaisesRegex(verifier.VerificationError, "wrong DATA receipt"):
            verifier.verify_segments(render(initiator), render(responder_events()))

    def test_rx_signal_presence_and_bounds_are_required(self) -> None:
        responder = responder_events()
        packet_index = event_index(
            responder, "direction=rx status=RNODE_PACKET step=EncryptedData"
        )
        responder[packet_index] = responder[packet_index].replace(
            "signal_present=true rssi_dbm=-67",
            "signal_present=false rssi_dbm=1",
        )
        with self.assertRaisesRegex(verifier.VerificationError, "RX signal"):
            verifier.verify_segments(render(initiator_events()), render(responder))

    def test_missing_or_extra_transmission_fails(self) -> None:
        responder = responder_events()
        responder.pop(
            event_index(
                responder,
                "direction=tx status=DRIVER_TX_DONE step=ResponderAnnounce",
            )
        )
        with self.assertRaises(verifier.VerificationError):
            verifier.verify_segments(render(initiator_events()), render(responder))

        initiator = initiator_events()
        tx = event_index(
            initiator, "direction=tx status=DRIVER_TX_DONE step=EncryptedData"
        )
        initiator.insert(tx + 1, initiator[tx])
        with self.assertRaises(verifier.VerificationError):
            verifier.verify_segments(render(initiator), render(responder_events()))

    def test_data_ingress_is_explicit_and_exact(self) -> None:
        responder = responder_events()
        ingress = event_index(responder, "stage=data-ingress")
        responder[ingress] = responder[ingress].replace(
            "proof_actions=1", "proof_actions=0"
        )
        with self.assertRaisesRegex(verifier.VerificationError, "DATA payload"):
            verifier.verify_segments(render(initiator_events()), render(responder))

    def test_wrong_state_order_fails(self) -> None:
        initiator = initiator_events()
        first = event_index(initiator, "completed=Transmit(InitiatorAnnounce)")
        initiator[first] = initiator[first].replace(
            "InitiatorAwaitResponderAnnounce", "InitiatorSendData"
        )
        with self.assertRaisesRegex(verifier.VerificationError, "state transition"):
            verifier.verify_segments(render(initiator), render(responder_events()))

    def test_firmware_failure_and_runtime_fatal_output_are_rejected(self) -> None:
        initiator = initiator_events()
        initiator.append(
            "e290-semantic-hil stage=terminal status=FAIL reason=test"
        )
        with self.assertRaisesRegex(verifier.VerificationError, "reported failure"):
            verifier.verify_segments(render(initiator), render(responder_events()))

        fatal = render(responder_events()) + b"Guru Meditation Error\r\n"
        with self.assertRaisesRegex(verifier.VerificationError, "fatal runtime"):
            verifier.verify_segments(render(initiator_events()), fatal)

    def test_four_packet_hashes_must_be_distinct(self) -> None:
        initiator = initiator_events()
        responder = responder_events()
        for events in (initiator, responder):
            for index, event in enumerate(events):
                if "step=ResponderAnnounce" in event and "stage=packet" in event:
                    events[index] = event.replace(
                        RESPONDER_ANNOUNCE_HASH, INITIATOR_ANNOUNCE_HASH
                    )
        with self.assertRaisesRegex(verifier.VerificationError, "distinct and nonzero"):
            verifier.verify_segments(render(initiator), render(responder))

    def test_cli_reports_capture_hashes_and_board_facts(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            initiator = root / "initiator.raw"
            responder = root / "responder.raw"
            initiator.write_bytes(render(initiator_events()))
            responder.write_bytes(render(responder_events()))
            stdout = StringIO()
            stderr = StringIO()
            with redirect_stdout(stdout), redirect_stderr(stderr):
                status = verifier.main([str(initiator), str(responder)])
            self.assertEqual(status, 0, stderr.getvalue())
            report = json.loads(stdout.getvalue())
            self.assertEqual(report["schema"], verifier.SCHEMA)
            self.assertEqual(report["boards"]["initiator"]["base_mac"], verifier.INITIATOR_BASE_MAC)
            self.assertEqual(report["data_receipt"], DATA_HASH)
            self.assertEqual(len(report["captures"]["responder"]["capture_sha256"]), 64)

    def test_firmware_source_exposes_the_verified_packet_and_data_fields(self) -> None:
        repository = Path(__file__).resolve().parents[2]
        source = (
            repository
            / "firmware/heltec-vision-master-e290-semantic-hil/src/main.rs"
        ).read_text()
        self.assertIn(
            "destination_hash={} data_receipt={} signal_present={}", source
        )
        self.assertIn(
            "stage=data-ingress status=SEMANTIC_VALIDATED", source
        )
        self.assertIn("stage=complete role={} radio_active={}", source)


if __name__ == "__main__":
    unittest.main()
