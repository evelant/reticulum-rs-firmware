from __future__ import annotations

from contextlib import redirect_stderr, redirect_stdout
from io import StringIO
import json
from pathlib import Path
import tempfile
import unittest

import verify_storage_hil_log as verifier


def valid_events() -> list[str]:
    events: list[str] = []
    for expectation in verifier._expected_sequence():
        if expectation.label == "boot 1 identity":
            events.append(
                "storage-hil stage=boot status=PASS "
                "base_mac=44:1b:f6:f8:e9:44 "
                "reset_reason=Some(CoreUsbJtag) rf_inert=true"
            )
        elif expectation.label == "boot 2 CoreSw identity":
            events.append(
                "storage-hil stage=boot status=PASS "
                "base_mac=44:1b:f6:f8:e9:44 "
                "reset_reason=Some(CoreSw) rf_inert=true"
            )
        else:
            # Exact expectations intentionally retain their fixed string in
            # the closure. Build the deterministic fixture through the known
            # contract values below rather than weakening production access.
            for candidate in production_exact_messages():
                if expectation.match(candidate):
                    events.append(candidate)
                    break
            else:
                raise AssertionError(f"no fixture for {expectation.label}")
    events.append(verifier.EXPECTED_HEARTBEAT)
    return events


def production_exact_messages() -> list[str]:
    messages = [
        "storage-hil stage=rf-interlock status=PASS sx1262_reset=low "
        "fem_power=low fem_csd=low fem_ctx=low sx1262_nss=high "
        "vext=low battery_divider=low",
        "storage-hil stage=capture-guard status=ARMED duration_ms=5000 "
        "retlog_access=false flash_mutation=false",
        "storage-hil stage=capture-guard status=COMPLETE duration_ms=5000 "
        "retlog_access=false flash_mutation=false",
        "storage-hil stage=preflight status=PASS flash_bytes=8388608 "
        "flash_encryption=false retlog_offset=0x00670000 "
        "retlog_len=0x00100000 retlog_plaintext=true retlog_writable=true",
        "storage-hil stage=raw-region status=PASS write_calls=0 erase_calls=0",
        "storage-hil stage=format status=PASS bank=A generation=1 records=0 "
        "write_calls=2 erase_calls=0",
        "storage-hil stage=mount status=PASS bank=A generation=1 records=0 "
        "consumed_slots=0 accepted=0 compaction_pending=false "
        "write_calls=2 erase_calls=0",
        verifier.EXPECTED_SEMANTIC_REPLAY,
        "storage-hil stage=exact-retry status=PASS write_calls=12 erase_calls=0",
        "storage-hil stage=logical-conflict status=PASS write_calls=12 "
        "erase_calls=0",
        "storage-hil stage=compact status=PASS bank=B generation=2 records=5 "
        "consumed_slots=5 write_calls=26 erase_calls=3",
        "storage-hil stage=software-reset status=ARMED "
        "reason=post-compaction source_generation=1 target_generation=2 "
        "delay_ms=250 rf_inert=true",
        "storage-hil stage=software-reset status=ISSUED "
        "reason=post-compaction source_generation=1 target_generation=2 "
        "flush_ms=100 rf_inert=true",
        "storage-hil stage=mount status=PASS bank=B generation=2 records=5 "
        "consumed_slots=5 accepted=1 compaction_pending=false "
        "write_calls=0 erase_calls=0",
        "storage-hil stage=final-replay status=PASS bank=B generation=2 "
        "records=5 accepted=1 write_calls=0 erase_calls=0 rf_inert=true",
    ]
    for index in range(5):
        records = index + 1
        messages.append(
            "storage-hil stage=seed status=PASS "
            f"record_index={index} records={records} consumed_slots={records} "
            f"write_calls={4 + 2 * index} erase_calls=0"
        )
    return messages


def render(events: list[str], *, ansi: bool = False) -> bytes:
    lines = ["ESP-ROM:esp32s3 unrelated boot text"]
    for event in events:
        prefix = "\x1b[0;32mINFO\x1b[0m - " if ansi else "INFO - "
        lines.append(prefix + event)
    return ("\r\n".join(lines) + "\r\n").encode()


