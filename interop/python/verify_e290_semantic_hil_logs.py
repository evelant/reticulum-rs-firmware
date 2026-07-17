#!/usr/bin/env python3
"""Fail-closed verifier for one counted two-board E290 semantic LoRa HIL.

The verifier consumes the exact project events emitted by the E290 initiator
and responder images, proves each local state-machine trace, and then binds the
four transmitted RNS packets to the peer observations.  Byte offsets select
the counted-reset segment in independently captured raw USB serial streams.
"""

from __future__ import annotations

import argparse
from dataclasses import asdict, dataclass
import hashlib
import json
from pathlib import Path
import re
import sys
from typing import Sequence


SCHEMA = "reticulum.e290-semantic-hil-logs.v1"
INITIATOR_BASE_MAC = "ac:a7:04:e1:3e:88"
RESPONDER_BASE_MAC = "ac:a7:04:e1:3f:88"
INITIATOR_DESTINATION = "aa2d77f65518c78ad1821ee056976b2a"
RESPONDER_DESTINATION = "fdc1997055c17cf3fbdb192c55ceb3ef"

EXPECTED_ROM_BOOT = "ESP-ROM:esp32s3-20210327"
EXPECTED_COUNTED_RESET = (
    "rst:0x15 (USB_UART_CHIP_RESET),boot:0x8 (SPI_FAST_FLASH_BOOT)"
)
EXPECTED_PROFILE = (
    "e290-semantic-hil stage=profile status=PASS region=NA915 "
    "frequency_hz=915000000 bandwidth_hz=125000 sf=7 "
    "coding_rate_denominator=5 preamble_symbols=24 explicit_header=true "
    "crc=true iq_inverted=false sync_word=0x1424 "
    "sx1262_requested_output_dbm=14 sx1262_raw_set_tx_params_dbm=22 "
    "power_claim=configuration-target-not-calibration rx_symbol_timeout=248 "
    "cad_operation_deadline_ms=500 receive_operation_deadline_ms=1750 "
    "transmit_operation_deadline_ms=1500 exchange_deadline_seconds=180"
)
EXPECTED_RUNTIME_PATCH = "esp-rtos-0.3.0-cpu0-cpu1-main-stack-words-v2"
EXPECTED_INERT_HEARTBEAT = (
    "e290-semantic-hil stage=inert-heartbeat rf_state=reset_low"
)
EXPECTED_PACKET_LENGTHS = {
    "InitiatorAnnounce": (167, 168),
    "ResponderAnnounce": (167, 168),
    "EncryptedData": (147, 148),
    "DeliveryProof": (115, 116),
}
EXPECTED_SEQUENCE = {
    "InitiatorAnnounce": 9,
    "ResponderAnnounce": 10,
    "EncryptedData": 11,
    "DeliveryProof": 12,
}

FATAL_OUTPUT_MARKERS = (
    "panicked at",
    "guru meditation",
    "abort()",
    "stack overflow",
)

ANSI_CSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
ANSI_OSC = re.compile(r"\x1b\][^\x07]*(?:\x07|\x1b\\)")
HEX_16 = r"[0-9a-f]{32}"
HEX_32 = r"[0-9a-f]{64}"

PACKET_PATTERN = re.compile(
    r"e290-semantic-hil stage=packet "
    r"direction=(?P<direction>tx|rx) "
    r"status=(?P<status>DRIVER_TX_DONE|RNODE_PACKET) "
    r"step=(?P<step>InitiatorAnnounce|ResponderAnnounce|EncryptedData|DeliveryProof) "
    r"rns_len=(?P<rns_len>[0-9]+) physical_len=(?P<physical_len>[0-9]+) "
    r"sequence=(?P<sequence>[0-9]+) packet_hash=(?P<packet_hash>"
    + HEX_32
    + r") destination_hash=(?P<destination_hash>none|"
    + HEX_16
    + r") data_receipt=(?P<data_receipt>none|"
    + HEX_32
    + r") signal_present=(?P<signal_present>true|false) "
    r"rssi_dbm=(?P<rssi_dbm>-?[0-9]+) snr_db=(?P<snr_db>-?[0-9]+)"
)
CAD_PATTERN = re.compile(
    r"e290-semantic-hil stage=cad status=(?P<status>CLEAR|BUSY) "
    r"step=(?P<step>InitiatorAnnounce|ResponderAnnounce|EncryptedData|DeliveryProof) "
    r"activity_detected=(?P<activity>true|false) "
    r"observed_at_us=(?P<observed_at_us>[0-9]+)"
)


