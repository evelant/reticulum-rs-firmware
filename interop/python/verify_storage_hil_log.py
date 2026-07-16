#!/usr/bin/env python3
"""Fail-closed verifier for one counted Heltec storage-HIL serial run.

The selected byte segment must contain exactly the two storage-HIL boots that
start with an erased ``retlog`` partition: generation-one formatting and
compaction, followed by the firmware-issued software-reset replay. Logger
prefixes, ANSI coloring, and unrelated ESP ROM output are ignored; every
``storage-hil`` event is otherwise matched exactly and in order.

An offset identifies the byte position recorded immediately before the
identity-qualified reset. Supplying an already trimmed segment is equivalent
to using the default offset of zero.
"""

from __future__ import annotations

import argparse
from dataclasses import asdict, dataclass
import hashlib
import json
from pathlib import Path
import re
import sys
from typing import Callable, Sequence


EXPECTED_BASE_MAC = "44:1b:f6:f8:e9:44"
EXPECTED_CAPTURE_GUARD_MS = 5_000
EXPECTED_RESET_LOG_FLUSH_MS = 100
FATAL_OUTPUT_MARKERS = ("panicked at", "Guru Meditation", "abort()")

ANSI_CSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
ANSI_OSC = re.compile(r"\x1b\][^\x07]*(?:\x07|\x1b\\)")
BOOT_PATTERN = re.compile(
    rf"storage-hil stage=boot status=PASS base_mac={re.escape(EXPECTED_BASE_MAC)} "
    r"reset_reason=(?P<reset_reason>Some\([A-Za-z0-9]+\)) rf_inert=true"
)


class VerificationError(RuntimeError):
    """The selected log segment does not prove the required HIL sequence."""


@dataclass(frozen=True)
class VerificationResult:
    """Machine-readable facts established by a successful verification."""

    schema: int
    status: str
    target_base_mac: str
    boots: int
    first_reset_reason: str
    second_reset_reason: str
    generation: int
    committed_records: int
    consumed_slots: int
    accepted_submissions: int
    write_calls: int
    erase_calls: int
    heartbeats: int
    rf_inert: bool


@dataclass(frozen=True)
class Expectation:
    label: str
    match: Callable[[str], bool]


def _exact(label: str, expected: str) -> Expectation:
    return Expectation(label, lambda actual: actual == expected)


def _delivered_state() -> str:
    packet_digest = ", ".join(["110"] * 32)
    attempt_token = ", ".join(["165"] * 32)
    return (
        "Final(Delivered(PreparedPacketDetails { packet_len: 97, "
        f"encoded_packet_sha256: EncodedPacketSha256([{packet_digest}]), "
        f"rns_attempt_token: RnsAttemptToken([{attempt_token}]) }}))"
    )


EXPECTED_SEMANTIC_REPLAY = (
    "storage-hil stage=semantic-replay status=PASS revision=4 "
    f"state={_delivered_state()}"
)

EXPECTED_HEARTBEAT = (
    "storage-hil heartbeat stage=complete status=PASS generation=2 "
    "write_calls=0 erase_calls=0 rf_inert=true"
)


def _boot_expectation(label: str, *, core_software: bool) -> Expectation:
    def matches(actual: str) -> bool:
        matched = BOOT_PATTERN.fullmatch(actual)
        if matched is None:
            return False
        return not core_software or matched.group("reset_reason") == "Some(CoreSw)"

    return Expectation(label, matches)


