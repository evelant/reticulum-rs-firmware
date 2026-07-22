#!/usr/bin/env python3
"""Build one signed opportunistic LXMF carrier from two E290 identity sources.

This is a qualification helper, not an onboard sender. It accepts either exact
16 MiB flash backups or identity-bound 8 KiB ``node_identity`` region reads. It
reads only the two mirrored identity records, validates their current primary
destinations, and writes public message material only. Private identity bytes
are never printed or written to a separate file.

Run this script with the isolated dependency set pinned by
``requirements-lxmf-1.0.1.txt``.
"""

from __future__ import annotations

import argparse
import hashlib
import hmac
import json
import math
import tempfile
from pathlib import Path
from typing import Any

import generate_lxmf_1_0_1_vectors as authority


FLASH_BYTES = 16 * 1024 * 1024
IDENTITY_PARTITION_OFFSET = 0x610000
IDENTITY_MIRROR_BYTES = 0x1000
IDENTITY_PARTITION_BYTES = 2 * IDENTITY_MIRROR_BYTES
IDENTITY_RECORD_BYTES = 0x100
IDENTITY_PRIVATE_KEY_BYTES = 64
IDENTITY_PRIVATE_KEY_OFFSET = 0x040
IDENTITY_RESERVED_OFFSET = 0x080
IDENTITY_DIGEST_OFFSET = 0x0C0
IDENTITY_COMMIT_OFFSET = 0x0E0
IDENTITY_CLAIM_MARKER = bytes.fromhex(
    "8d21e64a935c17b82fc069d4357aa10e"
    "f2489b631dc754ae7006db3985ec125f"
)
IDENTITY_HEADER_MAGIC = b"RTIDREC1"
IDENTITY_COMMIT_MARKER = bytes.fromhex(
    "47b20cf1689d35e41a73c856af04db91"
    "2e65f918834cb720da5b0fe674a931cd"
)
IDENTITY_DIGEST_DOMAIN = b"reticulum-rs-firmware/device-identity/record/v1\0"
MAX_OPPORTUNISTIC_CARRIER_BYTES = 383


def _hex_hash(value: str, label: str) -> str:
    normalized = value.lower()
    if len(normalized) != 32:
        raise ValueError(f"{label} must contain exactly 32 hexadecimal digits")
    try:
        bytes.fromhex(normalized)
    except ValueError as error:
        raise ValueError(f"{label} is not hexadecimal") from error
    return normalized


def _validated_mirror_key(mirror: bytearray, image: Path, label: str) -> bytearray:
    if len(mirror) != IDENTITY_MIRROR_BYTES:
        raise ValueError(f"{image} ended inside identity mirror {label}")
    record = memoryview(mirror)[:IDENTITY_RECORD_BYTES]
    if bytes(record[:0x020]) != IDENTITY_CLAIM_MARKER:
        raise ValueError(f"{image} identity mirror {label} has an invalid claim marker")
    if bytes(record[0x020:0x028]) != IDENTITY_HEADER_MAGIC:
        raise ValueError(f"{image} identity mirror {label} has an invalid header magic")
    fixed_fields = (
        int.from_bytes(record[0x028:0x02A], "little"),
        int.from_bytes(record[0x02A:0x02C], "little"),
        int.from_bytes(record[0x02C:0x02E], "little"),
        int.from_bytes(record[0x02E:0x030], "little"),
        int.from_bytes(record[0x030:0x038], "little"),
    )
    if fixed_fields != (1, 1, IDENTITY_RECORD_BYTES, IDENTITY_PRIVATE_KEY_BYTES, 1):
        raise ValueError(f"{image} identity mirror {label} has invalid format fields")
    if any(record[0x038:IDENTITY_PRIVATE_KEY_OFFSET]):
        raise ValueError(f"{image} identity mirror {label} has nonzero header padding")
    if any(record[IDENTITY_RESERVED_OFFSET:IDENTITY_DIGEST_OFFSET]):
        raise ValueError(f"{image} identity mirror {label} has nonzero reserved bytes")
    if bytes(record[IDENTITY_COMMIT_OFFSET:IDENTITY_RECORD_BYTES]) != IDENTITY_COMMIT_MARKER:
        raise ValueError(f"{image} identity mirror {label} has an invalid commit marker")
    digest = hashlib.sha256()
    digest.update(IDENTITY_DIGEST_DOMAIN)
    digest.update(record[:IDENTITY_DIGEST_OFFSET])
    if not hmac.compare_digest(
        digest.digest(), bytes(record[IDENTITY_DIGEST_OFFSET:IDENTITY_COMMIT_OFFSET])
    ):
        raise ValueError(f"{image} identity mirror {label} has an invalid record digest")
    if any(byte != 0xFF for byte in mirror[IDENTITY_RECORD_BYTES:]):
        raise ValueError(f"{image} identity mirror {label} has a programmed sector tail")

    key = bytearray(
        record[
            IDENTITY_PRIVATE_KEY_OFFSET : IDENTITY_PRIVATE_KEY_OFFSET
            + IDENTITY_PRIVATE_KEY_BYTES
        ]
    )
    if key == bytearray(IDENTITY_PRIVATE_KEY_BYTES) or key == bytearray(
        [0xFF] * IDENTITY_PRIVATE_KEY_BYTES
    ):
        for index in range(len(key)):
            key[index] = 0
        raise ValueError(f"{image} does not contain a provisioned identity")
    return key