class VerificationError(RuntimeError):
    """The selected pair of capture segments does not prove the HIL."""


@dataclass(frozen=True)
class PacketEvidence:
    """One independently logged RNS packet observation."""

    direction: str
    step: str
    sequence: int
    rns_len: int
    physical_len: int
    packet_hash: str
    destination_hash: str | None
    data_receipt: str | None
    rssi_dbm: int | None
    snr_db: int | None


@dataclass(frozen=True)
class BoardEvidence:
    """Facts established from one board's complete local serial trace."""

    role: str
    base_mac: str
    local_destination: str
    peer_destination: str
    packets: tuple[PacketEvidence, ...]
    cad_timestamps_us: tuple[int, int]
    data_receipt: str


@dataclass(frozen=True)
class VerificationResult:
    """Cross-board facts established by a successful verification."""

    schema: str
    status: str
    initiator: BoardEvidence
    responder: BoardEvidence
    packet_hashes: dict[str, str]
    data_receipt: str


def _normalized_serial_text(segment: bytes) -> str:
    text = segment.decode("utf-8", errors="replace")
    return ANSI_OSC.sub("", ANSI_CSI.sub("", text))


def extract_events(segment: bytes) -> list[str]:
    """Extract normalized E290 project events without weakening contents."""

    events: list[str] = []
    for line in _normalized_serial_text(segment).splitlines():
        marker = line.find("e290-semantic-hil ")
        if marker >= 0:
            events.append(line[marker:].strip())
    return events


