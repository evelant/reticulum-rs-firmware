#!/usr/bin/env python3
"""Generate the small deterministic Reticulum 1.4.2 foundation corpus."""

from __future__ import annotations

import argparse
import hashlib
import importlib.metadata
from importlib import import_module
import json
import math
from pathlib import Path
import re
import struct
import sys
import tempfile
from types import SimpleNamespace
from unittest.mock import patch

import RNS
from RNS.vendor import umsgpack


DEFAULT_OUTPUT = Path(__file__).parents[1] / "vectors" / "rns-1.4.2.json"
PEER_MANIFEST = Path(__file__).parents[1] / "peers.toml"
REQUIREMENTS = Path(__file__).with_name("requirements-rns-1.4.2.txt")
EXPECTED_PYTHON = (3, 13, 7)
EXPECTED_UMSGPACK = "2.7.1"
LRRTT_MEASURED_RTT = 0.25


class ScriptedClock:
    """Deterministic clock that also records ordering at mocked I/O boundaries."""

    def __init__(self, samples: list[tuple[str, float]]):
        self._samples = iter(samples)
        self.events: list[str] = []

    def time(self) -> float:
        try:
            label, value = next(self._samples)
        except StopIteration as error:
            raise RuntimeError("released RNS sampled the clock more often than expected") from error
        self.events.append(label)
        return value

    def mark(self, event: str) -> None:
        self.events.append(event)

    def assert_exhausted(self) -> None:
        try:
            next(self._samples)
        except StopIteration:
            return
        raise RuntimeError("released RNS sampled the clock fewer times than expected")


def released_peer() -> dict[str, str]:
    """Read the small released-Reticulum table without adding a TOML package."""
    manifest = PEER_MANIFEST.read_text(encoding="utf-8")
    section_match = re.search(
        r"(?ms)^\[reticulum\.released\]\s*$\n(.*?)(?=^\[|\Z)", manifest
    )
    if section_match is None:
        raise RuntimeError(f"missing [reticulum.released] in {PEER_MANIFEST}")

    values = dict(
        re.findall(r'^([a-z_]+)\s*=\s*"([^"]+)"\s*$', section_match.group(1), re.M)
    )
    required = {"version", "repository", "revision"}
    if not required.issubset(values):
        raise RuntimeError(f"incomplete [reticulum.released] in {PEER_MANIFEST}")
    return values


def fixed_identity(label: str) -> RNS.Identity:
    """Create the stable 64-byte private identity used by this corpus."""
    identity = RNS.Identity(create_keys=False)
    identity.load_private_key(hashlib.sha512(label.encode("utf-8")).digest())
    return identity


def verify_installed_peer(peer: dict[str, str]) -> str:
    """Bind the imported RNS distribution to the pinned Git revision."""
    installed = importlib.metadata.version("rns")
    if installed != peer["version"]:
        raise RuntimeError(
            f"expected rns {peer['version']}, found {installed}; "
            "install interop/python/requirements-rns-1.4.2.txt"
        )

    distribution = importlib.metadata.distribution("rns")
    imported_module = getattr(RNS, "__file__", None)
    if imported_module is None:
        raise RuntimeError("the imported RNS module has no filesystem origin")
    imported_module = Path(imported_module).resolve()
    installed_module = Path(
        distribution.locate_file("RNS/__init__.py")
    ).resolve()
    if imported_module != installed_module:
        raise RuntimeError(
            f"imported RNS module is {str(imported_module)!r}, but the pinned "
            f"rns distribution provides {str(installed_module)!r}; remove the "
            "shadowing checkout or PYTHONPATH entry"
        )

    direct_url_text = distribution.read_text("direct_url.json")
    if direct_url_text is None:
        raise RuntimeError(
            "the installed rns distribution has no PEP 610 direct_url.json; "
            "install the pinned Git requirement with a current pip"
        )
    direct_url = json.loads(direct_url_text)
    vcs_info = direct_url.get("vcs_info", {})
    if direct_url.get("url") != peer["repository"]:
        raise RuntimeError(
            f"installed rns source is {direct_url.get('url')!r}, "
            f"expected {peer['repository']!r}"
        )
    if vcs_info.get("vcs") != "git" or vcs_info.get("commit_id") != peer["revision"]:
        raise RuntimeError(
            f"installed rns commit is {vcs_info.get('commit_id')!r}, "
            f"expected {peer['revision']}"
        )
    return installed


def packet_hashable_part(raw: bytes) -> bytes:
    """Return Python Reticulum's HEADER_1 packet hash input."""
    return bytes([raw[0] & 0x0F]) + raw[2:]


