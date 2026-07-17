#!/usr/bin/env python3
"""Identity-safe host gates for E290 flash reads and writes.

The tool maps an ESP32-S3 native-USB serial identity to its macOS callout
device using the complete IORegistry stream, runs ``espflash board-info`` while
leaving the target in the loader, and validates every fact needed before a
plaintext read or write. Its action subcommands then own loader-preserving
full-flash backup, qualification-image write, hash-bound merged-image write and
readback, or post-run image-range verification. Each operation remains bound to
the qualified capacity and identity. It prints the qualified callout path only
as a final diagnostic, never as authority for a later shell command.
"""

from __future__ import annotations

import argparse
from dataclasses import asdict, dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
from typing import Callable, Sequence


ESPRESSIF_VENDOR_ID = 0x303A
ESP32S3_USB_PRODUCT_ID = 0x1001
SUPPORTED_FLASH_BYTES = frozenset((8 * 1024 * 1024, 16 * 1024 * 1024))
CONFIRMED_HF_MODULE = "HT-RA62-HF"
FLASH_DETECTION_FAILURES = (
    re.compile(r"could not detect flash size", re.IGNORECASE),
    re.compile(r"defaulting to 4\s*mb", re.IGNORECASE),
    re.compile(r"flashid=.*sizeid=", re.IGNORECASE),
)

IOREG_NODE = re.compile(
    r"^(?P<prefix>[ |]*)\+-o [^\n]*<class (?P<class_name>[^,>]+),",
    re.MULTILINE,
)
USB_SERIAL = re.compile(r'"kUSBSerialNumberString"\s*=\s*"([^"]+)"')
CALLOUT_DEVICE = re.compile(r'"IOCalloutDevice"\s*=\s*"([^"]+)"')
VENDOR_ID = re.compile(r'"idVendor"\s*=\s*(\d+)')
PRODUCT_ID = re.compile(r'"idProduct"\s*=\s*(\d+)')

CHIP_TYPE = re.compile(r"^Chip type:\s+(\S+)(?:\s|$)", re.MULTILINE)
FLASH_SIZE = re.compile(r"^Flash size:\s+(\d+)\s*(KB|MB)$", re.MULTILINE)
MAC_ADDRESS = re.compile(
    r"^MAC address:\s+([0-9a-f]{2}(?::[0-9a-f]{2}){5})$",
    re.IGNORECASE | re.MULTILINE,
)
SECURE_BOOT = re.compile(r"^Secure Boot:\s+(\S+)$", re.MULTILINE)
FLASH_ENCRYPTION = re.compile(r"^Flash Encryption:\s+(\S+)$", re.MULTILINE)
EVIDENCE_SUFFIXES = (
    ".ioreg-before.txt",
    ".board-info.stdout.txt",
    ".board-info.stderr.txt",
    ".ioreg-after.txt",
    ".board-info.verified.json",
)


class QualificationError(RuntimeError):
    """The host evidence did not establish a safe, expected target."""


class PostWriteEvidenceError(QualificationError):
    """The device write completed but its verification evidence did not."""


@dataclass(frozen=True)
class UsbDevice:
    usb_serial: str
    callout_device: str
    vendor_id: int
    product_id: int


@dataclass(frozen=True)
class BoardInfo:
    chip: str
    flash_bytes: int
    mac: str
    secure_boot: str
    flash_encryption: str


@dataclass
class _IoregDeviceRecord:
    own_text: str
    callouts: list[str]


def _single_match(pattern: re.Pattern[str], text: str, label: str) -> str:
    matches = pattern.findall(text)
    if len(matches) != 1:
        raise QualificationError(
            f"board-info must contain exactly one {label}, found {len(matches)}"
        )
    value = matches[0]
    if isinstance(value, tuple):
        raise AssertionError("tuple-valued pattern requires explicit handling")
    return value


