#!/usr/bin/env python3
"""Fail-closed verifier for one counted two-board semantic round-trip HIL.

The E9 initiator and E0 responder captures must each contain exactly one
``semantic-roundtrip-hil`` run.  The verifier checks each role's complete local
state-machine trace, then cross-binds every transmitted packet to the peer's
receive observation.  ANSI coloring, logger prefixes and unrelated ROM output
are ignored; every ``tx-hil`` event is otherwise consumed exactly once and in
order.

Byte offsets identify the start of the independently counted segment in each
raw capture.  Supplying already-trimmed captures is equivalent to using the
default offsets of zero.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import hashlib
import json
from pathlib import Path
import re
import sys
from typing import Sequence


INITIATOR_BASE_MAC = "44:1b:f6:f8:e9:44"
RESPONDER_BASE_MAC = "44:1b:f6:f8:e0:40"
INITIATOR_DESTINATION = "aa2d77f65518c78ad1821ee056976b2a"
RESPONDER_DESTINATION = "fdc1997055c17cf3fbdb192c55ceb3ef"

EXPECTED_PROFILE = (
    "tx-hil stage=profile status=PASS source=tx-bsp-fixed region=NA915 "
    "frequency_hz=915000000 bandwidth_hz=125000 sf=7 "
    "coding_rate_denominator=5 preamble_symbols=24 explicit_header=true "
    "crc=true iq_inverted=false sync_word=private standby_clock=rc "
    "fem_power_policy=prepowered-during-radio-init "
    "fem_ctx_assertion=before-packet-and-fifo-prepare "
    "power_profile=calibrated-minimum target_antenna_path_dbm=14 "
    "sx1262_output_dbm=0 hil_mode=semantic-roundtrip "
    "rns_policy=signed-announce+encrypted-data+delivery-proof"
)
EXPECTED_INERT_HEARTBEAT = (
    "tx-hil stage=inert-heartbeat rf_state=reset_low_fem_low"
)
EXPECTED_ROM_BOOT = "ESP-ROM:esp32s3-20210327"
EXPECTED_COUNTED_RESET = (
    "rst:0x15 (USB_UART_CHIP_RESET),boot:0x8 (SPI_FAST_FLASH_BOOT)"
)
EXPECTED_RUNTIME_SOURCE = "esp-rtos-upstream-b50efcb-stack-words-v1"
EXPECTED_PACKET_LENGTHS = {
    "InitiatorAnnounce": (167, 168),
    "ResponderAnnounce": (167, 168),
    "EncryptedData": (147, 148),
    "DeliveryProof": (115, 116),
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

HEAP_PATTERN = re.compile(
    r"tx-hil stage=semantic-roundtrip-heap "
    r"checkpoint=(?P<checkpoint>[a-z0-9-]+) "
    r"role=(?P<role>initiator|responder) "
    r"heap_size=(?P<size>[0-9]+) heap_used=(?P<used>[0-9]+) "
    r"heap_free=(?P<free>[0-9]+) heap_max_used=(?P<max_used>[0-9]+)"
)
PACKET_PATTERN = re.compile(
    r"tx-hil stage=semantic-roundtrip-packet "
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


class VerificationError(RuntimeError):
    """The selected pair of log segments does not prove the HIL exchange."""


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


@dataclass(frozen=True)
class HeapEvidence:
    """Aggregate of all validated heap checkpoints for one board."""

    checkpoints: int
    heap_size: int
    max_heap_used: int
    min_heap_free: int


@dataclass(frozen=True)
class BoardEvidence:
    """Facts established from one role's complete local serial trace."""

    role: str
    base_mac: str
    local_destination: str
    peer_destination: str
    packets: tuple[PacketEvidence, ...]
    data_receipt: str
    heap: HeapEvidence
    tx_done: int


@dataclass(frozen=True)
class VerificationResult:
    """Cross-board facts established by a successful verification."""

    schema: int
    status: str
    initiator: BoardEvidence
    responder: BoardEvidence
    packet_hashes: dict[str, str]
    data_receipt: str


def _normalized_serial_text(segment: bytes) -> str:
    text = segment.decode("utf-8", errors="replace")
    return ANSI_OSC.sub("", ANSI_CSI.sub("", text))


def extract_tx_hil_events(segment: bytes) -> list[str]:
    """Extract normalized project events without weakening their contents."""

    events: list[str] = []
    for line in _normalized_serial_text(segment).splitlines():
        marker = line.find("tx-hil ")
        if marker >= 0:
            events.append(line[marker:].strip())
    return events


