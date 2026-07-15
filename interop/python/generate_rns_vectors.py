#!/usr/bin/env python3
"""Generate the small deterministic Reticulum 1.3.8 foundation corpus."""

from __future__ import annotations

import argparse
import hashlib
import importlib.metadata
from importlib import import_module
import json
from pathlib import Path
import re
import sys
import tempfile
from types import SimpleNamespace
from unittest.mock import patch

import RNS


DEFAULT_OUTPUT = Path(__file__).parents[1] / "vectors" / "rns-1.3.8.json"
PEER_MANIFEST = Path(__file__).parents[1] / "peers.toml"
REQUIREMENTS = Path(__file__).with_name("requirements-rns-1.3.8.txt")
EXPECTED_PYTHON = (3, 13, 7)


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
    """Create the same stable 64-byte private identity used by Rete's corpus."""
    identity = RNS.Identity(create_keys=False)
    identity.load_private_key(hashlib.sha512(label.encode("utf-8")).digest())
    return identity


def verify_installed_peer(peer: dict[str, str]) -> str:
    """Bind the imported RNS distribution to the pinned Git revision."""
    installed = importlib.metadata.version("rns")
    if installed != peer["version"]:
        raise RuntimeError(
            f"expected rns {peer['version']}, found {installed}; "
            "install interop/python/requirements-rns-1.3.8.txt"
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
    # registered. A fresh temporary config creates no persistent node state.
    with tempfile.TemporaryDirectory(prefix="reticulum-rs-vectors-") as config_dir:
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
            "phase0",
        )
        plain_payload = b"rete foundation conformance"
        plain_packet = RNS.Packet(plain_destination, plain_payload)
        plain_packet.pack()

        announce_hashable = packet_hashable_part(announce_packet.raw)
        plain_hashable = packet_hashable_part(plain_packet.raw)

        assert hashlib.sha256(announce_hashable).digest() == announce_packet.packet_hash
        assert hashlib.sha256(plain_hashable).digest() == plain_packet.packet_hash
        assert public_key == identity.get_public_key()

        return {
            "schema": 1,
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
                "requirements": "interop/python/requirements-rns-1.3.8.txt",
                "command": "python interop/python/generate_rns_vectors.py",
                "python_version": ".".join(str(part) for part in EXPECTED_PYTHON),
                "source_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                "requirements_sha256": hashlib.sha256(REQUIREMENTS.read_bytes()).hexdigest(),
                "deterministic": True,
                "notes": (
                    "The corpus excludes encrypted packet creation because "
                    "Python correctly uses a fresh ephemeral key and IV."
                ),
            },
            "identity": {
                "label": "alice",
                "origin": "created by Python RNS 1.3.8 from the SHA-512 of the test label",
                "normalization": "lowercase hexadecimal; no wire-byte normalization",
                "test_only": True,
                "private_key_hex": identity.get_private_key().hex(),
                "public_key_hex": public_key.hex(),
                "identity_hash_hex": identity.hash.hex(),
            },
            "announce": {
                "app_name": "testapp",
                "aspects": ["aspect1"],
                "origin": "created and packed by Python RNS 1.3.8",
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
                "aspects": ["phase0"],
                "origin": "created and packed by Python RNS 1.3.8",
                "normalization": "lowercase hexadecimal; no wire-byte normalization",
                "payload_hex": plain_payload.hex(),
                "destination_hash_hex": plain_destination.hash.hex(),
                "raw_hex": plain_packet.raw.hex(),
                "packet_hash_hex": plain_packet.packet_hash.hex(),
            },
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
