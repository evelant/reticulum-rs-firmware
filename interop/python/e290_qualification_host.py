#!/usr/bin/env python3
"""Identity-safe host gates for E290 flash reads and writes.

The tool maps an ESP32-S3 native-USB serial identity to its macOS callout
device using the complete IORegistry stream, runs ``espflash board-info`` while
leaving the target in the loader, and validates every fact needed before a
plaintext read or write. Its action subcommands then own loader-preserving
full-flash or exact-region read, verified exact-region all-FF erase-equivalent write,
qualification-image write, hash-bound merged-image write and readback, or
post-run image-range verification. Each operation remains bound to the
qualified capacity and identity. It prints the qualified callout path only as a
final diagnostic, never as authority for a later shell command.
"""

from __future__ import annotations

import argparse
from contextlib import contextmanager
from dataclasses import asdict, dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import stat
import subprocess
import sys
import unicodedata
from typing import Callable, Iterator, Sequence


ESPRESSIF_VENDOR_ID = 0x303A
ESP32S3_USB_PRODUCT_ID = 0x1001
E290_FLASH_BYTES = 16 * 1024 * 1024
FLASH_SECTOR_BYTES = 4096
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


@dataclass(frozen=True)
class ActionTargetInfo:
    chip: str
    flash_bytes: int
    mac: str


@dataclass(frozen=True)
class ReservedReadOutput:
    """One private output inode held open across an espflash read."""

    path: Path
    descriptor: int
    device: int
    inode: int

    @property
    def action_path(self) -> str:
        # espflash opens this descriptor-backed path instead of the mutable
        # caller-visible pathname. A raced unlink/symlink replacement can make
        # verification fail, but cannot redirect the flash bytes into another
        # file.
        return f"/dev/fd/{self.descriptor}"


@dataclass(frozen=True)
class RetainedFlashInput:
    """One hash-bound input inode retained across a destructive flash action."""

    path: Path
    descriptor: int
    device: int
    inode: int
    bytes: int
    sha256: str

    @property
    def action_path(self) -> str:
        return f"/dev/fd/{self.descriptor}"


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