def _expected_sequence() -> list[Expectation]:
    sequence = [
        _boot_expectation("boot 1 identity", core_software=False),
        _exact(
            "boot 1 RF interlock",
            "storage-hil stage=rf-interlock status=PASS sx1262_reset=low "
            "fem_power=low fem_csd=low fem_ctx=low sx1262_nss=high "
            "vext=low battery_divider=low",
        ),
        _exact(
            "boot 1 capture guard armed",
            "storage-hil stage=capture-guard status=ARMED duration_ms="
            f"{EXPECTED_CAPTURE_GUARD_MS} retlog_access=false "
            "flash_mutation=false",
        ),
        _exact(
            "boot 1 capture guard complete",
            "storage-hil stage=capture-guard status=COMPLETE duration_ms="
            f"{EXPECTED_CAPTURE_GUARD_MS} retlog_access=false "
            "flash_mutation=false",
        ),
        _exact(
            "boot 1 preflight",
            "storage-hil stage=preflight status=PASS flash_bytes=8388608 "
            "flash_encryption=false retlog_offset=0x00670000 "
            "retlog_len=0x00100000 retlog_plaintext=true "
            "retlog_writable=true",
        ),
        _exact(
            "boot 1 raw region",
            "storage-hil stage=raw-region status=PASS write_calls=0 erase_calls=0",
        ),
        _exact(
            "generation 1 format",
            "storage-hil stage=format status=PASS bank=A generation=1 records=0 "
            "write_calls=2 erase_calls=0",
        ),
        _exact(
            "generation 1 empty mount",
            "storage-hil stage=mount status=PASS bank=A generation=1 records=0 "
            "consumed_slots=0 accepted=0 compaction_pending=false "
            "write_calls=2 erase_calls=0",
        ),
    ]

    for index in range(5):
        records = index + 1
        writes = 4 + index * 2
        sequence.append(
            _exact(
                f"seed record {index}",
                "storage-hil stage=seed status=PASS "
                f"record_index={index} records={records} "
                f"consumed_slots={records} write_calls={writes} erase_calls=0",
            )
        )

    sequence.extend(
        [
            _exact("generation 1 semantic replay", EXPECTED_SEMANTIC_REPLAY),
            _exact(
                "exact idempotent retry",
                "storage-hil stage=exact-retry status=PASS write_calls=12 "
                "erase_calls=0",
            ),
            _exact(
                "logical conflict",
                "storage-hil stage=logical-conflict status=PASS write_calls=12 "
                "erase_calls=0",
            ),
            _exact(
                "generation 2 compaction",
                "storage-hil stage=compact status=PASS bank=B generation=2 "
                "records=5 consumed_slots=5 write_calls=26 erase_calls=3",
            ),
            _exact(
                "post-compaction software reset armed",
                "storage-hil stage=software-reset status=ARMED "
                "reason=post-compaction source_generation=1 target_generation=2 "
                "delay_ms=250 rf_inert=true",
            ),
            _exact(
                "post-compaction software reset issued",
                "storage-hil stage=software-reset status=ISSUED "
                "reason=post-compaction source_generation=1 target_generation=2 "
                f"flush_ms={EXPECTED_RESET_LOG_FLUSH_MS} rf_inert=true",
            ),
            _boot_expectation("boot 2 CoreSw identity", core_software=True),
            _exact(
                "boot 2 RF interlock",
                "storage-hil stage=rf-interlock status=PASS sx1262_reset=low "
                "fem_power=low fem_csd=low fem_ctx=low sx1262_nss=high "
                "vext=low battery_divider=low",
            ),
            _exact(
                "boot 2 capture guard armed",
                "storage-hil stage=capture-guard status=ARMED duration_ms="
                f"{EXPECTED_CAPTURE_GUARD_MS} retlog_access=false "
                "flash_mutation=false",
            ),
            _exact(
                "boot 2 capture guard complete",
                "storage-hil stage=capture-guard status=COMPLETE duration_ms="
                f"{EXPECTED_CAPTURE_GUARD_MS} retlog_access=false "
                "flash_mutation=false",
            ),
            _exact(
                "boot 2 preflight",
                "storage-hil stage=preflight status=PASS flash_bytes=8388608 "
                "flash_encryption=false retlog_offset=0x00670000 "
                "retlog_len=0x00100000 retlog_plaintext=true "
                "retlog_writable=true",
            ),
            _exact(
                "boot 2 raw region",
                "storage-hil stage=raw-region status=PASS write_calls=0 "
                "erase_calls=0",
            ),
            _exact(
                "generation 2 replay mount",
                "storage-hil stage=mount status=PASS bank=B generation=2 records=5 "
                "consumed_slots=5 accepted=1 compaction_pending=false "
                "write_calls=0 erase_calls=0",
            ),
            _exact("generation 2 semantic replay", EXPECTED_SEMANTIC_REPLAY),
            _exact(
                "generation 2 final replay",
                "storage-hil stage=final-replay status=PASS bank=B generation=2 "
                "records=5 accepted=1 write_calls=0 erase_calls=0 rf_inert=true",
            ),
        ]
    )
    return sequence