def describe_python_value(value: object) -> dict[str, object]:
    """Represent an unpacked value without non-standard JSON numbers."""
    if type(value) is bool:
        return {
            "python_type": "bool",
            "value_class": "boolean",
            "value": value,
        }
    if type(value) is int:
        return {
            "python_type": "int",
            "value_class": "integer",
            "decimal": str(value),
        }
    if type(value) is float:
        if math.isnan(value):
            value_class = "nan"
        elif value == math.inf:
            value_class = "positive_infinity"
        elif value == -math.inf:
            value_class = "negative_infinity"
        else:
            value_class = "finite"
        return {
            "python_type": "float",
            "value_class": value_class,
            "f64_bits_hex": struct.pack(">d", value).hex(),
        }
    if value is None:
        return {"python_type": "NoneType", "value_class": "nil"}
    if type(value) is str:
        return {
            "python_type": "str",
            "value_class": "string",
            "utf8_hex": value.encode("utf-8").hex(),
        }
    if type(value) is list:
        return {
            "python_type": "list",
            "value_class": "array",
            "length": len(value),
        }
    if type(value) is dict:
        return {
            "python_type": "dict",
            "value_class": "map",
            "length": len(value),
        }
    raise TypeError(f"unsupported deterministic vector value {type(value).__name__}")


def unpack_outcome(payload: bytes) -> tuple[dict[str, object], object | None]:
    """Run the released peer's unpacker and return a JSON-safe outcome."""
    try:
        value = umsgpack.unpackb(payload)
    except Exception as error:
        return (
            {
                "result": "exception",
                "exception_type": type(error).__name__,
            },
            None,
        )
    return ({"result": "value", **describe_python_value(value)}, value)


def rns_rtt_formula_outcome(value: object) -> dict[str, object]:
    """Exercise RNS 1.4.2's exact ``max(measured_rtt, unpacked)`` formula."""
    try:
        rtt = max(LRRTT_MEASURED_RTT, value)
    except Exception as error:
        return {
            "result": "exception",
            "exception_type": type(error).__name__,
        }
    return {"result": "value", **describe_python_value(rtt)}


def lrrtt_case(
    name: str,
    payload: bytes,
    *,
    first_object: bytes | None = None,
    trailing: bytes | None = None,
) -> dict[str, object]:
    """Describe released u-msgpack and Link RTT behavior for one payload."""
    unpacked, value = unpack_outcome(payload)
    case: dict[str, object] = {
        "name": name,
        "wire_hex": payload.hex(),
        "python_unpack": unpacked,
    }
    if first_object is not None:
        case["first_object_wire_hex"] = first_object.hex()
    if trailing is not None:
        case["trailing_hex"] = trailing.hex()
    if unpacked["result"] == "value":
        case["python_rns_rtt_formula"] = rns_rtt_formula_outcome(value)
    else:
        case["python_rns_rtt_formula"] = {"result": "not_run"}
    return case