def _publish_verified_evidence(path: Path, contents: str) -> None:
    """Atomically publish a complete, durable, no-replace verified sentinel."""

    directory_flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
    directory = os.open(path.parent, directory_flags)
    staged_name: str | None = None
    linked = False
    committed = False
    try:
        for _attempt in range(128):
            candidate = f".{path.name}.{secrets.token_hex(16)}.tmp"
            try:
                descriptor = os.open(
                    candidate,
                    os.O_WRONLY
                    | os.O_CREAT
                    | os.O_EXCL
                    | getattr(os, "O_NOFOLLOW", 0),
                    0o600,
                    dir_fd=directory,
                )
            except FileExistsError:
                continue
            staged_name = candidate
            break
        else:
            raise QualificationError("could not reserve unique verified-evidence temp")
        with os.fdopen(descriptor, "w") as output:
            output.write(contents)
            output.flush()
            os.fsync(output.fileno())
        os.link(
            staged_name,
            path.name,
            src_dir_fd=directory,
            dst_dir_fd=directory,
            follow_symlinks=False,
        )
        linked = True
        os.fsync(directory)
        committed = True
        try:
            os.unlink(staged_name, dir_fd=directory)
            staged_name = None
            os.fsync(directory)
        except OSError:
            # The durable no-replace link is the commit point. A stale unique
            # temp is harmless and must not turn a committed sentinel into a
            # reported failure.
            pass
    except BaseException:
        if linked and not committed:
            try:
                os.unlink(path.name, dir_fd=directory)
                os.fsync(directory)
            except OSError:
                pass
        raise
    finally:
        if staged_name is not None:
            try:
                os.unlink(staged_name, dir_fd=directory)
                os.fsync(directory)
            except OSError:
                pass
        try:
            os.close(directory)
        except OSError:
            if not committed:
                raise


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
    _publish_verified_evidence(
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


def _finalize_read_output(
    output: ReservedReadOutput,
    *,
    expected_bytes: int,
    erased_offset: int | None = None,
) -> tuple[int, str]:
    """Hash, validate, make immutable, and durably sync one espflash output."""

    descriptor = output.descriptor
    initial = os.fstat(descriptor)
    if not stat.S_ISREG(initial.st_mode):
        raise QualificationError(f"flash output is not a regular file: {output.path}")
    if (initial.st_dev, initial.st_ino) != (output.device, output.inode):
        raise QualificationError("reserved flash output descriptor changed identity")
    _require_reserved_output_path(output)
    if initial.st_size != expected_bytes:
        raise QualificationError(
            f"flash output has {initial.st_size} bytes, expected {expected_bytes}"
        )
    os.lseek(descriptor, 0, os.SEEK_SET)
    digest = hashlib.sha256()
    first_programmed_offset: int | None = None
    observed_bytes = 0
    while block := os.read(descriptor, 1024 * 1024):
        digest.update(block)
        if (
            erased_offset is not None
            and first_programmed_offset is None
            and block.count(0xFF) != len(block)
        ):
            for index, value in enumerate(block):
                if value != 0xFF:
                    first_programmed_offset = observed_bytes + index
                    break
        observed_bytes += len(block)
    final = os.fstat(descriptor)
    if (
        observed_bytes != expected_bytes
        or final.st_size != expected_bytes
        or (final.st_dev, final.st_ino) != (output.device, output.inode)
    ):
        raise QualificationError(
            "flash output changed while verifying: "
            f"bytes={observed_bytes}/{expected_bytes}"
        )
    _require_reserved_output_path(output)
    if first_programmed_offset is not None:
        assert erased_offset is not None
        raise QualificationError(
            "erased-region readback contains a programmed byte at absolute "
            f"offset 0x{erased_offset + first_programmed_offset:x}"
        )
    try:
        os.fchmod(descriptor, 0o400)
        os.fsync(descriptor)
        directory = os.open(
            output.path.parent,
            os.O_RDONLY | getattr(os, "O_DIRECTORY", 0),
        )
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    except BaseException:
        # A 0400 readback is a claim that all finalization checks and their
        # durability barriers succeeded. Keep every failed transition private,
        # including failures opening, syncing, or closing the parent directory.
        # fchmod comes first in the rollback so even a subsequent fsync failure
        # cannot leave the inode carrying the verified-output mode.
        _mark_read_output_unverified(output)
        raise
    return observed_bytes, digest.hexdigest()


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


def _paths_may_alias(left: Path, right: Path) -> bool:
    normalized_left = _normalized_path(left)
    normalized_right = _normalized_path(right)
    folded_left = unicodedata.normalize("NFD", str(normalized_left)).casefold()
    folded_right = unicodedata.normalize("NFD", str(normalized_right)).casefold()
    if folded_left == folded_right:
        return True
    if os.path.lexists(left) and os.path.lexists(right):
        try:
            return os.path.samefile(left, right)
        except OSError:
            return False
    return False


def _require_fresh_output(
    output: Path,
    evidence_paths: Sequence[Path],
    *,
    label: str,
) -> None:
    aliases = [str(path) for path in evidence_paths if _paths_may_alias(output, path)]
    if aliases:
        raise QualificationError(f"{label} aliases qualification evidence: {aliases!r}")
    if os.path.lexists(output):
        raise QualificationError(f"refusing to overwrite {label} {output}")


def _require_reserved_output_path(output: ReservedReadOutput) -> None:
    try:
        identity = os.stat(output.path, follow_symlinks=False)
    except OSError as error:
        raise QualificationError(
            f"reserved flash output path is unavailable: {output.path}: {error}"
        ) from error
    if not stat.S_ISREG(identity.st_mode) or (
        identity.st_dev,
        identity.st_ino,
    ) != (output.device, output.inode):
        raise QualificationError(
            f"reserved flash output path changed identity: {output.path}"
        )


@contextmanager
def _reserved_private_output(
    path: Path,
    evidence_paths: Sequence[Path],
    *,
    label: str,
) -> Iterator[ReservedReadOutput]:
    """Exclusively reserve and retain a private output inode for espflash."""

    _require_fresh_output(path, evidence_paths, label=label)
    directory_flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
    directory = os.open(path.parent, directory_flags)
    descriptor: int | None = None
    output: ReservedReadOutput | None = None
    try:
        try:
            descriptor = os.open(
                path.name,
                os.O_RDWR
                | os.O_CREAT
                | os.O_EXCL
                | getattr(os, "O_NOFOLLOW", 0),
                0o600,
                dir_fd=directory,
            )
        except FileExistsError as error:
            raise QualificationError(f"refusing to overwrite {label} {path}") from error
        identity = os.fstat(descriptor)
        if not stat.S_ISREG(identity.st_mode):
            raise QualificationError(f"{label} is not a regular file: {path}")
        os.fchmod(descriptor, 0o600)
        os.fsync(descriptor)
        os.fsync(directory)
        output = ReservedReadOutput(
            path=path,
            descriptor=descriptor,
            device=identity.st_dev,
            inode=identity.st_ino,
        )
        _require_reserved_output_path(output)
        yield output
    finally:
        if descriptor is not None:
            os.close(descriptor)
        os.close(directory)


def _publish_read_verification(
    output: ReservedReadOutput, evidence_path: Path, contents: str
) -> None:
    """Publish verification or restore the still-unverified output to 0600."""

    try:
        _require_reserved_output_path(output)
        _publish_verified_evidence(evidence_path, contents)
    except BaseException:
        _mark_read_output_unverified(output)
        raise


def _mark_read_output_unverified(output: ReservedReadOutput) -> None:
    os.fchmod(output.descriptor, 0o600)
    os.fsync(output.descriptor)


def parse_action_target_info(
    stdout: str,
    stderr: str,
    *,
    expected_mac: str,
    expected_flash_bytes: int,
) -> ActionTargetInfo:
    """Validate the target facts printed by a non-board-info espflash action."""

    combined = f"{stdout}\n{stderr}"
    for pattern in FLASH_DETECTION_FAILURES:
        match = pattern.search(combined)
        if match is not None:
            raise QualificationError(
                f"action contains flash-detection failure: {match.group(0)!r}"
            )
    chip = _single_match(CHIP_TYPE, stdout, "action chip type").lower()
    flash_matches = FLASH_SIZE.findall(stdout)
    if len(flash_matches) != 1:
        raise QualificationError(
            "action stdout must contain exactly one flash size, "
            f"found {len(flash_matches)}"
        )
    flash_value, flash_unit = flash_matches[0]
    multiplier = 1024 if flash_unit == "KB" else 1024 * 1024
    flash_bytes = int(flash_value) * multiplier
    mac = _single_match(MAC_ADDRESS, stdout, "action MAC address").lower()
    if chip != "esp32s3":
        raise QualificationError(f"action expected esp32s3, found {chip!r}")
    if mac != expected_mac.lower():
        raise QualificationError(
            f"action expected MAC {expected_mac.lower()}, found {mac}"
        )
    if flash_bytes != expected_flash_bytes:
        raise QualificationError(
            f"action expected {expected_flash_bytes} flash bytes, found {flash_bytes}"
        )
    return ActionTargetInfo(chip=chip, flash_bytes=flash_bytes, mac=mac)


def _capture_unchanged_usb_mapping(
    *,
    device: UsbDevice,
    expected_usb_serial: str,
    evidence_path: Path,
    phase: str,
    ioreg_reader: Callable[[], str],
) -> UsbDevice:
    ioreg = ioreg_reader()
    _write_evidence(evidence_path, ioreg)
    after = select_usb_device(parse_ioreg(ioreg), expected_usb_serial)
    if after != device:
        raise QualificationError(
            f"USB mapping changed {phase}: before={device!r} after={after!r}"
        )
    return after


def _loader_board_info_command(device: UsbDevice) -> list[str]:
    return [
        "espflash",
        "board-info",
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
    ]


def _copy_retained_flash_input(
    source: Path, destination: Path
) -> RetainedFlashInput:
    """Copy, hash, and retain the exact inode that a flash action will read."""

    descriptor: int | None = None
    read_descriptor: int | None = None
    digest = hashlib.sha256()
    copied = 0
    try:
        descriptor = os.open(
            destination,
            os.O_RDWR
            | os.O_CREAT
            | os.O_EXCL
            | getattr(os, "O_NOFOLLOW", 0),
            0o600,
        )
        with source.open("rb") as input_file:
            while block := input_file.read(1024 * 1024):
                view = memoryview(block)
                while view:
                    written = os.write(descriptor, view)
                    if written <= 0:
                        raise OSError("short write while preserving flash input")
                    view = view[written:]
                copied += len(block)
                digest.update(block)
        os.fchmod(descriptor, 0o400)
        os.fsync(descriptor)
        identity = os.fstat(descriptor)
        read_descriptor = os.open(
            destination,
            os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0),
        )
        read_identity = os.fstat(read_descriptor)
        if (read_identity.st_dev, read_identity.st_ino) != (
            identity.st_dev,
            identity.st_ino,
        ):
            raise QualificationError(
                f"retained flash input path changed while reopening: {destination}"
            )
        os.close(descriptor)
        descriptor = read_descriptor
        read_descriptor = None
        retained = RetainedFlashInput(
            path=destination,
            descriptor=descriptor,
            device=identity.st_dev,
            inode=identity.st_ino,
            bytes=copied,
            sha256=digest.hexdigest(),
        )
        _require_retained_flash_input(retained)
        os.lseek(descriptor, 0, os.SEEK_SET)
        return retained
    except OSError as error:
        if read_descriptor is not None:
            os.close(read_descriptor)
        if descriptor is not None:
            os.close(descriptor)
        raise QualificationError(
            f"could not preserve retained flash input {source}: {error}"
        ) from error
    except BaseException:
        if read_descriptor is not None:
            os.close(read_descriptor)
        if descriptor is not None:
            os.close(descriptor)
        raise


