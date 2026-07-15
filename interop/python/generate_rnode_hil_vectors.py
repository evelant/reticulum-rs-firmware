#!/usr/bin/env python3
"""Generate deterministic RNode 1.86 Phase-1 HIL stimuli.

The corpus distinguishes ordinary RNode packets from promiscuous-mode raw
LoRa frames. Ordinary packets are split and receive a random four-bit sequence
header inside RNode firmware. Raw frames already contain that one-byte header
and are transmitted byte-for-byte by the pinned firmware.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import tomllib


ROOT = Path(__file__).parents[2]
DEFAULT_OUTPUT = ROOT / "interop" / "vectors" / "rnode-hil-v1.json"
PEERS = ROOT / "interop" / "peers.toml"
RNS_VECTORS = ROOT / "interop" / "vectors" / "rns-1.3.8.json"


def sha256_hex(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def pattern(label: str, length: int) -> bytes:
    """Return stable, non-secret bytes without embedding a repeated fill."""
    output = bytearray()
    counter = 0
    while len(output) < length:
        output.extend(
            hashlib.sha256(
                label.encode("utf-8") + counter.to_bytes(4, "big")
            ).digest()
        )
        counter += 1
    return bytes(output[:length])


def fixed_wait(milliseconds: int) -> dict[str, object]:
    return {"kind": "fixed", "milliseconds": milliseconds}


def fragment_timeout_wait(margin_ms: int = 1_000) -> dict[str, object]:
    return {
        "kind": "receiver_fragment_timeout",
        "margin_ms": margin_ms,
    }


def step(
    mode: str,
    payload: bytes,
    *,
    wait_after: dict[str, object] | None = None,
) -> dict[str, object]:
    if mode not in {"rnode_packet", "raw_lora_frame"}:
        raise ValueError(f"unsupported transmission mode {mode!r}")
    return {
        "mode": mode,
        "payload_hex": payload.hex(),
        "payload_len": len(payload),
        "payload_sha256": sha256_hex(payload),
        "wait_after": wait_after or fixed_wait(250),
    }


def completed(payload: bytes, *, admitted: bool, disposition: str) -> dict[str, object]:
    return {
        "packet_len": len(payload),
        "packet_sha256": sha256_hex(payload) if admitted else None,
        "rns_admitted": admitted,
        "rete_disposition": disposition,
    }


def scenario(
    name: str,
    description: str,
    steps: list[dict[str, object]],
    *,
    completed_packets: list[dict[str, object]] | None = None,
    pending_started: int = 0,
    pending_replaced: int = 0,
    pending_discarded: int = 0,
    pending_expired: int = 0,
    packets_too_long: int = 0,
    required_target_feature: str | None = None,
    target_expectations: dict[str, object] | None = None,
) -> dict[str, object]:
    value = {
        "name": name,
        "description": description,
        "steps": steps,
        # This is the deterministic, no-queue-stall Rust replay. A scenario
        # whose target artifact deliberately changes scheduling records those
        # target-only expectations separately below.
        "unstalled_reference_deltas": {
            "completed_packets": completed_packets or [],
            "pending_started": pending_started,
            "pending_replaced": pending_replaced,
            "pending_discarded": pending_discarded,
            "pending_expired": pending_expired,
            "packets_too_long": packets_too_long,
        },
    }
    if required_target_feature is not None:
        value["required_target_feature"] = required_target_feature
    if target_expectations is not None:
        value["target_expectations"] = target_expectations
    return value


def build_scenarios(announce: bytes) -> list[dict[str, object]]:
    single_1 = b"\x42"
    single_253 = pattern("rnode-hil-single-253", 253)
    single_254 = pattern("rnode-hil-single-254", 254)
    split_255 = pattern("rnode-hil-split-255", 255)
    split_256 = pattern("rnode-hil-split-256", 256)
    split_499 = pattern("rnode-hil-split-499", 499)
    orphan_data = pattern("rnode-hil-orphan", 254)

    replacement_first = pattern("rnode-hil-replacement-old", 40)
    replacement_second = pattern("rnode-hil-replacement-new-first", 50)
    replacement_third = pattern("rnode-hil-replacement-new-second", 60)
    replacement_packet = replacement_second + replacement_third

    discarded_first = pattern("rnode-hil-discarded-first", 32)
    after_discard = pattern("rnode-hil-after-discard", 8)

    duplicate_half = pattern("rnode-hil-duplicate-half", 254)
    reordered_original_first = pattern("rnode-hil-reordered-first", 254)
    reordered_original_second = pattern("rnode-hil-reordered-second", 46)
    reordered_arrival = reordered_original_second + reordered_original_first

    backpressure_first = pattern("rnode-hil-backpressure-first", 254)
    backpressure_second = pattern("rnode-hil-backpressure-second", 32)
    backpressure_extra_a = pattern("rnode-hil-backpressure-extra-a", 8)
    backpressure_extra_b = pattern("rnode-hil-backpressure-extra-b", 8)
    backpressure_packet = backpressure_first + backpressure_second

    # The returned-fault artifact rejects the IRQ-status read before the radio
    # can fetch this frame.  Use a deterministic, independently generated RNS
    # announce so the ordinary replay still proves that the exact same physical
    # stimulus would be a valid completed packet without the target-only hook.
    returned_fault_frame = bytes([0x20]) + announce

    exact_500 = bytes([0x06]) + bytes([0x5A]) * 499
    oversized = [pattern(f"rnode-hil-oversized-{length}", length) for length in range(501, 509)]

    return [
        scenario(
            "released-python-announce",
            "Pinned Python RNS 1.3.8 announce sent through ordinary RNode framing.",
            [step("rnode_packet", announce)],
            completed_packets=[completed(announce, admitted=True, disposition="processed")],
        ),
        scenario(
            "released-python-announce-duplicate",
            "The same independently generated announce is admitted once and then rejected by Rete deduplication.",
            [step("rnode_packet", announce), step("rnode_packet", announce)],
            completed_packets=[
                completed(announce, admitted=True, disposition="processed"),
                completed(announce, admitted=True, disposition="duplicate"),
            ],
        ),
        scenario(
            "raw-header-only",
            "One non-split RNode header and no RNS bytes; the empty packet is admitted then rejected by Rete.",
            [step("raw_lora_frame", bytes([0xA0]))],
            completed_packets=[completed(b"", admitted=True, disposition="invalid")],
        ),
        scenario(
            "raw-single-1",
            "One exact RNS byte in a non-split physical frame.",
            [step("raw_lora_frame", bytes([0xB0]) + single_1)],
            completed_packets=[completed(single_1, admitted=True, disposition="invalid")],
        ),
        scenario(
            "raw-single-253",
            "One below-maximum single-frame RNode payload, exercising the 253-byte boundary.",
            [step("raw_lora_frame", bytes([0xC0]) + single_253)],
            completed_packets=[completed(single_253, admitted=True, disposition="invalid")],
        ),
        scenario(
            "raw-single-254",
            "Maximum one-frame RNode payload, including KISS-escape bytes in the generated pattern.",
            [step("raw_lora_frame", bytes([0xC0]) + single_254)],
            completed_packets=[completed(single_254, admitted=True, disposition="invalid")],
        ),
        scenario(
            "rnode-split-255",
            "Smallest ordinary RNode packet requiring two physical frames.",
            [step("rnode_packet", split_255)],
            completed_packets=[
                completed(
                    split_255,
                    admitted=True,
                    disposition="no_observable_outcome",
                )
            ],
            pending_started=1,
        ),
        scenario(
            "rnode-split-256",
            "Ordinary RNode split with a two-byte continuation, exercising the 256-byte boundary.",
            [step("rnode_packet", split_256)],
            completed_packets=[
                completed(
                    split_256,
                    admitted=True,
                    disposition="no_observable_outcome",
                )
            ],
            pending_started=1,
        ),
        scenario(
            "rnode-split-499",
            "Large admitted ordinary RNode packet immediately below the exact RNS MTU case.",
            [step("rnode_packet", split_499)],
            completed_packets=[completed(split_499, admitted=True, disposition="invalid")],
            pending_started=1,
        ),
        scenario(
            "raw-orphan-split",
            "A maximum first half with no continuation; wait beyond the receiver-reported fragment timeout.",
            [
                step(
                    "raw_lora_frame",
                    bytes([0x31]) + orphan_data,
                    wait_after=fragment_timeout_wait(),
                )
            ],
            pending_started=1,
            pending_expired=1,
        ),
        scenario(
            "raw-split-replacement",
            "A different sequence replaces a pending first half, then a matching continuation completes the replacement.",
            [
                step("raw_lora_frame", bytes([0x41]) + replacement_first),
                step("raw_lora_frame", bytes([0x51]) + replacement_second),
                step("raw_lora_frame", bytes([0x51]) + replacement_third),
            ],
            completed_packets=[
                completed(replacement_packet, admitted=True, disposition="invalid")
            ],
            pending_started=2,
            pending_replaced=1,
        ),
        scenario(
            "raw-nonsplit-discards-pending",
            "A non-split frame discards an unrelated pending first half and completes independently.",
            [
                step("raw_lora_frame", bytes([0x61]) + discarded_first),
                step("raw_lora_frame", bytes([0x70]) + after_discard),
            ],
            completed_packets=[completed(after_discard, admitted=True, disposition="invalid")],
            pending_started=1,
            pending_discarded=1,
        ),
        scenario(
            "raw-duplicate-first-half",
            "The ambiguous RNode format treats a repeated same-sequence first half as a 508-byte completion, not as a duplicate fragment.",
            [
                step("raw_lora_frame", bytes([0x91]) + duplicate_half),
                step("raw_lora_frame", bytes([0x91]) + duplicate_half),
            ],
            completed_packets=[
                completed(
                    duplicate_half + duplicate_half,
                    admitted=False,
                    disposition="rns_packet_too_long",
                )
            ],
            pending_started=1,
            packets_too_long=1,
        ),
        scenario(
            "raw-reordered-same-sequence",
            "Same-sequence halves are concatenated strictly in arrival order; the wire format has no fragment index.",
            [
                step(
                    "raw_lora_frame",
                    bytes([0xA1]) + reordered_original_second,
                ),
                step(
                    "raw_lora_frame",
                    bytes([0xA1]) + reordered_original_first,
                ),
            ],
            completed_packets=[
                completed(reordered_arrival, admitted=True, disposition="invalid")
            ],
            pending_started=1,
        ),
        scenario(
            "raw-backpressure-four-frame",
            "One pending split half followed by three frames that fill and overflow the depth-two target handoff during the lab-only async stall.",
            [
                step("raw_lora_frame", bytes([0xD1]) + backpressure_first),
                step("raw_lora_frame", bytes([0xD1]) + backpressure_second),
                step("raw_lora_frame", bytes([0xE0]) + backpressure_extra_a),
                step("raw_lora_frame", bytes([0xF0]) + backpressure_extra_b),
            ],
            completed_packets=[
                completed(backpressure_packet, admitted=True, disposition="invalid"),
                completed(backpressure_extra_a, admitted=True, disposition="invalid"),
                completed(backpressure_extra_b, admitted=True, disposition="invalid"),
            ],
            pending_started=1,
            required_target_feature="lab-rx-backpressure",
            target_expectations={
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
            },
        ),
        scenario(
            "raw-returned-fault-trigger",
            "One benign peer frame drives the returned-fault artifact through its real DIO1 receive path; the target rejects GetIrqStatus before physical SPI and resets before reading the frame.",
            [step("raw_lora_frame", returned_fault_frame)],
            completed_packets=[
                completed(announce, admitted=True, disposition="processed")
            ],
            required_target_feature="lab-rx-returned-fault-hil",
            target_expectations={
                "kind": "returned_fault",
                "artifact_mode": "lab-rx-returned-fault-hil;trigger=get-irq-status-after-set-rx;policy=one-boot",
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
            },
        ),
        scenario(
            "raw-returned-fault-repeat-until-quarantine",
            "Invoke this same benign peer frame once on each of three armed boots in one powered session; each returned fault is correlated across CoreSw and the third reset quarantines before a fourth radio activation.",
            [step("raw_lora_frame", returned_fault_frame)],
            completed_packets=[
                completed(announce, admitted=True, disposition="processed")
            ],
            required_target_feature="lab-rx-returned-fault-hil",
            target_expectations={
                "kind": "returned_fault_repeat_until_quarantine",
                "artifact_mode": "lab-rx-returned-fault-hil;trigger=get-irq-status-after-set-rx;policy=repeat-until-quarantine",
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
            },
        ),
        scenario(
            "rnode-exact-500",
            "Exact RNS MTU packet matching the project boundary fixture.",
            [step("rnode_packet", exact_500)],
            completed_packets=[
                completed(exact_500, admitted=True, disposition="invalid_link_request")
            ],
            pending_started=1,
        ),
        scenario(
            "rnode-501-through-508",
            "Every physically representable packet above the RNS MTU; none may receive a digest or reach Rete.",
            [step("rnode_packet", payload) for payload in oversized],
            completed_packets=[
                completed(payload, admitted=False, disposition="rns_packet_too_long")
                for payload in oversized
            ],
            pending_started=len(oversized),
            packets_too_long=len(oversized),
        ),
    ]


def build_vectors() -> dict[str, object]:
    peers = tomllib.loads(PEERS.read_text(encoding="utf-8"))
    rnode = peers["rnode"]["released"]
    rns = json.loads(RNS_VECTORS.read_text(encoding="utf-8"))
    announce = bytes.fromhex(rns["announce"]["raw_hex"])

    return {
        "schema": 3,
        "protocol": "RNode LoRa framing",
        "lane": "phase-1-rx-hil",
        "peer": {
            "package": "RNode_Firmware",
            "version": rnode["version"],
            "repository": rnode["repository"],
            "revision": rnode["revision"],
            "required_capability": "CMD_PROMISC 0x0e raw-frame transmit",
        },
        "generator": {
            "script": "interop/python/generate_rnode_hil_vectors.py",
            "command": "python interop/python/generate_rnode_hil_vectors.py",
            "source_sha256": sha256_hex(Path(__file__).read_bytes()),
            "rns_vectors_sha256": sha256_hex(RNS_VECTORS.read_bytes()),
            "deterministic": True,
        },
        "wire_contract": {
            "rnode_packet": (
                "CMD_DATA payload supplied to ordinary RNode mode; firmware adds a random "
                "sequence header and splits after 254 packet bytes"
            ),
            "raw_lora_frame": (
                "CMD_DATA payload supplied after CMD_PROMISC=1; bytes already contain the "
                "RNode LoRa header and are transmitted byte-for-byte"
            ),
            "limitations": [
                "Pinned RNode 1.86 cannot deterministically enqueue a zero-byte physical LoRa frame.",
                "The transmitter tool cannot prove RF attribution; an independent observer remains mandatory.",
            ],
        },
        "scenarios": build_scenarios(announce),
    }


def encoded_vectors() -> str:
    return json.dumps(build_vectors(), indent=2, sort_keys=True) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument(
        "--check",
        action="store_true",
        help="fail if the committed corpus differs instead of rewriting it",
    )
    args = parser.parse_args()

    generated = encoded_vectors()
    if args.check:
        try:
            committed = args.output.read_text(encoding="utf-8")
        except FileNotFoundError:
            parser.error(f"missing vector file: {args.output}")
        if committed != generated:
            parser.error(
                f"{args.output} is stale; regenerate it with this script"
            )
        print(f"ok: {args.output} matches pinned RNode HIL source inputs")
        return 0

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(generated, encoding="utf-8")
    print(f"wrote {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