def parse_ioreg(text: str) -> tuple[UsbDevice, ...]:
    """Associate serial callouts with their nearest USB-device ancestor."""

    nodes = list(IOREG_NODE.finditer(text))
    records: list[_IoregDeviceRecord] = []
    # Each frame is (tree-column, nearest IOUSBHostDevice record). Tracking
    # every node, rather than slicing only top-level devices, prevents a hub's
    # serial or a sibling callout from being borrowed by a nested board.
    stack: list[tuple[int, int | None]] = []
    for index, node in enumerate(nodes):
        column = node.group(0).index("+-o")
        while stack and stack[-1][0] >= column:
            stack.pop()
        owner = stack[-1][1] if stack else None
        own_end = nodes[index + 1].start() if index + 1 < len(nodes) else len(text)
        own_text = text[node.start() : own_end]
        class_name = node.group("class_name")
        if class_name == "IOUSBHostDevice":
            owner = len(records)
            records.append(_IoregDeviceRecord(own_text=own_text, callouts=[]))
        elif class_name == "IOSerialBSDClient" and owner is not None:
            records[owner].callouts.extend(CALLOUT_DEVICE.findall(own_text))
        stack.append((column, owner))

    devices: list[UsbDevice] = []
    for record in records:
        serials = USB_SERIAL.findall(record.own_text)
        vendors = VENDOR_ID.findall(record.own_text)
        products = PRODUCT_ID.findall(record.own_text)
        callouts = record.callouts
        if not serials or not callouts or not vendors or not products:
            continue
        serial = serials[0]
        if any(candidate != serial for candidate in serials):
            raise QualificationError(
                f"IORegistry subtree contains inconsistent USB serials: {serials!r}"
            )
        unique_callouts = tuple(dict.fromkeys(callouts))
        if len(unique_callouts) != 1:
            raise QualificationError(
                f"USB serial {serial!r} has {len(unique_callouts)} callout devices"
            )
        devices.append(
            UsbDevice(
                usb_serial=serial,
                callout_device=unique_callouts[0],
                vendor_id=int(vendors[0]),
                product_id=int(products[0]),
            )
        )
    return tuple(devices)


def select_usb_device(devices: Sequence[UsbDevice], expected_serial: str) -> UsbDevice:
    matches = [
        device
        for device in devices
        if device.usb_serial.casefold() == expected_serial.casefold()
    ]
    if len(matches) != 1:
        raise QualificationError(
            f"USB serial {expected_serial!r} must map to exactly one device, "
            f"found {len(matches)}"
        )
    device = matches[0]
    if device.vendor_id != ESPRESSIF_VENDOR_ID:
        raise QualificationError(
            f"USB serial {expected_serial!r} has unexpected vendor "
            f"0x{device.vendor_id:04x}"
        )
    if device.product_id != ESP32S3_USB_PRODUCT_ID:
        raise QualificationError(
            f"USB serial {expected_serial!r} has unexpected product "
            f"0x{device.product_id:04x}"
        )
    if not device.callout_device.startswith("/dev/cu."):
        raise QualificationError(
            f"refusing non-callout serial path {device.callout_device!r}"
        )
    return device


def parse_board_info(
    stdout: str,
    stderr: str,
    *,
    expected_mac: str,
    expected_flash_bytes: int,
) -> BoardInfo:
    combined = f"{stdout}\n{stderr}"
    for pattern in FLASH_DETECTION_FAILURES:
        match = pattern.search(combined)
        if match is not None:
            raise QualificationError(
                f"board-info contains flash-detection failure: {match.group(0)!r}"
            )

    chip = _single_match(CHIP_TYPE, combined, "chip type").lower()
    flash_matches = FLASH_SIZE.findall(combined)
    if len(flash_matches) != 1:
        raise QualificationError(
            "board-info must contain exactly one flash size, "
            f"found {len(flash_matches)}"
        )
    flash_value, flash_unit = flash_matches[0]
    multiplier = 1024 if flash_unit == "KB" else 1024 * 1024
    flash_bytes = int(flash_value) * multiplier
    mac = _single_match(MAC_ADDRESS, combined, "MAC address").lower()
    secure_boot = _single_match(SECURE_BOOT, combined, "secure-boot state")
    flash_encryption = _single_match(
        FLASH_ENCRYPTION, combined, "flash-encryption state"
    )

    if chip != "esp32s3":
        raise QualificationError(f"expected esp32s3, found {chip!r}")
    if mac != expected_mac.lower():
        raise QualificationError(f"expected MAC {expected_mac.lower()}, found {mac}")
    if flash_bytes not in SUPPORTED_FLASH_BYTES:
        raise QualificationError(
            f"E290 flash capacity must be 8 or 16 MiB, found {flash_bytes} bytes"
        )
    if flash_bytes != expected_flash_bytes:
        raise QualificationError(
            f"expected {expected_flash_bytes} flash bytes, found {flash_bytes}"
        )
    if secure_boot != "Disabled":
        raise QualificationError(f"secure boot is not disabled: {secure_boot!r}")
    if flash_encryption != "Disabled":
        raise QualificationError(
            f"flash encryption is not disabled: {flash_encryption!r}"
        )
    return BoardInfo(
        chip=chip,
        flash_bytes=flash_bytes,
        mac=mac,
        secure_boot=secure_boot,
        flash_encryption=flash_encryption,
    )