def _create_retained_fill_input(
    destination: Path, *, length: int, value: int
) -> RetainedFlashInput:
    """Create and retain one exact, read-only repeated-byte flash input."""

    descriptor: int | None = None
    read_descriptor: int | None = None
    digest = hashlib.sha256()
    block = bytes((value,)) * min(length, 1024 * 1024)
    written_total = 0
    try:
        descriptor = os.open(
            destination,
            os.O_RDWR
            | os.O_CREAT
            | os.O_EXCL
            | getattr(os, "O_NOFOLLOW", 0),
            0o600,
        )
        while written_total < length:
            chunk = block[: min(len(block), length - written_total)]
            view = memoryview(chunk)
            while view:
                written = os.write(descriptor, view)
                if written <= 0:
                    raise OSError("short write while creating flash fill input")
                view = view[written:]
            digest.update(chunk)
            written_total += len(chunk)
        os.fchmod(descriptor, 0o400)
        os.fsync(descriptor)
        identity = os.fstat(descriptor)
        read_descriptor = os.open(
            destination,
            os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0),
        )
        read_identity = os.fstat(read_descriptor)
        if (read_identity.st_dev, read_identity.st_ino) != (
            identity.st_dev,
            identity.st_ino,
        ):
            raise QualificationError(
                f"flash fill input path changed while reopening: {destination}"
            )
        os.close(descriptor)
        descriptor = read_descriptor
        read_descriptor = None
        retained = RetainedFlashInput(
            path=destination,
            descriptor=descriptor,
            device=identity.st_dev,
            inode=identity.st_ino,
            bytes=written_total,
            sha256=digest.hexdigest(),
        )
        _require_retained_flash_input(retained)
        os.lseek(descriptor, 0, os.SEEK_SET)
        return retained
    except OSError as error:
        if read_descriptor is not None:
            os.close(read_descriptor)
        if descriptor is not None:
            os.close(descriptor)
        raise QualificationError(
            f"could not create retained flash fill input {destination}: {error}"
        ) from error
    except BaseException:
        if read_descriptor is not None:
            os.close(read_descriptor)
        if descriptor is not None:
            os.close(descriptor)
        raise


def _require_retained_flash_input(input_file: RetainedFlashInput) -> None:
    identity = os.fstat(input_file.descriptor)
    if not stat.S_ISREG(identity.st_mode) or (
        identity.st_dev,
        identity.st_ino,
    ) != (input_file.device, input_file.inode):
        raise QualificationError("retained flash input descriptor changed identity")
    try:
        path_identity = os.stat(input_file.path, follow_symlinks=False)
    except OSError as error:
        raise QualificationError(
            f"retained flash input path is unavailable: {input_file.path}: {error}"
        ) from error
    if not stat.S_ISREG(path_identity.st_mode) or (
        path_identity.st_dev,
        path_identity.st_ino,
    ) != (input_file.device, input_file.inode):
        raise QualificationError(
            f"retained flash input path changed identity: {input_file.path}"
        )


def _verify_retained_flash_input(input_file: RetainedFlashInput) -> None:
    _require_retained_flash_input(input_file)
    digest = hashlib.sha256()
    observed = 0
    os.lseek(input_file.descriptor, 0, os.SEEK_SET)
    while block := os.read(input_file.descriptor, 1024 * 1024):
        digest.update(block)
        observed += len(block)
    _require_retained_flash_input(input_file)
    if observed != input_file.bytes or digest.hexdigest() != input_file.sha256:
        raise QualificationError("retained flash input changed while flashing")


def _validate_region_request(
    *,
    expected_usb_serial: str,
    expected_flash_bytes: int,
    offset: int,
    length: int,
    require_erase_alignment: bool,
) -> None:
    if (
        not expected_usb_serial
        or expected_usb_serial != expected_usb_serial.strip()
        or expected_usb_serial != expected_usb_serial.upper()
    ):
        raise QualificationError(
            "region operations require the exact uppercase USB serial"
        )
    if expected_flash_bytes != E290_FLASH_BYTES:
        raise QualificationError(
            "E290 region operations require an expected flash size of "
            f"{E290_FLASH_BYTES} bytes"
        )
    if offset < 0:
        raise QualificationError("region offset must be non-negative")
    if length <= 0:
        raise QualificationError("region length must be positive")
    if offset > expected_flash_bytes or length > expected_flash_bytes - offset:
        raise QualificationError(
            f"region 0x{offset:x}..0x{offset + length:x} exceeds the "
            f"{expected_flash_bytes}-byte flash"
        )
    if require_erase_alignment and (
        offset % FLASH_SECTOR_BYTES != 0 or length % FLASH_SECTOR_BYTES != 0
    ):
        raise QualificationError(
            f"erase offset and length must be {FLASH_SECTOR_BYTES}-byte aligned"
        )