def lrrtt_messagepack_vectors() -> dict[str, object]:
    """Generate LRRTT scalar and malformed cases from vendored u-msgpack."""
    if umsgpack.__version__ != EXPECTED_UMSGPACK:
        raise RuntimeError(
            f"expected vendored u-msgpack {EXPECTED_UMSGPACK}, "
            f"found {umsgpack.__version__}"
        )

    canonical_values = ("0.001", "0.125", "1.0")
    canonical_float64 = []
    for decimal in canonical_values:
        value = float(decimal)
        payload = umsgpack.packb(value)
        if not payload.startswith(b"\xcb"):
            raise RuntimeError("released u-msgpack no longer defaults to float64")
        canonical_float64.append(
            {
                "input_decimal": decimal,
                **lrrtt_case(f"float64_{decimal}", payload),
            }
        )

    first_object = umsgpack.packb(1.0)
    trailing = b"\xc0\xc1"
    legacy_timestamp = (1_700_000_000).to_bytes(4, "big")
    decode_cases = [
        lrrtt_case(
            "float32_0.125",
            umsgpack.packb(0.125, force_float_precision="single"),
        ),
        lrrtt_case("positive_fixint_1", bytes.fromhex("01")),
        lrrtt_case("negative_fixint_minus_1", bytes.fromhex("ff")),
        lrrtt_case("uint8_1", bytes.fromhex("cc01")),
        lrrtt_case("uint16_1", bytes.fromhex("cd0001")),
        lrrtt_case("uint32_1", bytes.fromhex("ce00000001")),
        lrrtt_case("uint64_1", bytes.fromhex("cf0000000000000001")),
        lrrtt_case("int8_minus_1", bytes.fromhex("d0ff")),
        lrrtt_case("int16_minus_1", bytes.fromhex("d1ffff")),
        lrrtt_case("int32_minus_1", bytes.fromhex("d2ffffffff")),
        lrrtt_case("int64_minus_1", bytes.fromhex("d3ffffffffffffffff")),
        lrrtt_case("boolean_false", umsgpack.packb(False)),
        lrrtt_case("boolean_true", umsgpack.packb(True)),
        lrrtt_case("float64_positive_infinity", bytes.fromhex("cb7ff0000000000000")),
        lrrtt_case("float64_negative_infinity", bytes.fromhex("cbfff0000000000000")),
        lrrtt_case("float64_nan_payload_1", bytes.fromhex("cb7ff8000000000001")),
        lrrtt_case(
            "float64_1.0_with_trailing_nil_and_reserved",
            first_object + trailing,
            first_object=first_object,
            trailing=trailing,
        ),
        lrrtt_case(
            "legacy_raw_u32_timestamp",
            legacy_timestamp,
            first_object=legacy_timestamp[:1],
            trailing=legacy_timestamp[1:],
        ),
        lrrtt_case("nil", umsgpack.packb(None)),
        lrrtt_case("string_1.0", umsgpack.packb("1.0")),
        lrrtt_case("array_float64_1.0", umsgpack.packb([1.0])),
        lrrtt_case("map_rtt_float64_1.0", umsgpack.packb({"rtt": 1.0})),
        lrrtt_case("empty", b""),
        lrrtt_case("truncated_float64_1.0", first_object[:5]),
        lrrtt_case("reserved_code", b"\xc1"),
    ]

    source = Path(umsgpack.__file__)
    return {
        "origin": (
            "decoded by RNS 1.4.2's vendored RNS.vendor.umsgpack; canonical "
            "float64 cases are encoded by that same module"
        ),
        "scope": (
            "This records released-peer behavior; firmware conformance separately "
            "verifies pending-handshake numeric payload semantics."
        ),
        "umsgpack_version": umsgpack.__version__,
        "umsgpack_source_sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
        "measured_rtt_decimal": str(LRRTT_MEASURED_RTT),
        "rns_rtt_formula": "max(0.25, umsgpack.unpackb(plaintext))",
        "canonical_float64": canonical_float64,
        "decode_cases": decode_cases,
    }


def f64_timing_value(value: float | None) -> dict[str, str] | None:
    """Describe one finite Python timing value without JSON float ambiguity."""
    if value is None:
        return None
    if type(value) is not float or not math.isfinite(value):
        raise TypeError("LRRTT lifecycle timings must be finite Python floats")
    return {
        "decimal": repr(value),
        "f64_bits_hex": struct.pack(">d", value).hex(),
    }


def timing_interval_value(value: int | float) -> dict[str, str]:
    """Preserve Python's numeric type for derived keepalive intervals."""
    if type(value) is int:
        return {"python_type": "int", "decimal": str(value)}
    described = f64_timing_value(value)
    if described is None:
        raise TypeError("LRRTT lifecycle intervals cannot be None")
    return {"python_type": "float", **described}


def link_state_name(status: int) -> str:
    """Give the released Link status constants stable corpus names."""
    names = {
        RNS.Link.PENDING: "PENDING",
        RNS.Link.HANDSHAKE: "HANDSHAKE",
        RNS.Link.ACTIVE: "ACTIVE",
        RNS.Link.STALE: "STALE",
        RNS.Link.CLOSED: "CLOSED",
    }
    try:
        return names[status]
    except KeyError as error:
        raise RuntimeError(f"unexpected released Link state {status}") from error