def _read_identity_key(image: Path) -> bytearray:
    with image.open("rb") as handle:
        handle.seek(0, 2)
        image_bytes = handle.tell()
        if image_bytes == FLASH_BYTES:
            identity_offset = IDENTITY_PARTITION_OFFSET
        elif image_bytes == IDENTITY_PARTITION_BYTES:
            identity_offset = 0
        else:
            raise ValueError(
                f"{image} is {image_bytes} bytes; expected an exact 16 MiB flash "
                f"image or {IDENTITY_PARTITION_BYTES}-byte identity region"
            )
        handle.seek(identity_offset)
        mirror_a = bytearray(handle.read(IDENTITY_MIRROR_BYTES))
        mirror_b = bytearray(handle.read(IDENTITY_MIRROR_BYTES))

    key_a = bytearray()
    key_b = bytearray()
    try:
        key_a = _validated_mirror_key(mirror_a, image, "A")
        key_b = _validated_mirror_key(mirror_b, image, "B")
        if key_a != key_b:
            raise ValueError(f"{image} identity mirrors do not match")
        return key_a
    except BaseException:
        for index in range(len(key_a)):
            key_a[index] = 0
        raise
    finally:
        for owner in (key_b, mirror_a, mirror_b):
            for index in range(len(owner)):
                owner[index] = 0


def _load_board_identity(
    image: Path,
    expected_primary_hash: str,
) -> tuple[Any, Any, Any]:
    key = _read_identity_key(image)
    try:
        identity = authority.RNS.Identity(create_keys=False)
        if not identity.load_private_key(bytes(key)):
            raise ValueError(f"{image} contains an invalid Reticulum identity")
    finally:
        for index in range(len(key)):
            key[index] = 0

    primary = authority.RNS.Destination(
        identity,
        authority.RNS.Destination.IN,
        authority.RNS.Destination.SINGLE,
        "reticulum",
        "embedded-node",
    )
    delivery = authority.RNS.Destination(
        identity,
        authority.RNS.Destination.IN,
        authority.RNS.Destination.SINGLE,
        "lxmf",
        "delivery",
    )
    observed_primary = primary.hash.hex()
    if observed_primary != expected_primary_hash:
        raise ValueError(
            f"{image} primary destination is {observed_primary}, expected {expected_primary_hash}"
        )
    return identity, primary, delivery


def _configure_rns() -> tempfile.TemporaryDirectory[str]:
    root = tempfile.TemporaryDirectory(prefix="e290-lxmf-trial-")
    (Path(root.name) / "config").write_text(
        "[reticulum]\n"
        "  enable_transport = No\n"
        "  share_instance = No\n"
        "  panic_on_interface_error = No\n"
        "[logging]\n"
        "  loglevel = 0\n"
        "[interfaces]\n",
        encoding="utf-8",
    )
    authority.RNS.Reticulum(configdir=root.name)
    return root


