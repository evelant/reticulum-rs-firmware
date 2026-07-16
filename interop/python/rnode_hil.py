#!/usr/bin/env python3
"""Plan or transmit one committed Phase-1 RNode HIL scenario.

Transmission is deliberately a separate, explicit subcommand with no radio
defaults. It configures and verifies a pinned RNode peer, records every KISS
frame observed during its serial session, and sends exactly one corpus
scenario. An independent RF observer is still required; a serial transcript
cannot prove what appeared on air.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import platform
import sys
import time
from typing import BinaryIO, Callable, Iterable


ROOT = Path(__file__).parents[2]
DEFAULT_CORPUS = ROOT / "interop" / "vectors" / "rnode-hil-v1.json"
EXPECTED_PYTHON = (3, 13, 7)
EXPECTED_PYSERIAL = "3.5"
TRANSMIT_ACK = "I_ACCEPT_RF_TRANSMISSION"
FRESH_PEER_RESET_ACK = "I_RESET_THE_PEER_FOR_THIS_SCENARIO"
FRESH_TRACKER_BOOT_ACK = "I_STARTED_A_FRESH_TRACKER_BOOT_FOR_THIS_SCENARIO"

ALLOWED_MODES = {"rnode_packet", "raw_lora_frame"}
MAX_SCENARIO_STEPS = 16
# The smallest queue in the pinned RNode 1.86 board set is 5,120 bytes, and its
# closing-FEND path commits a packet only while queued_bytes remains strictly
# below that size. Leave one byte unused so READY's weaker "not full" report
# cannot hide an uncommitted final packet under the fresh-reset contract.
MAX_SCENARIO_PAYLOAD_BYTES = 5_119
MAX_FIXED_WAIT_MS = 300_000
MIN_FRAGMENT_WAIT_MARGIN_MS = 1_000
MAX_FRAGMENT_WAIT_MARGIN_MS = 60_000
MAX_TOTAL_WAIT_SECONDS = 3_600
FRAGMENT_TIMEOUT_GUARD_US = 5_000_000
MIN_LISTEN_DURATION_MS = 1
MAX_LISTEN_DURATION_MS = 300_000
RNS_MINIMUM_PACKET_LEN = 19

EXPECTED_PEER = {
    "package": "RNode_Firmware",
    "version": "1.86",
    "repository": "https://github.com/markqvist/RNode_Firmware.git",
    "revision": "9b39b6ce5962007fafefc22034082f354eff3374",
    "required_capability": "CMD_PROMISC 0x0e raw-frame transmit",
}
RETURNED_FAULT_ARTIFACT_MODE = (
    "lab-rx-returned-fault-hil;"
    "trigger=get-irq-status-after-set-rx;"
    "policy=one-boot"
)
RETURNED_FAULT_REPEAT_ARTIFACT_MODE = (
    "lab-rx-returned-fault-hil;"
    "trigger=get-irq-status-after-set-rx;"
    "policy=repeat-until-quarantine"
)
RNODE_PEER_ISOLATION_ARTIFACT_MODE = "rnode-peer-isolation"
TARGET_ARTIFACT_MODES = {
    "lab-rx",
    "lab-rx-backpressure-hil",
    RNODE_PEER_ISOLATION_ARTIFACT_MODE,
    RETURNED_FAULT_ARTIFACT_MODE,
    RETURNED_FAULT_REPEAT_ARTIFACT_MODE,
}
BACKPRESSURE_TARGET_EXPECTATIONS = {
    "kind": "backpressure",
    "artifact_mode": "lab-rx-backpressure-hil",
    "trigger": "first_awaiting_continuation",
    "peer_frames_observed": 4,
    "offered_during_stall": 3,
    "queued_during_stall": 2,
    "dropped_during_stall": 1,
    "pending_expired": 1,
    "queued_frames_rejected_by_expiry_watermark": 2,
    "completed_packets": 0,
    "rete_ingress_calls": 0,
    "tracker_transmissions": 0,
}
BACKPRESSURE_SCENARIO_SHA256 = (
    "618ecc03fc6fd6891e2033c920c4cb4d96087a8791d069898f957cd835ca2632"
)
RETURNED_FAULT_TARGET_EXPECTATIONS = {
    "kind": "returned_fault",
    "artifact_mode": RETURNED_FAULT_ARTIFACT_MODE,
    "trigger": "get-irq-status-after-set-rx",
    "policy": "one-boot",
    "peer_frames_observed": 1,
    "set_rx_forwarded": True,
    "get_irq_status_rejected_before_spi": True,
    "evidence_fired": True,
    "fault_phase": "receive",
    "fault_operation": "receive",
    "fault_primary": "radio_spi",
    "fault_cleanup": "none",
    "retained_fault_committed_before_reset": True,
    "core_software_reset": True,
    "post_reset_radio_constructed": False,
    "post_reset_spi_constructed": False,
    "post_reset_supervisor_watchdog_enabled": False,
    "completed_packets": 0,
    "rete_ingress_calls": 0,
    "tracker_transmissions": 0,
}
RETURNED_FAULT_SCENARIO_SHA256 = (
    "3a60b1d6ca5d07938017147d334aab731aa87becbd35638ef7c8199876344bf6"
)
RETURNED_FAULT_REPEAT_TARGET_EXPECTATIONS = {
    "kind": "returned_fault_repeat_until_quarantine",
    "artifact_mode": RETURNED_FAULT_REPEAT_ARTIFACT_MODE,
    "trigger": "get-irq-status-after-set-rx",
    "policy": "repeat-until-quarantine",
    "same_powered_session": True,
    "invocations_required": 3,
    "peer_frames_per_invocation": 1,
    "peer_frames_observed": 3,
    "set_rx_forwarded_each_fault": True,
    "get_irq_status_rejected_before_spi_each_fault": True,
    "evidence_fired_each_fault": True,
    "fault_phase": "receive",
    "fault_operation": "receive",
    "fault_primary": "radio_spi",
    "fault_cleanup": "none",
    "retained_fault_committed_before_each_reset": True,
    "initial_boot": {
        "reset_reason": "chip_power_on",
        "retained_streak": 0,
        "retained_total": 0,
        "retained_pending": False,
        "next_action": "arm_radio",
        "radio_activation_follows": True,
    },
    "post_fault_boots": [
        {
            "after_invocation": 1,
            "reset_reason": "core_software",
            "retained_streak": 1,
            "retained_total": 1,
            "retained_pending": False,
            "pending_fault_acknowledged_without_increment": True,
            "next_action": "rearm_radio",
            "radio_activation_follows": True,
            "quarantine_reason": None,
        },
        {
            "after_invocation": 2,
            "reset_reason": "core_software",
            "retained_streak": 2,
            "retained_total": 2,
            "retained_pending": False,
            "pending_fault_acknowledged_without_increment": True,
            "next_action": "rearm_radio",
            "radio_activation_follows": True,
            "quarantine_reason": None,
        },
        {
            "after_invocation": 3,
            "reset_reason": "core_software",
            "retained_streak": 3,
            "retained_total": 3,
            "retained_pending": False,
            "pending_fault_acknowledged_without_increment": True,
            "next_action": "quarantine",
            "radio_activation_follows": False,
            "quarantine_reason": "fault_streak",
        },
    ],
    "core_software_resets": 3,
    "radio_activations": 3,
    "quarantine_before_fourth_radio_activation": True,
    "quarantine_radio_constructed": False,
    "quarantine_spi_constructed": False,
    "quarantine_supervisor_watchdog_enabled": False,
    "completed_packets": 0,
    "rete_ingress_calls": 0,
    "tracker_transmissions": 0,
}
RETURNED_FAULT_REPEAT_SCENARIO_SHA256 = (
    "def8a452ccd1cdc1e542b069194b6f327d97cabb5ff4c66a58789ffb4b659d08"
)

FEND = 0xC0
FESC = 0xDB
TFEND = 0xDC
TFESC = 0xDD

CMD_DATA = 0x00
CMD_FREQUENCY = 0x01
CMD_BANDWIDTH = 0x02
CMD_TXPOWER = 0x03
CMD_SF = 0x04
CMD_CR = 0x05
CMD_RADIO_STATE = 0x06
CMD_DETECT = 0x08
CMD_IMPLICIT = 0x09
CMD_ST_ALOCK = 0x0B
CMD_LT_ALOCK = 0x0C
CMD_PROMISC = 0x0E
CMD_READY = 0x0F
CMD_STAT_RSSI = 0x23
CMD_STAT_SNR = 0x24
CMD_STAT_CHTM = 0x25
CMD_STAT_PHYPRM = 0x26
CMD_STAT_IRQ = 0x2A
CMD_BOARD = 0x47
CMD_PLATFORM = 0x48
CMD_MCU = 0x49
CMD_FW_VERSION = 0x50
CMD_ERROR = 0x90

DETECT_REQUEST = 0x73
DETECT_RESPONSE = 0x46
RADIO_STATE_OFF = 0x00
RADIO_STATE_ON = 0x01
CHANNEL_STATS_PAYLOAD_LEN = 11
CHANNEL_STATS_MAX_BASIS_POINTS = 10_000
CHANNEL_STATS_RSSI_OFFSET = 157
CHANNEL_STATS_TIMEOUT_SECONDS = 3.0
DRIVER_TX_EVIDENCE_TIMEOUT_SECONDS = 3.0
SX1262_IRQ_DIAGNOSTICS_SCHEMA = 1
SX1262_IRQ_DIAGNOSTICS_PAYLOAD_LEN = 5
SX1262_IRQ_FLAGS = (
    ("RxDone", 0x0002),
    ("PreambleDetected", 0x0004),
    ("SyncWordValid", 0x0008),
    ("HeaderValid", 0x0010),
    ("HeaderError", 0x0020),
    ("CrcError", 0x0040),
)


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(64 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def kiss_escape(payload: bytes) -> bytes:
    escaped = bytearray()
    for byte in payload:
        if byte == FEND:
            escaped.extend((FESC, TFEND))
        elif byte == FESC:
            escaped.extend((FESC, TFESC))
        else:
            escaped.append(byte)
    return bytes(escaped)


def kiss_frame(command: int, payload: bytes = b"") -> bytes:
    if not 0 <= command <= 0xFF:
        raise ValueError("KISS command must fit in one byte")
    return bytes([FEND, command]) + kiss_escape(payload) + bytes([FEND])


@dataclass(frozen=True)
class DecodedFrame:
    command: int
    payload: bytes
    wire: bytes


def decode_channel_stats(payload: bytes) -> dict[str, int | None]:
    """Decode pinned single-interface RNode CMD_STAT_CHTM telemetry.

    The first four values are basis-point gauges, not cumulative counters.
    Reject impossible values so callers never mistake corruption or a u16
    rollover for an airtime increase.
    """
    if len(payload) != CHANNEL_STATS_PAYLOAD_LEN:
        raise ValueError(
            "RNode channel telemetry must contain exactly "
            f"{CHANNEL_STATS_PAYLOAD_LEN} bytes, found {len(payload)}"
        )

    names = (
        "airtime_short_basis_points",
        "airtime_long_basis_points",
        "channel_load_short_basis_points",
        "channel_load_long_basis_points",
    )
    values = [
        int.from_bytes(payload[index : index + 2], "big")
        for index in range(0, 8, 2)
    ]
    stats: dict[str, int | None] = dict(zip(names, values, strict=True))
    for name, value in zip(names, values, strict=True):
        if value > CHANNEL_STATS_MAX_BASIS_POINTS:
            raise ValueError(
                f"RNode channel telemetry {name} is {value}, outside the "
                f"0..{CHANNEL_STATS_MAX_BASIS_POINTS} gauge domain"
            )

    stats["current_rssi_dbm"] = payload[8] - CHANNEL_STATS_RSSI_OFFSET
    stats["noise_floor_dbm"] = payload[9] - CHANNEL_STATS_RSSI_OFFSET
    stats["interference_dbm"] = (
        None
        if payload[10] == 0xFF
        else payload[10] - CHANNEL_STATS_RSSI_OFFSET
    )
    return stats


def require_fresh_airtime_baseline(observation: dict[str, object]) -> None:
    """Require the zero gauge promised by the fresh-peer-reset contract."""
    short = observation["airtime_short_basis_points"]
    long = observation["airtime_long_basis_points"]
    if short != 0 or long != 0:
        raise ValueError(
            "fresh RNode airtime baseline is not zero "
            f"(short={short}, long={long}); refusing an ambiguous TX inference"
        )


def driver_tx_evidence(
    baseline: dict[str, object],
    observations: list[dict[str, object]],
) -> dict[str, object]:
    """Summarize peer-driver TX evidence without making an RF claim.

    A zero fresh-reset baseline removes short-term decay ambiguity. The maximum
    across every retained post-enqueue sample preserves an increase even if a
    later sample decays. Values are validated gauges, so deltas are ordinary
    subtraction and never modular u16 arithmetic.
    """
    require_fresh_airtime_baseline(baseline)
    short_values = [
        int(observation["airtime_short_basis_points"])
        for observation in observations
    ]
    long_values = [
        int(observation["airtime_long_basis_points"])
        for observation in observations
    ]
    baseline_short = int(baseline["airtime_short_basis_points"])
    baseline_long = int(baseline["airtime_long_basis_points"])
    maximum_short = max(short_values, default=baseline_short)
    maximum_long = max(long_values, default=baseline_long)
    increase_observed = (
        maximum_short > baseline_short or maximum_long > baseline_long
    )
    return {
        "source": "RNode CMD_STAT_CHTM 0x25",
        "claim_scope": (
            "peer firmware returned from its LoRa endPacket path and accounted "
            "airtime with add_airtime; this is not independent RF observation"
        ),
        "wire_values_are_basis_point_gauges_not_counters": True,
        "u16_wrap_arithmetic_used": False,
        "fresh_zero_baseline_required": True,
        "baseline": baseline,
        "post_enqueue_observations": observations,
        "post_enqueue_observation_count": len(observations),
        "maximum_post_enqueue_airtime_short_basis_points": maximum_short,
        "maximum_post_enqueue_airtime_long_basis_points": maximum_long,
        "airtime_short_increase_basis_points": maximum_short - baseline_short,
        "airtime_long_increase_basis_points": maximum_long - baseline_long,
        "peer_driver_tx_observed": increase_observed,
        "independent_rf_observer_required": True,
        "rf_verified": False,
    }


def decode_packet_rssi(payload: bytes) -> int:
    if len(payload) != 1:
        raise ValueError(
            f"RNode packet RSSI telemetry must contain exactly one byte, found {len(payload)}"
        )
    return payload[0] - CHANNEL_STATS_RSSI_OFFSET


def decode_packet_snr(payload: bytes) -> float:
    if len(payload) != 1:
        raise ValueError(
            f"RNode packet SNR telemetry must contain exactly one byte, found {len(payload)}"
        )
    return int.from_bytes(payload, "big", signed=True) * 0.25


def decode_sx1262_irq_diagnostics(payload: bytes) -> dict[str, object]:
    """Decode the compile-gated RNode SX1262 IRQ diagnostic response."""
    if len(payload) != SX1262_IRQ_DIAGNOSTICS_PAYLOAD_LEN:
        raise ValueError(
            "RNode SX1262 IRQ diagnostics must contain exactly "
            f"{SX1262_IRQ_DIAGNOSTICS_PAYLOAD_LEN} bytes, found {len(payload)}"
        )
    schema = payload[0]
    if schema != SX1262_IRQ_DIAGNOSTICS_SCHEMA:
        raise ValueError(
            "unsupported RNode SX1262 IRQ diagnostics schema "
            f"{schema}, expected {SX1262_IRQ_DIAGNOSTICS_SCHEMA}"
        )

    dcd_irq_mask = int.from_bytes(payload[1:3], "big")
    dio1_irq_mask = int.from_bytes(payload[3:5], "big")

    def named_flags(mask: int) -> dict[str, bool]:
        return {name: bool(mask & bit) for name, bit in SX1262_IRQ_FLAGS}

    return {
        "schema": schema,
        "payload_hex": payload.hex(),
        "dcd_irq_mask": dcd_irq_mask,
        "dcd_irq_mask_hex": f"0x{dcd_irq_mask:04x}",
        "dio1_irq_mask": dio1_irq_mask,
        "dio1_irq_mask_hex": f"0x{dio1_irq_mask:04x}",
        "flags": {
            "dcd": named_flags(dcd_irq_mask),
            "dio1": named_flags(dio1_irq_mask),
        },
    }


def receive_evidence(
    expected_payload: bytes,
    observations: list[dict[str, object]],
) -> dict[str, object]:
    exact_matches = [
        observation
        for observation in observations
        if observation["payload_hex"] == expected_payload.hex()
    ]
    mismatches = [
        observation
        for observation in observations
        if observation["payload_hex"] != expected_payload.hex()
    ]
    unambiguous = len(observations) == 1 and len(exact_matches) == 1
    if not observations:
        result = "no_cmd_data_received"
    elif not exact_matches:
        result = "expected_payload_not_received"
    elif len(observations) != 1:
        result = "ambiguous_extra_cmd_data"
    else:
        result = "exact_payload_received_once"
    return {
        "source": "RNode peer-to-host CMD_DATA 0x00",
        "claim_scope": (
            "the RNode driver delivered these bytes over its serial KISS link; "
            "this does not establish Reticulum semantic validity"
        ),
        "expected_payload_hex": expected_payload.hex(),
        "expected_payload_len": len(expected_payload),
        "expected_payload_sha256": hashlib.sha256(expected_payload).hexdigest(),
        "cmd_data_observations": observations,
        "cmd_data_observation_count": len(observations),
        "exact_match_count": len(exact_matches),
        "mismatch_count": len(mismatches),
        "unambiguous_exact_payload_received": unambiguous,
        "rns_semantic_validity_checked": False,
        "result": result,
    }


def rns_semantic_evidence_placeholder(requested: bool) -> dict[str, object]:
    """Return the manifest state before optional RNS validation runs."""
    return {
        "requested": requested,
        "evaluated": False,
        "valid": None,
        "result": "pending" if requested else "not_requested",
        "source": "pinned Python Reticulum RNS.Packet and RNS.Identity",
        "claim_scope": (
            "syntax, first-hop packet policy, announce signature, public identity, "
            "and destination/name-hash binding; not live transport path admission"
        ),
        "peer": None,
        "received_payload_len": None,
        "received_payload_sha256": None,
        "packet": None,
        "announce": None,
        "failure": None,
    }


def load_pinned_rns_validation_peer() -> tuple[object, dict[str, object]]:
    """Load and provenance-check the released Python RNS validation peer.

    Importing is intentionally lazy: ordinary RNode HIL operations must not
    acquire an undeclared Python-RNS dependency. The shared vector generator
    owns the released-peer manifest and the PEP 610 source-revision checks, so
    this observer reuses those checks instead of maintaining a second pin.
    """
    try:
        import generate_rns_vectors
        import RNS
    except ImportError as error:
        raise RuntimeError(
            "--validate-rns-announce requires the exact environment from "
            "interop/python/requirements-rns-1.3.8.txt"
        ) from error

    peer = generate_rns_vectors.released_peer()
    installed_version = generate_rns_vectors.verify_installed_peer(peer)
    imported_module = getattr(RNS, "__file__", None)
    if imported_module is None:
        raise RuntimeError("the pinned RNS module has no filesystem origin")
    requirements = generate_rns_vectors.REQUIREMENTS.resolve()
    return RNS, {
        "package": "rns",
        "version": installed_version,
        "repository": peer["repository"],
        "revision": peer["revision"],
        "imported_module": str(Path(imported_module).resolve()),
        "requirements": str(requirements),
        "requirements_sha256": sha256_file(requirements),
        "full_reticulum_instance_started": False,
    }


def validate_rns_announce(
    raw: bytes,
    rns: object,
    peer: dict[str, object],
) -> dict[str, object]:
    """Validate one deframed first-hop announce with the pinned Python peer.

    This deliberately does not start a process-global ``RNS.Reticulum``
    instance. Packet parsing and announce validation are sufficient for this
    bounded semantic claim and avoid background interfaces, transport threads,
    and persistent node state.
    """
    evidence = rns_semantic_evidence_placeholder(True)
    evidence.update(
        {
            "evaluated": True,
            "valid": False,
            "result": "rejected",
            "peer": peer,
            "received_payload_len": len(raw),
            "received_payload_sha256": hashlib.sha256(raw).hexdigest(),
        }
    )

    def reject(reason: str, detail: str) -> dict[str, object]:
        evidence["failure"] = {"reason": reason, "detail": detail}
        return evidence

    try:
        maximum = int(rns.Reticulum.MTU)
        if len(raw) < RNS_MINIMUM_PACKET_LEN:
            return reject(
                "packet_too_short",
                f"received {len(raw)} bytes; minimum HEADER_1 packet is "
                f"{RNS_MINIMUM_PACKET_LEN}",
            )
        if len(raw) > maximum:
            return reject(
                "packet_too_long",
                f"received {len(raw)} bytes; base Reticulum MTU is {maximum}",
            )
        if raw[0] & 0x80:
            return reject(
                "unexpected_ifac_flag",
                "the unprotected RNode listener cannot authenticate IFAC bytes",
            )

        packet = rns.Packet(None, raw)
        if not packet.unpack():
            return reject("packet_unpack_failed", "RNS.Packet.unpack returned false")

        decoded = {
            "flags": packet.flags,
            "flags_hex": f"0x{packet.flags:02x}",
            "hops": packet.hops,
            "header_type": packet.header_type,
            "transport_type": packet.transport_type,
            "destination_type": packet.destination_type,
            "packet_type": packet.packet_type,
            "context": packet.context,
            "context_flag": packet.context_flag,
            "destination_hash_hex": packet.destination_hash.hex(),
            "packet_hash_hex": packet.get_hash().hex(),
        }
        evidence["packet"] = decoded

        policies = (
            (
                packet.header_type == rns.Packet.HEADER_1,
                "header_type",
                "semantic HIL requires a first-hop HEADER_1 packet",
            ),
            (
                packet.transport_type == rns.Transport.BROADCAST,
                "transport_type",
                "semantic HIL requires first-hop broadcast transport",
            ),
            (
                packet.destination_type == rns.Destination.SINGLE,
                "destination_type",
                "semantic HIL requires a SINGLE destination announce",
            ),
            (
                packet.packet_type == rns.Packet.ANNOUNCE,
                "packet_type",
                "semantic HIL requires an ANNOUNCE packet",
            ),
            (
                packet.context == rns.Packet.NONE,
                "context",
                "semantic HIL requires ordinary context NONE",
            ),
            (
                packet.context_flag == rns.Packet.FLAG_UNSET,
                "context_flag",
                "semantic HIL fixture must be a non-ratchet announce",
            ),
            (
                packet.hops == 0,
                "hops",
                "semantic HIL requires the transmitter's first-hop value zero",
            ),
        )
        for accepted, reason, detail in policies:
            if not accepted:
                return reject(f"unexpected_{reason}", detail)

        data = packet.data
        key_size = int(rns.Identity.KEYSIZE) // 8
        name_hash_size = int(rns.Identity.NAME_HASH_LENGTH) // 8
        signature_size = int(rns.Identity.SIGLENGTH) // 8
        minimum_announce_data = key_size + name_hash_size + 10 + signature_size
        if len(data) < minimum_announce_data:
            return reject(
                "announce_payload_too_short",
                f"received {len(data)} announce bytes; minimum is "
                f"{minimum_announce_data}",
            )

        public_key = data[:key_size]
        name_hash_start = key_size
        random_hash_start = name_hash_start + name_hash_size
        signature_start = random_hash_start + 10
        name_hash = data[name_hash_start:random_hash_start]
        random_hash = data[random_hash_start:signature_start]
        signature = data[signature_start : signature_start + signature_size]
        app_data = data[signature_start + signature_size :] or None

        if not rns.Identity.validate_announce(packet):
            return reject(
                "announce_cryptographic_validation_failed",
                "RNS.Identity.validate_announce rejected the signature or destination binding",
            )

        identity = rns.Identity.recall(packet.destination_hash, _no_use=True)
        if identity is None:
            return reject(
                "announced_identity_not_recalled",
                "validated announce did not populate the Python-RNS identity cache",
            )
        if identity.get_public_key() != public_key:
            return reject(
                "announced_public_key_mismatch",
                "recalled identity public key differs from the signed announce field",
            )

        expected_destination_hash = rns.Identity.full_hash(
            name_hash + identity.hash
        )[: int(rns.Reticulum.TRUNCATED_HASHLENGTH) // 8]
        if expected_destination_hash != packet.destination_hash:
            return reject(
                "destination_name_hash_binding_mismatch",
                "destination hash is not derived from name hash and identity hash",
            )

        recalled_app_data = rns.Identity.recall_app_data(
            packet.destination_hash,
            _no_use=True,
        )
        if recalled_app_data != app_data:
            return reject(
                "announce_app_data_mismatch",
                "recalled app data differs from the signed announce field",
            )

        evidence["announce"] = {
            "cryptographically_valid": True,
            "destination_name_hash_binding_valid": True,
            "public_key_hex": public_key.hex(),
            "identity_hash_hex": identity.hash.hex(),
            "name_hash_hex": name_hash.hex(),
            "random_hash_hex": random_hash.hex(),
            "signature_hex": signature.hex(),
            "app_data_hex": None if app_data is None else app_data.hex(),
        }
        evidence["valid"] = True
        evidence["result"] = "valid_expected_first_hop_announce"
        return evidence
    except Exception as error:
        return reject(
            "validator_exception",
            f"{type(error).__name__}: {error}",
        )


class KissDecoder:
    def __init__(self) -> None:
        self._in_frame = False
        self._command: int | None = None
        self._payload = bytearray()
        self._wire = bytearray()
        self._escape = False

    def feed(self, data: bytes) -> list[DecodedFrame]:
        frames: list[DecodedFrame] = []
        for byte in data:
            if byte == FEND:
                if self._in_frame and self._escape:
                    raise ValueError("truncated KISS escape before frame terminator")
                if self._in_frame and self._command is not None:
                    self._wire.append(byte)
                    frames.append(
                        DecodedFrame(
                            command=self._command,
                            payload=bytes(self._payload),
                            wire=bytes(self._wire),
                        )
                    )
                self._in_frame = True
                self._command = None
                self._payload.clear()
                self._wire.clear()
                self._wire.append(byte)
                self._escape = False
                continue

            if not self._in_frame:
                continue
            self._wire.append(byte)
            if self._command is None:
                self._command = byte
                continue
            if self._escape:
                if byte == TFEND:
                    self._payload.append(FEND)
                elif byte == TFESC:
                    self._payload.append(FESC)
                else:
                    raise ValueError(f"invalid KISS escape byte 0x{byte:02x}")
                self._escape = False
            elif byte == FESC:
                self._escape = True
            else:
                self._payload.append(byte)
        return frames


class Transcript:
    def __init__(self, sink: BinaryIO) -> None:
        self._sink = sink
        self._sequence = 0

    def record(self, direction: str, frame: DecodedFrame) -> None:
        entry = {
            "sequence": self._sequence,
            "utc": utc_now(),
            "monotonic_ns": time.monotonic_ns(),
            "direction": direction,
            "command": frame.command,
            "payload_hex": frame.payload.hex(),
            "wire_hex": frame.wire.hex(),
        }
        self._sequence += 1
        self._sink.write((json.dumps(entry, sort_keys=True) + "\n").encode("utf-8"))
        self._sink.flush()


class RNodePeer:
    def __init__(self, serial_port: object, transcript: Transcript) -> None:
        self.serial = serial_port
        self.transcript = transcript
        self.decoder = KissDecoder()
        self.latest_frames: dict[int, DecodedFrame] = {}
        self.data_observations: list[dict[str, object]] = []
        self.rssi_observations: list[dict[str, object]] = []
        self.snr_observations: list[dict[str, object]] = []
        self.channel_stats_observations: list[dict[str, object]] = []

    def write(self, command: int, payload: bytes = b"") -> None:
        wire = kiss_frame(command, payload)
        written = self.serial.write(wire)
        if written != len(wire):
            raise IOError(f"serial write accepted {written} of {len(wire)} bytes")
        if hasattr(self.serial, "flush"):
            self.serial.flush()
        self.transcript.record("host_to_peer", DecodedFrame(command, payload, wire))

    def _read_frames(self) -> list[DecodedFrame]:
        waiting = int(getattr(self.serial, "in_waiting", 0))
        chunk = self.serial.read(max(1, waiting))
        frames = self.decoder.feed(chunk)
        first_error: int | None = None
        for frame in frames:
            self.transcript.record("peer_to_host", frame)
            self.latest_frames[frame.command] = frame
            if frame.command == CMD_DATA:
                self.data_observations.append(
                    {
                        "utc": utc_now(),
                        "monotonic_ns": time.monotonic_ns(),
                        "payload_hex": frame.payload.hex(),
                        "payload_len": len(frame.payload),
                        "payload_sha256": hashlib.sha256(frame.payload).hexdigest(),
                    }
                )
            elif frame.command == CMD_STAT_RSSI:
                self.rssi_observations.append(
                    {
                        "utc": utc_now(),
                        "monotonic_ns": time.monotonic_ns(),
                        "payload_hex": frame.payload.hex(),
                        "rssi_dbm": decode_packet_rssi(frame.payload),
                    }
                )
            elif frame.command == CMD_STAT_SNR:
                self.snr_observations.append(
                    {
                        "utc": utc_now(),
                        "monotonic_ns": time.monotonic_ns(),
                        "payload_hex": frame.payload.hex(),
                        "snr_db": decode_packet_snr(frame.payload),
                    }
                )
            elif frame.command == CMD_STAT_CHTM:
                observation: dict[str, object] = {
                    "utc": utc_now(),
                    "monotonic_ns": time.monotonic_ns(),
                    "payload_hex": frame.payload.hex(),
                }
                observation.update(decode_channel_stats(frame.payload))
                self.channel_stats_observations.append(observation)
            if frame.command == CMD_ERROR and first_error is None:
                first_error = frame.payload[0] if frame.payload else -1
        if first_error is not None:
            code: int | None = None if first_error == -1 else first_error
            raise IOError(f"RNode reported error code {code!r}")
        return frames

    def wait_for(
        self,
        predicate: Callable[[DecodedFrame], bool],
        *,
        timeout_seconds: float,
        description: str,
    ) -> DecodedFrame:
        deadline = time.monotonic() + timeout_seconds
        while time.monotonic() < deadline:
            for frame in self._read_frames():
                if predicate(frame):
                    return frame
        raise TimeoutError(f"timed out waiting for {description}")

    def wait_for_observation(
        self,
        command: int,
        *,
        expected_length: int,
        timeout_seconds: float = 3.0,
    ) -> DecodedFrame:
        observed = self.latest_frames.get(command)
        if observed is not None and len(observed.payload) == expected_length:
            return observed
        return self.wait_for(
            lambda frame: frame.command == command
            and len(frame.payload) == expected_length,
            timeout_seconds=timeout_seconds,
            description=f"{expected_length}-byte command 0x{command:02x} observation",
        )

    def exchange(
        self,
        command: int,
        request: bytes,
        expected: bytes,
        *,
        timeout_seconds: float = 3.0,
    ) -> DecodedFrame:
        self.write(command, request)
        return self.wait_for(
            lambda frame: frame.command == command and frame.payload == expected,
            timeout_seconds=timeout_seconds,
            description=f"command 0x{command:02x} response {expected.hex()}",
        )

    def inspect_device(self) -> dict[str, object]:
        self.exchange(CMD_DETECT, bytes([DETECT_REQUEST]), bytes([DETECT_RESPONSE]))
        version = self.query(CMD_FW_VERSION, expected_length=2)
        board = self.query(CMD_BOARD, expected_length=1)
        platform = self.query(CMD_PLATFORM, expected_length=1)
        mcu = self.query(CMD_MCU, expected_length=1)
        return {
            "firmware_version": f"{version[0]}.{version[1]}",
            "firmware_version_bytes_hex": version.hex(),
            "board": board[0],
            "platform": platform[0],
            "mcu": mcu[0],
        }

    def query(self, command: int, *, expected_length: int) -> bytes:
        self.write(command, bytes([0x00]))
        frame = self.wait_for(
            lambda candidate: candidate.command == command
            and len(candidate.payload) == expected_length,
            timeout_seconds=3.0,
            description=f"{expected_length}-byte command 0x{command:02x} response",
        )
        return frame.payload

    def query_sx1262_irq_diagnostics(self) -> dict[str, object]:
        """Atomically snapshot and clear compile-gated peer IRQ accumulators."""
        self.write(CMD_STAT_IRQ, b"\x00")
        frame = self.wait_for(
            lambda candidate: candidate.command == CMD_STAT_IRQ,
            timeout_seconds=3.0,
            description="command 0x2a SX1262 IRQ diagnostics response",
        )
        return decode_sx1262_irq_diagnostics(frame.payload)

    def set_airtime_limit(self, command: int, requested_basis_points: int) -> int:
        self.write(command, requested_basis_points.to_bytes(2, "big"))
        frame = self.wait_for(
            lambda candidate: candidate.command == command
            and len(candidate.payload) == 2,
            timeout_seconds=3.0,
            description=f"two-byte command 0x{command:02x} response",
        )
        effective = int.from_bytes(frame.payload, "big")
        # RNode 1.86 stores this as a 32-bit float. Some requested values
        # round-trip one basis point lower. That is more restrictive and safe,
        # but it must be visible in the evidence rather than silently claimed
        # as the requested value.
        if effective > requested_basis_points or requested_basis_points - effective > 1:
            raise ValueError(
                f"RNode airtime command 0x{command:02x} applied {effective} "
                f"basis points for requested {requested_basis_points}"
            )
        return effective

    def configure(
        self,
        profile: dict[str, int],
        *,
        promiscuous: bool | None = None,
    ) -> dict[str, int]:
        # Saved standalone state can boot with the radio already online. Its
        # setters then echo globals without necessarily applying them to the
        # active modem. Going offline first makes the subsequent ON transition
        # apply this complete profile.
        self.exchange(
            CMD_RADIO_STATE,
            bytes([RADIO_STATE_OFF]),
            bytes([RADIO_STATE_OFF]),
        )
        self.exchange(
            CMD_FREQUENCY,
            profile["frequency_hz"].to_bytes(4, "big"),
            profile["frequency_hz"].to_bytes(4, "big"),
        )
        self.exchange(
            CMD_BANDWIDTH,
            profile["bandwidth_hz"].to_bytes(4, "big"),
            profile["bandwidth_hz"].to_bytes(4, "big"),
        )
        self.exchange(
            CMD_TXPOWER,
            bytes([profile["tx_power_dbm"]]),
            bytes([profile["tx_power_dbm"]]),
        )
        self.exchange(
            CMD_SF,
            bytes([profile["spreading_factor"]]),
            bytes([profile["spreading_factor"]]),
        )
        self.exchange(
            CMD_CR,
            bytes([profile["coding_rate_denominator"]]),
            bytes([profile["coding_rate_denominator"]]),
        )
        # Explicit-header mode is persistent peer state. A stale implicit
        # length can make emitted frames invisible to the Tracker profile.
        self.exchange(CMD_IMPLICIT, b"\x00", b"\x00")
        effective_short = self.set_airtime_limit(
            CMD_ST_ALOCK,
            profile["short_airtime_limit_basis_points"],
        )
        effective_long = self.set_airtime_limit(
            CMD_LT_ALOCK,
            profile["long_airtime_limit_basis_points"],
        )
        if promiscuous is not None:
            self.set_promiscuous(promiscuous)
        self.latest_frames.pop(CMD_STAT_PHYPRM, None)
        self.exchange(CMD_RADIO_STATE, bytes([RADIO_STATE_ON]), bytes([RADIO_STATE_ON]))
        self.wait_until_queue_accepts()
        physical = self.wait_for_observation(CMD_STAT_PHYPRM, expected_length=12)
        values = [
            int.from_bytes(physical.payload[index : index + 2], "big")
            for index in range(0, len(physical.payload), 2)
        ]
        stats = dict(
            zip(
                (
                    "symbol_time_us",
                    "symbol_rate",
                    "preamble_symbols",
                    "preamble_time_ms",
                    "csma_slot_ms",
                    "difs_ms",
                ),
                values,
                strict=True,
            )
        )
        if stats["preamble_symbols"] != profile["expected_peer_preamble_symbols"]:
            raise ValueError(
                "RNode reported preamble "
                f"{stats['preamble_symbols']}, expected "
                f"{profile['expected_peer_preamble_symbols']} from pinned RNode timing"
            )
        # Re-query every setting with a non-mutating sentinel where the pinned
        # protocol provides one. Re-applying zero implicit length and airtime
        # locks is idempotent. Together with the forced OFF -> ON transition,
        # these checks bind the recorded active configuration to the requested
        # globals instead of relying only on the setters' initial echoes.
        self.exchange(
            CMD_FREQUENCY,
            b"\x00" * 4,
            profile["frequency_hz"].to_bytes(4, "big"),
        )
        self.exchange(
            CMD_BANDWIDTH,
            b"\x00" * 4,
            profile["bandwidth_hz"].to_bytes(4, "big"),
        )
        self.exchange(CMD_TXPOWER, b"\xff", bytes([profile["tx_power_dbm"]]))
        self.exchange(CMD_SF, b"\xff", bytes([profile["spreading_factor"]]))
        self.exchange(
            CMD_CR,
            b"\xff",
            bytes([profile["coding_rate_denominator"]]),
        )
        self.exchange(CMD_IMPLICIT, b"\x00", b"\x00")
        if (
            self.set_airtime_limit(
                CMD_ST_ALOCK,
                profile["short_airtime_limit_basis_points"],
            )
            != effective_short
            or self.set_airtime_limit(
                CMD_LT_ALOCK,
                profile["long_airtime_limit_basis_points"],
            )
            != effective_long
        ):
            raise ValueError("RNode airtime limit changed during configuration verification")
        self.exchange(CMD_RADIO_STATE, b"\xff", bytes([RADIO_STATE_ON]))
        stats["effective_short_airtime_limit_basis_points"] = effective_short
        stats["effective_long_airtime_limit_basis_points"] = effective_long
        return stats

    def set_promiscuous(self, enabled: bool) -> None:
        value = bytes([int(enabled)])
        self.exchange(CMD_PROMISC, value, value)

    def wait_until_queue_accepts(self, *, timeout_seconds: float = 10.0) -> None:
        deadline = time.monotonic() + timeout_seconds
        while time.monotonic() < deadline:
            self.write(CMD_READY, bytes([0x00]))
            frame = self.wait_for(
                lambda candidate: candidate.command == CMD_READY
                and candidate.payload in {b"\x00", b"\x01"},
                timeout_seconds=1.0,
                description="RNode queue readiness",
            )
            if frame.payload == b"\x01":
                return
            time.sleep(0.05)
        raise TimeoutError("RNode queue did not accept another packet")

    def observe_for(self, duration_seconds: float) -> None:
        """Drain asynchronous peer reports for a bounded observation window."""
        deadline = time.monotonic() + duration_seconds
        while True:
            self._read_frames()
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return
            # Real serial reads are already bounded by their timeout; this
            # pause also prevents fake/nonblocking ports from spinning.
            if int(getattr(self.serial, "in_waiting", 0)) == 0:
                time.sleep(min(0.01, remaining))

    def wait_for_fresh_channel_stats(
        self,
        *,
        timeout_seconds: float = CHANNEL_STATS_TIMEOUT_SECONDS,
    ) -> dict[str, object]:
        """Wait for a CHTM report received after this method is entered."""
        observation_index = len(self.channel_stats_observations)
        self.wait_for(
            lambda frame: frame.command == CMD_STAT_CHTM
            and len(frame.payload) == CHANNEL_STATS_PAYLOAD_LEN,
            timeout_seconds=timeout_seconds,
            description="fresh 11-byte CMD_STAT_CHTM channel telemetry",
        )
        return self.channel_stats_observations[observation_index]

    def wait_for_driver_tx_evidence(
        self,
        baseline: dict[str, object],
        *,
        observation_index: int,
        timeout_seconds: float = DRIVER_TX_EVIDENCE_TIMEOUT_SECONDS,
    ) -> dict[str, object]:
        """Retain reports until a gauge increase appears or the bound expires."""
        deadline = time.monotonic() + timeout_seconds
        while True:
            evidence = driver_tx_evidence(
                baseline,
                self.channel_stats_observations[observation_index:],
            )
            if evidence["peer_driver_tx_observed"]:
                return evidence
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return evidence
            self._read_frames()
            if int(getattr(self.serial, "in_waiting", 0)) == 0:
                time.sleep(min(0.01, remaining))

    def transmit(self, payload: bytes) -> None:
        if not payload:
            raise ValueError("pinned RNode cannot deterministically enqueue an empty payload")
        self.wait_until_queue_accepts()
        self.write(CMD_DATA, payload)


def parse_corpus(raw: bytes, path: Path) -> dict[str, object]:
    corpus = json.loads(raw)
    if (
        type(corpus.get("schema")) is not int
        or corpus.get("schema") != 3
        or corpus.get("lane") != "phase-1-rx-hil"
    ):
        raise ValueError(f"unsupported HIL corpus schema in {path}")
    return corpus


def load_corpus_snapshot(path: Path) -> tuple[dict[str, object], str]:
    raw = path.read_bytes()
    return parse_corpus(raw, path), hashlib.sha256(raw).hexdigest()


def load_corpus(path: Path) -> dict[str, object]:
    return load_corpus_snapshot(path)[0]


def find_scenario(corpus: dict[str, object], name: str) -> dict[str, object]:
    scenarios = corpus.get("scenarios")
    if not isinstance(scenarios, list):
        raise ValueError("HIL corpus scenarios must be a list")
    matching = [
        scenario
        for scenario in scenarios
        if isinstance(scenario, dict) and scenario.get("name") == name
    ]
    if len(matching) != 1:
        raise ValueError(f"expected exactly one scenario named {name!r}, found {len(matching)}")
    return matching[0]


def scenario_modes(scenario: dict[str, object]) -> set[str]:
    return {step["mode"] for step in scenario["steps"]}


def resolve_wait_seconds(
    wait: dict[str, object],
    *,
    receiver_fragment_timeout_us: int | None,
    receiver_maximum_frame_airtime_us: int | None,
    peer_preamble_extension_us: int | None,
) -> float:
    if not isinstance(wait, dict):
        raise ValueError("scenario wait_after must be an object")
    kind = wait.get("kind")
    if kind == "fixed":
        milliseconds = wait.get("milliseconds")
        if type(milliseconds) is not int or not 0 <= milliseconds <= MAX_FIXED_WAIT_MS:
            raise ValueError(
                f"fixed wait milliseconds must be an integer from 0 through {MAX_FIXED_WAIT_MS}"
            )
        return milliseconds / 1_000
    if kind == "receiver_fragment_timeout":
        margin_ms = wait.get("margin_ms")
        if (
            type(margin_ms) is not int
            or not MIN_FRAGMENT_WAIT_MARGIN_MS
            <= margin_ms
            <= MAX_FRAGMENT_WAIT_MARGIN_MS
        ):
            raise ValueError(
                "receiver fragment wait margin_ms must be an integer from "
                f"{MIN_FRAGMENT_WAIT_MARGIN_MS} through {MAX_FRAGMENT_WAIT_MARGIN_MS}"
            )
        if (
            receiver_fragment_timeout_us is None
            or receiver_maximum_frame_airtime_us is None
            or peer_preamble_extension_us is None
        ):
            raise ValueError(
                "this scenario requires --receiver-fragment-timeout-us and "
                "--receiver-maximum-frame-airtime-us from the Tracker activation log"
            )
        # The receiver starts its timer only after capturing the first frame.
        # This host wait starts earlier, at KISS enqueue, so include one full
        # physical-frame airtime before the receiver's own timeout and margin.
        return (
            receiver_maximum_frame_airtime_us
            + peer_preamble_extension_us
            + receiver_fragment_timeout_us
        ) / 1_000_000 + margin_ms / 1_000
    raise ValueError(f"unsupported wait kind {kind!r}")


def validate_scenario_for_send(
    corpus: dict[str, object],
    scenario: dict[str, object],
    *,
    receiver_fragment_timeout_us: int | None,
    receiver_maximum_frame_airtime_us: int | None,
    peer_preamble_extension_us: int | None,
) -> tuple[bool, list[bytes], list[float]]:
    if corpus.get("peer") != EXPECTED_PEER:
        raise ValueError("HIL send requires the exact pinned RNode peer metadata")
    if not isinstance(scenario.get("name"), str) or not scenario["name"]:
        raise ValueError("scenario name must be a nonempty string")
    if not isinstance(scenario.get("description"), str):
        raise ValueError("scenario description must be a string")
    steps = scenario.get("steps")
    if not isinstance(steps, list) or not 1 <= len(steps) <= MAX_SCENARIO_STEPS:
        raise ValueError(f"scenario must contain 1 through {MAX_SCENARIO_STEPS} steps")

    modes: set[str] = set()
    payloads: list[bytes] = []
    waits: list[float] = []
    cumulative_bytes = 0
    for index, step in enumerate(steps):
        if not isinstance(step, dict):
            raise ValueError(f"scenario step {index} must be an object")
        mode = step.get("mode")
        if mode not in ALLOWED_MODES:
            raise ValueError(f"scenario step {index} has unsupported mode {mode!r}")
        modes.add(mode)
        payload_hex = step.get("payload_hex")
        if not isinstance(payload_hex, str):
            raise ValueError(f"scenario step {index} payload_hex must be a string")
        try:
            payload = bytes.fromhex(payload_hex)
        except ValueError as error:
            raise ValueError(f"scenario step {index} payload_hex is invalid") from error
        if payload.hex() != payload_hex:
            raise ValueError(
                f"scenario step {index} payload_hex must be canonical lowercase hex"
            )
        payload_len = step.get("payload_len")
        if type(payload_len) is not int or payload_len != len(payload):
            raise ValueError(f"scenario step {index} payload_len does not match payload_hex")
        if not payload:
            raise ValueError(f"scenario step {index} payload must not be empty")
        maximum = 255 if mode == "raw_lora_frame" else 508
        if len(payload) > maximum:
            raise ValueError(
                f"scenario step {index} exceeds the {maximum}-byte {mode} limit"
            )
        if step.get("payload_sha256") != hashlib.sha256(payload).hexdigest():
            raise ValueError(
                f"scenario step {index} payload_sha256 does not match payload_hex"
            )
        wait_seconds = resolve_wait_seconds(
            step.get("wait_after"),
            receiver_fragment_timeout_us=receiver_fragment_timeout_us,
            receiver_maximum_frame_airtime_us=receiver_maximum_frame_airtime_us,
            peer_preamble_extension_us=peer_preamble_extension_us,
        )
        payloads.append(payload)
        waits.append(wait_seconds)
        cumulative_bytes += len(payload)

    if len(modes) != 1:
        raise ValueError("one invocation cannot safely switch RNode mode while TX may be queued")
    if cumulative_bytes > MAX_SCENARIO_PAYLOAD_BYTES:
        raise ValueError(
            f"scenario exceeds the {MAX_SCENARIO_PAYLOAD_BYTES}-byte cumulative limit"
        )
    if sum(waits) > MAX_TOTAL_WAIT_SECONDS:
        raise ValueError(
            f"scenario exceeds the {MAX_TOTAL_WAIT_SECONDS}-second cumulative wait limit"
        )
    return modes == {"raw_lora_frame"}, payloads, waits


def resolve_listen_expectation(
    args: argparse.Namespace,
    corpus: dict[str, object],
) -> tuple[bool, bytes, dict[str, object]]:
    """Resolve one exact expected host-delivered payload and receive mode."""
    if corpus.get("peer") != EXPECTED_PEER:
        raise ValueError("HIL listen requires the exact pinned RNode peer metadata")

    expected_scenario = args.expected_scenario
    expected_payload_hex = args.expected_payload_hex
    expected_mode = args.expected_mode
    if expected_scenario is not None:
        if expected_payload_hex is not None:
            raise ValueError("listen expectation must use either scenario or CLI hex")
        if expected_mode is not None:
            raise ValueError("--expected-mode is inferred for --expected-scenario")
        scenario = find_scenario(corpus, expected_scenario)
        steps = scenario.get("steps")
        if not isinstance(steps, list) or len(steps) != 1:
            raise ValueError(
                "listen requires a committed scenario with exactly one step"
            )
        step = steps[0]
        if not isinstance(step, dict):
            raise ValueError("listen scenario step must be an object")
        mode = step.get("mode")
        payload_hex = step.get("payload_hex")
        source: dict[str, object] = {
            "kind": "committed_scenario",
            "scenario": expected_scenario,
            "scenario_description": scenario.get("description"),
        }
    else:
        if expected_payload_hex is None:
            raise ValueError(
                "listen requires --expected-payload-hex or --expected-scenario"
            )
        if expected_mode is None:
            raise ValueError("--expected-mode is required with --expected-payload-hex")
        mode = expected_mode
        payload_hex = expected_payload_hex
        step = None
        source = {"kind": "cli_hex"}

    if mode not in ALLOWED_MODES:
        raise ValueError(f"listen expectation has unsupported mode {mode!r}")
    if not isinstance(payload_hex, str):
        raise ValueError("listen expected payload hex must be a string")
    try:
        payload = bytes.fromhex(payload_hex)
    except ValueError as error:
        raise ValueError("listen expected payload hex is invalid") from error
    if payload.hex() != payload_hex:
        raise ValueError("listen expected payload must be canonical lowercase hex")
    if not payload:
        raise ValueError("listen expected payload must not be empty")
    maximum = 255 if mode == "raw_lora_frame" else 508
    if len(payload) > maximum:
        raise ValueError(
            f"listen expected payload exceeds the {maximum}-byte {mode} limit"
        )
    payload_sha256 = hashlib.sha256(payload).hexdigest()
    if step is not None:
        if step.get("payload_len") != len(payload):
            raise ValueError("listen scenario payload_len does not match payload_hex")
        if step.get("payload_sha256") != payload_sha256:
            raise ValueError("listen scenario payload_sha256 does not match payload_hex")

    raw_mode = mode == "raw_lora_frame"
    source.update(
        {
            "mode": mode,
            "promiscuous_receive": raw_mode,
            "payload_hex": payload.hex(),
            "payload_len": len(payload),
            "payload_sha256": payload_sha256,
            "rnode_delivery_interpretation": (
                "raw physical LoRa frame bytes"
                if raw_mode
                else "ordinary RNode deframed/reassembled packet bytes"
            ),
            "rns_semantic_validity_checked_during_listen": False,
        }
    )
    return raw_mode, payload, source


def validate_target_artifact_mode(
    scenario: dict[str, object], target_artifact_mode: str
) -> None:
    required_feature = scenario.get("required_target_feature")
    if required_feature is None:
        if "target_expectations" in scenario:
            raise ValueError(
                "ordinary scenario must not declare target-only expectations"
            )
        if target_artifact_mode not in {
            "lab-rx",
            RNODE_PEER_ISOLATION_ARTIFACT_MODE,
        }:
            raise ValueError(
                "ordinary scenario requires --target-artifact-mode 'lab-rx' or "
                f"{RNODE_PEER_ISOLATION_ARTIFACT_MODE!r}, not "
                f"{target_artifact_mode!r}"
            )
        return
    elif required_feature == "lab-rx-backpressure":
        expected = "lab-rx-backpressure-hil"
        if scenario.get("target_expectations") != BACKPRESSURE_TARGET_EXPECTATIONS:
            raise ValueError(
                "lab-rx-backpressure scenario has invalid target expectations"
            )
        canonical = json.dumps(
            scenario, sort_keys=True, separators=(",", ":")
        ).encode("utf-8")
        if hashlib.sha256(canonical).hexdigest() != BACKPRESSURE_SCENARIO_SHA256:
            raise ValueError(
                "lab-rx-backpressure requires the exact committed pressure stimulus"
            )
    elif required_feature == "lab-rx-returned-fault-hil":
        expectations = scenario.get("target_expectations")
        kind = expectations.get("kind") if isinstance(expectations, dict) else None
        if kind == "returned_fault":
            expected = RETURNED_FAULT_ARTIFACT_MODE
            expected_expectations = RETURNED_FAULT_TARGET_EXPECTATIONS
            expected_sha256 = RETURNED_FAULT_SCENARIO_SHA256
        elif kind == "returned_fault_repeat_until_quarantine":
            expected = RETURNED_FAULT_REPEAT_ARTIFACT_MODE
            expected_expectations = RETURNED_FAULT_REPEAT_TARGET_EXPECTATIONS
            expected_sha256 = RETURNED_FAULT_REPEAT_SCENARIO_SHA256
        else:
            raise ValueError(
                "lab-rx-returned-fault-hil scenario has invalid target expectations"
            )
        if expectations != expected_expectations:
            raise ValueError(
                "lab-rx-returned-fault-hil scenario has invalid target expectations"
            )
        canonical = json.dumps(
            scenario, sort_keys=True, separators=(",", ":")
        ).encode("utf-8")
        if hashlib.sha256(canonical).hexdigest() != expected_sha256:
            raise ValueError(
                "lab-rx-returned-fault-hil requires the exact committed trigger stimulus"
            )
    else:
        raise ValueError(
            f"scenario has unsupported required_target_feature {required_feature!r}"
        )
    if target_artifact_mode != expected:
        raise ValueError(
            f"scenario requires --target-artifact-mode {expected!r}, "
            f"not {target_artifact_mode!r}"
        )


def peer_preamble_extension_us(
    *,
    receiver_preamble_symbols: int,
    peer_preamble_symbols: int,
    spreading_factor: int,
    bandwidth_hz: int,
) -> int:
    extra_symbols = max(peer_preamble_symbols - receiver_preamble_symbols, 0)
    numerator = extra_symbols * (1 << spreading_factor) * 1_000_000
    return (numerator + bandwidth_hz - 1) // bandwidth_hz


def validate_send_args(args: argparse.Namespace, corpus: dict[str, object]) -> None:
    if sys.version_info[:3] != EXPECTED_PYTHON:
        expected = ".".join(str(value) for value in EXPECTED_PYTHON)
        actual = ".".join(str(value) for value in sys.version_info[:3])
        raise ValueError(f"qualification requires Python {expected}, found {actual}")
    if args.transmit_ack != TRANSMIT_ACK:
        raise ValueError(f"--transmit-ack must be exactly {TRANSMIT_ACK!r}")
    if args.fresh_peer_reset_ack != FRESH_PEER_RESET_ACK:
        raise ValueError(
            f"--fresh-peer-reset-ack must be exactly {FRESH_PEER_RESET_ACK!r}"
        )
    if args.fresh_tracker_boot_ack != FRESH_TRACKER_BOOT_ACK:
        raise ValueError(
            f"--fresh-tracker-boot-ack must be exactly {FRESH_TRACKER_BOOT_ACK!r}"
        )
    if not args.antenna_or_load_attached:
        raise ValueError("--antenna-or-load-attached is required")
    if not args.region_basis.strip():
        raise ValueError("--region-basis must record the operator's regulatory basis")
    if not 137_000_000 <= args.frequency_hz <= 3_000_000_000:
        raise ValueError("frequency is outside the pinned RNode firmware range")
    if not 7_800 <= args.bandwidth_hz <= 1_625_000:
        raise ValueError("bandwidth is outside the pinned RNode firmware range")
    if not 5 <= args.spreading_factor <= 12:
        raise ValueError("spreading factor must be 5 through 12")
    if not 5 <= args.coding_rate_denominator <= 8:
        raise ValueError("coding rate denominator must be 5 through 8")
    if not 0 <= args.tx_power_dbm <= 37:
        raise ValueError("TX power must be 0 through 37 dBm before device clamping")
    if not 0 <= args.short_airtime_limit_basis_points < 10_000:
        raise ValueError("short airtime limit must be 0 through 9999 basis points")
    if not 0 <= args.long_airtime_limit_basis_points < 10_000:
        raise ValueError("long airtime limit must be 0 through 9999 basis points")
    if args.expected_peer_preamble_symbols < 1:
        raise ValueError("expected peer preamble symbols must be positive")
    if args.receiver_preamble_symbols < 1:
        raise ValueError("receiver preamble symbols must be positive")
    if (args.receiver_fragment_timeout_us is None) != (
        args.receiver_maximum_frame_airtime_us is None
    ):
        raise ValueError(
            "receiver fragment timeout and maximum frame airtime must be supplied together"
        )
    if args.receiver_fragment_timeout_us is not None:
        if args.receiver_fragment_timeout_us <= 0:
            raise ValueError("receiver fragment timeout must be positive")
        if args.receiver_maximum_frame_airtime_us <= 0:
            raise ValueError("receiver maximum frame airtime must be positive")
        expected_timeout = (
            args.receiver_maximum_frame_airtime_us * 2
            + FRAGMENT_TIMEOUT_GUARD_US
        )
        if args.receiver_fragment_timeout_us != expected_timeout:
            raise ValueError(
                "receiver values do not match the Phase-1 timeout contract: "
                "fragment_timeout_us must equal 2 * maximum_frame_airtime_us + 5000000"
            )
    if not 0 <= args.post_enqueue_observation_ms <= 300_000:
        raise ValueError("--post-enqueue-observation-ms must be 0 through 300000")
    if args.output_dir.exists():
        if not args.output_dir.is_dir():
            raise ValueError("--output-dir exists and is not a directory")
        if any(args.output_dir.iterdir()):
            raise ValueError("--output-dir must not exist or must be empty")
    if corpus.get("peer") != EXPECTED_PEER:
        raise ValueError("HIL send requires the exact pinned RNode peer metadata")
    expected_version = EXPECTED_PEER["version"]
    if args.expected_firmware != expected_version:
        raise ValueError(
            f"--expected-firmware must match corpus version {expected_version!r}"
        )


def validate_listen_args(args: argparse.Namespace, corpus: dict[str, object]) -> None:
    if sys.version_info[:3] != EXPECTED_PYTHON:
        expected = ".".join(str(value) for value in EXPECTED_PYTHON)
        actual = ".".join(str(value) for value in sys.version_info[:3])
        raise ValueError(f"qualification requires Python {expected}, found {actual}")
    if args.fresh_peer_reset_ack != FRESH_PEER_RESET_ACK:
        raise ValueError(
            f"--fresh-peer-reset-ack must be exactly {FRESH_PEER_RESET_ACK!r}"
        )
    if not args.antenna_or_load_attached:
        raise ValueError("--antenna-or-load-attached is required")
    if not args.region_basis.strip():
        raise ValueError("--region-basis must record the operator's regulatory basis")
    if not 137_000_000 <= args.frequency_hz <= 3_000_000_000:
        raise ValueError("frequency is outside the pinned RNode firmware range")
    if not 7_800 <= args.bandwidth_hz <= 1_625_000:
        raise ValueError("bandwidth is outside the pinned RNode firmware range")
    if not 5 <= args.spreading_factor <= 12:
        raise ValueError("spreading factor must be 5 through 12")
    if not 5 <= args.coding_rate_denominator <= 8:
        raise ValueError("coding rate denominator must be 5 through 8")
    if not 0 <= args.tx_power_dbm <= 37:
        raise ValueError("TX power must be 0 through 37 dBm before device clamping")
    if not 0 <= args.short_airtime_limit_basis_points < 10_000:
        raise ValueError("short airtime limit must be 0 through 9999 basis points")
    if not 0 <= args.long_airtime_limit_basis_points < 10_000:
        raise ValueError("long airtime limit must be 0 through 9999 basis points")
    if args.expected_peer_preamble_symbols < 1:
        raise ValueError("expected peer preamble symbols must be positive")
    if not MIN_LISTEN_DURATION_MS <= args.listen_duration_ms <= MAX_LISTEN_DURATION_MS:
        raise ValueError(
            "--listen-duration-ms must be "
            f"{MIN_LISTEN_DURATION_MS} through {MAX_LISTEN_DURATION_MS}"
        )
    if args.output_dir.exists():
        if not args.output_dir.is_dir():
            raise ValueError("--output-dir exists and is not a directory")
        if any(args.output_dir.iterdir()):
            raise ValueError("--output-dir must not exist or must be empty")
    if corpus.get("peer") != EXPECTED_PEER:
        raise ValueError("HIL listen requires the exact pinned RNode peer metadata")
    expected_version = EXPECTED_PEER["version"]
    if args.expected_firmware != expected_version:
        raise ValueError(
            f"--expected-firmware must match corpus version {expected_version!r}"
        )
    if args.validate_rns_announce:
        raw_mode, _, _ = resolve_listen_expectation(args, corpus)
        if raw_mode:
            raise ValueError(
                "--validate-rns-announce requires ordinary rnode_packet mode; "
                "raw_lora_frame includes the RNode physical header"
            )


def write_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def run_send(
    args: argparse.Namespace,
    corpus: dict[str, object],
    corpus_sha256: str,
) -> int:
    scenario = find_scenario(corpus, args.scenario)
    validate_send_args(args, corpus)
    validate_target_artifact_mode(scenario, args.target_artifact_mode)
    preamble_extension_us = peer_preamble_extension_us(
        receiver_preamble_symbols=args.receiver_preamble_symbols,
        peer_preamble_symbols=args.expected_peer_preamble_symbols,
        spreading_factor=args.spreading_factor,
        bandwidth_hz=args.bandwidth_hz,
    )
    # Validate every byte, bound, mode and wait before creating evidence output,
    # importing the serial implementation, or authorizing any RF state.
    raw_mode, payloads, waits = validate_scenario_for_send(
        corpus,
        scenario,
        receiver_fragment_timeout_us=args.receiver_fragment_timeout_us,
        receiver_maximum_frame_airtime_us=args.receiver_maximum_frame_airtime_us,
        peer_preamble_extension_us=preamble_extension_us,
    )

    args.output_dir.mkdir(parents=True, exist_ok=True)
    transcript_path = args.output_dir / "peer-transcript.jsonl"
    manifest_path = args.output_dir / "peer-manifest.json"
    started = utc_now()
    manifest: dict[str, object] = {
        "schema": 1,
        "status": "running",
        "started_utc": started,
        "finished_utc": None,
        "corpus": str(args.corpus.resolve()),
        "corpus_sha256": corpus_sha256,
        "tool": str(Path(__file__).resolve()),
        "tool_sha256": sha256_file(Path(__file__)),
        "scenario": scenario,
        "serial_port": args.port,
        "target_artifact_mode": args.target_artifact_mode,
        "profile": {
            "frequency_hz": args.frequency_hz,
            "bandwidth_hz": args.bandwidth_hz,
            "spreading_factor": args.spreading_factor,
            "coding_rate_denominator": args.coding_rate_denominator,
            "tx_power_dbm": args.tx_power_dbm,
            "expected_peer_preamble_symbols": args.expected_peer_preamble_symbols,
            "receiver_preamble_symbols": args.receiver_preamble_symbols,
            "short_airtime_limit_basis_points": args.short_airtime_limit_basis_points,
            "long_airtime_limit_basis_points": args.long_airtime_limit_basis_points,
        },
        "receiver_fragment_timeout_us": args.receiver_fragment_timeout_us,
        "receiver_maximum_frame_airtime_us": args.receiver_maximum_frame_airtime_us,
        "peer_preamble_extension_us": preamble_extension_us,
        "post_enqueue_observation_ms": args.post_enqueue_observation_ms,
        "channel_stats_baseline_timeout_ms": int(
            CHANNEL_STATS_TIMEOUT_SECONDS * 1_000
        ),
        "driver_tx_evidence_timeout_ms": int(
            DRIVER_TX_EVIDENCE_TIMEOUT_SECONDS * 1_000
        ),
        "region_basis": args.region_basis,
        "antenna_or_load_attached": True,
        "fresh_peer_reset_acknowledged": True,
        "fresh_tracker_boot_acknowledged": True,
        "independent_rf_observer_required": True,
        "peer_driver_tx_evidence": None,
        "runtime": {
            "python_implementation": platform.python_implementation(),
            "python_version": platform.python_version(),
            "pyserial_version": None,
            "serial": {
                "baudrate": 115_200,
                "bytesize": 8,
                "parity": "N",
                "stopbits": 1,
                "timeout_seconds": 0.1,
                "write_timeout_seconds": 3.0,
                "xonxoff": False,
                "rtscts": False,
                "dsrdtr": False,
            },
        },
        "enqueued_steps": 0,
        "device": None,
        "peer_physical_timing": None,
        "error": None,
    }
    write_json(manifest_path, manifest)

    try:
        import serial

        if serial.__version__ != EXPECTED_PYSERIAL:
            raise ValueError(
                f"qualification requires pyserial {EXPECTED_PYSERIAL}, "
                f"found {serial.__version__}"
            )
        manifest["runtime"]["pyserial_version"] = serial.__version__
        write_json(manifest_path, manifest)

        profile = manifest["profile"]
        with transcript_path.open("wb") as transcript_file:
            with serial.Serial(
                args.port,
                baudrate=115_200,
                bytesize=serial.EIGHTBITS,
                parity=serial.PARITY_NONE,
                stopbits=serial.STOPBITS_ONE,
                timeout=0.1,
                write_timeout=3.0,
                xonxoff=False,
                rtscts=False,
                dsrdtr=False,
            ) as serial_port:
                serial_port.reset_input_buffer()
                peer = RNodePeer(serial_port, Transcript(transcript_file))
                device = peer.inspect_device()
                if device["firmware_version"] != args.expected_firmware:
                    raise ValueError(
                        f"connected RNode firmware is {device['firmware_version']!r}, "
                        f"expected {args.expected_firmware!r}"
                    )
                manifest["device"] = device
                write_json(manifest_path, manifest)

                manifest["peer_physical_timing"] = peer.configure(profile)
                write_json(manifest_path, manifest)
                peer.set_promiscuous(raw_mode)
                baseline = peer.wait_for_fresh_channel_stats()
                require_fresh_airtime_baseline(baseline)
                manifest["peer_driver_tx_evidence"] = driver_tx_evidence(
                    baseline,
                    [],
                )
                write_json(manifest_path, manifest)
                post_enqueue_observation_index = len(
                    peer.channel_stats_observations
                )
                for payload, wait_seconds in zip(payloads, waits, strict=True):
                    peer.transmit(payload)
                    manifest["enqueued_steps"] += 1
                    write_json(manifest_path, manifest)
                    peer.observe_for(wait_seconds)
                peer.wait_until_queue_accepts()
                peer.observe_for(args.post_enqueue_observation_ms / 1_000)
                evidence = peer.wait_for_driver_tx_evidence(
                    baseline,
                    observation_index=post_enqueue_observation_index,
                )
                manifest["peer_driver_tx_evidence"] = evidence
                write_json(manifest_path, manifest)
                if not evidence["peer_driver_tx_observed"]:
                    raise RuntimeError(
                        "RNode did not report a post-enqueue airtime increase; "
                        "driver TX completion was not observed"
                    )

        manifest["status"] = "enqueued_driver_tx_observed_not_rf_verified"
        manifest["finished_utc"] = utc_now()
        manifest["transcript_sha256"] = sha256_file(transcript_path)
        write_json(manifest_path, manifest)
        print(
            f"enqueued scenario {args.scenario!r} and observed peer driver TX; "
            f"retain {args.output_dir} and complete independent RF verification"
        )
        return 0
    except BaseException as error:
        manifest["status"] = (
            "failed_after_enqueue"
            if manifest["enqueued_steps"]
            else "failed_before_enqueue"
        )
        manifest["finished_utc"] = utc_now()
        manifest["error"] = f"{type(error).__name__}: {error}"
        if transcript_path.exists():
            manifest["transcript_sha256"] = sha256_file(transcript_path)
        write_json(manifest_path, manifest)
        raise


def run_listen(
    args: argparse.Namespace,
    corpus: dict[str, object],
    corpus_sha256: str,
) -> int:
    raw_mode, expected_payload, expected = resolve_listen_expectation(args, corpus)
    validate_listen_args(args, corpus)

    args.output_dir.mkdir(parents=True, exist_ok=True)
    transcript_path = args.output_dir / "peer-transcript.jsonl"
    manifest_path = args.output_dir / "peer-manifest.json"
    profile: dict[str, int] = {
        "frequency_hz": args.frequency_hz,
        "bandwidth_hz": args.bandwidth_hz,
        "spreading_factor": args.spreading_factor,
        "coding_rate_denominator": args.coding_rate_denominator,
        "tx_power_dbm": args.tx_power_dbm,
        "expected_peer_preamble_symbols": args.expected_peer_preamble_symbols,
        "short_airtime_limit_basis_points": args.short_airtime_limit_basis_points,
        "long_airtime_limit_basis_points": args.long_airtime_limit_basis_points,
    }
    irq_diagnostics: dict[str, object] = {
        "requested": args.sx1262_irq_diagnostics,
        "source": "compile-gated RNode CMD_STAT_IRQ 0x2a",
        "request_payload_hex": "00",
        "query_atomically_snapshots_and_clears": True,
        "sync_word_valid_semantics": (
            "GFSK-only SX1262 IRQ; not LoRa receive-stage evidence"
        ),
        "baseline_after_configure": None,
        "final_after_listen_window": None,
    }
    semantic_evidence = rns_semantic_evidence_placeholder(
        args.validate_rns_announce
    )
    manifest: dict[str, object] = {
        "schema": 1,
        "operation": "listen",
        "status": "running",
        "started_utc": utc_now(),
        "finished_utc": None,
        "corpus": str(args.corpus.resolve()),
        "corpus_sha256": corpus_sha256,
        "tool": str(Path(__file__).resolve()),
        "tool_sha256": sha256_file(Path(__file__)),
        "serial_port": args.port,
        "expected": expected,
        "profile": profile,
        "listen_duration_ms": args.listen_duration_ms,
        "region_basis": args.region_basis,
        "antenna_or_load_attached": True,
        "fresh_peer_reset_acknowledged": True,
        "evidence_scope": (
            "RNode RF receive and peer-to-host serial delivery only; Reticulum "
            "packet semantics are not checked"
        ),
        "rns_semantic_validity_checked": False,
        "runtime": {
            "python_implementation": platform.python_implementation(),
            "python_version": platform.python_version(),
            "pyserial_version": None,
            "serial": {
                "baudrate": 115_200,
                "bytesize": 8,
                "parity": "N",
                "stopbits": 1,
                "timeout_seconds": 0.1,
                "write_timeout_seconds": 3.0,
                "xonxoff": False,
                "rtscts": False,
                "dsrdtr": False,
            },
        },
        "radio_activated": False,
        "device": None,
        "peer_physical_timing": None,
        "serial_delivery_evidence": receive_evidence(expected_payload, []),
        "radio_telemetry": {
            "packet_rssi": [],
            "packet_snr": [],
            "channel_stats": [],
        },
        "sx1262_irq_diagnostics": irq_diagnostics,
        "rns_semantic_evidence": semantic_evidence,
        "error": None,
    }
    write_json(manifest_path, manifest)

    try:
        rns: object | None = None
        semantic_peer: dict[str, object] | None = None
        if args.validate_rns_announce:
            rns, semantic_peer = load_pinned_rns_validation_peer()
            semantic_evidence["peer"] = semantic_peer
            write_json(manifest_path, manifest)

        import serial

        if serial.__version__ != EXPECTED_PYSERIAL:
            raise ValueError(
                f"qualification requires pyserial {EXPECTED_PYSERIAL}, "
                f"found {serial.__version__}"
            )
        manifest["runtime"]["pyserial_version"] = serial.__version__
        write_json(manifest_path, manifest)

        with transcript_path.open("wb") as transcript_file:
            with serial.Serial(
                args.port,
                baudrate=115_200,
                bytesize=serial.EIGHTBITS,
                parity=serial.PARITY_NONE,
                stopbits=serial.STOPBITS_ONE,
                timeout=0.1,
                write_timeout=3.0,
                xonxoff=False,
                rtscts=False,
                dsrdtr=False,
            ) as serial_port:
                serial_port.reset_input_buffer()
                peer = RNodePeer(serial_port, Transcript(transcript_file))
                device = peer.inspect_device()
                if device["firmware_version"] != args.expected_firmware:
                    raise ValueError(
                        f"connected RNode firmware is {device['firmware_version']!r}, "
                        f"expected {args.expected_firmware!r}"
                    )
                manifest["device"] = device
                write_json(manifest_path, manifest)

                data_index = len(peer.data_observations)
                rssi_index = len(peer.rssi_observations)
                snr_index = len(peer.snr_observations)
                channel_stats_index = len(peer.channel_stats_observations)
                manifest["peer_physical_timing"] = peer.configure(
                    profile,
                    promiscuous=raw_mode,
                )
                manifest["radio_activated"] = True
                if args.sx1262_irq_diagnostics:
                    irq_diagnostics["baseline_after_configure"] = (
                        peer.query_sx1262_irq_diagnostics()
                    )
                write_json(manifest_path, manifest)

                manifest["listen_window_started_utc"] = utc_now()
                write_json(manifest_path, manifest)
                print(
                    "RNode configured and listening for one exact "
                    f"{expected['mode']} payload for {args.listen_duration_ms} ms",
                    flush=True,
                )
                peer.observe_for(args.listen_duration_ms / 1_000)
                manifest["listen_window_finished_utc"] = utc_now()

                observations = list(peer.data_observations[data_index:])
                packet_rssi = list(peer.rssi_observations[rssi_index:])
                packet_snr = list(peer.snr_observations[snr_index:])
                channel_stats = list(
                    peer.channel_stats_observations[channel_stats_index:]
                )
                if args.sx1262_irq_diagnostics:
                    irq_diagnostics["final_after_listen_window"] = (
                        peer.query_sx1262_irq_diagnostics()
                    )
                evidence = receive_evidence(expected_payload, observations)
                manifest["serial_delivery_evidence"] = evidence
                manifest["radio_telemetry"] = {
                    "packet_rssi": packet_rssi,
                    "packet_snr": packet_snr,
                    "channel_stats": channel_stats,
                }
                write_json(manifest_path, manifest)
                if not evidence["unambiguous_exact_payload_received"]:
                    if args.validate_rns_announce:
                        semantic_evidence["result"] = (
                            "not_evaluated_due_to_serial_delivery_failure"
                        )
                        write_json(manifest_path, manifest)
                    raise RuntimeError(
                        "RNode listen did not receive exactly one unambiguous "
                        f"expected CMD_DATA payload ({evidence['result']})"
                    )

                if args.validate_rns_announce:
                    if rns is None or semantic_peer is None:
                        raise RuntimeError(
                            "pinned Python-RNS validation peer was not loaded"
                        )
                    received_payload = bytes.fromhex(
                        observations[0]["payload_hex"]
                    )
                    semantic_evidence = validate_rns_announce(
                        received_payload,
                        rns,
                        semantic_peer,
                    )
                    manifest["rns_semantic_evidence"] = semantic_evidence
                    manifest["rns_semantic_validity_checked"] = bool(
                        semantic_evidence["evaluated"]
                    )
                    expected["rns_semantic_validity_checked_during_listen"] = bool(
                        semantic_evidence["evaluated"]
                    )
                    evidence["rns_semantic_validity_checked"] = bool(
                        semantic_evidence["evaluated"]
                    )
                    evidence["rns_semantically_valid"] = semantic_evidence["valid"]
                    evidence["claim_scope"] = (
                        "the RNode driver delivered these exact deframed bytes over "
                        "its serial KISS link; semantic validity is reported "
                        "separately in rns_semantic_evidence"
                    )
                    write_json(manifest_path, manifest)
                    if not semantic_evidence["valid"]:
                        failure = semantic_evidence["failure"]
                        reason = (
                            failure["reason"]
                            if isinstance(failure, dict)
                            else semantic_evidence["result"]
                        )
                        raise RuntimeError(
                            "RNS announce semantic validation failed "
                            f"({reason})"
                        )

        if args.validate_rns_announce:
            manifest["status"] = (
                "expected_rns_announce_received_and_semantically_validated"
            )
            manifest["evidence_scope"] = (
                "RNode RF receive, ordinary physical deframing, exact peer-to-host "
                "serial delivery, and pinned Python-RNS first-hop announce validation"
            )
        else:
            manifest["status"] = (
                "expected_payload_received_via_rnode_serial_"
                "rns_semantics_not_verified"
            )
        manifest["finished_utc"] = utc_now()
        manifest["transcript_sha256"] = sha256_file(transcript_path)
        write_json(manifest_path, manifest)
        if args.validate_rns_announce:
            print(
                "received the exact expected payload once and validated a signed "
                f"first-hop RNS announce; retain {args.output_dir}"
            )
        else:
            print(
                "received the exact expected payload once via RNode serial delivery; "
                f"retain {args.output_dir}; Reticulum semantics were not verified"
            )
        return 0
    except BaseException as error:
        manifest["status"] = (
            "failed_after_radio_activation"
            if manifest["radio_activated"]
            else "failed_before_radio_activation"
        )
        manifest["finished_utc"] = utc_now()
        manifest["error"] = f"{type(error).__name__}: {error}"
        if transcript_path.exists():
            manifest["transcript_sha256"] = sha256_file(transcript_path)
        write_json(manifest_path, manifest)
        raise


def add_send_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("scenario")
    parser.add_argument("--port", required=True)
    parser.add_argument(
        "--target-artifact-mode", choices=sorted(TARGET_ARTIFACT_MODES), required=True
    )
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--frequency-hz", type=int, required=True)
    parser.add_argument("--bandwidth-hz", type=int, required=True)
    parser.add_argument("--spreading-factor", type=int, required=True)
    parser.add_argument("--coding-rate-denominator", type=int, required=True)
    parser.add_argument("--tx-power-dbm", type=int, required=True)
    parser.add_argument("--expected-peer-preamble-symbols", type=int, required=True)
    parser.add_argument("--receiver-preamble-symbols", type=int, required=True)
    parser.add_argument(
        "--short-airtime-limit-basis-points", type=int, required=True
    )
    parser.add_argument(
        "--long-airtime-limit-basis-points", type=int, required=True
    )
    parser.add_argument("--receiver-fragment-timeout-us", type=int)
    parser.add_argument("--receiver-maximum-frame-airtime-us", type=int)
    parser.add_argument("--post-enqueue-observation-ms", type=int, required=True)
    parser.add_argument("--expected-firmware", required=True)
    parser.add_argument("--region-basis", required=True)
    parser.add_argument("--antenna-or-load-attached", action="store_true")
    parser.add_argument("--fresh-peer-reset-ack", required=True)
    parser.add_argument("--fresh-tracker-boot-ack", required=True)
    parser.add_argument("--transmit-ack", required=True)


def add_listen_arguments(parser: argparse.ArgumentParser) -> None:
    expectation = parser.add_mutually_exclusive_group(required=True)
    expectation.add_argument("--expected-payload-hex")
    expectation.add_argument("--expected-scenario")
    parser.add_argument("--expected-mode", choices=sorted(ALLOWED_MODES))
    parser.add_argument("--port", required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--frequency-hz", type=int, required=True)
    parser.add_argument("--bandwidth-hz", type=int, required=True)
    parser.add_argument("--spreading-factor", type=int, required=True)
    parser.add_argument("--coding-rate-denominator", type=int, required=True)
    parser.add_argument("--tx-power-dbm", type=int, required=True)
    parser.add_argument("--expected-peer-preamble-symbols", type=int, required=True)
    parser.add_argument(
        "--short-airtime-limit-basis-points", type=int, required=True
    )
    parser.add_argument(
        "--long-airtime-limit-basis-points", type=int, required=True
    )
    parser.add_argument("--listen-duration-ms", type=int, required=True)
    parser.add_argument("--expected-firmware", required=True)
    parser.add_argument("--region-basis", required=True)
    parser.add_argument("--antenna-or-load-attached", action="store_true")
    parser.add_argument("--fresh-peer-reset-ack", required=True)
    parser.add_argument(
        "--sx1262-irq-diagnostics",
        action="store_true",
        help=(
            "query compile-gated CMD_STAT_IRQ 0x2a accumulators immediately "
            "after configuration and after the bounded listen window"
        ),
    )
    parser.add_argument(
        "--validate-rns-announce",
        action="store_true",
        help=(
            "explicitly require the one deframed rnode_packet payload to pass "
            "pinned Python-RNS first-hop announce validation"
        ),
    )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser()
    parser.add_argument("--corpus", type=Path, default=DEFAULT_CORPUS)
    subparsers = parser.add_subparsers(dest="command", required=True)
    subparsers.add_parser("list", help="list committed scenarios without opening a device")
    plan = subparsers.add_parser("plan", help="print one scenario without opening a device")
    plan.add_argument("scenario")
    send = subparsers.add_parser("send", help="explicitly configure an RNode and transmit one scenario")
    add_send_arguments(send)
    listen = subparsers.add_parser(
        "listen",
        help="configure an RNode and require one exact inbound payload",
    )
    add_listen_arguments(listen)
    return parser


def main(argv: Iterable[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    corpus, corpus_sha256 = load_corpus_snapshot(args.corpus)
    if args.command == "list":
        for scenario in corpus["scenarios"]:
            print(f"{scenario['name']}: {scenario['description']}")
        return 0
    if args.command == "plan":
        print(json.dumps(find_scenario(corpus, args.scenario), indent=2, sort_keys=True))
        return 0
    if args.command == "send":
        return run_send(args, corpus, corpus_sha256)
    if args.command == "listen":
        return run_listen(args, corpus, corpus_sha256)
    parser.error(f"unknown command {args.command!r}")
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