class VerificationTests(unittest.TestCase):
    def test_accepts_exact_sequence_with_ansi_and_logger_prefixes(self) -> None:
        result = verifier.verify_segment(render(valid_events(), ansi=True))
        self.assertEqual(result.status, "PASS")
        self.assertEqual(result.boots, 2)
        self.assertEqual(result.first_reset_reason, "Some(CoreUsbJtag)")
        self.assertEqual(result.second_reset_reason, "Some(CoreSw)")
        self.assertEqual(result.heartbeats, 1)

    def test_byte_offset_excludes_an_uncounted_activation(self) -> None:
        prefix = render(
            [
                "storage-hil stage=boot status=PASS "
                "base_mac=44:1b:f6:f8:e0:40 "
                "reset_reason=Some(ChipPowerOn) rf_inert=true"
            ]
        )
        payload = prefix + render(valid_events())
        result = verifier.verify_capture(payload, len(prefix))
        self.assertEqual(result.first_reset_reason, "Some(CoreUsbJtag)")

    def test_wrong_target_identity_fails_closed(self) -> None:
        events = valid_events()
        events[0] = events[0].replace("e9:44", "e0:40")
        with self.assertRaisesRegex(verifier.VerificationError, "boot 1 identity"):
            verifier.verify_segment(render(events))

    def test_wrong_seed_counter_fails_closed(self) -> None:
        events = valid_events()
        seed = next(i for i, event in enumerate(events) if "record_index=3" in event)
        events[seed] = events[seed].replace("write_calls=10", "write_calls=9")
        with self.assertRaisesRegex(verifier.VerificationError, "seed record 3"):
            verifier.verify_segment(render(events))

    def test_old_capture_guard_vocabulary_fails_closed(self) -> None:
        events = valid_events()
        guard = next(i for i, event in enumerate(events) if "status=ARMED" in event)
        events[guard] = events[guard].replace(
            "retlog_access=false flash_mutation=false", "flash_access=false"
        )
        with self.assertRaisesRegex(verifier.VerificationError, "capture guard armed"):
            verifier.verify_segment(render(events))

    def test_missing_software_reset_flush_marker_fails_closed(self) -> None:
        events = valid_events()
        issued = next(
            i
            for i, event in enumerate(events)
            if "stage=software-reset status=ISSUED" in event
        )
        events[issued] = events[issued].replace(" flush_ms=100", "")
        with self.assertRaisesRegex(verifier.VerificationError, "reset issued"):
            verifier.verify_segment(render(events))

    def test_second_boot_must_report_core_software_reset(self) -> None:
        events = valid_events()
        boot_indices = [
            i for i, event in enumerate(events) if "stage=boot status=PASS" in event
        ]
        events[boot_indices[1]] = events[boot_indices[1]].replace(
            "Some(CoreSw)", "Some(CoreUsbJtag)"
        )
        with self.assertRaisesRegex(verifier.VerificationError, "boot 2 CoreSw"):
            verifier.verify_segment(render(events))

    def test_firmware_failure_is_never_ignored(self) -> None:
        events = valid_events()
        events.append("storage-hil stage=mount status=FAIL reason=corrupt")
        with self.assertRaisesRegex(verifier.VerificationError, "reported failure"):
            verifier.verify_segment(render(events))

    def test_fatal_non_project_runtime_output_is_never_ignored(self) -> None:
        valid = render(valid_events())
        for marker in ("panicked at src/main.rs", "Guru Meditation Error", "abort()"):
            with self.subTest(marker=marker), self.assertRaisesRegex(
                verifier.VerificationError, "fatal runtime output"
            ):
                verifier.verify_segment(valid + f"ERROR - {marker}\r\n".encode())

    def test_first_post_replay_event_must_be_exact_heartbeat(self) -> None:
        events = valid_events()
        events[-1] = events[-1].replace("write_calls=0", "write_calls=1")
        with self.assertRaisesRegex(verifier.VerificationError, "exact generation-2"):
            verifier.verify_segment(render(events))

    def test_extra_exact_heartbeats_are_allowed(self) -> None:
        events = valid_events() + [verifier.EXPECTED_HEARTBEAT]
        result = verifier.verify_segment(render(events))
        self.assertEqual(result.heartbeats, 2)

    def test_offset_beyond_capture_fails_closed(self) -> None:
        with self.assertRaisesRegex(verifier.VerificationError, "exceeds capture"):
            verifier.verify_capture(b"short", 6)


class CommandLineTests(unittest.TestCase):
    def test_success_report_binds_capture_and_segment_hashes(self) -> None:
        prefix = b"uncounted\n"
        payload = prefix + render(valid_events())
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "serial.log"
            log.write_bytes(payload)
            stdout = StringIO()
            with redirect_stdout(stdout):
                status = verifier.main(
                    [str(log), "--byte-offset", hex(len(prefix))]
                )
        self.assertEqual(status, 0)
        report = json.loads(stdout.getvalue())
        self.assertEqual(report["status"], "PASS")
        self.assertEqual(report["byte_offset"], len(prefix))
        self.assertEqual(report["boots"], 2)
        self.assertEqual(report["heartbeats"], 1)
        self.assertEqual(len(report["capture_sha256"]), 64)
        self.assertEqual(len(report["segment_sha256"]), 64)

    def test_failure_returns_nonzero_without_pass_report(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "serial.log"
            log.write_bytes(b"not evidence")
            stdout = StringIO()
            stderr = StringIO()
            with redirect_stdout(stdout), redirect_stderr(stderr):
                status = verifier.main([str(log)])
        self.assertEqual(status, 1)
        self.assertEqual(stdout.getvalue(), "")
        self.assertIn("status=FAIL", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