def _read_region_command(
    device: UsbDevice, offset: int, length: int, output: Path
) -> list[str]:
    return [
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
        str(offset),
        str(length),
        str(output),
    ]


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
        ".ioreg-after-read-flash.txt",
        ".read-flash.verified.json",
    )
    _reserve_action_evidence(evidence_prefix, action_suffixes)
    all_evidence_paths = [
        _evidence_path(evidence_prefix, suffix)
        for suffix in (*EVIDENCE_SUFFIXES, *action_suffixes)
    ]
    _require_fresh_output(output, all_evidence_paths, label="flash backup output")
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
    # Recheck after qualification has created its evidence. This closes both a
    # raced output creation and case-insensitive aliases on the host volume.
    with _reserved_private_output(
        output, all_evidence_paths, label="flash backup output"
    ) as reserved_output:
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
            reserved_output.action_path,
        ]
        result = command_runner(
            command,
            check=False,
            capture_output=True,
            text=True,
            pass_fds=(reserved_output.descriptor,),
        )
        _write_evidence(
            _evidence_path(evidence_prefix, ".read-flash.stdout.txt"), result.stdout
        )
        _write_evidence(
            _evidence_path(evidence_prefix, ".read-flash.stderr.txt"), result.stderr
        )
        if result.returncode != 0:
            raise QualificationError(f"espflash read-flash exited {result.returncode}")
        _capture_unchanged_usb_mapping(
            device=device,
            expected_usb_serial=expected_usb_serial,
            evidence_path=_evidence_path(
                evidence_prefix, ".ioreg-after-read-flash.txt"
            ),
            phase="during full-flash read",
            ioreg_reader=ioreg_reader,
        )
        action_target = parse_action_target_info(
            result.stdout,
            result.stderr,
            expected_mac=expected_mac,
            expected_flash_bytes=expected_flash_bytes,
        )
        try:
            output_bytes, digest = _finalize_read_output(
                reserved_output, expected_bytes=board.flash_bytes
            )
        except (OSError, QualificationError) as error:
            raise QualificationError(
                f"could not finalize flash backup: {error}"
            ) from error
        _publish_read_verification(
            reserved_output,
            _evidence_path(evidence_prefix, ".read-flash.verified.json"),
            json.dumps(
                {
                    "output": str(output),
                    "bytes": output_bytes,
                    "sha256": digest,
                    "board_info": asdict(board),
                    "read_target": asdict(action_target),
                    "usb": asdict(device),
                },
                indent=2,
                sort_keys=True,
            )
            + "\n",
        )
    return device, board, digest


def read_region(
    *,
    expected_usb_serial: str,
    expected_mac: str,
    expected_flash_bytes: int,
    evidence_prefix: Path,
    offset: int,
    length: int,
    output: Path,
    ioreg_reader: Callable[[], str] = read_ioreg,
    command_runner: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
    path_is_character_device: Callable[[str], bool] = is_character_device,
) -> tuple[UsbDevice, BoardInfo, str]:
    """Identity-qualify and capture one exact E290 flash region."""

    _validate_region_request(
        expected_usb_serial=expected_usb_serial,
        expected_flash_bytes=expected_flash_bytes,
        offset=offset,
        length=length,
        require_erase_alignment=False,
    )
    action_suffixes = (
        ".read-region.stdout.txt",
        ".read-region.stderr.txt",
        ".ioreg-after-read-region.txt",
        ".read-region.verified.json",
    )
    _reserve_action_evidence(evidence_prefix, action_suffixes)
    all_evidence_paths = [
        _evidence_path(evidence_prefix, suffix)
        for suffix in (*EVIDENCE_SUFFIXES, *action_suffixes)
    ]
    _require_fresh_output(output, all_evidence_paths, label="region output")
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
    with _reserved_private_output(
        output, all_evidence_paths, label="region output"
    ) as reserved_output:
        command = _read_region_command(
            device, offset, length, Path(reserved_output.action_path)
        )
        result = command_runner(
            command,
            check=False,
            capture_output=True,
            text=True,
            pass_fds=(reserved_output.descriptor,),
        )
        _write_evidence(
            _evidence_path(evidence_prefix, ".read-region.stdout.txt"), result.stdout
        )
        _write_evidence(
            _evidence_path(evidence_prefix, ".read-region.stderr.txt"), result.stderr
        )
        if result.returncode != 0:
            raise QualificationError(f"espflash read-flash exited {result.returncode}")
        _capture_unchanged_usb_mapping(
            device=device,
            expected_usb_serial=expected_usb_serial,
            evidence_path=_evidence_path(
                evidence_prefix, ".ioreg-after-read-region.txt"
            ),
            phase="during region read",
            ioreg_reader=ioreg_reader,
        )
        action_target = parse_action_target_info(
            result.stdout,
            result.stderr,
            expected_mac=expected_mac,
            expected_flash_bytes=expected_flash_bytes,
        )
        try:
            output_bytes, digest = _finalize_read_output(
                reserved_output, expected_bytes=length
            )
        except QualificationError:
            raise
        except OSError as error:
            raise QualificationError(
                f"could not finalize region-read evidence: {error}"
            ) from error
        try:
            _publish_read_verification(
                reserved_output,
            _evidence_path(evidence_prefix, ".read-region.verified.json"),
            json.dumps(
                {
                    "board_info": asdict(board),
                    "read_target": asdict(action_target),
                    "usb": asdict(device),
                    "offset": offset,
                    "length": length,
                    "output": str(output),
                    "output_bytes": output_bytes,
                    "sha256": digest,
                },
                indent=2,
                sort_keys=True,
            )
                + "\n",
            )
        except OSError as error:
            raise QualificationError(
                f"could not finalize region-read evidence: {error}"
            ) from error
    return device, board, digest