def lrrtt_request_time_ordering(identity: RNS.Identity) -> dict[str, object]:
    """Execute released Link setup paths around deterministic send boundaries."""
    link_module = import_module("RNS.Link")
    packet_module = import_module("RNS.Packet")
    outgoing_destination = RNS.Destination(
        identity,
        RNS.Destination.OUT,
        RNS.Destination.SINGLE,
        "reticulum_rs_firmware",
        "lrrtt_timing",
    )

    initiator_clock = ScriptedClock(
        [
            ("request_time_sample", 10.0),
            ("last_outbound_sample", 10.5),
        ]
    )

    def observe_link_request_egress(packet: RNS.Packet) -> bool:
        if packet.packet_type != RNS.Packet.LINKREQUEST:
            raise RuntimeError("initiator probe egress was not a link request")
        initiator_clock.mark("transport_outbound_link_request")
        packet.sent = True
        return True

    with (
        patch.object(link_module, "time", initiator_clock),
        patch.object(packet_module, "time", initiator_clock),
        patch.object(RNS.Transport, "hops_to", return_value=1),
        patch.object(
            RNS.Transport,
            "next_hop_interface_hw_mtu",
            return_value=None,
        ),
        patch.object(RNS.Transport, "register_link"),
        patch.object(
            RNS.Transport,
            "outbound",
            new=observe_link_request_egress,
        ),
        patch.object(RNS.Link, "start_watchdog"),
    ):
        initiator = RNS.Link(outgoing_destination)
    initiator_clock.assert_exhausted()

    responder_clock = ScriptedClock(
        [
            ("proof_packet_last_outbound_sample", 20.0),
            ("proof_had_outbound_sample", 20.125),
            ("request_time_sample", 20.25),
            ("last_inbound_sample", 20.5),
        ]
    )
    owner = SimpleNamespace(identity=fixed_identity("lrrtt-responder"))
    peer_public_key = fixed_identity("lrrtt-initiator").get_public_key()
    interface = object()
    receiving_destination = RNS.Destination(
        owner.identity,
        RNS.Destination.IN,
        RNS.Destination.SINGLE,
        "reticulum_rs_firmware",
        "lrrtt_timing_responder",
    )
    request_packet = RNS.Packet(
        receiving_destination,
        peer_public_key,
        packet_type=RNS.Packet.LINKREQUEST,
    )
    request_packet.hops = 1
    request_packet.pack()
    request_packet.receiving_interface = interface
    request_packet.rssi = 0.0
    request_packet.snr = 0.0
    request_packet.q = 0.0

    def observe_link_proof_egress(packet: RNS.Packet) -> bool:
        if (
            packet.packet_type != RNS.Packet.PROOF
            or packet.context != RNS.Packet.LRPROOF
        ):
            raise RuntimeError("responder probe egress was not an LRPROOF")
        responder_clock.mark("transport_outbound_link_proof")
        packet.sent = True
        return True

    with (
        patch.object(link_module, "time", responder_clock),
        patch.object(packet_module, "time", responder_clock),
        patch.object(RNS.Link, "start_watchdog"),
        patch.object(RNS.Transport, "register_link"),
        patch.object(
            RNS.Transport,
            "outbound",
            new=observe_link_proof_egress,
        ),
    ):
        responder = RNS.Link.validate_request(
            owner,
            peer_public_key,
            request_packet,
        )
    responder_clock.assert_exhausted()
    if responder is None:
        raise RuntimeError("released RNS rejected the deterministic LRRTT request probe")

    return {
        "initiator": {
            "released_methods": ["RNS.Link.__init__", "RNS.Packet.send"],
            "observed_event_order": initiator_clock.events,
            "request_time": f64_timing_value(initiator.request_time),
            "last_outbound": f64_timing_value(initiator.last_outbound),
            "semantic": "request_time is sampled before link request send",
        },
        "responder": {
            "released_methods": [
                "RNS.Link.validate_request",
                "RNS.Link.prove",
                "RNS.Packet.send",
            ],
            "observed_event_order": responder_clock.events,
            "request_time": f64_timing_value(responder.request_time),
            "last_inbound": f64_timing_value(responder.last_inbound),
            "semantic": "request_time is sampled after link proof send returns",
        },
    }


class ProbeToken:
    """Minimal authenticated-decrypt boundary used by released Link.decrypt."""

    def __init__(self, plaintext: bytes | None, *, fail_authentication: bool = False):
        self.plaintext = plaintext
        self.fail_authentication = fail_authentication

    def decrypt(self, _ciphertext: bytes) -> bytes:
        if self.fail_authentication:
            raise ValueError("deterministic authentication failure")
        if self.plaintext is None:
            raise RuntimeError("successful probe decrypt requires plaintext")
        return self.plaintext


def make_lrrtt_responder_probe(
    callback_observations: list[dict[str, object]],
    teardown_observations: list[str],
) -> RNS.Link:
    """Build only the fields consumed by released Link.receive/rtt_packet."""
    link = object.__new__(RNS.Link)
    link.initiator = False
    link.status = RNS.Link.HANDSHAKE
    link.request_time = 100.0
    link.rtt = None
    link.activated_at = None
    link.expected_hops = None
    link.establishment_cost = 100
    link.establishment_rate = None
    link.keepalive = RNS.Link.KEEPALIVE
    link.stale_time = RNS.Link.STALE_TIME
    link.attached_interface = object()
    link.last_inbound = 0.0
    link.last_outbound = 0.0
    link.last_keepalive = 0.0
    link.last_proof = 0.0
    link.last_data = 0.0
    link.rx = 0
    link.rxbytes = 0
    link.link_id = bytes(16)
    link.token = ProbeToken(umsgpack.packb(0.125))
    link.derived_key = None
    link._Link__update_phy_stats = lambda *_args, **_kwargs: None

    def established_callback(callback_link: RNS.Link) -> None:
        callback_observations.append(
            {
                "state": link_state_name(callback_link.status),
                "rtt": f64_timing_value(callback_link.rtt),
                "activated_at": f64_timing_value(callback_link.activated_at),
                "expected_hops": callback_link.expected_hops,
            }
        )

    link.owner = SimpleNamespace(
        callbacks=SimpleNamespace(link_established=established_callback)
    )

    def observe_teardown() -> None:
        teardown_observations.append("teardown")
        link.status = RNS.Link.CLOSED

    # The actual Link.rtt_packet method decides whether this boundary is called;
    # the network-emitting teardown implementation is replaced for determinism.
    link.teardown = observe_teardown
    return link