def build_trial(args: argparse.Namespace) -> dict[str, object]:
    provenance = authority.authority_provenance()
    expected_source = _hex_hash(args.source_primary_hash, "--source-primary-hash")
    expected_destination = _hex_hash(
        args.destination_primary_hash, "--destination-primary-hash"
    )
    if not math.isfinite(args.timestamp) or args.timestamp <= 0:
        raise ValueError("--timestamp must be a positive finite number")

    rns_root = _configure_rns()
    try:
        source_identity, source_primary, source_delivery = _load_board_identity(
            args.source_flash, expected_source
        )
        destination_identity, destination_primary, destination_delivery_in = (
            _load_board_identity(args.destination_flash, expected_destination)
        )
        if source_identity.hash == destination_identity.hash:
            raise ValueError("source and destination flash images contain the same identity")

        destination_delivery = authority.RNS.Destination(
            destination_identity,
            authority.RNS.Destination.OUT,
            authority.RNS.Destination.SINGLE,
            "lxmf",
            "delivery",
        )
        if destination_delivery.hash != destination_delivery_in.hash:
            raise RuntimeError("inbound and outbound LXMF destination derivation disagreed")

        message = authority.LXMessage(
            destination_delivery,
            source_delivery,
            args.content.encode("utf-8"),
            args.title.encode("utf-8"),
            fields={},
            desired_method=authority.LXMessage.OPPORTUNISTIC,
        )
        message.timestamp = args.timestamp
        message.pack()

        if message.method != authority.LXMessage.OPPORTUNISTIC:
            raise ValueError("message did not fit opportunistic LXMF delivery")
        if message.representation != authority.LXMessage.PACKET:
            raise ValueError("message did not fit packet representation")
        full_wire = bytes(message.packed)
        carrier = full_wire[authority.LXMessage.DESTINATION_LENGTH :]
        if len(carrier) > MAX_OPPORTUNISTIC_CARRIER_BYTES:
            raise ValueError(
                f"carrier is {len(carrier)} bytes; E290 DATA limit is "
                f"{MAX_OPPORTUNISTIC_CARRIER_BYTES}"
            )
        if full_wire != destination_delivery.hash + carrier:
            raise RuntimeError("opportunistic carrier normalization invariant failed")
        if message.source_hash != source_delivery.hash:
            raise RuntimeError("LXMF source destination binding changed")
        if message.destination_hash != destination_delivery.hash:
            raise RuntimeError("LXMF destination binding changed")

        signature = full_wire[
            2 * authority.LXMessage.DESTINATION_LENGTH :
            2 * authority.LXMessage.DESTINATION_LENGTH
            + authority.LXMessage.SIGNATURE_LENGTH
        ]
        payload = full_wire[
            2 * authority.LXMessage.DESTINATION_LENGTH
            + authority.LXMessage.SIGNATURE_LENGTH :
        ]
        hashed_part = destination_delivery.hash + source_delivery.hash + payload
        message_id = authority.RNS.Identity.full_hash(hashed_part)
        if message_id != message.message_id:
            raise RuntimeError("Python LXMF message ID verification failed")
        if not source_identity.validate(signature, hashed_part + message_id):
            raise RuntimeError("Python LXMF signature verification failed")

        return {
            "schema": "reticulum.e290-lxmf-trial.v1",
            "authority": {
                "lxmf_version": provenance["version"],
                "lxmf_revision": provenance["revision"],
                "rns_version": provenance["reticulum"]["version"],
                "rns_revision": provenance["reticulum"]["revision"],
            },
            "source_primary_hash": source_primary.hash.hex(),
            "source_lxmf_hash": source_delivery.hash.hex(),
            "destination_primary_hash": destination_primary.hash.hex(),
            "destination_lxmf_hash": destination_delivery.hash.hex(),
            "message_id": message_id.hex(),
            "full_wire_sha256": hashlib.sha256(full_wire).hexdigest(),
            "carrier_sha256": hashlib.sha256(carrier).hexdigest(),
            "carrier_bytes": len(carrier),
            "carrier_hex": carrier.hex(),
        }
    finally:
        rns_root.cleanup()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--source-flash",
        type=Path,
        required=True,
        help="exact 16 MiB flash backup or 8 KiB node_identity region read",
    )
    parser.add_argument(
        "--destination-flash",
        type=Path,
        required=True,
        help="exact 16 MiB flash backup or 8 KiB node_identity region read",
    )
    parser.add_argument("--source-primary-hash", required=True)
    parser.add_argument("--destination-primary-hash", required=True)
    parser.add_argument("--timestamp", type=float, required=True)
    parser.add_argument("--title", required=True)
    parser.add_argument("--content", required=True)
    parser.add_argument("--output", type=Path, required=True)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    if args.output.exists():
        raise FileExistsError(f"refusing to overwrite {args.output}")
    record = build_trial(args)
    with args.output.open("x", encoding="utf-8") as handle:
        json.dump(record, handle, indent=2, sort_keys=True)
        handle.write("\n")


if __name__ == "__main__":
    main()