def _normalized_serial_text(segment: bytes) -> str:
    text = segment.decode("utf-8", errors="replace")
    return ANSI_OSC.sub("", ANSI_CSI.sub("", text))


def extract_storage_events(segment: bytes) -> list[str]:
    """Extract normalized project events without weakening their contents."""

    text = _normalized_serial_text(segment)
    events: list[str] = []
    for line in text.splitlines():
        marker = line.find("storage-hil ")
        if marker >= 0:
            events.append(line[marker:].strip())
    return events


def verify_segment(segment: bytes) -> VerificationResult:
    """Verify one already selected serial segment or raise VerificationError."""

    text = _normalized_serial_text(segment)
    for marker in FATAL_OUTPUT_MARKERS:
        if marker in text:
            raise VerificationError(f"fatal runtime output contains {marker!r}")

    events = extract_storage_events(segment)
    if not events:
        raise VerificationError("selected segment contains no storage-hil events")

    failed = [event for event in events if " status=FAIL" in event]
    if failed:
        raise VerificationError(f"firmware reported failure: {failed[0]}")

    expected = _expected_sequence()
    if len(events) < len(expected) + 1:
        raise VerificationError(
            f"incomplete sequence: expected at least {len(expected) + 1} "
            f"storage-hil events, found {len(events)}"
        )

    for index, (actual, contract) in enumerate(zip(events, expected, strict=False)):
        if not contract.match(actual):
            raise VerificationError(
                f"event {index + 1} violates {contract.label}: {actual}"
            )

    heartbeats = events[len(expected) :]
    for index, heartbeat in enumerate(heartbeats, start=1):
        if heartbeat != EXPECTED_HEARTBEAT:
            raise VerificationError(
                f"event {len(expected) + index} is not an exact generation-2 "
                f"heartbeat: {heartbeat}"
            )

    boot_matches = [
        matched
        for event in events[: len(expected)]
        if (matched := BOOT_PATTERN.fullmatch(event)) is not None
    ]
    if len(boot_matches) != 2:
        raise AssertionError("boot expectations and extraction disagreed")
    first_boot, second_boot = boot_matches

    return VerificationResult(
        schema=1,
        status="PASS",
        target_base_mac=EXPECTED_BASE_MAC,
        boots=2,
        first_reset_reason=first_boot.group("reset_reason"),
        second_reset_reason=second_boot.group("reset_reason"),
        generation=2,
        committed_records=5,
        consumed_slots=5,
        accepted_submissions=1,
        write_calls=0,
        erase_calls=0,
        heartbeats=len(heartbeats),
        rf_inert=True,
    )


def verify_capture(payload: bytes, byte_offset: int = 0) -> VerificationResult:
    """Select a byte offset and verify the remaining capture."""

    if byte_offset < 0:
        raise VerificationError("byte offset must not be negative")
    if byte_offset > len(payload):
        raise VerificationError(
            f"byte offset {byte_offset} exceeds capture length {len(payload)}"
        )
    return verify_segment(payload[byte_offset:])


def _byte_offset(argument: str) -> int:
    try:
        value = int(argument, 0)
    except ValueError as error:
        raise argparse.ArgumentTypeError(
            "byte offset must be a decimal or 0x-prefixed integer"
        ) from error
    if value < 0:
        raise argparse.ArgumentTypeError("byte offset must not be negative")
    return value


def parse_args(arguments: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="verify one counted two-boot Heltec storage-HIL serial log"
    )
    parser.add_argument("log", type=Path)
    parser.add_argument(
        "--byte-offset",
        type=_byte_offset,
        default=0,
        help="start at this recorded byte offset (default: already trimmed)",
    )
    return parser.parse_args(arguments)


def main(arguments: Sequence[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if arguments is None else arguments)
    try:
        payload = args.log.read_bytes()
        result = verify_capture(payload, args.byte_offset)
    except (OSError, VerificationError) as error:
        print(f"storage-hil-log status=FAIL reason={error}", file=sys.stderr)
        return 1

    segment = payload[args.byte_offset :]
    report = {
        **asdict(result),
        "log": str(args.log),
        "byte_offset": args.byte_offset,
        "capture_bytes": len(payload),
        "capture_sha256": hashlib.sha256(payload).hexdigest(),
        "segment_bytes": len(segment),
        "segment_sha256": hashlib.sha256(segment).hexdigest(),
    }
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