def lrrtt_responder_lifecycle() -> dict[str, object]:
    """Execute repeated LRRTT DATA packets through released Link.receive."""
    link_module = import_module("RNS.Link")
    callback_observations: list[dict[str, object]] = []
    teardown_observations: list[str] = []
    link = make_lrrtt_responder_probe(
        callback_observations,
        teardown_observations,
    )

    def run_case(
        name: str,
        *,
        clock_samples: list[tuple[str, float]],
        token: ProbeToken,
        ciphertext_marker: int,
        hops: int,
        forced_state: int | None = None,
    ) -> dict[str, object]:
        if forced_state is not None:
            link.status = forced_state
        state_before = link.status
        request_time_before = link.request_time
        rtt_before = link.rtt
        activated_before = link.activated_at
        callback_count_before = len(callback_observations)
        teardown_count_before = len(teardown_observations)
        link.token = token
        ciphertext = b"deterministic-encrypted-lrrt" + bytes([ciphertext_marker])
        packet = SimpleNamespace(
            data=ciphertext,
            context=RNS.Packet.LRRTT,
            packet_type=RNS.Packet.DATA,
            hops=hops,
            receiving_interface=link.attached_interface,
        )
        clock = ScriptedClock(clock_samples)
        with patch.object(link_module, "time", clock):
            RNS.Link.receive(link, packet)
        clock.assert_exhausted()
        return {
            "name": name,
            "ciphertext_hex": ciphertext.hex(),
            "ciphertext_provenance": (
                "case-unique synthetic bytes passed directly to RNS.Link.receive"
            ),
            "state_before": link_state_name(state_before),
            "state_after": link_state_name(link.status),
            "observed_clock_order": clock.events,
            "request_time_before": f64_timing_value(request_time_before),
            "request_time_after": f64_timing_value(link.request_time),
            "rtt_before": f64_timing_value(rtt_before),
            "rtt_after": f64_timing_value(link.rtt),
            "activated_at_before": f64_timing_value(activated_before),
            "activated_at_after": f64_timing_value(link.activated_at),
            "last_inbound_after": f64_timing_value(link.last_inbound),
            "last_data_after": f64_timing_value(link.last_data),
            "expected_hops_after": link.expected_hops,
            "keepalive_after": timing_interval_value(link.keepalive),
            "stale_time_after": timing_interval_value(link.stale_time),
            "callback_delta": len(callback_observations) - callback_count_before,
            "callback_count_after": len(callback_observations),
            "teardown_delta": len(teardown_observations) - teardown_count_before,
            "teardown_count_after": len(teardown_observations),
            "rx_after": link.rx,
            "rxbytes_after": link.rxbytes,
        }

    cases = [
        run_case(
            "handshake_valid",
            clock_samples=[
                ("receive_liveness_sample", 100.125),
                ("measured_rtt_sample", 100.25),
                ("activation_sample", 100.375),
            ],
            token=ProbeToken(umsgpack.packb(0.125)),
            ciphertext_marker=1,
            hops=2,
        ),
        run_case(
            "active_valid_repeat",
            clock_samples=[
                ("receive_liveness_sample", 101.125),
                ("measured_rtt_sample", 101.25),
                ("activation_sample", 101.375),
            ],
            token=ProbeToken(umsgpack.packb(0.125)),
            ciphertext_marker=2,
            hops=3,
        ),
        run_case(
            "stale_valid_repeat",
            clock_samples=[
                ("receive_liveness_sample", 102.125),
                ("measured_rtt_sample", 102.25),
                ("activation_sample", 102.375),
            ],
            token=ProbeToken(umsgpack.packb(0.125)),
            ciphertext_marker=3,
            hops=4,
            forced_state=RNS.Link.STALE,
        ),
        run_case(
            "stale_decrypt_failure_repeat",
            clock_samples=[
                ("receive_liveness_sample", 103.125),
                ("measured_rtt_sample", 103.25),
            ],
            token=ProbeToken(None, fail_authentication=True),
            ciphertext_marker=4,
            hops=5,
            forced_state=RNS.Link.STALE,
        ),
        run_case(
            "active_authenticated_malformed_repeat",
            clock_samples=[
                ("receive_liveness_sample", 104.125),
                ("measured_rtt_sample", 104.25),
            ],
            token=ProbeToken(bytes.fromhex("c1")),
            ciphertext_marker=5,
            hops=6,
        ),
    ]
    ciphertexts = [case["ciphertext_hex"] for case in cases]
    if len(set(ciphertexts)) != len(ciphertexts):
        raise RuntimeError("LRRTT lifecycle probe ciphertexts must be case-unique")
    return {
        "released_entrypoint": "RNS.Link.receive -> RNS.Link.rtt_packet",
        "ingress_scope": (
            "each case is passed directly to RNS.Link.receive; RNS.Transport "
            "inbound exact-replay deduplication is outside this corpus"
        ),
        "transport_exact_replay_dedup_exercised": False,
        "request_time_semantic": (
            "immutable responder request_time; each valid repeat measures from "
            "the original post-proof sample"
        ),
        "peer_rtt_wire_hex": umsgpack.packb(0.125).hex(),
        "authenticated_malformed_plaintext_hex": "c1",
        "teardown_boundary": (
            "RNS.Link.rtt_packet calls Link.teardown; the probe replaces that "
            "method with a recorder that sets CLOSED, so teardown packet "
            "egress, key purge and close callbacks are not exercised"
        ),
        "cases": cases,
        "callback_observations": callback_observations,
    }