def read_ioreg() -> str:
    result = subprocess.run(
        ["ioreg", "-r", "-c", "IOUSBHostDevice", "-l", "-w0"],
        check=True,
        capture_output=True,
        text=True,
    )
    if result.stderr:
        raise QualificationError(f"ioreg wrote to stderr: {result.stderr.strip()}")
    return result.stdout


def is_character_device(path: str) -> bool:
    return stat.S_ISCHR(os.stat(path).st_mode)


def _evidence_path(prefix: Path, suffix: str) -> Path:
    return prefix.parent / f"{prefix.name}{suffix}"


def _write_evidence(path: Path, contents: str) -> None:
    with path.open("x") as output:
        output.write(contents)


def qualify_port(
    *,
    expected_usb_serial: str,
    expected_mac: str,
    expected_flash_bytes: int,
    evidence_prefix: Path,
    ioreg_reader: Callable[[], str] = read_ioreg,
    command_runner: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
    path_is_character_device: Callable[[str], bool] = is_character_device,
) -> tuple[UsbDevice, BoardInfo]:
    evidence_prefix.parent.mkdir(parents=True, exist_ok=True)
    evidence_paths = [
        _evidence_path(evidence_prefix, suffix) for suffix in EVIDENCE_SUFFIXES
    ]
    collisions = [str(path) for path in evidence_paths if os.path.lexists(path)]
    if collisions:
        raise QualificationError(
            f"refusing to overwrite existing evidence paths: {collisions!r}"
        )

    before_ioreg = ioreg_reader()
    before = select_usb_device(parse_ioreg(before_ioreg), expected_usb_serial)
    try:
        is_character = path_is_character_device(before.callout_device)
    except OSError as error:
        raise QualificationError(
            f"could not stat serial path {before.callout_device}: {error}"
        ) from error
    if not is_character:
        raise QualificationError(
            f"serial path is not a character device: {before.callout_device}"
        )
    _write_evidence(
        _evidence_path(evidence_prefix, ".ioreg-before.txt"), before_ioreg
    )

    result = command_runner(
        [
            "espflash",
            "board-info",
            "--chip",
            "esp32s3",
            "--port",
            before.callout_device,
            "--after",
            "no-reset",
            "--non-interactive",
            "--skip-update-check",
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    _write_evidence(
        _evidence_path(evidence_prefix, ".board-info.stdout.txt"), result.stdout
    )
    _write_evidence(
        _evidence_path(evidence_prefix, ".board-info.stderr.txt"), result.stderr
    )
    if result.returncode != 0:
        raise QualificationError(f"espflash board-info exited {result.returncode}")

    board = parse_board_info(
        result.stdout,
        result.stderr,
        expected_mac=expected_mac,
        expected_flash_bytes=expected_flash_bytes,
    )
    after_ioreg = ioreg_reader()
    _write_evidence(
        _evidence_path(evidence_prefix, ".ioreg-after.txt"), after_ioreg
    )
    after = select_usb_device(parse_ioreg(after_ioreg), expected_usb_serial)
    if after != before:
        raise QualificationError(
            f"USB mapping changed during board-info: before={before!r} after={after!r}"
        )
    _write_evidence(
        _evidence_path(evidence_prefix, ".board-info.verified.json"),
        json.dumps(
            {"usb": asdict(before), "board_info": asdict(board)},
            indent=2,
            sort_keys=True,
        )
        + "\n",
    )
    return before, board


def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while block := source.read(1024 * 1024):
            digest.update(block)
    return digest.hexdigest()


def _reserve_action_evidence(prefix: Path, suffixes: Sequence[str]) -> None:
    collisions = [
        str(_evidence_path(prefix, suffix))
        for suffix in suffixes
        if os.path.lexists(_evidence_path(prefix, suffix))
    ]
    if collisions:
        raise QualificationError(
            f"refusing to overwrite existing action evidence: {collisions!r}"
        )


def _normalized_path(path: Path) -> Path:
    return path.expanduser().resolve(strict=False)


def _copy_immutable_input(source: Path, destination: Path) -> str:
    digest = hashlib.sha256()
    try:
        with source.open("rb") as input_file, destination.open("xb") as output_file:
            while block := input_file.read(1024 * 1024):
                output_file.write(block)
                digest.update(block)
            output_file.flush()
            os.fsync(output_file.fileno())
        destination.chmod(0o444)
    except OSError as error:
        raise QualificationError(
            f"could not preserve immutable flash input {source}: {error}"
        ) from error
    return digest.hexdigest()


def read_full_flash(
    *,
    expected_usb_serial: str,
    expected_mac: str,
    expected_flash_bytes: int,
    evidence_prefix: Path,
    output: Path,
    ioreg_reader: Callable[[], str] = read_ioreg,
    command_runner: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
    path_is_character_device: Callable[[str], bool] = is_character_device,
) -> tuple[UsbDevice, BoardInfo, str]:
    """Identity-qualify and read the complete physical flash without resetting."""

    action_suffixes = (
        ".read-flash.stdout.txt",
        ".read-flash.stderr.txt",
        ".read-flash.verified.json",
    )
    _reserve_action_evidence(evidence_prefix, action_suffixes)
    all_evidence_paths = [
        _evidence_path(evidence_prefix, suffix)
        for suffix in (*EVIDENCE_SUFFIXES, *action_suffixes)
    ]
    normalized_output = _normalized_path(output)
    if normalized_output in map(_normalized_path, all_evidence_paths):
        raise QualificationError(
            f"flash backup output aliases qualification evidence: {output}"
        )
    if os.path.lexists(output):
        raise QualificationError(f"refusing to overwrite flash backup {output}")
    output.parent.mkdir(parents=True, exist_ok=True)
    device, board = qualify_port(
        expected_usb_serial=expected_usb_serial,
        expected_mac=expected_mac,
        expected_flash_bytes=expected_flash_bytes,
        evidence_prefix=evidence_prefix,
        ioreg_reader=ioreg_reader,
        command_runner=command_runner,
        path_is_character_device=path_is_character_device,
    )
    command = [
        "espflash",
        "read-flash",
        "--chip",
        "esp32s3",
        "--port",
        device.callout_device,
        "--before",
        "no-reset",
        "--after",
        "no-reset",
        "--non-interactive",
        "--skip-update-check",
        "0x0",
        str(board.flash_bytes),
        str(output),
    ]
    result = command_runner(
        command,
        check=False,
        capture_output=True,
        text=True,
    )
    _write_evidence(
        _evidence_path(evidence_prefix, ".read-flash.stdout.txt"), result.stdout
    )
    _write_evidence(
        _evidence_path(evidence_prefix, ".read-flash.stderr.txt"), result.stderr
    )
    if result.returncode != 0:
        raise QualificationError(f"espflash read-flash exited {result.returncode}")
    try:
        output_bytes = output.stat().st_size
    except OSError as error:
        raise QualificationError(f"flash backup is missing: {output}") from error
    if output_bytes != board.flash_bytes:
        raise QualificationError(
            f"flash backup has {output_bytes} bytes, expected {board.flash_bytes}"
        )
    digest = _sha256_file(output)
    _write_evidence(
        _evidence_path(evidence_prefix, ".read-flash.verified.json"),
        json.dumps(
            {
                "output": str(output),
                "bytes": output_bytes,
                "sha256": digest,
                "board_info": asdict(board),
                "usb": asdict(device),
            },
            indent=2,
            sort_keys=True,
        )
        + "\n",
    )
    return device, board, digest


def flash_size_token(flash_bytes: int) -> str:
    if flash_bytes == 8 * 1024 * 1024:
        return "8mb"
    if flash_bytes == 16 * 1024 * 1024:
        return "16mb"
    raise QualificationError(f"unsupported flash capacity {flash_bytes}")


def _validate_merged_image(image: Path, *, expected_flash_bytes: int) -> int:
    try:
        image_bytes = image.stat().st_size
        with image.open("rb") as source:
            header = source.read(4)
    except OSError as error:
        raise QualificationError(
            f"could not inspect merged flash image {image}: {error}"
        ) from error
    if image_bytes < 4:
        raise QualificationError(
            "merged flash image is shorter than its ESP image header"
        )
    if image_bytes > expected_flash_bytes:
        raise QualificationError(
            f"merged flash image has {image_bytes} bytes, exceeding "
            f"{expected_flash_bytes}-byte flash"
        )
    if header[0] != 0xE9:
        raise QualificationError(
            f"merged flash image has invalid ESP image magic 0x{header[0]:02x}"
        )
    if header[2] != 0x02:
        raise QualificationError(
            f"merged flash image must encode DIO flash mode, found 0x{header[2]:02x}"
        )
    encoded_size = {0x3: 8 * 1024 * 1024, 0x4: 16 * 1024 * 1024}.get(
        header[3] >> 4
    )
    if encoded_size != expected_flash_bytes:
        raise QualificationError(
            "merged flash image header capacity does not match the qualified target: "
            f"encoded={encoded_size!r} expected={expected_flash_bytes}"
        )
    return image_bytes


def flash_merged_image(
    *,
    expected_usb_serial: str,
    expected_mac: str,
    expected_flash_bytes: int,
    evidence_prefix: Path,
    image: Path,
    expected_image_sha256: str,
    confirmed_radio_module: str,
    ioreg_reader: Callable[[], str] = read_ioreg,
    command_runner: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
    path_is_character_device: Callable[[str], bool] = is_character_device,
) -> tuple[UsbDevice, BoardInfo, str]:
    """Identity-qualify, flash, and read back one complete merged image."""

    if confirmed_radio_module != CONFIRMED_HF_MODULE:
        raise QualificationError(
            f"RF-capable image requires confirmed module {CONFIRMED_HF_MODULE!r}"
        )
    normalized_expected_digest = expected_image_sha256.lower()
    if re.fullmatch(r"[0-9a-f]{64}", normalized_expected_digest) is None:
        raise QualificationError("expected image SHA-256 must be 64 hexadecimal digits")
    if not image.is_file():
        raise QualificationError(f"merged flash image is not a regular file: {image}")
    image_bytes = _validate_merged_image(
        image, expected_flash_bytes=expected_flash_bytes
    )

    action_suffixes = (
        ".flash-input.bin",
        ".write-bin.stdout.txt",
        ".write-bin.stderr.txt",
        ".readback.bin",
        ".readback.stdout.txt",
        ".readback.stderr.txt",
        ".flash-image.verified.json",
    )
    evidence_prefix.parent.mkdir(parents=True, exist_ok=True)
    _reserve_action_evidence(evidence_prefix, action_suffixes)
    preserved_image = _evidence_path(evidence_prefix, ".flash-input.bin")
    readback = _evidence_path(evidence_prefix, ".readback.bin")
    image_digest = _copy_immutable_input(image, preserved_image)
    if image_digest != normalized_expected_digest:
        raise QualificationError(
            "merged flash image SHA-256 mismatch: "
            f"expected={normalized_expected_digest} actual={image_digest}"
        )

    device, board = qualify_port(
        expected_usb_serial=expected_usb_serial,
        expected_mac=expected_mac,
        expected_flash_bytes=expected_flash_bytes,
        evidence_prefix=evidence_prefix,
        ioreg_reader=ioreg_reader,
        command_runner=command_runner,
        path_is_character_device=path_is_character_device,
    )
    write_command = [
        "espflash",
        "write-bin",
        "--chip",
        "esp32s3",
        "--port",
        device.callout_device,
        "--before",
        "no-reset",
        "--after",
        "no-reset",
        "--non-interactive",
        "--skip-update-check",
        "0x0",
        str(preserved_image),
    ]
    write_result = command_runner(
        write_command,
        check=False,
        capture_output=True,
        text=True,
    )
    try:
        _write_evidence(
            _evidence_path(evidence_prefix, ".write-bin.stdout.txt"),
            write_result.stdout,
        )
        _write_evidence(
            _evidence_path(evidence_prefix, ".write-bin.stderr.txt"),
            write_result.stderr,
        )
    except OSError as error:
        if write_result.returncode == 0:
            raise PostWriteEvidenceError(
                "merged-image flash completed but action streams were not "
                f"preserved: {error}"
            ) from error
        raise QualificationError(
            f"merged-image flash failed and action streams were not preserved: {error}"
        ) from error
    if write_result.returncode != 0:
        raise QualificationError(f"espflash write-bin exited {write_result.returncode}")

    read_command = [
        "espflash",
        "read-flash",
        "--chip",
        "esp32s3",
        "--port",
        device.callout_device,
        "--before",
        "no-reset",
        "--after",
        "no-reset",
        "--non-interactive",
        "--skip-update-check",
        "0x0",
        str(image_bytes),
        str(readback),
    ]
    read_result = command_runner(
        read_command,
        check=False,
        capture_output=True,
        text=True,
    )
    try:
        _write_evidence(
            _evidence_path(evidence_prefix, ".readback.stdout.txt"),
            read_result.stdout,
        )
        _write_evidence(
            _evidence_path(evidence_prefix, ".readback.stderr.txt"),
            read_result.stderr,
        )
    except OSError as error:
        raise PostWriteEvidenceError(
            "merged-image flash completed but readback streams were not "
            f"preserved: {error}"
        ) from error
    if read_result.returncode != 0:
        raise PostWriteEvidenceError(
            "merged-image flash completed but espflash read-flash exited "
            f"{read_result.returncode}"
        )
    try:
        readback_bytes = readback.stat().st_size
        readback_digest = _sha256_file(readback)
        if readback_bytes != image_bytes or readback_digest != image_digest:
            raise PostWriteEvidenceError(
                "merged-image readback mismatch: "
                f"bytes={readback_bytes}/{image_bytes} "
                f"sha256={readback_digest}/{image_digest}"
            )
        if _sha256_file(preserved_image) != image_digest:
            raise PostWriteEvidenceError(
                "merged-image flash completed but its immutable input copy changed"
            )
        readback.chmod(0o444)
        _write_evidence(
            _evidence_path(evidence_prefix, ".flash-image.verified.json"),
            json.dumps(
                {
                    "board_info": asdict(board),
                    "usb": asdict(device),
                    "confirmed_radio_module": confirmed_radio_module,
                    "address": 0,
                    "source_image": str(image),
                    "preserved_image": str(preserved_image),
                    "image_bytes": image_bytes,
                    "expected_image_sha256": normalized_expected_digest,
                    "image_sha256": image_digest,
                    "readback": str(readback),
                    "readback_bytes": readback_bytes,
                    "readback_sha256": readback_digest,
                },
                indent=2,
                sort_keys=True,
            )
            + "\n",
        )
    except PostWriteEvidenceError:
        raise
    except OSError as error:
        raise PostWriteEvidenceError(
            f"merged-image flash completed but evidence finalization failed: {error}"
        ) from error
    return device, board, image_digest


def verify_merged_image(
    *,
    expected_usb_serial: str,
    expected_mac: str,
    expected_flash_bytes: int,
    evidence_prefix: Path,
    image: Path,
    expected_image_sha256: str,
    confirmed_radio_module: str,
    ioreg_reader: Callable[[], str] = read_ioreg,
    command_runner: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
    path_is_character_device: Callable[[str], bool] = is_character_device,
) -> tuple[UsbDevice, BoardInfo, str]:
    """Identity-qualify and compare a merged image with its flashed range."""

    if confirmed_radio_module != CONFIRMED_HF_MODULE:
        raise QualificationError(
            f"RF-capable image requires confirmed module {CONFIRMED_HF_MODULE!r}"
        )
    normalized_expected_digest = expected_image_sha256.lower()
    if re.fullmatch(r"[0-9a-f]{64}", normalized_expected_digest) is None:
        raise QualificationError("expected image SHA-256 must be 64 hexadecimal digits")
    if not image.is_file():
        raise QualificationError(f"merged flash image is not a regular file: {image}")
    image_bytes = _validate_merged_image(
        image, expected_flash_bytes=expected_flash_bytes
    )

    action_suffixes = (
        ".verify-input.bin",
        ".verify-readback.bin",
        ".verify-readback.stdout.txt",
        ".verify-readback.stderr.txt",
        ".verify-image.verified.json",
    )
    evidence_prefix.parent.mkdir(parents=True, exist_ok=True)
    _reserve_action_evidence(evidence_prefix, action_suffixes)
    preserved_image = _evidence_path(evidence_prefix, ".verify-input.bin")
    readback = _evidence_path(evidence_prefix, ".verify-readback.bin")
    image_digest = _copy_immutable_input(image, preserved_image)
    if image_digest != normalized_expected_digest:
        raise QualificationError(
            "merged flash image SHA-256 mismatch: "
            f"expected={normalized_expected_digest} actual={image_digest}"
        )

    device, board = qualify_port(
        expected_usb_serial=expected_usb_serial,
        expected_mac=expected_mac,
        expected_flash_bytes=expected_flash_bytes,
        evidence_prefix=evidence_prefix,
        ioreg_reader=ioreg_reader,
        command_runner=command_runner,
        path_is_character_device=path_is_character_device,
    )
    command = [
        "espflash",
        "read-flash",
        "--chip",
        "esp32s3",
        "--port",
        device.callout_device,
        "--before",
        "no-reset",
        "--after",
        "no-reset",
        "--non-interactive",
        "--skip-update-check",
        "0x0",
        str(image_bytes),
        str(readback),
    ]
    result = command_runner(
        command,
        check=False,
        capture_output=True,
        text=True,
    )
    _write_evidence(
        _evidence_path(evidence_prefix, ".verify-readback.stdout.txt"),
        result.stdout,
    )
    _write_evidence(
        _evidence_path(evidence_prefix, ".verify-readback.stderr.txt"),
        result.stderr,
    )
    if result.returncode != 0:
        raise QualificationError(f"espflash read-flash exited {result.returncode}")
    try:
        readback_bytes = readback.stat().st_size
        readback_digest = _sha256_file(readback)
        if readback_bytes != image_bytes or readback_digest != image_digest:
            raise QualificationError(
                "merged-image readback mismatch: "
                f"bytes={readback_bytes}/{image_bytes} "
                f"sha256={readback_digest}/{image_digest}"
            )
        if _sha256_file(preserved_image) != image_digest:
            raise QualificationError("immutable verification input copy changed")
        readback.chmod(0o444)
        _write_evidence(
            _evidence_path(evidence_prefix, ".verify-image.verified.json"),
            json.dumps(
                {
                    "board_info": asdict(board),
                    "usb": asdict(device),
                    "confirmed_radio_module": confirmed_radio_module,
                    "address": 0,
                    "source_image": str(image),
                    "preserved_image": str(preserved_image),
                    "image_bytes": image_bytes,
                    "expected_image_sha256": normalized_expected_digest,
                    "image_sha256": image_digest,
                    "readback": str(readback),
                    "readback_bytes": readback_bytes,
                    "readback_sha256": readback_digest,
                },
                indent=2,
                sort_keys=True,
            )
            + "\n",
        )
    except QualificationError:
        raise
    except OSError as error:
        raise QualificationError(
            f"could not finalize merged-image verification evidence: {error}"
        ) from error
    return device, board, image_digest


def flash_qualification_image(
    *,
    expected_usb_serial: str,
    expected_mac: str,
    expected_flash_bytes: int,
    evidence_prefix: Path,
    elf: Path,
    partition_table: Path,
    ioreg_reader: Callable[[], str] = read_ioreg,
    command_runner: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
    path_is_character_device: Callable[[str], bool] = is_character_device,
) -> tuple[UsbDevice, BoardInfo]:
    """Identity-qualify and flash the RF-inert image with a derived header size."""

    evidence_prefix.parent.mkdir(parents=True, exist_ok=True)
    action_suffixes = (
        ".flash-input.elf",
        ".flash-input.partition.csv",
        ".flash.stdout.txt",
        ".flash.stderr.txt",
        ".flash.verified.json",
    )
    _reserve_action_evidence(evidence_prefix, action_suffixes)
    if not elf.is_file():
        raise QualificationError(f"qualification ELF is not a regular file: {elf}")
    if not partition_table.is_file():
        raise QualificationError(
            f"qualification partition table is not a regular file: {partition_table}"
        )
    preserved_elf = _evidence_path(evidence_prefix, ".flash-input.elf")
    preserved_partition_table = _evidence_path(
        evidence_prefix, ".flash-input.partition.csv"
    )
    elf_digest = _copy_immutable_input(elf, preserved_elf)
    partition_digest = _copy_immutable_input(
        partition_table, preserved_partition_table
    )
    device, board = qualify_port(
        expected_usb_serial=expected_usb_serial,
        expected_mac=expected_mac,
        expected_flash_bytes=expected_flash_bytes,
        evidence_prefix=evidence_prefix,
        ioreg_reader=ioreg_reader,
        command_runner=command_runner,
        path_is_character_device=path_is_character_device,
    )
    size_token = flash_size_token(board.flash_bytes)
    command = [
        "espflash",
        "flash",
        "--chip",
        "esp32s3",
        "--port",
        device.callout_device,
        "--before",
        "no-reset",
        "--after",
        "no-reset",
        "--non-interactive",
        "--skip-update-check",
        "--flash-size",
        size_token,
        "--flash-freq",
        "40mhz",
        "--flash-mode",
        "dio",
        "--partition-table",
        str(preserved_partition_table),
        str(preserved_elf),
    ]
    result = command_runner(
        command,
        check=False,
        capture_output=True,
        text=True,
    )
    try:
        _write_evidence(
            _evidence_path(evidence_prefix, ".flash.stdout.txt"), result.stdout
        )
        _write_evidence(
            _evidence_path(evidence_prefix, ".flash.stderr.txt"), result.stderr
        )
    except OSError as error:
        if result.returncode == 0:
            raise PostWriteEvidenceError(
                f"qualification flash completed but action streams were not preserved: {error}"
            ) from error
        raise QualificationError(
            f"qualification flash failed and action streams were not preserved: {error}"
        ) from error
    if result.returncode != 0:
        raise QualificationError(f"espflash flash exited {result.returncode}")
    try:
        if (
            _sha256_file(preserved_elf) != elf_digest
            or _sha256_file(preserved_partition_table) != partition_digest
        ):
            raise PostWriteEvidenceError(
                "qualification flash completed but an immutable input copy changed"
            )
        _write_evidence(
            _evidence_path(evidence_prefix, ".flash.verified.json"),
            json.dumps(
                {
                    "board_info": asdict(board),
                    "usb": asdict(device),
                    "flash_size_token": size_token,
                    "source_elf": str(elf),
                    "preserved_elf": str(preserved_elf),
                    "elf_sha256": elf_digest,
                    "source_partition_table": str(partition_table),
                    "preserved_partition_table": str(preserved_partition_table),
                    "partition_table_sha256": partition_digest,
                },
                indent=2,
                sort_keys=True,
            )
            + "\n",
        )
    except PostWriteEvidenceError:
        raise
    except OSError as error:
        raise PostWriteEvidenceError(
            f"qualification flash completed but evidence finalization failed: {error}"
        ) from error
    return device, board


def _parse_flash_bytes(value: str) -> int:
    try:
        parsed = int(value, 0)
    except ValueError as error:
        raise argparse.ArgumentTypeError("must be an integer byte count") from error
    if parsed not in SUPPORTED_FLASH_BYTES:
        raise argparse.ArgumentTypeError("must be exactly 8388608 or 16777216")
    return parsed


def _add_identity_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--usb-serial", required=True)
    parser.add_argument("--expected-mac", required=True)
    parser.add_argument(
        "--expected-flash-bytes", required=True, type=_parse_flash_bytes
    )
    parser.add_argument("--evidence-prefix", required=True, type=Path)


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    actions = parser.add_subparsers(dest="action", required=True)
    verify = actions.add_parser("verify", help="run the read-only identity gate")
    _add_identity_arguments(verify)
    backup = actions.add_parser(
        "read-flash", help="identity-qualify and read the complete flash"
    )
    _add_identity_arguments(backup)
    backup.add_argument("--output", required=True, type=Path)
    flash = actions.add_parser(
        "flash-qualification", help="identity-qualify and flash the RF-inert ELF"
    )
    _add_identity_arguments(flash)
    flash.add_argument("--elf", required=True, type=Path)
    flash.add_argument("--partition-table", required=True, type=Path)
    merged = actions.add_parser(
        "flash-merged",
        help="identity-qualify, flash, and read back an RF-capable merged image",
    )
    _add_identity_arguments(merged)
    merged.add_argument("--image", required=True, type=Path)
    merged.add_argument("--expected-image-sha256", required=True)
    merged.add_argument(
        "--confirmed-radio-module", required=True, choices=(CONFIRMED_HF_MODULE,)
    )
    verify_merged = actions.add_parser(
        "verify-merged",
        help="identity-qualify and compare an RF-capable merged-image range",
    )
    _add_identity_arguments(verify_merged)
    verify_merged.add_argument("--image", required=True, type=Path)
    verify_merged.add_argument("--expected-image-sha256", required=True)
    verify_merged.add_argument(
        "--confirmed-radio-module", required=True, choices=(CONFIRMED_HF_MODULE,)
    )
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    try:
        common = {
            "expected_usb_serial": args.usb_serial,
            "expected_mac": args.expected_mac,
            "expected_flash_bytes": args.expected_flash_bytes,
            "evidence_prefix": args.evidence_prefix,
        }
        if args.action == "verify":
            device, _board = qualify_port(**common)
        elif args.action == "read-flash":
            device, _board, _digest = read_full_flash(output=args.output, **common)
        elif args.action == "flash-qualification":
            device, _board = flash_qualification_image(
                elf=args.elf,
                partition_table=args.partition_table,
                **common,
            )
        elif args.action == "flash-merged":
            device, _board, _digest = flash_merged_image(
                image=args.image,
                expected_image_sha256=args.expected_image_sha256,
                confirmed_radio_module=args.confirmed_radio_module,
                **common,
            )
        elif args.action == "verify-merged":
            device, _board, _digest = verify_merged_image(
                image=args.image,
                expected_image_sha256=args.expected_image_sha256,
                confirmed_radio_module=args.confirmed_radio_module,
                **common,
            )
        else:
            raise AssertionError(f"unknown action {args.action!r}")
    except (OSError, subprocess.SubprocessError, QualificationError) as error:
        print(f"E290 qualification host gate failed: {error}", file=sys.stderr)
        return 1
    print(device.callout_device)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