class _BoardVerifier:
    def __init__(self, segment: bytes, role: str) -> None:
        self.role = role
        self.events = extract_events(segment)
        self.index = 0
        self.packets: list[PacketEvidence] = []
        self.cad_timestamps: list[int] = []

        text = _normalized_serial_text(segment)
        lines = [line.strip() for line in text.splitlines()]
        boot_indices = [
            index for index, line in enumerate(lines) if line == EXPECTED_ROM_BOOT
        ]
        reset_indices = [
            index for index, line in enumerate(lines) if line == EXPECTED_COUNTED_RESET
        ]
        first_event_index = next(
            (index for index, line in enumerate(lines) if "e290-semantic-hil " in line),
            len(lines),
        )
        if len(boot_indices) != 1 or len(reset_indices) != 1:
            raise VerificationError(
                f"{role} segment must contain exactly one counted ESP32-S3 boot "
                f"and USB-UART reset (boots={len(boot_indices)} "
                f"resets={len(reset_indices)})"
            )
        if not (boot_indices[0] < reset_indices[0] < first_event_index):
            raise VerificationError(
                f"{role} counted boot/reset markers are not before the firmware trace"
            )
        folded = text.casefold()
        for marker in FATAL_OUTPUT_MARKERS:
            if marker.casefold() in folded:
                raise VerificationError(
                    f"{role} fatal runtime output contains {marker!r}"
                )
        if not self.events:
            raise VerificationError(f"{role} segment contains no E290 HIL events")
        failures = [event for event in self.events if " status=FAIL" in event]
        if failures:
            raise VerificationError(f"{role} firmware reported failure: {failures[0]}")

    def _next(self, label: str) -> str:
        if self.index >= len(self.events):
            raise VerificationError(
                f"{self.role} trace ended before {label} at event {self.index + 1}"
            )
        event = self.events[self.index]
        self.index += 1
        return event

    def exact(self, label: str, expected: str) -> None:
        actual = self._next(label)
        if actual != expected:
            raise VerificationError(
                f"{self.role} event {self.index} violates {label}: {actual}"
            )

    def match(self, label: str, pattern: re.Pattern[str]) -> re.Match[str]:
        actual = self._next(label)
        matched = pattern.fullmatch(actual)
        if matched is None:
            raise VerificationError(
                f"{self.role} event {self.index} violates {label}: {actual}"
            )
        return matched

    def cad(self, step: str) -> None:
        matched = self.match(f"clear CAD for {step}", CAD_PATTERN)
        observed_at_us = int(matched.group("observed_at_us"))
        if (
            matched.group("status") != "CLEAR"
            or matched.group("step") != step
            or matched.group("activity") != "false"
            or observed_at_us <= 0
        ):
            raise VerificationError(
                f"{self.role} {step} did not have one valid clear CAD observation"
            )
        if self.cad_timestamps and observed_at_us <= self.cad_timestamps[-1]:
            raise VerificationError(
                f"{self.role} CAD timestamps are not strictly increasing"
            )
        self.cad_timestamps.append(observed_at_us)

    def packet(
        self,
        *,
        direction: str,
        step: str,
        destination_hash: str | None,
        receipt_kind: str,
    ) -> PacketEvidence:
        matched = self.match(f"{direction} packet {step}", PACKET_PATTERN)
        status = "DRIVER_TX_DONE" if direction == "tx" else "RNODE_PACKET"
        expected_destination = destination_hash if destination_hash is not None else "none"
        if (
            matched.group("direction") != direction
            or matched.group("status") != status
            or matched.group("step") != step
            or int(matched.group("sequence")) != EXPECTED_SEQUENCE[step]
            or matched.group("destination_hash") != expected_destination
        ):
            raise VerificationError(
                f"{self.role} {step} packet has wrong direction, status, sequence, or destination"
            )

        rns_len = int(matched.group("rns_len"))
        physical_len = int(matched.group("physical_len"))
        if (rns_len, physical_len) != EXPECTED_PACKET_LENGTHS[step]:
            raise VerificationError(
                f"{self.role} {step} packet has wrong fixture length: "
                f"RNS={rns_len} physical={physical_len}"
            )
        if physical_len != rns_len + 1 or physical_len > 255:
            raise VerificationError(
                f"{self.role} {step} is not one canonical RNode physical frame"
            )

        packet_hash = matched.group("packet_hash")
        receipt_text = matched.group("data_receipt")
        receipt = None if receipt_text == "none" else receipt_text
        if receipt_kind == "none" and receipt is not None:
            raise VerificationError(f"{self.role} {step} unexpectedly logged a receipt")
        if receipt_kind == "packet" and receipt != packet_hash:
            raise VerificationError(
                f"{self.role} DATA packet hash does not equal its receipt"
            )
        if receipt_kind == "required" and receipt is None:
            raise VerificationError(f"{self.role} proof omitted the covered DATA receipt")

        signal_present = matched.group("signal_present") == "true"
        rssi = int(matched.group("rssi_dbm"))
        snr = int(matched.group("snr_db"))
        if direction == "tx":
            if signal_present or rssi != 0 or snr != 0:
                raise VerificationError(
                    f"{self.role} TX packet claimed receive signal metadata"
                )
            signal_rssi: int | None = None
            signal_snr: int | None = None
        else:
            if not signal_present or not (-200 <= rssi <= 0) or not (-40 <= snr <= 40):
                raise VerificationError(
                    f"{self.role} RX signal is absent or outside conservative bounds"
                )
            signal_rssi = rssi
            signal_snr = snr

        evidence = PacketEvidence(
            direction=direction,
            step=step,
            sequence=EXPECTED_SEQUENCE[step],
            rns_len=rns_len,
            physical_len=physical_len,
            packet_hash=packet_hash,
            destination_hash=destination_hash,
            data_receipt=receipt,
            rssi_dbm=signal_rssi,
            snr_db=signal_snr,
        )
        self.packets.append(evidence)
        return evidence

    def finish(self) -> None:
        while (
            self.index < len(self.events)
            and self.events[self.index] == EXPECTED_INERT_HEARTBEAT
        ):
            self.index += 1
        if self.index != len(self.events):
            raise VerificationError(
                f"{self.role} trace has {len(self.events) - self.index} unexpected "
                f"event(s), first: {self.events[self.index]}"
            )
        if len(self.cad_timestamps) != 2:
            raise VerificationError(
                f"{self.role} trace has {len(self.cad_timestamps)} CAD observations, expected 2"
            )


def _common_prefix(
    verifier: _BoardVerifier,
    *,
    base_mac: str,
    role: str,
    phase: str,
    local_destination: str,
    peer_destination: str,
) -> None:
    verifier.exact(
        "physical MAC role gate",
        "e290-semantic-hil stage=mac-gate "
        f"base_mac={base_mac} role={role} exact_match=true "
        "radio_constructed=false spi_constructed=false "
        "rf_state=reset_low_nss_high",
    )
    verifier.exact("fixed NA915 profile", EXPECTED_PROFILE)
    verifier.exact(
        "runtime patch identity",
        "e290-semantic-hil stage=runtime-patch "
        f"esp_rtos_main_stack_slice={EXPECTED_RUNTIME_PATCH}",
    )
    verifier.exact(
        "radio initialization",
        "e290-semantic-hil stage=radio-init status=PASS "
        f"role={role} regulator=dcdc rf_switch=dio2 tcxo=dio3_1v8 "
        "tx_budget_packets=2",
    )
    verifier.exact(
        "exchange readiness",
        "e290-semantic-hil stage=exchange status=ARMED "
        f"role={role} phase={phase} local_destination={local_destination} "
        f"peer_destination={peer_destination} payload_len=36 tx_budget=2 "
        "maximum_rx_windows=48",
    )