def lrrtt_lifecycle_vectors(identity: RNS.Identity) -> dict[str, object]:
    """Freeze released RNS 1.4.2 LRRTT timing and repeat-state behavior."""
    link_source = Path(import_module("RNS.Link").__file__)
    packet_source = Path(import_module("RNS.Packet").__file__)
    return {
        "origin": (
            "source-hash-backed deterministic method probes: request ordering "
            "executes released Link.__init__, Link.validate_request, Link.prove "
            "and Packet.send through a recorded Transport.outbound boundary; "
            "lifecycle cases directly invoke released Link.receive, "
            "Link.rtt_packet and Link.decrypt on a field-scaffolded Link"
        ),
        "link_source_sha256": hashlib.sha256(link_source.read_bytes()).hexdigest(),
        "packet_source_sha256": hashlib.sha256(packet_source.read_bytes()).hexdigest(),
        "clock": "Python time.time() float seconds",
        "probe_scaffolding": {
            "request_time_ordering": [
                "fixed identities and an actual RNS.Packet link request",
                "scripted RNS.Link and RNS.Packet module clocks",
                "fixed Transport hop and next-hop MTU lookups for the initiator",
                "no-op Transport.register_link",
                "recorded Transport.outbound instead of interface/network egress",
                "no-op Link.start_watchdog",
            ],
            "responder_lifecycle": [
                "Link allocated without its constructor and required fields populated",
                "scripted RNS.Link module clock",
                "ProbeToken substituted at the cryptographic token decrypt boundary",
                "no-op Link physical-statistics updater",
                "recording owner link-established callback",
                "recording Link.teardown replacement that sets CLOSED",
                "case-unique synthetic packets passed directly to Link.receive",
            ],
        },
        "request_time_ordering": lrrtt_request_time_ordering(identity),
        "responder_lifecycle": lrrtt_responder_lifecycle(),
    }