def erase_region(
    *,
    expected_usb_serial: str,
    expected_mac: str,
    expected_flash_bytes: int,
    evidence_prefix: Path,
    offset: int,
    length: int,
    ioreg_reader: Callable[[], str] = read_ioreg,
    command_runner: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
    path_is_character_device: Callable[[str], bool] = is_character_device,
) -> tuple[UsbDevice, BoardInfo, str]:
    """Identity-bind an all-FF write and verify one exact E290 flash region."""

    _validate_region_request(
        expected_usb_serial=expected_usb_serial,
        expected_flash_bytes=expected_flash_bytes,
        offset=offset,
        length=length,
        require_erase_alignment=True,
    )
    action_suffixes = (
        ".erase-region.input.bin",
        ".erase-region.stdout.txt",
        ".erase-region.stderr.txt",
        ".ioreg-after-erase-region.txt",
        ".erase-region.post-board-info.stdout.txt",
        ".erase-region.post-board-info.stderr.txt",
        ".erase-region.readback.bin",
        ".erase-region.readback.stdout.txt",
        ".erase-region.readback.stderr.txt",
        ".ioreg-after-erase-readback.txt",
        ".erase-region.verified.json",
    )
    evidence_prefix.parent.mkdir(parents=True, exist_ok=True)
    _reserve_action_evidence(evidence_prefix, action_suffixes)
    erase_input_path = _evidence_path(evidence_prefix, ".erase-region.input.bin")
    readback = _evidence_path(evidence_prefix, ".erase-region.readback.bin")
    erase_input = _create_retained_fill_input(
        erase_input_path, length=length, value=0xFF
    )

    try:
        device, board = qualify_port(
            expected_usb_serial=expected_usb_serial,
            expected_mac=expected_mac,
            expected_flash_bytes=expected_flash_bytes,
            evidence_prefix=evidence_prefix,
            ioreg_reader=ioreg_reader,
            command_runner=command_runner,
            path_is_character_device=path_is_character_device,
        )
    except BaseException:
        os.close(erase_input.descriptor)
        raise

    erase_command = [
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
        str(offset),
        erase_input.action_path,
    ]
    try:
        try:
            erase_result = command_runner(
                erase_command,
                check=False,
                capture_output=True,
                text=True,
                pass_fds=(erase_input.descriptor,),
            )
        except (OSError, subprocess.SubprocessError) as error:
            raise PostWriteEvidenceError(
                f"identity-bound all-FF write invocation failed: {error}; "
                "target range state is unverified"
            ) from error
        try:
            _write_evidence(
                _evidence_path(evidence_prefix, ".erase-region.stdout.txt"),
                erase_result.stdout,
            )
            _write_evidence(
                _evidence_path(evidence_prefix, ".erase-region.stderr.txt"),
                erase_result.stderr,
            )
        except OSError as error:
            raise PostWriteEvidenceError(
                "identity-bound all-FF write was attempted but its action "
                f"streams were not preserved: {error}"
            ) from error
        try:
            _verify_retained_flash_input(erase_input)
        except QualificationError as error:
            raise PostWriteEvidenceError(
                f"all-FF write input is unverified after the action: {error}"
            ) from error
        if erase_result.returncode != 0:
            raise PostWriteEvidenceError(
                f"espflash write-bin exited {erase_result.returncode}; "
                "target range state is unverified"
            )
        try:
            erase_write_action_target = parse_action_target_info(
                erase_result.stdout,
                erase_result.stderr,
                expected_mac=expected_mac,
                expected_flash_bytes=expected_flash_bytes,
            )
        except QualificationError as error:
            raise PostWriteEvidenceError(
                f"all-FF write action target is unverified: {error}"
            ) from error
    finally:
        os.close(erase_input.descriptor)

    try:
        _capture_unchanged_usb_mapping(
            device=device,
            expected_usb_serial=expected_usb_serial,
            evidence_path=_evidence_path(
                evidence_prefix, ".ioreg-after-erase-region.txt"
            ),
            phase="during identity-bound all-FF write",
            ioreg_reader=ioreg_reader,
        )
    except (OSError, subprocess.SubprocessError, QualificationError) as error:
        raise PostWriteEvidenceError(
            f"all-FF write completed but its USB mapping is unverified: {error}"
        ) from error

    try:
        post_erase_board_result = command_runner(
            _loader_board_info_command(device),
            check=False,
            capture_output=True,
            text=True,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise PostWriteEvidenceError(
            f"all-FF write completed but post-write board-info failed: {error}"
        ) from error
    try:
        _write_evidence(
            _evidence_path(
                evidence_prefix, ".erase-region.post-board-info.stdout.txt"
            ),
            post_erase_board_result.stdout,
        )
        _write_evidence(
            _evidence_path(
                evidence_prefix, ".erase-region.post-board-info.stderr.txt"
            ),
            post_erase_board_result.stderr,
        )
    except OSError as error:
        raise PostWriteEvidenceError(
            "all-FF write completed but post-write target streams were not "
            f"preserved: {error}"
        ) from error
    if post_erase_board_result.returncode != 0:
        raise PostWriteEvidenceError(
            "all-FF write completed but post-write espflash board-info exited "
            f"{post_erase_board_result.returncode}"
        )
    try:
        post_write_target = parse_board_info(
            post_erase_board_result.stdout,
            post_erase_board_result.stderr,
            expected_mac=expected_mac,
            expected_flash_bytes=expected_flash_bytes,
        )
    except QualificationError as error:
        raise PostWriteEvidenceError(
            f"all-FF write completed on an unverified post-write target: {error}"
        ) from error

    readback_alias_paths = [
        _evidence_path(evidence_prefix, suffix)
        for suffix in (*EVIDENCE_SUFFIXES, *action_suffixes)
        if _evidence_path(evidence_prefix, suffix) != readback
    ]
    try:
        with _reserved_private_output(
            readback,
            readback_alias_paths,
            label="erased-region readback output",
        ) as reserved_readback:
            read_command = _read_region_command(
                device, offset, length, Path(reserved_readback.action_path)
            )
            read_result = command_runner(
                read_command,
                check=False,
                capture_output=True,
                text=True,
                pass_fds=(reserved_readback.descriptor,),
            )
            try:
                _write_evidence(
                    _evidence_path(
                        evidence_prefix, ".erase-region.readback.stdout.txt"
                    ),
                    read_result.stdout,
                )
                _write_evidence(
                    _evidence_path(
                        evidence_prefix, ".erase-region.readback.stderr.txt"
                    ),
                    read_result.stderr,
                )
            except OSError as error:
                raise PostWriteEvidenceError(
                    "all-FF write completed but readback streams were not "
                    f"preserved: {error}"
                ) from error
            if read_result.returncode != 0:
                raise PostWriteEvidenceError(
                    "all-FF write completed but espflash read-flash exited "
                    f"{read_result.returncode}"
                )
            try:
                _capture_unchanged_usb_mapping(
                    device=device,
                    expected_usb_serial=expected_usb_serial,
                    evidence_path=_evidence_path(
                        evidence_prefix, ".ioreg-after-erase-readback.txt"
                    ),
                    phase="during erased-region readback",
                    ioreg_reader=ioreg_reader,
                )
                read_target = parse_action_target_info(
                    read_result.stdout,
                    read_result.stderr,
                    expected_mac=expected_mac,
                    expected_flash_bytes=expected_flash_bytes,
                )
            except (OSError, subprocess.SubprocessError, QualificationError) as error:
                raise PostWriteEvidenceError(
                    "all-FF write completed but readback target is unverified: "
                    f"{error}"
                ) from error
            readback_bytes, readback_digest = _finalize_read_output(
                reserved_readback,
                expected_bytes=length,
                erased_offset=offset,
            )
            _publish_read_verification(
                reserved_readback,
                _evidence_path(evidence_prefix, ".erase-region.verified.json"),
                json.dumps(
                    {
                        "operation": "identity_bound_all_ff_write",
                        "board_info": asdict(board),
                        "erase_write_action_target": asdict(
                            erase_write_action_target
                        ),
                        "post_write_target": asdict(post_write_target),
                        "read_target": asdict(read_target),
                        "usb": asdict(device),
                        "offset": offset,
                        "length": length,
                        "erased_byte": 0xFF,
                        "erase_input": str(erase_input_path),
                        "erase_input_bytes": erase_input.bytes,
                        "erase_input_sha256": erase_input.sha256,
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
    except (OSError, subprocess.SubprocessError, QualificationError) as error:
        raise PostWriteEvidenceError(
            f"all-FF write completed but evidence finalization failed: {error}"
        ) from error
    return device, board, readback_digest


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
    return _validate_merged_image_properties(
        image_bytes=image_bytes,
        header=header,
        expected_flash_bytes=expected_flash_bytes,
    )


def _validate_merged_image_properties(
    *, image_bytes: int, header: bytes, expected_flash_bytes: int
) -> int:
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


def _validate_retained_merged_image(
    image: RetainedFlashInput, *, expected_flash_bytes: int
) -> int:
    _require_retained_flash_input(image)
    os.lseek(image.descriptor, 0, os.SEEK_SET)
    header = os.read(image.descriptor, 4)
    os.lseek(image.descriptor, 0, os.SEEK_SET)
    return _validate_merged_image_properties(
        image_bytes=image.bytes,
        header=header,
        expected_flash_bytes=expected_flash_bytes,
    )


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
        ".ioreg-after-write-bin.txt",
        ".post-write-board-info.stdout.txt",
        ".post-write-board-info.stderr.txt",
        ".readback.bin",
        ".readback.stdout.txt",
        ".readback.stderr.txt",
        ".ioreg-after-readback.txt",
        ".flash-image.verified.json",
    )
    evidence_prefix.parent.mkdir(parents=True, exist_ok=True)
    _reserve_action_evidence(evidence_prefix, action_suffixes)
    preserved_image = _evidence_path(evidence_prefix, ".flash-input.bin")
    readback = _evidence_path(evidence_prefix, ".readback.bin")
    retained_image = _copy_retained_flash_input(image, preserved_image)
    image_digest = retained_image.sha256
    try:
        image_bytes = _validate_retained_merged_image(
            retained_image, expected_flash_bytes=expected_flash_bytes
        )
    except BaseException:
        os.close(retained_image.descriptor)
        raise
    if image_digest != normalized_expected_digest:
        os.close(retained_image.descriptor)
        raise QualificationError(
            "merged flash image SHA-256 mismatch: "
            f"expected={normalized_expected_digest} actual={image_digest}"
        )

    try:
        device, board = qualify_port(
            expected_usb_serial=expected_usb_serial,
            expected_mac=expected_mac,
            expected_flash_bytes=expected_flash_bytes,
            evidence_prefix=evidence_prefix,
            ioreg_reader=ioreg_reader,
            command_runner=command_runner,
            path_is_character_device=path_is_character_device,
        )
    except BaseException:
        os.close(retained_image.descriptor)
        raise
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
        retained_image.action_path,
    ]
    try:
        write_result = command_runner(
            write_command,
            check=False,
            capture_output=True,
            text=True,
            pass_fds=(retained_image.descriptor,),
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
            raise PostWriteEvidenceError(
                "merged-image flash was attempted but action streams were not "
                f"preserved: {error}"
            ) from error
        try:
            _verify_retained_flash_input(retained_image)
        except QualificationError as error:
            raise PostWriteEvidenceError(
                "merged-image flash was attempted but its retained input is "
                f"unverified: {error}"
            ) from error
        if write_result.returncode != 0:
            raise PostWriteEvidenceError(
                f"espflash write-bin exited {write_result.returncode}; "
                "target flash state is unverified"
            )
        try:
            write_action_target = parse_action_target_info(
                write_result.stdout,
                write_result.stderr,
                expected_mac=expected_mac,
                expected_flash_bytes=expected_flash_bytes,
            )
        except QualificationError as error:
            raise PostWriteEvidenceError(
                f"merged-image write action target is unverified: {error}"
            ) from error
    finally:
        os.close(retained_image.descriptor)

    try:
        _capture_unchanged_usb_mapping(
            device=device,
            expected_usb_serial=expected_usb_serial,
            evidence_path=_evidence_path(
                evidence_prefix, ".ioreg-after-write-bin.txt"
            ),
            phase="during merged-image write",
            ioreg_reader=ioreg_reader,
        )
    except (OSError, subprocess.SubprocessError, QualificationError) as error:
        raise PostWriteEvidenceError(
            f"merged-image flash completed but its USB mapping is unverified: {error}"
        ) from error

    try:
        post_write_board_result = command_runner(
            _loader_board_info_command(device),
            check=False,
            capture_output=True,
            text=True,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise PostWriteEvidenceError(
            f"merged-image flash completed but post-write board-info failed: {error}"
        ) from error
    try:
        _write_evidence(
            _evidence_path(evidence_prefix, ".post-write-board-info.stdout.txt"),
            post_write_board_result.stdout,
        )
        _write_evidence(
            _evidence_path(evidence_prefix, ".post-write-board-info.stderr.txt"),
            post_write_board_result.stderr,
        )
    except OSError as error:
        raise PostWriteEvidenceError(
            "merged-image flash completed but post-write target streams were not "
            f"preserved: {error}"
        ) from error
    if post_write_board_result.returncode != 0:
        raise PostWriteEvidenceError(
            "merged-image flash completed but post-write espflash board-info exited "
            f"{post_write_board_result.returncode}"
        )
    try:
        post_write_target = parse_board_info(
            post_write_board_result.stdout,
            post_write_board_result.stderr,
            expected_mac=expected_mac,
            expected_flash_bytes=expected_flash_bytes,
        )
    except QualificationError as error:
        raise PostWriteEvidenceError(
            f"merged-image flash completed on an unverified target: {error}"
        ) from error

    readback_alias_paths = [
        _evidence_path(evidence_prefix, suffix)
        for suffix in (*EVIDENCE_SUFFIXES, *action_suffixes)
        if _evidence_path(evidence_prefix, suffix) != readback
    ]
    try:
        with _reserved_private_output(
            readback,
            readback_alias_paths,
            label="merged-image readback output",
        ) as reserved_readback:
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
                reserved_readback.action_path,
            ]
            read_result = command_runner(
                read_command,
                check=False,
                capture_output=True,
                text=True,
                pass_fds=(reserved_readback.descriptor,),
            )
            _write_evidence(
                _evidence_path(evidence_prefix, ".readback.stdout.txt"),
                read_result.stdout,
            )
            _write_evidence(
                _evidence_path(evidence_prefix, ".readback.stderr.txt"),
                read_result.stderr,
            )
            if read_result.returncode != 0:
                raise PostWriteEvidenceError(
                    "merged-image flash completed but espflash read-flash exited "
                    f"{read_result.returncode}"
                )
            _capture_unchanged_usb_mapping(
                device=device,
                expected_usb_serial=expected_usb_serial,
                evidence_path=_evidence_path(
                    evidence_prefix, ".ioreg-after-readback.txt"
                ),
                phase="during merged-image readback",
                ioreg_reader=ioreg_reader,
            )
            read_target = parse_action_target_info(
                read_result.stdout,
                read_result.stderr,
                expected_mac=expected_mac,
                expected_flash_bytes=expected_flash_bytes,
            )
            try:
                readback_bytes, readback_digest = _finalize_read_output(
                    reserved_readback, expected_bytes=image_bytes
                )
            except QualificationError as error:
                raise PostWriteEvidenceError(
                    f"merged-image readback mismatch: {error}"
                ) from error
            try:
                if readback_bytes != image_bytes or readback_digest != image_digest:
                    raise PostWriteEvidenceError(
                        "merged-image readback mismatch: "
                        f"bytes={readback_bytes}/{image_bytes} "
                        f"sha256={readback_digest}/{image_digest}"
                    )
                if _sha256_file(preserved_image) != image_digest:
                    raise PostWriteEvidenceError(
                        "merged-image flash completed but its immutable input copy "
                        "changed"
                    )
                _publish_read_verification(
                    reserved_readback,
                    _evidence_path(evidence_prefix, ".flash-image.verified.json"),
                    json.dumps(
                        {
                            "board_info": asdict(board),
                            "write_action_target": asdict(write_action_target),
                            "post_write_target": asdict(post_write_target),
                            "read_target": asdict(read_target),
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
            except BaseException:
                _mark_read_output_unverified(reserved_readback)
                raise
    except PostWriteEvidenceError:
        raise
    except (OSError, subprocess.SubprocessError, QualificationError) as error:
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
        ".ioreg-after-verify-readback.txt",
        ".verify-image.verified.json",
    )
    evidence_prefix.parent.mkdir(parents=True, exist_ok=True)
    _reserve_action_evidence(evidence_prefix, action_suffixes)
    preserved_image = _evidence_path(evidence_prefix, ".verify-input.bin")
    readback = _evidence_path(evidence_prefix, ".verify-readback.bin")
    retained_image = _copy_retained_flash_input(image, preserved_image)
    image_digest = retained_image.sha256
    if image_digest != normalized_expected_digest:
        os.close(retained_image.descriptor)
        raise QualificationError(
            "merged flash image SHA-256 mismatch: "
            f"expected={normalized_expected_digest} actual={image_digest}"
        )
    try:
        image_bytes = _validate_retained_merged_image(
            retained_image, expected_flash_bytes=expected_flash_bytes
        )
    except BaseException:
        os.close(retained_image.descriptor)
        raise

    try:
        device, board = qualify_port(
            expected_usb_serial=expected_usb_serial,
            expected_mac=expected_mac,
            expected_flash_bytes=expected_flash_bytes,
            evidence_prefix=evidence_prefix,
            ioreg_reader=ioreg_reader,
            command_runner=command_runner,
            path_is_character_device=path_is_character_device,
        )
    except BaseException:
        os.close(retained_image.descriptor)
        raise
    readback_alias_paths = [
        _evidence_path(evidence_prefix, suffix)
        for suffix in (*EVIDENCE_SUFFIXES, *action_suffixes)
        if _evidence_path(evidence_prefix, suffix) != readback
    ]
    try:
        with _reserved_private_output(
            readback,
            readback_alias_paths,
            label="merged-image verification readback",
        ) as reserved_readback:
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
                reserved_readback.action_path,
            ]
            result = command_runner(
                command,
                check=False,
                capture_output=True,
                text=True,
                pass_fds=(reserved_readback.descriptor,),
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
                raise QualificationError(
                    f"espflash read-flash exited {result.returncode}"
                )
            _capture_unchanged_usb_mapping(
                device=device,
                expected_usb_serial=expected_usb_serial,
                evidence_path=_evidence_path(
                    evidence_prefix, ".ioreg-after-verify-readback.txt"
                ),
                phase="during merged-image verification readback",
                ioreg_reader=ioreg_reader,
            )
            read_target = parse_action_target_info(
                result.stdout,
                result.stderr,
                expected_mac=expected_mac,
                expected_flash_bytes=expected_flash_bytes,
            )
            try:
                readback_bytes, readback_digest = _finalize_read_output(
                    reserved_readback, expected_bytes=image_bytes
                )
            except QualificationError as error:
                raise QualificationError(
                    f"merged-image readback mismatch: {error}"
                ) from error
            try:
                if readback_bytes != image_bytes or readback_digest != image_digest:
                    raise QualificationError(
                        "merged-image readback mismatch: "
                        f"bytes={readback_bytes}/{image_bytes} "
                        f"sha256={readback_digest}/{image_digest}"
                    )
                _verify_retained_flash_input(retained_image)
                _publish_read_verification(
                    reserved_readback,
                    _evidence_path(evidence_prefix, ".verify-image.verified.json"),
                    json.dumps(
                        {
                            "board_info": asdict(board),
                            "read_target": asdict(read_target),
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
            except BaseException:
                _mark_read_output_unverified(reserved_readback)
                raise
    except QualificationError:
        raise
    except OSError as error:
        raise QualificationError(
            f"could not finalize merged-image verification evidence: {error}"
        ) from error
    finally:
        os.close(retained_image.descriptor)
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
        ".ioreg-after-flash.txt",
        ".post-flash-board-info.stdout.txt",
        ".post-flash-board-info.stderr.txt",
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
    retained_elf = _copy_retained_flash_input(elf, preserved_elf)
    try:
        retained_partition = _copy_retained_flash_input(
            partition_table, preserved_partition_table
        )
    except BaseException:
        os.close(retained_elf.descriptor)
        raise
    elf_digest = retained_elf.sha256
    partition_digest = retained_partition.sha256
    try:
        device, board = qualify_port(
            expected_usb_serial=expected_usb_serial,
            expected_mac=expected_mac,
            expected_flash_bytes=expected_flash_bytes,
            evidence_prefix=evidence_prefix,
            ioreg_reader=ioreg_reader,
            command_runner=command_runner,
            path_is_character_device=path_is_character_device,
        )
    except BaseException:
        os.close(retained_elf.descriptor)
        os.close(retained_partition.descriptor)
        raise
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
        retained_partition.action_path,
        retained_elf.action_path,
    ]
    try:
        result = command_runner(
            command,
            check=False,
            capture_output=True,
            text=True,
            pass_fds=(retained_partition.descriptor, retained_elf.descriptor),
        )
        try:
            _write_evidence(
                _evidence_path(evidence_prefix, ".flash.stdout.txt"), result.stdout
            )
            _write_evidence(
                _evidence_path(evidence_prefix, ".flash.stderr.txt"), result.stderr
            )
        except OSError as error:
            raise PostWriteEvidenceError(
                "qualification flash was attempted but action streams were not "
                f"preserved: {error}"
            ) from error
        try:
            _verify_retained_flash_input(retained_elf)
            _verify_retained_flash_input(retained_partition)
        except QualificationError as error:
            raise PostWriteEvidenceError(
                "qualification flash was attempted but a retained input is "
                f"unverified: {error}"
            ) from error
        if result.returncode != 0:
            raise PostWriteEvidenceError(
                f"espflash flash exited {result.returncode}; target flash state is "
                "unverified"
            )
        try:
            flash_action_target = parse_action_target_info(
                result.stdout,
                result.stderr,
                expected_mac=expected_mac,
                expected_flash_bytes=expected_flash_bytes,
            )
        except QualificationError as error:
            raise PostWriteEvidenceError(
                f"qualification flash action target is unverified: {error}"
            ) from error
    finally:
        os.close(retained_elf.descriptor)
        os.close(retained_partition.descriptor)

    try:
        _capture_unchanged_usb_mapping(
            device=device,
            expected_usb_serial=expected_usb_serial,
            evidence_path=_evidence_path(evidence_prefix, ".ioreg-after-flash.txt"),
            phase="during qualification flash",
            ioreg_reader=ioreg_reader,
        )
    except (OSError, subprocess.SubprocessError, QualificationError) as error:
        raise PostWriteEvidenceError(
            f"qualification flash completed but its USB mapping is unverified: {error}"
        ) from error

    try:
        post_flash_result = command_runner(
            _loader_board_info_command(device),
            check=False,
            capture_output=True,
            text=True,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise PostWriteEvidenceError(
            f"qualification flash completed but post-flash board-info failed: {error}"
        ) from error
    try:
        _write_evidence(
            _evidence_path(evidence_prefix, ".post-flash-board-info.stdout.txt"),
            post_flash_result.stdout,
        )
        _write_evidence(
            _evidence_path(evidence_prefix, ".post-flash-board-info.stderr.txt"),
            post_flash_result.stderr,
        )
    except OSError as error:
        raise PostWriteEvidenceError(
            "qualification flash completed but post-flash target streams were not "
            f"preserved: {error}"
        ) from error
    if post_flash_result.returncode != 0:
        raise PostWriteEvidenceError(
            "qualification flash completed but post-flash espflash board-info exited "
            f"{post_flash_result.returncode}"
        )
    try:
        post_flash_target = parse_board_info(
            post_flash_result.stdout,
            post_flash_result.stderr,
            expected_mac=expected_mac,
            expected_flash_bytes=expected_flash_bytes,
        )
    except QualificationError as error:
        raise PostWriteEvidenceError(
            f"qualification flash completed on an unverified target: {error}"
        ) from error

    try:
        if _sha256_file(preserved_elf) != elf_digest or _sha256_file(
            preserved_partition_table
        ) != partition_digest:
            raise PostWriteEvidenceError(
                "qualification flash completed but an immutable input copy changed"
            )
        _publish_verified_evidence(
            _evidence_path(evidence_prefix, ".flash.verified.json"),
            json.dumps(
                {
                    "board_info": asdict(board),
                    "flash_action_target": asdict(flash_action_target),
                    "post_flash_target": asdict(post_flash_target),
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
    except (OSError, QualificationError) as error:
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


def _parse_nonnegative_integer(value: str) -> int:
    try:
        parsed = int(value, 0)
    except ValueError as error:
        raise argparse.ArgumentTypeError("must be an integer") from error
    if parsed < 0:
        raise argparse.ArgumentTypeError("must be non-negative")
    return parsed


def _parse_positive_integer(value: str) -> int:
    try:
        parsed = int(value, 0)
    except ValueError as error:
        raise argparse.ArgumentTypeError("must be an integer") from error
    if parsed <= 0:
        raise argparse.ArgumentTypeError("must be positive")
    return parsed


def _add_identity_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "--usb-serial",
        required=True,
        help="expected native-USB serial (exact uppercase for region actions)",
    )
    parser.add_argument(
        "--expected-mac", required=True, help="expected eFuse MAC address"
    )
    parser.add_argument(
        "--expected-flash-bytes",
        required=True,
        type=_parse_flash_bytes,
        help="expected physical flash capacity (region actions require 16777216)",
    )
    parser.add_argument(
        "--evidence-prefix",
        required=True,
        type=Path,
        help="fresh path prefix for exclusive identity and action evidence",
    )


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
    region_read = actions.add_parser(
        "read-region",
        help="identity-qualify and hash one exact 16 MiB E290 flash region",
    )
    _add_identity_arguments(region_read)
    region_read.add_argument(
        "--offset",
        required=True,
        type=_parse_nonnegative_integer,
        help="exact zero-based flash byte offset (decimal or 0x-prefixed)",
    )
    region_read.add_argument(
        "--length",
        required=True,
        type=_parse_positive_integer,
        help="exact positive byte count (decimal or 0x-prefixed)",
    )
    region_read.add_argument(
        "--output", required=True, type=Path, help="fresh region output file"
    )
    region_erase = actions.add_parser(
        "erase-region",
        help=(
            "identity-bind an all-FF write and verify one exact 16 MiB E290 "
            "region (erase-equivalent logical blanking)"
        ),
        description=(
            "Identity-bind a sector-aligned all-FF write-bin action and exact "
            "readback. This proves erase-equivalent logical blank state; it "
            "does not invoke espflash's native erase-region command."
        ),
    )
    _add_identity_arguments(region_erase)
    region_erase.add_argument(
        "--offset",
        required=True,
        type=_parse_nonnegative_integer,
        help="sector-aligned flash byte offset (decimal or 0x-prefixed)",
    )
    region_erase.add_argument(
        "--length",
        required=True,
        type=_parse_positive_integer,
        help="sector-aligned positive byte count (decimal or 0x-prefixed)",
    )
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
        elif args.action == "read-region":
            device, _board, _digest = read_region(
                offset=args.offset,
                length=args.length,
                output=args.output,
                **common,
            )
        elif args.action == "erase-region":
            device, _board, _digest = erase_region(
                offset=args.offset,
                length=args.length,
                **common,
            )
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