def _state(verifier: _BoardVerifier, completed: str, next_phase: str) -> None:
    verifier.exact(
        f"state transition after {completed}",
        "e290-semantic-hil stage=state status=ADVANCED "
        f"completed={completed} next_phase={next_phase}",
    )


def _rx_armed(verifier: _BoardVerifier, role: str, step: str) -> None:
    verifier.exact(
        f"bounded receive for {step}",
        "e290-semantic-hil stage=rx status=ARMED "
        f"role={role} step={step} maximum_windows=48",
    )


def _terminal(
    verifier: _BoardVerifier,
    *,
    role: str,
    local_destination: str,
    peer_destination: str,
    receipt: str,
) -> None:
    verifier.exact(
        "successful semantic terminal",
        "e290-semantic-hil stage=terminal status=PASS "
        f"role={role} tx_done=2 local_destination={local_destination} "
        f"peer_destination={peer_destination} data_receipt={receipt} "
        "radio_shutdown=next",
    )
    verifier.exact(
        "radio shutdown completion",
        "e290-semantic-hil stage=complete "
        f"role={role} radio_active=false action=permanent-rf-inert",
    )


def _verify_initiator(segment: bytes) -> BoardEvidence:
    verifier = _BoardVerifier(segment, "initiator")
    _common_prefix(
        verifier,
        base_mac=INITIATOR_BASE_MAC,
        role="initiator",
        phase="InitiatorSendAnnounce",
        local_destination=INITIATOR_DESTINATION,
        peer_destination=RESPONDER_DESTINATION,
    )
    verifier.cad("InitiatorAnnounce")
    verifier.packet(
        direction="tx",
        step="InitiatorAnnounce",
        destination_hash=INITIATOR_DESTINATION,
        receipt_kind="none",
    )
    _state(
        verifier,
        "Transmit(InitiatorAnnounce)",
        "InitiatorAwaitResponderAnnounce",
    )
    _rx_armed(verifier, "initiator", "ResponderAnnounce")
    verifier.packet(
        direction="rx",
        step="ResponderAnnounce",
        destination_hash=RESPONDER_DESTINATION,
        receipt_kind="none",
    )
    verifier.exact(
        "responder announce semantic ingress",
        "e290-semantic-hil stage=announce-ingress status=SEMANTIC_VALIDATED "
        f"peer_destination={RESPONDER_DESTINATION} route_learned=true",
    )
    _state(verifier, "Receive(ResponderAnnounce)", "InitiatorSendData")
    verifier.cad("EncryptedData")
    data = verifier.packet(
        direction="tx",
        step="EncryptedData",
        destination_hash=None,
        receipt_kind="packet",
    )
    _state(verifier, "Transmit(EncryptedData)", "InitiatorAwaitProof")
    _rx_armed(verifier, "initiator", "DeliveryProof")
    proof = verifier.packet(
        direction="rx",
        step="DeliveryProof",
        destination_hash=None,
        receipt_kind="required",
    )
    if proof.data_receipt != data.packet_hash:
        raise VerificationError("initiator proof covers the wrong DATA receipt")
    verifier.exact(
        "proof receipt delivery ingress",
        "e290-semantic-hil stage=proof-ingress status=SEMANTIC_VALIDATED "
        f"receipt={data.packet_hash} terminal=Delivered receipt_slots_used=0 "
        f"proof_packet_hash={proof.packet_hash}",
    )
    _state(verifier, "Receive(DeliveryProof)", "Complete")
    _terminal(
        verifier,
        role="initiator",
        local_destination=INITIATOR_DESTINATION,
        peer_destination=RESPONDER_DESTINATION,
        receipt=data.packet_hash,
    )
    verifier.finish()
    return BoardEvidence(
        role="initiator",
        base_mac=INITIATOR_BASE_MAC,
        local_destination=INITIATOR_DESTINATION,
        peer_destination=RESPONDER_DESTINATION,
        packets=tuple(verifier.packets),
        cad_timestamps_us=(verifier.cad_timestamps[0], verifier.cad_timestamps[1]),
        data_receipt=data.packet_hash,
    )