def proof_strategy_vectors(identity: RNS.Identity) -> dict[str, object]:
    """Exercise released RNS destination delivery and proof callback ordering."""
    app_name = "reticulum_rs_firmware"
    aspects = ("proof_strategy",)
    inbound = RNS.Destination(
        identity,
        RNS.Destination.IN,
        RNS.Destination.SINGLE,
        app_name,
        *aspects,
    )
    outbound = RNS.Destination(
        identity,
        RNS.Destination.OUT,
        RNS.Destination.SINGLE,
        app_name,
        *aspects,
    )
    cases: list[dict[str, object]] = []
    active_events: list[str] = []
    delivered_payload: bytes | None = None
    delivered_from_packed: bool | None = None

    def on_delivery(data: bytes, packet: RNS.Packet) -> None:
        nonlocal delivered_payload, delivered_from_packed
        active_events.append("delivery")
        delivered_payload = data
        delivered_from_packed = packet.fromPacked

    def on_proof_requested(packet: RNS.Packet) -> bool:
        active_events.append("proof_requested")
        return active_decision

    def record_proof(packet: RNS.Packet, destination=None) -> None:
        del packet, destination
        active_events.append("proof")

    inbound.set_packet_callback(on_delivery)
    inbound.set_proof_requested_callback(on_proof_requested)

    active_decision = False
    definitions = (
        ("prove_none", RNS.Destination.PROVE_NONE, False),
        ("prove_all", RNS.Destination.PROVE_ALL, False),
        ("prove_app_accept", RNS.Destination.PROVE_APP, True),
        ("prove_app_reject", RNS.Destination.PROVE_APP, False),
    )
    with patch.object(RNS.Packet, "prove", record_proof):
        for name, strategy, decision in definitions:
            active_events.clear()
            active_decision = decision
            delivered_payload = None
            delivered_from_packed = None
            payload = f"rns-1.4.2-{name}".encode("ascii")
            inbound.set_proof_strategy(strategy)
            packet = RNS.Packet(outbound, payload)
            packet.pack()

            # Run the released process-global transport path. It decrypts and
            # invokes the destination callback before applying PROVE_ALL or
            # synchronously consulting the PROVE_APP callback.
            RNS.Transport.inbound(packet.raw)
            if delivered_payload != payload:
                raise RuntimeError(f"released RNS did not deliver {name}")
            if delivered_from_packed is not True:
                raise RuntimeError(f"released RNS did not unpack {name} before delivery")
            cases.append(
                {
                    "name": name,
                    "strategy": strategy,
                    "proof_requested_decision": decision,
                    "payload_hex": payload.hex(),
                    "events": list(active_events),
                    "delivered_from_packed": delivered_from_packed,
                }
            )

    destination_module = import_module("RNS.Destination")
    transport_module = import_module("RNS.Transport")
    return {
        "constants": {
            "prove_none": RNS.Destination.PROVE_NONE,
            "prove_app": RNS.Destination.PROVE_APP,
            "prove_all": RNS.Destination.PROVE_ALL,
        },
        "destination_source_sha256": hashlib.sha256(
            Path(destination_module.__file__).read_bytes()
        ).hexdigest(),
        "transport_source_sha256": hashlib.sha256(
            Path(transport_module.__file__).read_bytes()
        ).hexdigest(),
        "cases": cases,
    }