class _BoardVerifier:
    def __init__(self, segment: bytes, role: str) -> None:
        self.role = role
        self.events = extract_tx_hil_events(segment)
        self.index = 0
        self.heap_samples: list[tuple[int, int, int, int]] = []
        self.packets: list[PacketEvidence] = []

        text = _normalized_serial_text(segment)
        lines = [line.strip() for line in text.splitlines()]
        boot_indices = [
            index for index, line in enumerate(lines) if line == EXPECTED_ROM_BOOT
        ]
        reset_indices = [
            index for index, line in enumerate(lines) if line == EXPECTED_COUNTED_RESET
        ]
        first_event_index = next(
            (index for index, line in enumerate(lines) if "tx-hil " in line),
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
            raise VerificationError(f"{role} segment contains no tx-hil events")
        failures = [event for event in self.events if " status=FAIL" in event]
        if failures:
            raise VerificationError(
                f"{role} firmware reported failure: {failures[0]}"
            )

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

    def heap(self, checkpoint: str) -> None:
        matched = self.match(f"heap checkpoint {checkpoint}", HEAP_PATTERN)
        if matched.group("checkpoint") != checkpoint or matched.group("role") != self.role:
            raise VerificationError(
                f"{self.role} event {self.index} is the wrong heap checkpoint or role"
            )
        size = int(matched.group("size"))
        used = int(matched.group("used"))
        free = int(matched.group("free"))
        max_used = int(matched.group("max_used"))
        if (
            size <= 0
            or used < 0
            or used > size
            or free != size - used
            or max_used < used
            or max_used > size
        ):
            raise VerificationError(
                f"{self.role} heap checkpoint {checkpoint} is inconsistent: "
                f"size={size} used={used} free={free} max_used={max_used}"
            )
        if self.heap_samples:
            prior_size, _, _, prior_max = self.heap_samples[-1]
            if size != prior_size:
                raise VerificationError(f"{self.role} heap size changed between checkpoints")
            if max_used < prior_max:
                raise VerificationError(
                    f"{self.role} heap maximum regressed at checkpoint {checkpoint}"
                )
        self.heap_samples.append((size, used, free, max_used))

    def packet(
        self,
        *,
        direction: str,
        step: str,
        sequence: int,
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
            or int(matched.group("sequence")) != sequence
            or matched.group("destination_hash") != expected_destination
        ):
            raise VerificationError(
                f"{self.role} {step} packet has wrong direction, status, sequence, or destination"
            )

        rns_len = int(matched.group("rns_len"))
        physical_len = int(matched.group("physical_len"))
        if rns_len < 20 or physical_len != rns_len + 1 or physical_len > 255:
            raise VerificationError(
                f"{self.role} {step} packet lengths are not one canonical RNode frame"
            )
        if (rns_len, physical_len) != EXPECTED_PACKET_LENGTHS[step]:
            raise VerificationError(
                f"{self.role} {step} packet has wrong fixture length: "
                f"RNS={rns_len} physical={physical_len}"
            )

        signal_present = matched.group("signal_present") == "true"
        rssi = int(matched.group("rssi_dbm"))
        snr = int(matched.group("snr_db"))
        if direction == "tx":
            if signal_present or rssi != 0 or snr != 0:
                raise VerificationError(f"{self.role} TX packet claimed receive signal data")
        elif not signal_present:
            raise VerificationError(f"{self.role} RX packet omitted receive signal data")

        packet_hash = matched.group("packet_hash")
        receipt_text = matched.group("data_receipt")
        receipt = None if receipt_text == "none" else receipt_text
        if receipt_kind == "none" and receipt is not None:
            raise VerificationError(f"{self.role} {step} unexpectedly logged a data receipt")
        if receipt_kind == "packet" and receipt != packet_hash:
            raise VerificationError(
                f"{self.role} DATA packet hash does not equal its current receipt"
            )
        if receipt_kind == "required" and receipt is None:
            raise VerificationError(f"{self.role} proof packet omitted the covered receipt")

        evidence = PacketEvidence(
            direction=direction,
            step=step,
            sequence=sequence,
            rns_len=rns_len,
            physical_len=physical_len,
            packet_hash=packet_hash,
            destination_hash=destination_hash,
            data_receipt=receipt,
        )
        self.packets.append(evidence)
        return evidence

    def finish_heap(self) -> HeapEvidence:
        if not self.heap_samples:
            raise AssertionError("verified trace unexpectedly has no heap samples")
        return HeapEvidence(
            checkpoints=len(self.heap_samples),
            heap_size=self.heap_samples[0][0],
            max_heap_used=max(sample[3] for sample in self.heap_samples),
            min_heap_free=min(sample[2] for sample in self.heap_samples),
        )

    def finish_events(self) -> None:
        while (
            self.index < len(self.events)
            and self.events[self.index] == EXPECTED_INERT_HEARTBEAT
        ):
            self.index += 1
        if self.index != len(self.events):
            raise VerificationError(
                f"{self.role} trace has {len(self.events) - self.index} unexpected "
                f"tx-hil event(s), first: {self.events[self.index]}"
            )


def _common_prefix(
    verifier: _BoardVerifier,
    *,
    base_mac: str,
    local_destination: str,
    peer_destination: str,
    initial_phase: str,
) -> None:
    role = verifier.role
    verifier.exact(
        "exact MAC/role gate",
        "tx-hil stage=mac-gate "
        f"base_mac={base_mac} role={role} exact_match=true "
        "radio_constructed=false spi_constructed=false "
        "rf_state=reset_low_fem_low",
    )
    verifier.exact("semantic-roundtrip NA915 profile", EXPECTED_PROFILE)
    verifier.exact(
        "runtime source identity",
        "tx-hil stage=runtime-source "
        f"esp_rtos_source={EXPECTED_RUNTIME_SOURCE}",
    )
    verifier.exact(
        "radio readiness",
        "tx-hil stage=radio-init status=PASS "
        f"role={role} fem_state=powered_settled_ctx_rx "
        "sx1262_preamble_symbol_timeout=248 "
        "receive_whole_operation_outer_deadline_ms=1500 "
        "transmit_whole_operation_outer_deadline_ms=1500 tx_budget_frames=2",
    )
    verifier.heap("before-node-construction")
    verifier.heap("after-node-construction")
    verifier.exact(
        "semantic round-trip readiness",
        "tx-hil stage=semantic-roundtrip-start status=ARMED "
        f"role={role} phase={initial_phase} "
        f"local_destination={local_destination} peer_destination={peer_destination} "
        "payload_len=36 tx_budget=2 maximum_rx_windows=48",
    )


def _state(verifier: _BoardVerifier, completed: str, next_phase: str) -> None:
    verifier.exact(
        f"state transition {completed}",
        "tx-hil stage=semantic-roundtrip-state status=ADVANCED "
        f"completed={completed} next_phase={next_phase}",
    )


def _rx_armed(verifier: _BoardVerifier, step: str, tx_done: int) -> None:
    verifier.exact(
        f"RX readiness {step}",
        "tx-hil stage=semantic-roundtrip-rx status=ARMED "
        f"role={verifier.role} step={step} maximum_windows=48 tx_done={tx_done}",
    )


def _verify_initiator(segment: bytes) -> BoardEvidence:
    verifier = _BoardVerifier(segment, "initiator")
    _common_prefix(
        verifier,
        base_mac=INITIATOR_BASE_MAC,
        local_destination=INITIATOR_DESTINATION,
        peer_destination=RESPONDER_DESTINATION,
        initial_phase="InitiatorSendAnnounce",
    )
    verifier.exact(
        "responder startup delay",
        "tx-hil stage=semantic-roundtrip-delay role=initiator "
        "purpose=responder-startup delay_driver=blocking-esp-hal "
        "delay_ms=3000 tx_done=0",
    )
    verifier.heap("before-initiator-announce-sign")
    verifier.heap("after-initiator-announce-sign")
    verifier.packet(
        direction="tx",
        step="InitiatorAnnounce",
        sequence=9,
        destination_hash=INITIATOR_DESTINATION,
        receipt_kind="none",
    )
    _state(
        verifier,
        "Transmit(InitiatorAnnounce)",
        "InitiatorAwaitResponderAnnounce",
    )
    _rx_armed(verifier, "ResponderAnnounce", 1)
    verifier.packet(
        direction="rx",
        step="ResponderAnnounce",
        sequence=10,
        destination_hash=RESPONDER_DESTINATION,
        receipt_kind="none",
    )
    verifier.heap("before-announce-validation")
    verifier.heap("after-announce-validation")
    verifier.exact(
        "responder announce semantic ingress",
        "tx-hil stage=semantic-roundtrip-announce-ingress "
        "status=SEMANTIC_VALIDATED role=initiator step=ResponderAnnounce "
        f"peer_destination={RESPONDER_DESTINATION} "
        "route_learned=true extra_actions=0",
    )
    _state(
        verifier,
        "Receive(ResponderAnnounce)",
        "InitiatorSendData",
    )
    verifier.exact(
        "responder receive-rearm delay",
        "tx-hil stage=semantic-roundtrip-delay role=initiator "
        "purpose=responder-rx-rearm delay_driver=blocking-esp-hal "
        "delay_ms=250 tx_done=1",
    )
    verifier.heap("before-data-encrypt")
    verifier.heap("after-data-encrypt")
    data = verifier.packet(
        direction="tx",
        step="EncryptedData",
        sequence=11,
        destination_hash=None,
        receipt_kind="packet",
    )
    _state(verifier, "Transmit(EncryptedData)", "InitiatorAwaitProof")
    _rx_armed(verifier, "DeliveryProof", 2)
    proof = verifier.packet(
        direction="rx",
        step="DeliveryProof",
        sequence=12,
        destination_hash=None,
        receipt_kind="required",
    )
    if proof.data_receipt != data.packet_hash:
        raise VerificationError("initiator proof packet covers the wrong DATA receipt")
    verifier.heap("before-proof-validation")
    verifier.heap("after-proof-validation")
    verifier.exact(
        "proof receipt delivery ingress",
        "tx-hil stage=semantic-roundtrip-proof-ingress "
        "status=SEMANTIC_VALIDATED role=initiator receipt_kind=Data "
        f"candidate={data.packet_hash} terminal=Delivered receipt_slots_used=0 "
        f"extra_actions=0 proof_packet_hash={proof.packet_hash}",
    )
    _state(verifier, "Receive(DeliveryProof)", "Complete")
    verifier.heap("terminal")
    verifier.exact(
        "successful semantic terminal",
        "tx-hil stage=semantic-roundtrip-terminal status=PASS "
        "role=initiator tx_done=2 "
        f"local_destination={INITIATOR_DESTINATION} "
        f"peer_destination={RESPONDER_DESTINATION} "
        f"data_receipt={data.packet_hash} radio_shutdown=next",
    )
    verifier.exact(
        "radio shutdown completion",
        "tx-hil stage=complete role=initiator radio_active=false "
        "action=permanent-rf-inert-hold",
    )
    verifier.finish_events()
    return BoardEvidence(
        role="initiator",
        base_mac=INITIATOR_BASE_MAC,
        local_destination=INITIATOR_DESTINATION,
        peer_destination=RESPONDER_DESTINATION,
        packets=tuple(verifier.packets),
        data_receipt=data.packet_hash,
        heap=verifier.finish_heap(),
        tx_done=2,
    )


def _verify_responder(segment: bytes) -> BoardEvidence:
    verifier = _BoardVerifier(segment, "responder")
    _common_prefix(
        verifier,
        base_mac=RESPONDER_BASE_MAC,
        local_destination=RESPONDER_DESTINATION,
        peer_destination=INITIATOR_DESTINATION,
        initial_phase="ResponderAwaitInitiatorAnnounce",
    )
    _rx_armed(verifier, "InitiatorAnnounce", 0)
    verifier.packet(
        direction="rx",
        step="InitiatorAnnounce",
        sequence=9,
        destination_hash=INITIATOR_DESTINATION,
        receipt_kind="none",
    )
    verifier.heap("before-announce-validation")
    verifier.heap("after-announce-validation")
    verifier.exact(
        "initiator announce semantic ingress",
        "tx-hil stage=semantic-roundtrip-announce-ingress "
        "status=SEMANTIC_VALIDATED role=responder step=InitiatorAnnounce "
        f"peer_destination={INITIATOR_DESTINATION} "
        "route_learned=true extra_actions=0",
    )
    _state(
        verifier,
        "Receive(InitiatorAnnounce)",
        "ResponderSendAnnounce",
    )
    verifier.heap("before-responder-announce-sign")
    verifier.heap("after-responder-announce-sign")
    verifier.packet(
        direction="tx",
        step="ResponderAnnounce",
        sequence=10,
        destination_hash=RESPONDER_DESTINATION,
        receipt_kind="none",
    )
    _state(verifier, "Transmit(ResponderAnnounce)", "ResponderAwaitData")
    _rx_armed(verifier, "EncryptedData", 1)
    data = verifier.packet(
        direction="rx",
        step="EncryptedData",
        sequence=11,
        destination_hash=None,
        receipt_kind="packet",
    )
    verifier.heap("before-data-decrypt-and-proof")
    verifier.heap("after-data-decrypt-and-proof")
    verifier.exact(
        "exact DATA payload and proof action ingress",
        "tx-hil stage=semantic-roundtrip-data-ingress "
        "status=SEMANTIC_VALIDATED role=responder payload_len=36 "
        f"destination={RESPONDER_DESTINATION} data_receipt={data.packet_hash} "
        "proof_actions=1 extra_actions=0",
    )
    _state(verifier, "Receive(EncryptedData)", "ResponderSendProof")
    proof = verifier.packet(
        direction="tx",
        step="DeliveryProof",
        sequence=12,
        destination_hash=None,
        receipt_kind="required",
    )
    if proof.data_receipt != data.packet_hash:
        raise VerificationError("responder proof packet covers the wrong DATA receipt")
    _state(verifier, "Transmit(DeliveryProof)", "Complete")
    verifier.heap("terminal")
    verifier.exact(
        "successful semantic terminal",
        "tx-hil stage=semantic-roundtrip-terminal status=PASS "
        "role=responder tx_done=2 "
        f"local_destination={RESPONDER_DESTINATION} "
        f"peer_destination={INITIATOR_DESTINATION} "
        f"data_receipt={data.packet_hash} radio_shutdown=next",
    )
    verifier.exact(
        "radio shutdown completion",
        "tx-hil stage=complete role=responder radio_active=false "
        "action=permanent-rf-inert-hold",
    )
    verifier.finish_events()
    return BoardEvidence(
        role="responder",
        base_mac=RESPONDER_BASE_MAC,
        local_destination=RESPONDER_DESTINATION,
        peer_destination=INITIATOR_DESTINATION,
        packets=tuple(verifier.packets),
        data_receipt=data.packet_hash,
        heap=verifier.finish_heap(),
        tx_done=2,
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

    if initiator.data_receipt != responder.data_receipt or data.packet_hash != proof.data_receipt:
        raise VerificationError("cross-board DATA receipt correlation failed")

    hashes = {
        initiator_announce.packet_hash,
        responder_announce.packet_hash,
        data.packet_hash,
        proof.packet_hash,
    }
    if len(hashes) != 4 or "00" * 32 in hashes:
        raise VerificationError("the four semantic packet hashes are not distinct and nonzero")

    return VerificationResult(
        schema=1,
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
    parser = argparse.ArgumentParser(
        description="verify paired E9/E0 semantic Reticulum round-trip HIL logs"
    )
    parser.add_argument("initiator_log", type=Path, help="E9 initiator capture")
    parser.add_argument("responder_log", type=Path, help="E0 responder capture")
    parser.add_argument(
        "--initiator-byte-offset",
        "--e9-byte-offset",
        dest="initiator_byte_offset",
        type=_byte_offset,
        default=0,
        help="start byte in the E9 capture (default: already trimmed)",
    )
    parser.add_argument(
        "--responder-byte-offset",
        "--e0-byte-offset",
        dest="responder_byte_offset",
        type=_byte_offset,
        default=0,
        help="start byte in the E0 capture (default: already trimmed)",
    )
    return parser.parse_args(arguments)


def _board_report(board: BoardEvidence) -> dict[str, object]:
    return {
        "base_mac": board.base_mac,
        "role": board.role,
        "destination_hash": board.local_destination,
        "peer_destination_hash": board.peer_destination,
    }


def _capture_report(path: Path, payload: bytes, offset: int) -> dict[str, object]:
    segment = payload[offset:]
    return {
        "log": str(path),
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
        print(f"semantic-roundtrip-hil-logs status=FAIL reason={error}", file=sys.stderr)
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
        "identities": {
            "initiator": _board_report(result.initiator),
            "responder": _board_report(result.responder),
        },
        "packet_hashes": result.packet_hashes,
        "data_receipt": result.data_receipt,
        "heap": {
            "initiator": {
                "checkpoints": result.initiator.heap.checkpoints,
                "heap_size": result.initiator.heap.heap_size,
                "max_heap_used": result.initiator.heap.max_heap_used,
                "min_heap_free": result.initiator.heap.min_heap_free,
            },
            "responder": {
                "checkpoints": result.responder.heap.checkpoints,
                "heap_size": result.responder.heap.heap_size,
                "max_heap_used": result.responder.heap.max_heap_used,
                "min_heap_free": result.responder.heap.min_heap_free,
            },
        },
    }
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