def _verify_responder(segment: bytes) -> BoardEvidence:
    verifier = _BoardVerifier(segment, "responder")
    _common_prefix(
        verifier,
        base_mac=RESPONDER_BASE_MAC,
        role="responder",
        phase="ResponderAwaitInitiatorAnnounce",
        local_destination=RESPONDER_DESTINATION,
        peer_destination=INITIATOR_DESTINATION,
    )
    _rx_armed(verifier, "responder", "InitiatorAnnounce")
    verifier.packet(
        direction="rx",
        step="InitiatorAnnounce",
        destination_hash=INITIATOR_DESTINATION,
        receipt_kind="none",
    )
    verifier.exact(
        "initiator announce semantic ingress",
        "e290-semantic-hil stage=announce-ingress status=SEMANTIC_VALIDATED "
        f"peer_destination={INITIATOR_DESTINATION} route_learned=true",
    )
    _state(verifier, "Receive(InitiatorAnnounce)", "ResponderSendAnnounce")
    verifier.cad("ResponderAnnounce")
    verifier.packet(
        direction="tx",
        step="ResponderAnnounce",
        destination_hash=RESPONDER_DESTINATION,
        receipt_kind="none",
    )
    _state(verifier, "Transmit(ResponderAnnounce)", "ResponderAwaitData")
    _rx_armed(verifier, "responder", "EncryptedData")
    data = verifier.packet(
        direction="rx",
        step="EncryptedData",
        destination_hash=None,
        receipt_kind="packet",
    )
    verifier.exact(
        "exact DATA payload and proof action ingress",
        "e290-semantic-hil stage=data-ingress status=SEMANTIC_VALIDATED "
        "role=responder payload_len=36 "
        f"destination={RESPONDER_DESTINATION} data_receipt={data.packet_hash} "
        "proof_actions=1 extra_actions=0",
    )
    _state(verifier, "Receive(EncryptedData)", "ResponderSendProof")
    verifier.cad("DeliveryProof")
    proof = verifier.packet(
        direction="tx",
        step="DeliveryProof",
        destination_hash=None,
        receipt_kind="required",
    )
    if proof.data_receipt != data.packet_hash:
        raise VerificationError("responder proof covers the wrong DATA receipt")
    _state(verifier, "Transmit(DeliveryProof)", "Complete")
    _terminal(
        verifier,
        role="responder",
        local_destination=RESPONDER_DESTINATION,
        peer_destination=INITIATOR_DESTINATION,
        receipt=data.packet_hash,
    )
    verifier.finish()
    return BoardEvidence(
        role="responder",
        base_mac=RESPONDER_BASE_MAC,
        local_destination=RESPONDER_DESTINATION,
        peer_destination=INITIATOR_DESTINATION,
        packets=tuple(verifier.packets),
        cad_timestamps_us=(verifier.cad_timestamps[0], verifier.cad_timestamps[1]),
        data_receipt=data.packet_hash,
    )


def _packet(board: BoardEvidence, step: str) -> PacketEvidence:
    matches = [packet for packet in board.packets if packet.step == step]
    if len(matches) != 1:
        raise AssertionError(f"verified {board.role} trace has wrong {step} count")
    return matches[0]


def _cross_bind(
    label: str, transmitted: PacketEvidence, received: PacketEvidence
) -> None:
    if transmitted.direction != "tx" or received.direction != "rx":
        raise AssertionError("cross-binding called with wrong packet directions")
    fields = (
        "step",
        "sequence",
        "rns_len",
        "physical_len",
        "packet_hash",
        "destination_hash",
        "data_receipt",
    )
    differences = [
        field
        for field in fields
        if getattr(transmitted, field) != getattr(received, field)
    ]
    if differences:
        raise VerificationError(
            f"{label} TX/RX observations differ in {', '.join(differences)}"
        )