def build_vectors() -> dict[str, object]:
    peer = released_peer()
    if sys.version_info[:3] != EXPECTED_PYTHON:
        expected_python = ".".join(str(part) for part in EXPECTED_PYTHON)
        raise RuntimeError(
            f"expected Python {expected_python}, "
            f"found {sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}"
        )
    installed = verify_installed_peer(peer)

    # Reticulum requires one process-global instance before destinations are
    # registered. Force a standalone temporary instance so a developer's
    # running shared instance cannot consume the generator process or affect
    # packet delivery.
    with tempfile.TemporaryDirectory(prefix="reticulum-rs-vectors-") as config_dir:
        Path(config_dir, "config").write_text(
            "[reticulum]\n"
            "  share_instance = No\n"
            "  enable_transport = No\n"
            "\n"
            "[interfaces]\n",
            encoding="utf-8",
        )
        RNS.Reticulum(configdir=config_dir, loglevel=RNS.LOG_CRITICAL)

        identity = fixed_identity("alice")
        destination = RNS.Destination(
            identity,
            RNS.Destination.IN,
            RNS.Destination.SINGLE,
            "testapp",
            "aspect1",
        )

        # Exercise the released peer's announce implementation directly. Patch
        # only the two nondeterministic inputs that Destination.announce() reads:
        # its module-local clock binding and Identity.get_random_hash().
        fixed_timestamp = 1_700_000_000
        fixed_random_hash = bytes(RNS.Identity.TRUNCATED_HASHLENGTH // 8)
        destination_module = import_module("RNS.Destination")
        fixed_clock = SimpleNamespace(time=lambda: fixed_timestamp)
        with (
            patch.object(destination_module, "time", fixed_clock),
            patch.object(
                RNS.Identity,
                "get_random_hash",
                return_value=fixed_random_hash,
            ),
        ):
            announce_packet = destination.announce(send=False)
        announce_packet.pack()

        if not RNS.Identity.validate_announce(announce_packet):
            raise RuntimeError("Python RNS rejected its generated announce")

        if announce_packet.context_flag != RNS.Packet.FLAG_UNSET:
            raise RuntimeError("foundation fixture unexpectedly contains a ratchet")
        key_size = RNS.Identity.KEYSIZE // 8
        name_hash_size = RNS.Identity.NAME_HASH_LENGTH // 8
        signature_size = RNS.Identity.SIGLENGTH // 8
        random_hash_start = key_size + name_hash_size
        signature_start = random_hash_start + 10
        public_key = announce_packet.data[:key_size]
        random_hash = announce_packet.data[random_hash_start:signature_start]
        signature = announce_packet.data[
            signature_start : signature_start + signature_size
        ]

        plain_destination = RNS.Destination(
            None,
            RNS.Destination.OUT,
            RNS.Destination.PLAIN,
            "reticulum_rs_firmware",
            "interop",
        )
        plain_payload = b"prns migration conformance"
        plain_packet = RNS.Packet(plain_destination, plain_payload)
        plain_packet.pack()

        announce_hashable = packet_hashable_part(announce_packet.raw)
        plain_hashable = packet_hashable_part(plain_packet.raw)

        assert hashlib.sha256(announce_hashable).digest() == announce_packet.packet_hash
        assert hashlib.sha256(plain_hashable).digest() == plain_packet.packet_hash
        assert public_key == identity.get_public_key()

        return {
            "schema": 3,
            "protocol": "Reticulum",
            "lane": "released",
            "peer": {
                "package": "rns",
                "version": installed,
                "repository": peer["repository"],
                "revision": peer["revision"],
            },
            "generator": {
                "script": "interop/python/generate_rns_vectors.py",
                "requirements": "interop/python/requirements-rns-1.4.2.txt",
                "command": "python interop/python/generate_rns_vectors.py",
                "python_version": ".".join(str(part) for part in EXPECTED_PYTHON),
                "source_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                "requirements_sha256": hashlib.sha256(REQUIREMENTS.read_bytes()).hexdigest(),
                "deterministic": True,
                "notes": (
                    "The foundation packet corpus excludes encrypted packet "
                    "creation because Python correctly uses a fresh ephemeral "
                    "key and IV. LRRTT sections record released-peer decode, "
                    "RTT-formula, request-clock ordering and direct Link.receive "
                    "repeat-lifecycle behavior. The proof section runs the "
                    "released Transport.inbound path and records immediate "
                    "destination and PROVE_APP callback ordering; candidate "
                    "conformance is checked separately."
                ),
            },
            "identity": {
                "label": "alice",
                "origin": "created by Python RNS 1.4.2 from the SHA-512 of the test label",
                "normalization": "lowercase hexadecimal; no wire-byte normalization",
                "test_only": True,
                "private_key_hex": identity.get_private_key().hex(),
                "public_key_hex": public_key.hex(),
                "identity_hash_hex": identity.hash.hex(),
            },
            "announce": {
                "app_name": "testapp",
                "aspects": ["aspect1"],
                "origin": "created and packed by Python RNS 1.4.2",
                "normalization": "fixed five-byte random prefix and timestamp for reproducibility",
                "fixed_timestamp": fixed_timestamp,
                "destination_hash_hex": destination.hash.hex(),
                "name_hash_hex": destination.name_hash.hex(),
                "random_hash_hex": random_hash.hex(),
                "signature_hex": signature.hex(),
                "raw_hex": announce_packet.raw.hex(),
                "packet_hash_hex": announce_packet.packet_hash.hex(),
            },
            "plain_data": {
                "app_name": "reticulum_rs_firmware",
                "aspects": ["interop"],
                "origin": "created and packed by Python RNS 1.4.2",
                "normalization": "lowercase hexadecimal; no wire-byte normalization",
                "payload_hex": plain_payload.hex(),
                "destination_hash_hex": plain_destination.hash.hex(),
                "raw_hex": plain_packet.raw.hex(),
                "packet_hash_hex": plain_packet.packet_hash.hex(),
            },
            "proof_strategy": proof_strategy_vectors(identity),
            "lrrtt_messagepack": lrrtt_messagepack_vectors(),
            "lrrtt_lifecycle": lrrtt_lifecycle_vectors(identity),
        }


def encoded_vectors() -> str:
    return json.dumps(build_vectors(), indent=2, sort_keys=True) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument(
        "--check",
        action="store_true",
        help="fail if the committed output differs instead of rewriting it",
    )
    args = parser.parse_args()

    generated = encoded_vectors()
    expected_version = released_peer()["version"]
    if args.check:
        try:
            committed = args.output.read_text(encoding="utf-8")
        except FileNotFoundError:
            parser.error(f"missing vector file: {args.output}")
        if committed != generated:
            parser.error(
                f"{args.output} is stale; regenerate it with this script"
            )
        print(f"ok: {args.output} matches Python RNS {expected_version}")
        return 0

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(generated, encoding="utf-8")
    print(f"wrote {args.output} from Python RNS {expected_version}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