def verify_segments(
    initiator_segment: bytes, responder_segment: bytes
) -> VerificationResult:
    """Verify two already selected serial segments or raise VerificationError."""

    initiator = _verify_initiator(initiator_segment)
    responder = _verify_responder(responder_segment)

    initiator_announce = _packet(initiator, "InitiatorAnnounce")
    responder_rx_initiator_announce = _packet(responder, "InitiatorAnnounce")
    responder_announce = _packet(responder, "ResponderAnnounce")
    initiator_rx_responder_announce = _packet(initiator, "ResponderAnnounce")
    data = _packet(initiator, "EncryptedData")
    responder_rx_data = _packet(responder, "EncryptedData")
    proof = _packet(responder, "DeliveryProof")
    initiator_rx_proof = _packet(initiator, "DeliveryProof")

    _cross_bind(
        "initiator announce", initiator_announce, responder_rx_initiator_announce
    )
    _cross_bind(
        "responder announce", responder_announce, initiator_rx_responder_announce
    )
    _cross_bind("encrypted DATA", data, responder_rx_data)
    _cross_bind("delivery proof", proof, initiator_rx_proof)

    if (
        initiator.data_receipt != responder.data_receipt
        or data.packet_hash != proof.data_receipt
    ):
        raise VerificationError("cross-board DATA receipt correlation failed")

    hashes = {
        initiator_announce.packet_hash,
        responder_announce.packet_hash,
        data.packet_hash,
        proof.packet_hash,
    }
    if len(hashes) != 4 or "00" * 32 in hashes:
        raise VerificationError(
            "the four semantic packet hashes are not distinct and nonzero"
        )

    return VerificationResult(
        schema=SCHEMA,
        status="PASS",
        initiator=initiator,
        responder=responder,
        packet_hashes={
            "initiator_announce": initiator_announce.packet_hash,
            "responder_announce": responder_announce.packet_hash,
            "encrypted_data": data.packet_hash,
            "delivery_proof": proof.packet_hash,
        },
        data_receipt=data.packet_hash,
    )


def _select_segment(payload: bytes, byte_offset: int, label: str) -> bytes:
    if byte_offset < 0:
        raise VerificationError(f"{label} byte offset must not be negative")
    if byte_offset > len(payload):
        raise VerificationError(
            f"{label} byte offset {byte_offset} exceeds capture length {len(payload)}"
        )
    return payload[byte_offset:]


def verify_captures(
    initiator_payload: bytes,
    responder_payload: bytes,
    initiator_byte_offset: int = 0,
    responder_byte_offset: int = 0,
) -> VerificationResult:
    """Trim two captures at independent byte offsets, then verify them."""

    return verify_segments(
        _select_segment(initiator_payload, initiator_byte_offset, "initiator"),
        _select_segment(responder_payload, responder_byte_offset, "responder"),
    )


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
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("initiator_log", type=Path, help="E290 A initiator capture")
    parser.add_argument("responder_log", type=Path, help="E290 B responder capture")
    parser.add_argument(
        "--initiator-byte-offset",
        type=_byte_offset,
        default=0,
        help="start byte in the initiator capture (default: already trimmed)",
    )
    parser.add_argument(
        "--responder-byte-offset",
        type=_byte_offset,
        default=0,
        help="start byte in the responder capture (default: already trimmed)",
    )
    return parser.parse_args(arguments)


def _capture_report(path: Path, payload: bytes, offset: int) -> dict[str, object]:
    segment = payload[offset:]
    return {
        "path": str(path),
        "byte_offset": offset,
        "capture_bytes": len(payload),
        "capture_sha256": hashlib.sha256(payload).hexdigest(),
        "segment_bytes": len(segment),
        "segment_sha256": hashlib.sha256(segment).hexdigest(),
    }


def main(arguments: Sequence[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if arguments is None else arguments)
    try:
        initiator_payload = args.initiator_log.read_bytes()
        responder_payload = args.responder_log.read_bytes()
        result = verify_captures(
            initiator_payload,
            responder_payload,
            args.initiator_byte_offset,
            args.responder_byte_offset,
        )
    except (OSError, VerificationError) as error:
        print(f"e290-semantic-hil-logs status=FAIL reason={error}", file=sys.stderr)
        return 1

    report = {
        "schema": result.schema,
        "status": result.status,
        "captures": {
            "initiator": _capture_report(
                args.initiator_log, initiator_payload, args.initiator_byte_offset
            ),
            "responder": _capture_report(
                args.responder_log, responder_payload, args.responder_byte_offset
            ),
        },
        "boards": {
            "initiator": asdict(result.initiator),
            "responder": asdict(result.responder),
        },
        "packet_hashes": result.packet_hashes,
        "data_receipt": result.data_receipt,
    }
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
