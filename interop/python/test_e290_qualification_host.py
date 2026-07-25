from __future__ import annotations

import csv
import errno
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import tempfile
import unittest
from unittest import mock

import e290_qualification_host as host


MAC_A = "aa:bb:cc:dd:ee:01"
MAC_B = "aa:bb:cc:dd:ee:02"
ROOT = Path(__file__).resolve().parents[2]


def ioreg_device(serial: str, port: str, *, vendor: int = 12346) -> str:
    return f'''\
+-o USB JTAG/serial debug unit@00100000  <class IOUSBHostDevice, id 1>
  | {{
  |   "idProduct" = 4097
  |   "kUSBSerialNumberString" = "{serial.upper()}"
  |   "idVendor" = {vendor}
  | }}
  | +-o IOSerialBSDClient  <class IOSerialBSDClient, id 2>
  |     {{
  |       "IOCalloutDevice" = "{port}"
  |     }}
'''


def board_info(
    *,
    mac: str = MAC_A,
    flash: str = "16MB",
    secure_boot: str = "Disabled",
    flash_encryption: str = "Disabled",
) -> str:
    return f'''\
Chip type:         esp32s3 (revision v0.2)
Crystal frequency: 40 MHz
Flash size:        {flash}
Features:          WiFi, BLE, Embedded Flash
MAC address:       {mac}

Security Information:
=====================
Secure Boot: {secure_boot}
Flash Encryption: {flash_encryption}
'''


def merged_image_bytes(payload: bytes = b"test-application") -> bytes:
    image = bytearray(
        b"\xFF"
        * (host.E290_FACTORY_OFFSET + host.ESP_IMAGE_HEADER_BYTES + len(payload))
    )
    header = bytearray(host.ESP_IMAGE_HEADER_BYTES)
    header[:4] = bytes((0xE9, 0x01, 0x02, 0x4F))
    header[12:14] = host.ESP32S3_IMAGE_CHIP_ID.to_bytes(2, "little")
    image[: host.ESP_IMAGE_HEADER_BYTES] = header
    image[
        host.E290_PARTITION_TABLE_OFFSET : host.E290_PARTITION_TABLE_OFFSET
        + host.E290_PARTITION_TABLE_BYTES
    ] = host.E290_PARTITION_TABLE_REGION
    image[
        host.E290_FACTORY_OFFSET : host.E290_FACTORY_OFFSET
        + host.ESP_IMAGE_HEADER_BYTES
    ] = header
    image[host.E290_FACTORY_OFFSET + host.ESP_IMAGE_HEADER_BYTES :] = payload
    return bytes(image)


def checked_in_e290_partitions() -> tuple[tuple[str, int, int, int, int, int], ...]:
    table = ROOT / "partitions" / "heltec-vision-master-e290-node.csv"
    partition_types = {"app": 0x00, "data": 0x01}
    partition_subtypes = {
        ("app", "factory"): 0x00,
        ("data", "phy"): 0x01,
        ("data", "nvs"): 0x02,
        ("data", "undefined"): 0x06,
    }
    rows = []
    with table.open(newline="") as source:
        data_lines = (
            line for line in source if line.strip() and not line.lstrip().startswith("#")
        )
        for raw_row in csv.reader(data_lines):
            label, type_name, subtype_name, offset, size, flags = (
                field.strip() for field in raw_row
            )
            rows.append(
                (
                    label,
                    partition_types[type_name],
                    partition_subtypes[(type_name, subtype_name)],
                    int(offset, 0),
                    int(size, 0),
                    int(flags, 0) if flags else 0,
                )
            )
    return tuple(rows)


class E290FlashRunbookPolicyTests(unittest.TestCase):
    @staticmethod
    def e290_runbooks() -> tuple[Path, ...]:
        docs = ROOT / "docs"
        return (
            ROOT / "README.md",
            docs / "heltec-vision-master-e290.md",
            *sorted(docs.glob("e290-*.md")),
        )

    @staticmethod
    def shell_blocks(source: str) -> tuple[str, ...]:
        return tuple(
            match.group(1)
            for match in re.finditer(r"```(?:sh|bash)\n(.*?)```", source, re.DOTALL)
        )

    def test_direct_e290_espflash_flash_commands_pin_product_geometry(self) -> None:
        direct_flash = re.compile(r"\bespflash\s+(?:\\\s*)?flash\b")
        commands: list[tuple[Path, str]] = []
        for runbook in self.e290_runbooks():
            source = runbook.read_text(encoding="utf-8")
            commands.extend(
                (runbook, block)
                for block in self.shell_blocks(source)
                if direct_flash.search(block)
            )

        self.assertTrue(commands, "expected at least one direct E290 flash runbook")
        for runbook, command in commands:
            with self.subTest(runbook=runbook):
                self.assertIn("--flash-size 16mb", command)
                self.assertIn(
                    "--partition-table partitions/heltec-vision-master-e290-node.csv",
                    command,
                )

    def test_merged_image_runbooks_pin_capacity_and_explain_the_exception(
        self,
    ) -> None:
        runbooks_with_merged_flash = 0
        for runbook in self.e290_runbooks():
            source = runbook.read_text(encoding="utf-8")
            blocks = [
                block
                for block in self.shell_blocks(source)
                if "e290_qualification_host.py flash-merged" in block
            ]
            if not blocks:
                continue
            runbooks_with_merged_flash += 1
            with self.subTest(runbook=runbook):
                self.assertIn("intentional exception", source)
                for block in blocks:
                    self.assertIn("--expected-flash-bytes 16777216", block)

        self.assertGreaterEqual(runbooks_with_merged_flash, 2)

    def test_only_memory_qualification_documents_an_alternate_csv(self) -> None:
        display = (ROOT / "docs" / "e290-display-hil.md").read_text(encoding="utf-8")
        semantic = (ROOT / "docs" / "e290-semantic-hil.md").read_text(
            encoding="utf-8"
        )
        dossier = (ROOT / "docs" / "heltec-vision-master-e290.md").read_text(
            encoding="utf-8"
        )

        self.assertIn(
            "--partition-table partitions/heltec-vision-master-e290-node.csv",
            display,
        )
        self.assertIn(
            "--partition-table partitions/heltec-vision-master-e290-node.csv",
            semantic,
        )
        self.assertNotIn(
            "--partition-table partitions/heltec-vision-master-e290-semantic-hil.csv",
            semantic,
        )
        self.assertIn("intentional exception to the\nproduct CSV rule", dossier)
        self.assertIn(
            "--partition-table partitions/heltec-vision-master-e290-qualification.csv",
            dossier,
        )


class IoregParserTests(unittest.TestCase):
    def test_nested_hub_topology_keeps_each_callout_with_its_board(self) -> None:
        nested = f'''\
+-o Root Hub  <class IOUSBHostDevice, id 1>
  | {{
  |   "idProduct" = 1
  |   "kUSBSerialNumberString" = "unrelated-hub"
  |   "idVendor" = 2
  | }}
  +-o Port A  <class AppleUSB20HubPort, id 2>
    +-o Board A  <class IOUSBHostDevice, id 3>
      | {{
      |   "idProduct" = 4097
      |   "kUSBSerialNumberString" = "{MAC_A.upper()}"
      |   "idVendor" = 12346
      | }}
      +-o Interface A  <class IOUSBHostInterface, id 4>
        +-o Serial A  <class IOSerialBSDClient, id 5>
          | {{
          |   "IOCalloutDevice" = "/dev/cu.usbmodem-a"
          | }}
  +-o Port B  <class AppleUSB20HubPort, id 6>
    +-o Board B  <class IOUSBHostDevice, id 7>
      | {{
      |   "idProduct" = 4097
      |   "kUSBSerialNumberString" = "{MAC_B.upper()}"
      |   "idVendor" = 12346
      | }}
      +-o Interface B  <class IOUSBHostInterface, id 8>
        +-o Serial B  <class IOSerialBSDClient, id 9>
          | {{
          |   "IOCalloutDevice" = "/dev/cu.usbmodem-b"
          | }}
'''
        devices = host.parse_ioreg(nested)
        self.assertEqual(len(devices), 2)
        self.assertEqual(
            host.select_usb_device(devices, MAC_A).callout_device,
            "/dev/cu.usbmodem-a",
        )
        self.assertEqual(
            host.select_usb_device(devices, MAC_B).callout_device,
            "/dev/cu.usbmodem-b",
        )

    def test_complete_stream_maps_each_serial_to_its_callout(self) -> None:
        devices = host.parse_ioreg(
            ioreg_device(MAC_A, "/dev/cu.usbmodem101")
            + ioreg_device(MAC_B, "/dev/cu.usbmodem201")
        )
        selected = host.select_usb_device(devices, MAC_B.upper())
        self.assertEqual(selected.usb_serial, MAC_B.upper())
        self.assertEqual(selected.callout_device, "/dev/cu.usbmodem201")

    def test_duplicate_serial_fails_closed(self) -> None:
        devices = host.parse_ioreg(
            ioreg_device(MAC_A, "/dev/cu.usbmodem101")
            + ioreg_device(MAC_A, "/dev/cu.usbmodem201")
        )
        with self.assertRaisesRegex(host.QualificationError, "exactly one device"):
            host.select_usb_device(devices, MAC_A)

    def test_wrong_usb_vendor_fails_closed(self) -> None:
        devices = host.parse_ioreg(
            ioreg_device(MAC_A, "/dev/cu.usbmodem101", vendor=0x1234)
        )
        with self.assertRaisesRegex(host.QualificationError, "unexpected vendor"):
            host.select_usb_device(devices, MAC_A)


class BoardInfoParserTests(unittest.TestCase):
    def test_accepts_exact_16_mib_plaintext_development_board(self) -> None:
        parsed = host.parse_board_info(
            board_info(),
            "[WARN] Setting baud rate higher than 115,200 can cause issues\n",
            expected_mac=MAC_A,
            expected_flash_bytes=16 * 1024 * 1024,
        )
        self.assertEqual(parsed.chip, "esp32s3")
        self.assertEqual(parsed.flash_bytes, 16 * 1024 * 1024)
        self.assertEqual(parsed.mac, MAC_A)

    def test_rejects_flash_detection_fallback(self) -> None:
        with self.assertRaisesRegex(host.QualificationError, "flash-detection"):
            host.parse_board_info(
                board_info(flash="4MB"),
                "Could not detect flash size (FlashID=1, SizeID=2), defaulting to 4MB",
                expected_mac=MAC_A,
                expected_flash_bytes=8 * 1024 * 1024,
            )

    def test_rejects_enabled_secure_boot(self) -> None:
        with self.assertRaisesRegex(host.QualificationError, "secure boot"):
            host.parse_board_info(
                board_info(secure_boot="Enabled"),
                "",
                expected_mac=MAC_A,
                expected_flash_bytes=16 * 1024 * 1024,
            )

    def test_rejects_enabled_flash_encryption(self) -> None:
        with self.assertRaisesRegex(host.QualificationError, "flash encryption"):
            host.parse_board_info(
                board_info(flash_encryption="Enabled"),
                "",
                expected_mac=MAC_A,
                expected_flash_bytes=16 * 1024 * 1024,
            )

    def test_rejects_changed_identity_or_capacity(self) -> None:
        with self.assertRaisesRegex(host.QualificationError, "expected MAC"):
            host.parse_board_info(
                board_info(mac=MAC_B),
                "",
                expected_mac=MAC_A,
                expected_flash_bytes=16 * 1024 * 1024,
            )
        with self.assertRaisesRegex(host.QualificationError, "expected 8388608"):
            host.parse_board_info(
                board_info(),
                "",
                expected_mac=MAC_A,
                expected_flash_bytes=8 * 1024 * 1024,
            )


class QualificationFlowTests(unittest.TestCase):
    def test_preserves_evidence_and_revalidates_unchanged_mapping(self) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")
        calls: list[list[str]] = []

        def runner(command: list[str], **kwargs) -> subprocess.CompletedProcess[str]:
            calls.append(command)
            return subprocess.CompletedProcess(command, 0, board_info(), "")

        with tempfile.TemporaryDirectory() as directory:
            prefix = Path(directory) / "board-a"
            device, info = host.qualify_port(
                expected_usb_serial=MAC_A,
                expected_mac=MAC_A,
                expected_flash_bytes=16 * 1024 * 1024,
                evidence_prefix=prefix,
                ioreg_reader=lambda: ioreg,
                command_runner=runner,
                path_is_character_device=lambda path: path == "/dev/cu.usbmodem101",
            )
            self.assertEqual(device.callout_device, "/dev/cu.usbmodem101")
            self.assertEqual(info.flash_bytes, 16 * 1024 * 1024)
            self.assertIn("--after", calls[0])
            self.assertIn("no-reset", calls[0])
            self.assertTrue(
                host._evidence_path(prefix, ".board-info.verified.json").is_file()
            )
            self.assertTrue(
                host._evidence_path(prefix, ".ioreg-before.txt").is_file()
            )
            self.assertTrue(host._evidence_path(prefix, ".ioreg-after.txt").is_file())

    def test_mapping_change_after_board_info_fails_closed(self) -> None:
        streams = iter(
            (
                ioreg_device(MAC_A, "/dev/cu.usbmodem101"),
                ioreg_device(MAC_A, "/dev/cu.usbmodem201"),
            )
        )
        result = subprocess.CompletedProcess([], 0, board_info(), "")
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(host.QualificationError, "mapping changed"):
                host.qualify_port(
                    expected_usb_serial=MAC_A,
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=Path(directory) / "board-a",
                    ioreg_reader=lambda: next(streams),
                    command_runner=lambda *_args, **_kwargs: result,
                    path_is_character_device=lambda _path: True,
                )

    def test_existing_evidence_or_non_character_port_fails_before_board_info(self) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")
        calls = 0

        def runner(*_args, **_kwargs):
            nonlocal calls
            calls += 1
            return subprocess.CompletedProcess([], 0, board_info(), "")

        with tempfile.TemporaryDirectory() as directory:
            prefix = Path(directory) / "board-a"
            host._evidence_path(prefix, ".board-info.verified.json").write_text(
                "old evidence\n"
            )
            with self.assertRaisesRegex(host.QualificationError, "refusing to overwrite"):
                host.qualify_port(
                    expected_usb_serial=MAC_A,
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=prefix,
                    ioreg_reader=lambda: ioreg,
                    command_runner=runner,
                    path_is_character_device=lambda _path: True,
                )
            self.assertEqual(calls, 0)

        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(host.QualificationError, "not a character device"):
                host.qualify_port(
                    expected_usb_serial=MAC_A,
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=Path(directory) / "board-a",
                    ioreg_reader=lambda: ioreg,
                    command_runner=runner,
                    path_is_character_device=lambda _path: False,
                )
            self.assertEqual(calls, 0)

    def test_dotted_evidence_prefixes_remain_distinct(self) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")
        result = subprocess.CompletedProcess([], 0, board_info(), "")
        with tempfile.TemporaryDirectory() as directory:
            first = Path(directory) / "board-a.run1"
            second = Path(directory) / "board-a.run2"
            for prefix in (first, second):
                host.qualify_port(
                    expected_usb_serial=MAC_A,
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=prefix,
                    ioreg_reader=lambda: ioreg,
                    command_runner=lambda *_args, **_kwargs: result,
                    path_is_character_device=lambda _path: True,
                )
            first_record = host._evidence_path(
                first, ".board-info.verified.json"
            )
            second_record = host._evidence_path(
                second, ".board-info.verified.json"
            )
            self.assertNotEqual(first_record, second_record)
            self.assertTrue(first_record.is_file())
            self.assertTrue(second_record.is_file())


class VerifiedEvidenceTests(unittest.TestCase):
    def test_partial_temp_write_failure_leaves_no_sentinel_or_temp(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            final = Path(directory) / "operation.verified.json"
            with mock.patch.object(host.os, "fsync", side_effect=OSError("injected")):
                with self.assertRaisesRegex(OSError, "injected"):
                    host._publish_verified_evidence(final, '{"verified": true}\n')
            self.assertFalse(final.exists())
            self.assertEqual(list(final.parent.glob(f".{final.name}.*.tmp")), [])

    def test_existing_sentinel_is_never_replaced(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            final = Path(directory) / "operation.verified.json"
            final.write_text("existing\n")
            with self.assertRaises(FileExistsError):
                host._publish_verified_evidence(final, "replacement\n")
            self.assertEqual(final.read_text(), "existing\n")
            self.assertEqual(list(final.parent.glob(f".{final.name}.*.tmp")), [])


class ReadOutputFinalizationTests(unittest.TestCase):
    def test_file_fsync_failure_restores_private_mode(self) -> None:
        payload = b"retained flash readback"
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "readback.bin"
            with host._reserved_private_output(
                path, (), label="flash readback"
            ) as output:
                os.write(output.descriptor, payload)
                real_fsync = host.os.fsync
                injected = False

                def fail_first_output_sync(descriptor: int) -> None:
                    nonlocal injected
                    if descriptor == output.descriptor and not injected:
                        injected = True
                        self.assertEqual(
                            stat.S_IMODE(os.fstat(descriptor).st_mode), 0o400
                        )
                        raise OSError("injected file fsync failure")
                    real_fsync(descriptor)

                with mock.patch.object(
                    host.os, "fsync", side_effect=fail_first_output_sync
                ):
                    with self.assertRaisesRegex(OSError, "file fsync"):
                        host._finalize_read_output(
                            output, expected_bytes=len(payload)
                        )
                self.assertTrue(injected)
                self.assertEqual(
                    stat.S_IMODE(os.fstat(output.descriptor).st_mode), 0o600
                )
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)

    def test_parent_open_failure_restores_private_mode(self) -> None:
        payload = b"retained flash readback"
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "readback.bin"
            with host._reserved_private_output(
                path, (), label="flash readback"
            ) as output:
                os.write(output.descriptor, payload)

                def fail_parent_open(*_args, **_kwargs):
                    self.assertEqual(
                        stat.S_IMODE(os.fstat(output.descriptor).st_mode), 0o400
                    )
                    raise OSError("injected parent open failure")

                with mock.patch.object(
                    host.os, "open", side_effect=fail_parent_open
                ):
                    with self.assertRaisesRegex(OSError, "parent open"):
                        host._finalize_read_output(
                            output, expected_bytes=len(payload)
                        )
                self.assertEqual(
                    stat.S_IMODE(os.fstat(output.descriptor).st_mode), 0o600
                )
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)

    def test_parent_fsync_failure_restores_private_mode(self) -> None:
        payload = b"retained flash readback"
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "readback.bin"
            with host._reserved_private_output(
                path, (), label="flash readback"
            ) as output:
                os.write(output.descriptor, payload)
                real_fsync = host.os.fsync
                injected = False

                def fail_parent_sync(descriptor: int) -> None:
                    nonlocal injected
                    if descriptor != output.descriptor and not injected:
                        injected = True
                        self.assertEqual(
                            stat.S_IMODE(
                                os.fstat(output.descriptor).st_mode
                            ),
                            0o400,
                        )
                        raise OSError("injected parent fsync failure")
                    real_fsync(descriptor)

                with mock.patch.object(
                    host.os, "fsync", side_effect=fail_parent_sync
                ):
                    with self.assertRaisesRegex(OSError, "parent fsync"):
                        host._finalize_read_output(
                            output, expected_bytes=len(payload)
                        )
                self.assertTrue(injected)
                self.assertEqual(
                    stat.S_IMODE(os.fstat(output.descriptor).st_mode), 0o600
                )
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)

    def test_parent_close_failure_restores_private_mode(self) -> None:
        payload = b"retained flash readback"
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "readback.bin"
            with host._reserved_private_output(
                path, (), label="flash readback"
            ) as output:
                os.write(output.descriptor, payload)
                real_close = host.os.close
                injected = False

                def fail_parent_close(descriptor: int) -> None:
                    nonlocal injected
                    if descriptor != output.descriptor and not injected:
                        injected = True
                        self.assertEqual(
                            stat.S_IMODE(
                                os.fstat(output.descriptor).st_mode
                            ),
                            0o400,
                        )
                        real_close(descriptor)
                        raise OSError("injected parent close failure")
                    real_close(descriptor)

                with mock.patch.object(
                    host.os, "close", side_effect=fail_parent_close
                ):
                    with self.assertRaisesRegex(OSError, "parent close"):
                        host._finalize_read_output(
                            output, expected_bytes=len(payload)
                        )
                self.assertTrue(injected)
                self.assertEqual(
                    stat.S_IMODE(os.fstat(output.descriptor).st_mode), 0o600
                )
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)


class BoundActionTests(unittest.TestCase):
    def test_region_read_is_identity_gated_and_records_exact_range(self) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")
        commands: list[list[str]] = []
        payload = bytes(range(251)) + b"region-tail"

        def runner(command: list[str], **_kwargs) -> subprocess.CompletedProcess[str]:
            commands.append(command)
            if command[1] == "board-info":
                return subprocess.CompletedProcess(command, 0, board_info(), "")
            self.assertEqual(command[1], "read-flash")
            self.assertEqual(command[command.index("--before") + 1], "no-reset")
            self.assertEqual(command[command.index("--after") + 1], "no-reset")
            self.assertEqual(command[-3], str(0x612345))
            self.assertEqual(command[-2], str(len(payload)))
            Path(command[-1]).write_bytes(payload)
            return subprocess.CompletedProcess(command, 0, board_info(), "")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / "region.bin"
            prefix = root / "board-a-region"
            _device, board, digest = host.read_region(
                expected_usb_serial=MAC_A.upper(),
                expected_mac=MAC_A,
                expected_flash_bytes=16 * 1024 * 1024,
                evidence_prefix=prefix,
                offset=0x612345,
                length=len(payload),
                output=output,
                ioreg_reader=lambda: ioreg,
                command_runner=runner,
                path_is_character_device=lambda _path: True,
            )
            self.assertEqual(board.flash_bytes, 16 * 1024 * 1024)
            self.assertEqual(digest, hashlib.sha256(payload).hexdigest())
            self.assertEqual(
                [command[1] for command in commands], ["board-info", "read-flash"]
            )
            evidence = json.loads(
                host._evidence_path(prefix, ".read-region.verified.json").read_text()
            )
            self.assertEqual(evidence["offset"], 0x612345)
            self.assertEqual(evidence["length"], len(payload))
            self.assertEqual(evidence["output"], str(output))
            self.assertEqual(evidence["output_bytes"], len(payload))
            self.assertEqual(evidence["sha256"], digest)
            self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o400)

    def test_region_output_case_alias_is_rejected_before_hardware(self) -> None:
        calls = 0

        def forbidden(*_args, **_kwargs):
            nonlocal calls
            calls += 1
            raise AssertionError("hardware access must not run")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            prefix = root / "board-a"
            with self.assertRaisesRegex(host.QualificationError, "aliases"):
                host.read_region(
                    expected_usb_serial=MAC_A.upper(),
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=prefix,
                    offset=0,
                    length=1,
                    output=root / "BOARD-A.IOREG-BEFORE.TXT",
                    ioreg_reader=forbidden,
                    command_runner=forbidden,
                )
        self.assertEqual(calls, 0)

    def test_region_output_created_during_qualification_is_rejected(self) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            prefix = root / "board-a"
            output = root / "late-output.bin"
            reads = 0

            def changing_ioreg() -> str:
                nonlocal reads
                reads += 1
                if reads == 2:
                    os.link(
                        host._evidence_path(prefix, ".ioreg-before.txt"),
                        output,
                    )
                return ioreg

            with self.assertRaisesRegex(host.QualificationError, "aliases"):
                host.read_region(
                    expected_usb_serial=MAC_A.upper(),
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=prefix,
                    offset=0,
                    length=1,
                    output=output,
                    ioreg_reader=changing_ioreg,
                    command_runner=lambda command, **_kwargs: subprocess.CompletedProcess(
                        command, 0, board_info(), ""
                    ),
                    path_is_character_device=lambda _path: True,
                )
            self.assertEqual(reads, 2)

    def test_region_preexisting_and_dangling_symlink_collisions_fail_closed(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for kind in ("regular-output", "dangling-output", "dangling-evidence"):
                with self.subTest(kind=kind):
                    prefix = root / kind / "board-a"
                    prefix.parent.mkdir()
                    output = prefix.parent / "region.bin"
                    if kind == "regular-output":
                        output.write_bytes(b"existing")
                    elif kind == "dangling-output":
                        output.symlink_to(prefix.parent / "missing-output-target")
                    else:
                        host._evidence_path(
                            prefix, ".read-region.stdout.txt"
                        ).symlink_to(prefix.parent / "missing-evidence-target")
                    with self.assertRaisesRegex(
                        host.QualificationError, "overwrite|evidence"
                    ):
                        host.read_region(
                            expected_usb_serial=MAC_A.upper(),
                            expected_mac=MAC_A,
                            expected_flash_bytes=16 * 1024 * 1024,
                            evidence_prefix=prefix,
                            offset=0,
                            length=1,
                            output=output,
                            ioreg_reader=lambda: (_ for _ in ()).throw(
                                AssertionError("hardware access must not run")
                            ),
                        )

    def test_region_operations_reject_ambiguous_identity_or_range_before_io(
        self,
    ) -> None:
        calls = 0

        def forbidden(*_args, **_kwargs):
            nonlocal calls
            calls += 1
            raise AssertionError("hardware access must not run")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cases = (
                {
                    "expected_usb_serial": MAC_A,
                    "expected_flash_bytes": 16 * 1024 * 1024,
                    "offset": 0,
                    "length": 1,
                    "message": "uppercase USB serial",
                },
                {
                    "expected_usb_serial": MAC_A.upper(),
                    "expected_flash_bytes": 8 * 1024 * 1024,
                    "offset": 0,
                    "length": 1,
                    "message": "16777216 bytes",
                },
                {
                    "expected_usb_serial": MAC_A.upper(),
                    "expected_flash_bytes": 16 * 1024 * 1024,
                    "offset": 16 * 1024 * 1024,
                    "length": 1,
                    "message": "exceeds",
                },
            )
            for index, case in enumerate(cases):
                with self.subTest(case=case["message"]):
                    with self.assertRaisesRegex(
                        host.QualificationError, str(case["message"])
                    ):
                        host.read_region(
                            expected_usb_serial=str(case["expected_usb_serial"]),
                            expected_mac=MAC_A,
                            expected_flash_bytes=int(case["expected_flash_bytes"]),
                            evidence_prefix=root / f"invalid-{index}",
                            offset=int(case["offset"]),
                            length=int(case["length"]),
                            output=root / f"invalid-{index}.bin",
                            ioreg_reader=forbidden,
                            command_runner=forbidden,
                        )
            with self.assertRaisesRegex(host.QualificationError, "4096-byte aligned"):
                host.erase_region(
                    expected_usb_serial=MAC_A.upper(),
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=root / "unaligned",
                    offset=0x630001,
                    length=0x100000,
                    ioreg_reader=forbidden,
                    command_runner=forbidden,
                )
        self.assertEqual(calls, 0)

    def test_region_read_size_mismatch_has_no_verified_record(self) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")

        def runner(command: list[str], **_kwargs) -> subprocess.CompletedProcess[str]:
            if command[1] == "board-info":
                return subprocess.CompletedProcess(command, 0, board_info(), "")
            Path(command[-1]).write_bytes(b"short")
            return subprocess.CompletedProcess(command, 0, board_info(), "")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            prefix = root / "board-a-region"
            with self.assertRaisesRegex(host.QualificationError, "bytes, expected"):
                host.read_region(
                    expected_usb_serial=MAC_A.upper(),
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=prefix,
                    offset=0x612345,
                    length=16,
                    output=root / "region.bin",
                    ioreg_reader=lambda: ioreg,
                    command_runner=runner,
                    path_is_character_device=lambda _path: True,
                )
            self.assertFalse(
                host._evidence_path(prefix, ".read-region.verified.json").exists()
            )

    def test_failed_full_and_region_reads_leave_private_reserved_outputs(
        self,
    ) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cases = (
                ("full", root / "full.bin"),
                ("region", root / "region.bin"),
            )
            for operation, output in cases:
                with self.subTest(operation=operation):
                    prefix = root / f"board-a-{operation}"

                    def runner(
                        command: list[str], **kwargs
                    ) -> subprocess.CompletedProcess[str]:
                        if command[1] == "board-info":
                            return subprocess.CompletedProcess(
                                command, 0, board_info(), ""
                            )
                        self.assertEqual(
                            kwargs["pass_fds"], (int(command[-1].rsplit("/", 1)[1]),)
                        )
                        self.assertEqual(
                            stat.S_IMODE(output.stat().st_mode), 0o600
                        )
                        Path(command[-1]).write_bytes(b"partial private dump")
                        return subprocess.CompletedProcess(
                            command, 7, board_info(), "read failed\n"
                        )

                    with self.assertRaisesRegex(
                        host.QualificationError, "read-flash exited 7"
                    ):
                        if operation == "full":
                            host.read_full_flash(
                                expected_usb_serial=MAC_A,
                                expected_mac=MAC_A,
                                expected_flash_bytes=16 * 1024 * 1024,
                                evidence_prefix=prefix,
                                output=output,
                                ioreg_reader=lambda: ioreg,
                                command_runner=runner,
                                path_is_character_device=lambda _path: True,
                            )
                        else:
                            host.read_region(
                                expected_usb_serial=MAC_A.upper(),
                                expected_mac=MAC_A,
                                expected_flash_bytes=16 * 1024 * 1024,
                                evidence_prefix=prefix,
                                offset=0x610000,
                                length=host.FLASH_SECTOR_BYTES,
                                output=output,
                                ioreg_reader=lambda: ioreg,
                                command_runner=runner,
                                path_is_character_device=lambda _path: True,
                            )
                    self.assertTrue(output.is_file())
                    self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o600)
                    self.assertEqual(output.read_bytes(), b"partial private dump")

    def test_region_read_rejects_wrong_mac_from_action_stdout(self) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")

        def runner(command: list[str], **_kwargs) -> subprocess.CompletedProcess[str]:
            if command[1] == "board-info":
                return subprocess.CompletedProcess(command, 0, board_info(), "")
            Path(command[-1]).write_bytes(b"expected payload")
            return subprocess.CompletedProcess(
                command, 0, board_info(mac=MAC_B), ""
            )

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            prefix = root / "board-a-region"
            with self.assertRaisesRegex(host.QualificationError, "action expected MAC"):
                host.read_region(
                    expected_usb_serial=MAC_A.upper(),
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=prefix,
                    offset=0x612345,
                    length=len(b"expected payload"),
                    output=root / "region.bin",
                    ioreg_reader=lambda: ioreg,
                    command_runner=runner,
                    path_is_character_device=lambda _path: True,
                )
            self.assertTrue(
                host._evidence_path(prefix, ".ioreg-after-read-region.txt").exists()
            )
            self.assertFalse(
                host._evidence_path(prefix, ".read-region.verified.json").exists()
            )

    def test_region_read_path_swap_cannot_redirect_dump_into_symlink_target(
        self,
    ) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / "region.bin"
            protected_target = root / "must-not-be-clobbered.bin"
            protected_target.write_bytes(b"protected")

            def runner(
                command: list[str], **_kwargs
            ) -> subprocess.CompletedProcess[str]:
                if command[1] == "board-info":
                    return subprocess.CompletedProcess(command, 0, board_info(), "")
                output.unlink()
                output.symlink_to(protected_target)
                # The action writes through the inherited descriptor, not the
                # raced caller-visible symlink.
                Path(command[-1]).write_bytes(b"flash payload")
                return subprocess.CompletedProcess(command, 0, board_info(), "")

            prefix = root / "board-a-region"
            with self.assertRaisesRegex(host.QualificationError, "changed identity"):
                host.read_region(
                    expected_usb_serial=MAC_A.upper(),
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=prefix,
                    offset=0,
                    length=len(b"flash payload"),
                    output=output,
                    ioreg_reader=lambda: ioreg,
                    command_runner=runner,
                    path_is_character_device=lambda _path: True,
                )
            self.assertEqual(protected_target.read_bytes(), b"protected")
            self.assertTrue(output.is_symlink())
            self.assertFalse(
                host._evidence_path(prefix, ".read-region.verified.json").exists()
            )

    def test_region_erase_stays_in_loader_and_verifies_every_byte(self) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")
        commands: list[list[str]] = []
        offset = 0x630000
        length = 2 * host.FLASH_SECTOR_BYTES
        erased = b"\xff" * length

        def runner(command: list[str], **kwargs) -> subprocess.CompletedProcess[str]:
            commands.append(command)
            if command[1] == "board-info":
                return subprocess.CompletedProcess(command, 0, board_info(), "")
            self.assertEqual(command[command.index("--before") + 1], "no-reset")
            self.assertEqual(command[command.index("--after") + 1], "no-reset")
            if command[1] == "write-bin":
                self.assertEqual(command[-2], str(offset))
                self.assertEqual(
                    kwargs["pass_fds"], (int(command[-1].rsplit("/", 1)[1]),)
                )
                self.assertEqual(Path(command[-1]).read_bytes(), erased)
            else:
                self.assertEqual(command[1], "read-flash")
                self.assertEqual(command[-3], str(offset))
                self.assertEqual(command[-2], str(length))
                Path(command[-1]).write_bytes(erased)
            return subprocess.CompletedProcess(command, 0, board_info(), "")

        with tempfile.TemporaryDirectory() as directory:
            prefix = Path(directory) / "board-a-erase"
            _device, board, digest = host.erase_region(
                expected_usb_serial=MAC_A.upper(),
                expected_mac=MAC_A,
                expected_flash_bytes=16 * 1024 * 1024,
                evidence_prefix=prefix,
                offset=offset,
                length=length,
                ioreg_reader=lambda: ioreg,
                command_runner=runner,
                path_is_character_device=lambda _path: True,
            )
            expected_digest = hashlib.sha256(erased).hexdigest()
            self.assertEqual(board.flash_bytes, 16 * 1024 * 1024)
            self.assertEqual(digest, expected_digest)
            self.assertEqual(
                [command[1] for command in commands],
                ["board-info", "write-bin", "board-info", "read-flash"],
            )
            evidence = json.loads(
                host._evidence_path(prefix, ".erase-region.verified.json").read_text()
            )
            self.assertEqual(evidence["offset"], offset)
            self.assertEqual(evidence["length"], length)
            self.assertEqual(evidence["erased_byte"], 0xFF)
            self.assertEqual(evidence["operation"], "identity_bound_all_ff_write")
            self.assertNotIn("erase_target", evidence)
            self.assertEqual(evidence["erase_write_action_target"]["mac"], MAC_A)
            self.assertEqual(evidence["post_write_target"]["mac"], MAC_A)
            self.assertEqual(evidence["erase_input_bytes"], length)
            self.assertEqual(evidence["erase_input_sha256"], expected_digest)
            self.assertEqual(
                stat.S_IMODE(
                    host._evidence_path(
                        prefix, ".erase-region.input.bin"
                    ).stat().st_mode
                ),
                0o400,
            )
            self.assertEqual(evidence["readback_bytes"], length)
            self.assertEqual(evidence["readback_sha256"], expected_digest)
            self.assertEqual(
                stat.S_IMODE(
                    host._evidence_path(
                        prefix, ".erase-region.readback.bin"
                    ).stat().st_mode
                ),
                0o400,
            )

    def test_region_erase_rejects_programmed_last_byte_without_verified_record(
        self,
    ) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")
        offset = 0x630000
        length = 1024 * 1024 + host.FLASH_SECTOR_BYTES

        def runner(command: list[str], **_kwargs) -> subprocess.CompletedProcess[str]:
            if command[1] == "board-info":
                return subprocess.CompletedProcess(command, 0, board_info(), "")
            if command[1] == "read-flash":
                Path(command[-1]).write_bytes(b"\xff" * (length - 1) + b"\x00")
            return subprocess.CompletedProcess(command, 0, board_info(), "")

        with tempfile.TemporaryDirectory() as directory:
            prefix = Path(directory) / "board-a-erase"
            with self.assertRaisesRegex(
                host.PostWriteEvidenceError,
                f"0x{offset + length - 1:x}",
            ):
                host.erase_region(
                    expected_usb_serial=MAC_A.upper(),
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=prefix,
                    offset=offset,
                    length=length,
                    ioreg_reader=lambda: ioreg,
                    command_runner=runner,
                    path_is_character_device=lambda _path: True,
                )
            self.assertFalse(
                host._evidence_path(prefix, ".erase-region.verified.json").exists()
            )

    def test_region_erase_rejects_swap_between_erase_and_readback(self) -> None:
        board_a = ioreg_device(MAC_A, "/dev/cu.usbmodem101")
        board_b = ioreg_device(MAC_B, "/dev/cu.usbmodem201")
        ioregs = iter((board_a, board_a, board_a, board_b))
        length = host.FLASH_SECTOR_BYTES

        def runner(command: list[str], **_kwargs) -> subprocess.CompletedProcess[str]:
            if command[1] == "board-info":
                return subprocess.CompletedProcess(command, 0, board_info(), "")
            if command[1] == "read-flash":
                Path(command[-1]).write_bytes(b"\xff" * length)
            return subprocess.CompletedProcess(command, 0, board_info(), "")

        with tempfile.TemporaryDirectory() as directory:
            prefix = Path(directory) / "board-a-erase"
            with self.assertRaisesRegex(
                host.PostWriteEvidenceError, "readback target is unverified"
            ):
                host.erase_region(
                    expected_usb_serial=MAC_A.upper(),
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=prefix,
                    offset=0x630000,
                    length=length,
                    ioreg_reader=lambda: next(ioregs),
                    command_runner=runner,
                    path_is_character_device=lambda _path: True,
                )
            self.assertTrue(
                host._evidence_path(
                    prefix, ".ioreg-after-erase-readback.txt"
                ).exists()
            )
            self.assertFalse(
                host._evidence_path(prefix, ".erase-region.verified.json").exists()
            )

    def test_region_erase_rejects_wrong_post_erase_mac(self) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")
        board_info_calls = 0
        commands: list[str] = []

        def runner(command: list[str], **_kwargs) -> subprocess.CompletedProcess[str]:
            nonlocal board_info_calls
            commands.append(command[1])
            if command[1] == "board-info":
                board_info_calls += 1
                mac = MAC_A if board_info_calls == 1 else MAC_B
                return subprocess.CompletedProcess(
                    command, 0, board_info(mac=mac), ""
                )
            return subprocess.CompletedProcess(command, 0, board_info(), "")

        with tempfile.TemporaryDirectory() as directory:
            prefix = Path(directory) / "board-a-erase"
            with self.assertRaisesRegex(
                host.PostWriteEvidenceError, "unverified post-write target"
            ):
                host.erase_region(
                    expected_usb_serial=MAC_A.upper(),
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=prefix,
                    offset=0x630000,
                    length=host.FLASH_SECTOR_BYTES,
                    ioreg_reader=lambda: ioreg,
                    command_runner=runner,
                    path_is_character_device=lambda _path: True,
                )
            self.assertEqual(commands, ["board-info", "write-bin", "board-info"])
            self.assertFalse(
                host._evidence_path(prefix, ".erase-region.verified.json").exists()
            )

    def test_region_erase_equivalent_rejects_wrong_write_action_before_swap_back(
        self,
    ) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")
        commands: list[str] = []

        def runner(command: list[str], **_kwargs) -> subprocess.CompletedProcess[str]:
            commands.append(command[1])
            if command[1] == "board-info":
                return subprocess.CompletedProcess(command, 0, board_info(), "")
            self.assertEqual(command[1], "write-bin")
            return subprocess.CompletedProcess(
                command, 0, board_info(mac=MAC_B), ""
            )

        with tempfile.TemporaryDirectory() as directory:
            prefix = Path(directory) / "board-a-erase"
            with self.assertRaisesRegex(
                host.PostWriteEvidenceError, "write action target is unverified"
            ):
                host.erase_region(
                    expected_usb_serial=MAC_A.upper(),
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=prefix,
                    offset=0x630000,
                    length=host.FLASH_SECTOR_BYTES,
                    ioreg_reader=lambda: ioreg,
                    command_runner=runner,
                    path_is_character_device=lambda _path: True,
                )
            self.assertEqual(commands, ["board-info", "write-bin"])
            self.assertFalse(
                host._evidence_path(prefix, ".erase-region.verified.json").exists()
            )

    def test_region_erase_readback_runner_error_is_post_write_unverified(
        self,
    ) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")
        commands: list[str] = []

        def runner(command: list[str], **_kwargs) -> subprocess.CompletedProcess[str]:
            commands.append(command[1])
            if command[1] == "read-flash":
                raise subprocess.SubprocessError("injected readback runner failure")
            return subprocess.CompletedProcess(command, 0, board_info(), "")

        with tempfile.TemporaryDirectory() as directory:
            prefix = Path(directory) / "board-a-erase"
            with self.assertRaisesRegex(
                host.PostWriteEvidenceError,
                "evidence finalization failed.*readback runner failure",
            ):
                host.erase_region(
                    expected_usb_serial=MAC_A.upper(),
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=prefix,
                    offset=0x630000,
                    length=host.FLASH_SECTOR_BYTES,
                    ioreg_reader=lambda: ioreg,
                    command_runner=runner,
                    path_is_character_device=lambda _path: True,
                )
            self.assertEqual(
                commands, ["board-info", "write-bin", "board-info", "read-flash"]
            )
            self.assertFalse(
                host._evidence_path(prefix, ".erase-region.verified.json").exists()
            )
            readback = host._evidence_path(prefix, ".erase-region.readback.bin")
            self.assertTrue(readback.is_file())
            self.assertEqual(stat.S_IMODE(readback.stat().st_mode), 0o600)

    def test_region_erase_readback_nonzero_short_or_missing_is_unverified(
        self,
    ) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")
        length = host.FLASH_SECTOR_BYTES
        for failure in ("nonzero", "short", "missing"):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                def runner(
                    command: list[str], **_kwargs
                ) -> subprocess.CompletedProcess[str]:
                    if command[1] == "board-info":
                        return subprocess.CompletedProcess(
                            command, 0, board_info(), ""
                        )
                    if command[1] == "read-flash":
                        if failure == "nonzero":
                            return subprocess.CompletedProcess(
                                command, 9, board_info(), "read failed\n"
                            )
                        if failure == "short":
                            Path(command[-1]).write_bytes(b"\xff" * (length - 1))
                    return subprocess.CompletedProcess(
                        command, 0, board_info(), ""
                    )

                prefix = Path(directory) / "board-a-erase"
                with self.assertRaises(host.PostWriteEvidenceError):
                    host.erase_region(
                        expected_usb_serial=MAC_A.upper(),
                        expected_mac=MAC_A,
                        expected_flash_bytes=16 * 1024 * 1024,
                        evidence_prefix=prefix,
                        offset=0x630000,
                        length=length,
                        ioreg_reader=lambda: ioreg,
                        command_runner=runner,
                        path_is_character_device=lambda _path: True,
                    )
                self.assertFalse(
                    host._evidence_path(
                        prefix, ".erase-region.verified.json"
                    ).exists()
                )
                readback = host._evidence_path(
                    prefix, ".erase-region.readback.bin"
                )
                self.assertTrue(readback.is_file())
                self.assertEqual(stat.S_IMODE(readback.stat().st_mode), 0o600)

    def test_post_erase_verified_publish_failure_leaves_no_final_or_temp(
        self,
    ) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")
        length = host.FLASH_SECTOR_BYTES

        def runner(command: list[str], **_kwargs) -> subprocess.CompletedProcess[str]:
            if command[1] == "board-info":
                return subprocess.CompletedProcess(command, 0, board_info(), "")
            if command[1] == "read-flash":
                Path(command[-1]).write_bytes(b"\xff" * length)
            return subprocess.CompletedProcess(command, 0, board_info(), "")

        real_link = host.os.link

        def failing_final_link(source: str, destination: str, **kwargs) -> None:
            if destination.endswith(".erase-region.verified.json"):
                raise OSError("injected verified publish failure")
            real_link(source, destination, **kwargs)

        with tempfile.TemporaryDirectory() as directory:
            prefix = Path(directory) / "board-a-erase"
            with mock.patch.object(host.os, "link", side_effect=failing_final_link):
                with self.assertRaisesRegex(
                    host.PostWriteEvidenceError, "injected verified publish failure"
                ):
                    host.erase_region(
                        expected_usb_serial=MAC_A.upper(),
                        expected_mac=MAC_A,
                        expected_flash_bytes=16 * 1024 * 1024,
                        evidence_prefix=prefix,
                        offset=0x630000,
                        length=length,
                        ioreg_reader=lambda: ioreg,
                        command_runner=runner,
                        path_is_character_device=lambda _path: True,
                    )
            final = host._evidence_path(prefix, ".erase-region.verified.json")
            self.assertFalse(final.exists())
            self.assertEqual(list(final.parent.glob(f".{final.name}.*.tmp")), [])
            readback = host._evidence_path(prefix, ".erase-region.readback.bin")
            self.assertEqual(stat.S_IMODE(readback.stat().st_mode), 0o600)

    def test_region_erase_command_failure_is_unverified_and_stops(self) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")
        commands: list[list[str]] = []

        def runner(command: list[str], **_kwargs) -> subprocess.CompletedProcess[str]:
            commands.append(command)
            if command[1] == "board-info":
                return subprocess.CompletedProcess(command, 0, board_info(), "")
            return subprocess.CompletedProcess(command, 7, "", "erase failed\n")

        with tempfile.TemporaryDirectory() as directory:
            prefix = Path(directory) / "board-a-erase"
            with self.assertRaisesRegex(
                host.PostWriteEvidenceError, "target range state is unverified"
            ):
                host.erase_region(
                    expected_usb_serial=MAC_A.upper(),
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=prefix,
                    offset=0x630000,
                    length=0x100000,
                    ioreg_reader=lambda: ioreg,
                    command_runner=runner,
                    path_is_character_device=lambda _path: True,
                )
            self.assertEqual(
                [command[1] for command in commands], ["board-info", "write-bin"]
            )
            self.assertFalse(
                host._evidence_path(prefix, ".erase-region.verified.json").exists()
            )

    def test_canonical_merged_image_layout_is_accepted(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "merged.bin"
            payload = merged_image_bytes()
            image.write_bytes(payload)

            info = host._validate_merged_image(
                image, expected_flash_bytes=host.E290_FLASH_BYTES
            )

            self.assertEqual(info.bytes, len(payload))
            self.assertEqual(
                info.partition_table_sha256,
                # Anchored to espflash 4.5.0 output from the checked-in CSV.
                "9f183d86eda1be9898f27b6d283a02a5ed730df27151733b965096cbd714aa3a",
            )

    def test_host_guard_matches_checked_in_e290_partition_csv(self) -> None:
        self.assertEqual(checked_in_e290_partitions(), host.E290_PARTITIONS)

    def test_merged_image_guard_rejects_header_only_image(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "merged.bin"
            image.write_bytes(bytes((0xE9, 0x01, 0x02, 0x4F)))

            with self.assertRaisesRegex(host.QualificationError, "truncated"):
                host._validate_merged_image(
                    image, expected_flash_bytes=host.E290_FLASH_BYTES
                )

    def test_merged_image_guard_rejects_protected_partition_overlap(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "merged.bin"
            image.write_bytes(merged_image_bytes())
            with image.open("r+b") as destination:
                destination.truncate(host.E290_FACTORY_END + 1)

            with self.assertRaisesRegex(
                host.QualificationError, "protected product write boundary"
            ):
                host._validate_merged_image(
                    image, expected_flash_bytes=host.E290_FLASH_BYTES
                )

    def test_merged_image_guard_rejects_noncanonical_partition_entry(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "merged.bin"
            payload = bytearray(merged_image_bytes())
            factory_offset_field = (
                host.E290_PARTITION_TABLE_OFFSET
                + 2 * host.ESP_PARTITION_ENTRY_BYTES
                + 4
            )
            payload[factory_offset_field] ^= 0x01
            image.write_bytes(payload)

            with self.assertRaisesRegex(host.QualificationError, "canonical E290"):
                host._validate_merged_image(
                    image, expected_flash_bytes=host.E290_FLASH_BYTES
                )

    def test_merged_image_guard_rejects_invalid_partition_md5(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "merged.bin"
            payload = bytearray(merged_image_bytes())
            digest_offset = (
                host.E290_PARTITION_TABLE_OFFSET
                + len(host.E290_PARTITION_ENTRIES)
                + 16
            )
            payload[digest_offset] ^= 0x01
            image.write_bytes(payload)

            with self.assertRaisesRegex(host.QualificationError, "MD5"):
                host._validate_merged_image(
                    image, expected_flash_bytes=host.E290_FLASH_BYTES
                )

    def test_merged_image_guard_rejects_non_erased_partition_tail(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "merged.bin"
            payload = bytearray(merged_image_bytes())
            payload[
                host.E290_PARTITION_TABLE_OFFSET
                + host.E290_PARTITION_TABLE_BYTES
                - 1
            ] = 0
            image.write_bytes(payload)

            with self.assertRaisesRegex(host.QualificationError, "non-erased trailing"):
                host._validate_merged_image(
                    image, expected_flash_bytes=host.E290_FLASH_BYTES
                )

    def test_merged_image_guard_rejects_invalid_factory_app_header(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "merged.bin"
            payload = bytearray(merged_image_bytes())
            payload[host.E290_FACTORY_OFFSET] = 0
            image.write_bytes(payload)

            with self.assertRaisesRegex(
                host.QualificationError, "factory application.*magic"
            ):
                host._validate_merged_image(
                    image, expected_flash_bytes=host.E290_FLASH_BYTES
                )

    def test_merged_image_guard_rejects_wrong_esp_chip(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "merged.bin"
            payload = bytearray(merged_image_bytes())
            payload[12:14] = (0).to_bytes(2, "little")
            image.write_bytes(payload)

            with self.assertRaisesRegex(host.QualificationError, "ESP32-S3"):
                host._validate_merged_image(
                    image, expected_flash_bytes=host.E290_FLASH_BYTES
                )

    def test_merged_flash_rejects_bad_layout_before_hardware_access(self) -> None:
        hardware_calls = 0

        def forbidden(*_args, **_kwargs):
            nonlocal hardware_calls
            hardware_calls += 1
            raise AssertionError("hardware access must not run")

        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "merged.bin"
            image.write_bytes(bytes((0xE9, 0x01, 0x02, 0x4F)))
            digest = hashlib.sha256(image.read_bytes()).hexdigest()

            with self.assertRaisesRegex(host.QualificationError, "truncated"):
                host.flash_merged_image(
                    expected_usb_serial=MAC_A,
                    expected_mac=MAC_A,
                    expected_flash_bytes=host.E290_FLASH_BYTES,
                    evidence_prefix=Path(directory) / "board-a-flash",
                    image=image,
                    expected_image_sha256=digest,
                    confirmed_radio_module=host.CONFIRMED_HF_MODULE,
                    ioreg_reader=forbidden,
                    command_runner=forbidden,
                )
            self.assertEqual(hardware_calls, 0)

    def test_merged_flash_revalidates_the_retained_input_before_hardware(self) -> None:
        hardware_calls = 0

        def forbidden(*_args, **_kwargs):
            nonlocal hardware_calls
            hardware_calls += 1
            raise AssertionError("hardware access must not run")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image = root / "merged.bin"
            image.write_bytes(merged_image_bytes())
            corrupt_source = root / "corrupt.bin"
            corrupt_payload = bytearray(merged_image_bytes())
            corrupt_payload[host.E290_PARTITION_TABLE_OFFSET] = 0
            corrupt_source.write_bytes(corrupt_payload)
            corrupt_digest = hashlib.sha256(corrupt_payload).hexdigest()
            real_copy = host._copy_retained_flash_input

            def substitute_retained_input(
                _source: Path, destination: Path
            ) -> host.RetainedFlashInput:
                return real_copy(corrupt_source, destination)

            with mock.patch.object(
                host,
                "_copy_retained_flash_input",
                side_effect=substitute_retained_input,
            ):
                with self.assertRaisesRegex(host.QualificationError, "canonical E290"):
                    host.flash_merged_image(
                        expected_usb_serial=MAC_A,
                        expected_mac=MAC_A,
                        expected_flash_bytes=host.E290_FLASH_BYTES,
                        evidence_prefix=root / "board-a-flash",
                        image=image,
                        expected_image_sha256=corrupt_digest,
                        confirmed_radio_module=host.CONFIRMED_HF_MODULE,
                        ioreg_reader=forbidden,
                        command_runner=forbidden,
                    )
            self.assertEqual(hardware_calls, 0)

    def test_merged_verification_is_identity_gated_and_hash_bound(self) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")
        commands: list[list[str]] = []

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image = root / "merged.bin"
            image.write_bytes(merged_image_bytes(b"merged-image"))
            digest = hashlib.sha256(image.read_bytes()).hexdigest()

            def runner(
                command: list[str], **_kwargs
            ) -> subprocess.CompletedProcess[str]:
                commands.append(command)
                if command[1] == "board-info":
                    return subprocess.CompletedProcess(command, 0, board_info(), "")
                self.assertEqual(command[1], "read-flash")
                self.assertEqual(command[command.index("--before") + 1], "no-reset")
                self.assertEqual(command[command.index("--after") + 1], "no-reset")
                self.assertEqual(command[-3], "0x0")
                self.assertEqual(command[-2], str(image.stat().st_size))
                Path(command[-1]).write_bytes(image.read_bytes())
                return subprocess.CompletedProcess(command, 0, board_info(), "")

            prefix = root / "board-a-post-capture"
            _device, board, verified_digest = host.verify_merged_image(
                expected_usb_serial=MAC_A,
                expected_mac=MAC_A,
                expected_flash_bytes=16 * 1024 * 1024,
                evidence_prefix=prefix,
                image=image,
                expected_image_sha256=digest,
                confirmed_radio_module=host.CONFIRMED_HF_MODULE,
                ioreg_reader=lambda: ioreg,
                command_runner=runner,
                path_is_character_device=lambda _path: True,
            )
            self.assertEqual(board.flash_bytes, 16 * 1024 * 1024)
            self.assertEqual(verified_digest, digest)
            self.assertEqual(
                [command[1] for command in commands], ["board-info", "read-flash"]
            )
            self.assertTrue(
                host._evidence_path(prefix, ".verify-image.verified.json").is_file()
            )
            evidence = json.loads(
                host._evidence_path(prefix, ".verify-image.verified.json").read_text()
            )
            self.assertEqual(evidence["read_target"]["mac"], MAC_A)
            self.assertEqual(
                evidence["partition_table_sha256"],
                hashlib.sha256(host.E290_PARTITION_TABLE_REGION).hexdigest(),
            )
            self.assertEqual(
                stat.S_IMODE(
                    host._evidence_path(
                        prefix, ".verify-readback.bin"
                    ).stat().st_mode
                ),
                0o400,
            )

    def test_merged_verification_rejects_mismatched_readback(self) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image = root / "merged.bin"
            image.write_bytes(merged_image_bytes(b"merged-image"))

            def runner(
                command: list[str], **_kwargs
            ) -> subprocess.CompletedProcess[str]:
                if command[1] == "board-info":
                    return subprocess.CompletedProcess(command, 0, board_info(), "")
                Path(command[-1]).write_bytes(b"\x00" * image.stat().st_size)
                return subprocess.CompletedProcess(command, 0, board_info(), "")

            prefix = root / "board-a-post-capture"
            with self.assertRaisesRegex(host.QualificationError, "readback mismatch"):
                host.verify_merged_image(
                    expected_usb_serial=MAC_A,
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=prefix,
                    image=image,
                    expected_image_sha256=hashlib.sha256(
                        image.read_bytes()
                    ).hexdigest(),
                    confirmed_radio_module=host.CONFIRMED_HF_MODULE,
                    ioreg_reader=lambda: ioreg,
                    command_runner=runner,
                    path_is_character_device=lambda _path: True,
                )
            self.assertFalse(
                host._evidence_path(prefix, ".verify-image.verified.json").exists()
            )
            self.assertEqual(
                stat.S_IMODE(
                    host._evidence_path(
                        prefix, ".verify-readback.bin"
                    ).stat().st_mode
                ),
                0o600,
            )

    def test_verify_input_path_swap_cannot_chmod_symlink_victim(self) -> None:
        hardware_calls = 0

        def forbidden(*_args, **_kwargs):
            nonlocal hardware_calls
            hardware_calls += 1
            raise AssertionError("hardware access must not run")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image = root / "merged.bin"
            image.write_bytes(merged_image_bytes(b"merged-image"))
            victim = root / "victim.bin"
            victim.write_bytes(b"must remain untouched")
            victim.chmod(0o640)
            prefix = root / "board-a-verify"
            preserved = host._evidence_path(prefix, ".verify-input.bin")
            real_fchmod = host.os.fchmod
            raced = False

            def race_after_fd_chmod(descriptor: int, mode: int) -> None:
                nonlocal raced
                real_fchmod(descriptor, mode)
                if mode == 0o400 and not raced:
                    raced = True
                    preserved.unlink()
                    preserved.symlink_to(victim)

            with mock.patch.object(
                host.os, "fchmod", side_effect=race_after_fd_chmod
            ):
                with self.assertRaises(host.QualificationError):
                    host.verify_merged_image(
                        expected_usb_serial=MAC_A,
                        expected_mac=MAC_A,
                        expected_flash_bytes=16 * 1024 * 1024,
                        evidence_prefix=prefix,
                        image=image,
                        expected_image_sha256=hashlib.sha256(
                            image.read_bytes()
                        ).hexdigest(),
                        confirmed_radio_module=host.CONFIRMED_HF_MODULE,
                        ioreg_reader=forbidden,
                        command_runner=forbidden,
                    )
            self.assertTrue(raced)
            self.assertEqual(victim.read_bytes(), b"must remain untouched")
            self.assertEqual(stat.S_IMODE(victim.stat().st_mode), 0o640)
            self.assertEqual(hardware_calls, 0)

    def test_merged_verification_rejects_wrong_action_mac_and_keeps_private_dump(
        self,
    ) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image = root / "merged.bin"
            image.write_bytes(merged_image_bytes(b"merged-image"))

            def runner(
                command: list[str], **_kwargs
            ) -> subprocess.CompletedProcess[str]:
                if command[1] == "board-info":
                    return subprocess.CompletedProcess(command, 0, board_info(), "")
                Path(command[-1]).write_bytes(image.read_bytes())
                return subprocess.CompletedProcess(
                    command, 0, board_info(mac=MAC_B), ""
                )

            prefix = root / "board-a-verify"
            with self.assertRaisesRegex(host.QualificationError, "action expected MAC"):
                host.verify_merged_image(
                    expected_usb_serial=MAC_A,
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=prefix,
                    image=image,
                    expected_image_sha256=hashlib.sha256(
                        image.read_bytes()
                    ).hexdigest(),
                    confirmed_radio_module=host.CONFIRMED_HF_MODULE,
                    ioreg_reader=lambda: ioreg,
                    command_runner=runner,
                    path_is_character_device=lambda _path: True,
                )
            readback = host._evidence_path(prefix, ".verify-readback.bin")
            self.assertTrue(readback.is_file())
            self.assertEqual(stat.S_IMODE(readback.stat().st_mode), 0o600)
            self.assertFalse(
                host._evidence_path(prefix, ".verify-image.verified.json").exists()
            )

    def test_merged_verification_rejects_mapping_swap_during_read(self) -> None:
        before = ioreg_device(MAC_A, "/dev/cu.usbmodem101")
        swapped = ioreg_device(MAC_A, "/dev/cu.usbmodem202")
        ioregs = iter((before, before, swapped))

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image = root / "merged.bin"
            image.write_bytes(merged_image_bytes(b"merged-image"))

            def runner(
                command: list[str], **_kwargs
            ) -> subprocess.CompletedProcess[str]:
                if command[1] == "board-info":
                    return subprocess.CompletedProcess(command, 0, board_info(), "")
                Path(command[-1]).write_bytes(image.read_bytes())
                return subprocess.CompletedProcess(command, 0, board_info(), "")

            prefix = root / "board-a-verify"
            with self.assertRaisesRegex(host.QualificationError, "mapping changed"):
                host.verify_merged_image(
                    expected_usb_serial=MAC_A,
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=prefix,
                    image=image,
                    expected_image_sha256=hashlib.sha256(
                        image.read_bytes()
                    ).hexdigest(),
                    confirmed_radio_module=host.CONFIRMED_HF_MODULE,
                    ioreg_reader=lambda: next(ioregs),
                    command_runner=runner,
                    path_is_character_device=lambda _path: True,
                )
            self.assertFalse(
                host._evidence_path(prefix, ".verify-image.verified.json").exists()
            )

    def test_merged_flash_is_identity_gated_and_verified_by_readback(self) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")
        commands: list[list[str]] = []
        flashed_payload: bytes | None = None

        def runner(command: list[str], **kwargs) -> subprocess.CompletedProcess[str]:
            nonlocal flashed_payload
            commands.append(command)
            if command[1] == "board-info":
                return subprocess.CompletedProcess(command, 0, board_info(), "")
            if command[1] == "write-bin":
                self.assertEqual(
                    command[command.index("--before") + 1], "no-reset"
                )
                self.assertEqual(command[command.index("--after") + 1], "no-reset")
                self.assertEqual(command[-2], "0x0")
                self.assertEqual(
                    kwargs["pass_fds"], (int(command[-1].rsplit("/", 1)[1]),)
                )
                with self.assertRaises(OSError) as denied:
                    os.write(kwargs["pass_fds"][0], b"unauthorized mutation")
                self.assertIn(denied.exception.errno, (errno.EBADF, errno.EACCES))
                flashed_payload = Path(command[-1]).read_bytes()
                return subprocess.CompletedProcess(
                    command, 0, board_info() + "write complete\n", ""
                )
            self.assertEqual(command[1], "read-flash")
            self.assertEqual(command[command.index("--before") + 1], "no-reset")
            self.assertEqual(command[command.index("--after") + 1], "no-reset")
            self.assertEqual(
                kwargs["pass_fds"], (int(command[-1].rsplit("/", 1)[1]),)
            )
            assert flashed_payload is not None
            Path(command[-1]).write_bytes(flashed_payload)
            return subprocess.CompletedProcess(command, 0, board_info(), "")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image = root / "merged.bin"
            image.write_bytes(merged_image_bytes(b"merged-image"))
            digest = hashlib.sha256(image.read_bytes()).hexdigest()
            prefix = root / "board-a-flash"
            _device, board, verified_digest = host.flash_merged_image(
                expected_usb_serial=MAC_A,
                expected_mac=MAC_A,
                expected_flash_bytes=16 * 1024 * 1024,
                evidence_prefix=prefix,
                image=image,
                expected_image_sha256=digest,
                confirmed_radio_module=host.CONFIRMED_HF_MODULE,
                ioreg_reader=lambda: ioreg,
                command_runner=runner,
                path_is_character_device=lambda _path: True,
            )
            self.assertEqual(board.flash_bytes, 16 * 1024 * 1024)
            self.assertEqual(verified_digest, digest)
            self.assertEqual(
                [command[1] for command in commands],
                ["board-info", "write-bin", "board-info", "read-flash"],
            )
            evidence = host._evidence_path(prefix, ".flash-image.verified.json")
            self.assertTrue(evidence.is_file())
            record = json.loads(evidence.read_text())
            self.assertEqual(record["write_action_target"]["mac"], MAC_A)
            self.assertEqual(record["post_write_target"]["mac"], MAC_A)
            self.assertEqual(record["read_target"]["mac"], MAC_A)
            self.assertEqual(
                record["partition_table_sha256"],
                hashlib.sha256(host.E290_PARTITION_TABLE_REGION).hexdigest(),
            )
            self.assertTrue(
                host._evidence_path(prefix, ".ioreg-after-write-bin.txt").is_file()
            )
            self.assertTrue(
                host._evidence_path(prefix, ".ioreg-after-readback.txt").is_file()
            )
            self.assertEqual(
                host._sha256_file(host._evidence_path(prefix, ".readback.bin")),
                digest,
            )
            self.assertEqual(
                stat.S_IMODE(
                    host._evidence_path(prefix, ".readback.bin").stat().st_mode
                ),
                0o400,
            )

    def test_merged_flash_rejects_usb_mapping_swap_after_write(self) -> None:
        before = ioreg_device(MAC_A, "/dev/cu.usbmodem101")
        swapped = ioreg_device(MAC_A, "/dev/cu.usbmodem202")
        ioregs = iter((before, before, swapped))
        commands: list[str] = []

        def runner(command: list[str], **_kwargs) -> subprocess.CompletedProcess[str]:
            commands.append(command[1])
            if command[1] == "board-info":
                return subprocess.CompletedProcess(command, 0, board_info(), "")
            return subprocess.CompletedProcess(
                command, 0, board_info() + "write complete\n", ""
            )

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image = root / "merged.bin"
            image.write_bytes(merged_image_bytes(b"image"))
            prefix = root / "board-a-flash"
            with self.assertRaisesRegex(
                host.PostWriteEvidenceError, "USB mapping is unverified"
            ):
                host.flash_merged_image(
                    expected_usb_serial=MAC_A,
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=prefix,
                    image=image,
                    expected_image_sha256=hashlib.sha256(
                        image.read_bytes()
                    ).hexdigest(),
                    confirmed_radio_module=host.CONFIRMED_HF_MODULE,
                    ioreg_reader=lambda: next(ioregs),
                    command_runner=runner,
                    path_is_character_device=lambda _path: True,
                )
            self.assertEqual(commands, ["board-info", "write-bin"])
            self.assertFalse(
                host._evidence_path(prefix, ".flash-image.verified.json").exists()
            )

    def test_merged_flash_rejects_wrong_post_write_mac(self) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")
        board_info_calls = 0
        commands: list[str] = []

        def runner(command: list[str], **_kwargs) -> subprocess.CompletedProcess[str]:
            nonlocal board_info_calls
            commands.append(command[1])
            if command[1] == "board-info":
                board_info_calls += 1
                mac = MAC_A if board_info_calls == 1 else MAC_B
                return subprocess.CompletedProcess(
                    command, 0, board_info(mac=mac), ""
                )
            return subprocess.CompletedProcess(
                command, 0, board_info() + "write complete\n", ""
            )

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image = root / "merged.bin"
            image.write_bytes(merged_image_bytes(b"image"))
            prefix = root / "board-a-flash"
            with self.assertRaisesRegex(
                host.PostWriteEvidenceError, "unverified target"
            ):
                host.flash_merged_image(
                    expected_usb_serial=MAC_A,
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=prefix,
                    image=image,
                    expected_image_sha256=hashlib.sha256(
                        image.read_bytes()
                    ).hexdigest(),
                    confirmed_radio_module=host.CONFIRMED_HF_MODULE,
                    ioreg_reader=lambda: ioreg,
                    command_runner=runner,
                    path_is_character_device=lambda _path: True,
                )
            self.assertEqual(commands, ["board-info", "write-bin", "board-info"])
            self.assertFalse(
                host._evidence_path(prefix, ".flash-image.verified.json").exists()
            )

    def test_merged_flash_rejects_wrong_write_action_mac_before_swap_back(
        self,
    ) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")
        commands: list[str] = []

        def runner(command: list[str], **_kwargs) -> subprocess.CompletedProcess[str]:
            commands.append(command[1])
            if command[1] == "board-info":
                return subprocess.CompletedProcess(command, 0, board_info(), "")
            self.assertEqual(command[1], "write-bin")
            return subprocess.CompletedProcess(
                command, 0, board_info(mac=MAC_B) + "write complete\n", ""
            )

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image = root / "merged.bin"
            image.write_bytes(merged_image_bytes(b"image"))
            prefix = root / "board-a-flash"
            with self.assertRaisesRegex(
                host.PostWriteEvidenceError, "write action target is unverified"
            ):
                host.flash_merged_image(
                    expected_usb_serial=MAC_A,
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=prefix,
                    image=image,
                    expected_image_sha256=hashlib.sha256(
                        image.read_bytes()
                    ).hexdigest(),
                    confirmed_radio_module=host.CONFIRMED_HF_MODULE,
                    ioreg_reader=lambda: ioreg,
                    command_runner=runner,
                    path_is_character_device=lambda _path: True,
                )
            self.assertEqual(commands, ["board-info", "write-bin"])
            self.assertFalse(
                host._evidence_path(prefix, ".flash-image.verified.json").exists()
            )

    def test_merged_flash_requires_exact_hf_module_confirmation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "merged.bin"
            image.write_bytes(merged_image_bytes())
            with self.assertRaisesRegex(host.QualificationError, "confirmed module"):
                host.flash_merged_image(
                    expected_usb_serial=MAC_A,
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=Path(directory) / "board-a-flash",
                    image=image,
                    expected_image_sha256=hashlib.sha256(
                        image.read_bytes()
                    ).hexdigest(),
                    confirmed_radio_module="unknown",
                )

    def test_merged_flash_rejects_hash_mismatch_before_hardware_access(self) -> None:
        calls = 0

        def runner(*_args, **_kwargs):
            nonlocal calls
            calls += 1
            raise AssertionError("hardware command must not run")

        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "merged.bin"
            image.write_bytes(merged_image_bytes())
            with self.assertRaisesRegex(host.QualificationError, "SHA-256 mismatch"):
                host.flash_merged_image(
                    expected_usb_serial=MAC_A,
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=Path(directory) / "board-a-flash",
                    image=image,
                    expected_image_sha256="0" * 64,
                    confirmed_radio_module=host.CONFIRMED_HF_MODULE,
                    command_runner=runner,
                )
            self.assertEqual(calls, 0)

    def test_merged_flash_readback_mismatch_is_not_verified(self) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image = root / "merged.bin"
            image.write_bytes(merged_image_bytes(b"image"))

            def runner(
                command: list[str], **_kwargs
            ) -> subprocess.CompletedProcess[str]:
                if command[1] == "board-info":
                    return subprocess.CompletedProcess(command, 0, board_info(), "")
                if command[1] == "read-flash":
                    Path(command[-1]).write_bytes(b"\x00" * image.stat().st_size)
                    return subprocess.CompletedProcess(command, 0, board_info(), "")
                return subprocess.CompletedProcess(command, 0, board_info(), "")

            prefix = root / "board-a-flash"
            with self.assertRaisesRegex(
                host.PostWriteEvidenceError, "readback mismatch"
            ):
                host.flash_merged_image(
                    expected_usb_serial=MAC_A,
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=prefix,
                    image=image,
                    expected_image_sha256=hashlib.sha256(
                        image.read_bytes()
                    ).hexdigest(),
                    confirmed_radio_module=host.CONFIRMED_HF_MODULE,
                    ioreg_reader=lambda: ioreg,
                    command_runner=runner,
                    path_is_character_device=lambda _path: True,
                )
            self.assertFalse(
                host._evidence_path(prefix, ".flash-image.verified.json").exists()
            )
            readback = host._evidence_path(prefix, ".readback.bin")
            self.assertTrue(readback.is_file())
            self.assertEqual(stat.S_IMODE(readback.stat().st_mode), 0o600)

    def test_full_backup_uses_verified_capacity_and_loader_preserving_mode(self) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")
        commands: list[list[str]] = []

        def runner(command: list[str], **_kwargs) -> subprocess.CompletedProcess[str]:
            commands.append(command)
            if command[1] == "board-info":
                return subprocess.CompletedProcess(command, 0, board_info(), "")
            self.assertEqual(command[1], "read-flash")
            self.assertIn("--before", command)
            self.assertEqual(command[command.index("--before") + 1], "no-reset")
            self.assertEqual(command[-2], str(16 * 1024 * 1024))
            with Path(command[-1]).open("wb") as output:
                output.truncate(16 * 1024 * 1024)
            return subprocess.CompletedProcess(command, 0, board_info(), "")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _device, board, digest = host.read_full_flash(
                expected_usb_serial=MAC_A,
                expected_mac=MAC_A,
                expected_flash_bytes=16 * 1024 * 1024,
                evidence_prefix=root / "board-a-backup-1",
                output=root / "flash.bin",
                ioreg_reader=lambda: ioreg,
                command_runner=runner,
                path_is_character_device=lambda _path: True,
            )
            self.assertEqual(board.flash_bytes, 16 * 1024 * 1024)
            self.assertEqual(len(digest), 64)
            self.assertEqual(
                stat.S_IMODE((root / "flash.bin").stat().st_mode), 0o400
            )
            self.assertEqual([command[1] for command in commands], ["board-info", "read-flash"])

    def test_flash_derives_header_size_from_qualified_capacity(self) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")
        commands: list[list[str]] = []

        def runner(command: list[str], **kwargs) -> subprocess.CompletedProcess[str]:
            commands.append(command)
            if command[1] == "board-info":
                return subprocess.CompletedProcess(command, 0, board_info(), "")
            self.assertEqual(command[1], "flash")
            self.assertEqual(command[command.index("--before") + 1], "no-reset")
            self.assertEqual(command[command.index("--flash-size") + 1], "16mb")
            partition_fd = int(
                command[command.index("--partition-table") + 1].rsplit("/", 1)[1]
            )
            elf_fd = int(command[-1].rsplit("/", 1)[1])
            self.assertEqual(kwargs["pass_fds"], (partition_fd, elf_fd))
            return subprocess.CompletedProcess(
                command, 0, board_info() + "flash complete\n", ""
            )

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            elf = root / "qualification.elf"
            partition = root / "partition.csv"
            elf.write_bytes(b"elf")
            partition.write_text("partition\n")
            _device, board = host.flash_qualification_image(
                expected_usb_serial=MAC_A,
                expected_mac=MAC_A,
                expected_flash_bytes=16 * 1024 * 1024,
                evidence_prefix=root / "fresh" / "hardware" / "board-a-flash",
                elf=elf,
                partition_table=partition,
                ioreg_reader=lambda: ioreg,
                command_runner=runner,
                path_is_character_device=lambda _path: True,
            )
            self.assertEqual(board.flash_bytes, 16 * 1024 * 1024)
            self.assertEqual(
                [command[1] for command in commands],
                ["board-info", "flash", "board-info"],
            )
            flash_command = commands[1]
            self.assertNotEqual(Path(flash_command[-1]), elf)
            self.assertNotEqual(
                Path(flash_command[flash_command.index("--partition-table") + 1]),
                partition,
            )
            self.assertTrue(
                host._evidence_path(
                    root / "fresh" / "hardware" / "board-a-flash",
                    ".flash-input.elf",
                ).is_file()
            )
            evidence = json.loads(
                host._evidence_path(
                    root / "fresh" / "hardware" / "board-a-flash",
                    ".flash.verified.json",
                ).read_text()
            )
            self.assertEqual(evidence["flash_action_target"]["mac"], MAC_A)
            self.assertEqual(evidence["post_flash_target"]["mac"], MAC_A)

    def test_qualification_flash_rejects_wrong_action_mac_before_swap_back(
        self,
    ) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")
        commands: list[str] = []

        def runner(command: list[str], **_kwargs) -> subprocess.CompletedProcess[str]:
            commands.append(command[1])
            if command[1] == "board-info":
                return subprocess.CompletedProcess(command, 0, board_info(), "")
            return subprocess.CompletedProcess(
                command, 0, board_info(mac=MAC_B) + "flash complete\n", ""
            )

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            elf = root / "qualification.elf"
            partition = root / "partition.csv"
            prefix = root / "board-a-flash"
            elf.write_bytes(b"elf")
            partition.write_text("partition\n")
            with self.assertRaisesRegex(
                host.PostWriteEvidenceError, "flash action target is unverified"
            ):
                host.flash_qualification_image(
                    expected_usb_serial=MAC_A,
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=prefix,
                    elf=elf,
                    partition_table=partition,
                    ioreg_reader=lambda: ioreg,
                    command_runner=runner,
                    path_is_character_device=lambda _path: True,
                )
            self.assertEqual(commands, ["board-info", "flash"])
            self.assertFalse(
                host._evidence_path(prefix, ".flash.verified.json").exists()
            )

    def test_qualification_flash_rejects_wrong_post_flash_mac(self) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")
        board_info_calls = 0
        commands: list[str] = []

        def runner(command: list[str], **_kwargs) -> subprocess.CompletedProcess[str]:
            nonlocal board_info_calls
            commands.append(command[1])
            if command[1] == "board-info":
                board_info_calls += 1
                mac = MAC_A if board_info_calls == 1 else MAC_B
                return subprocess.CompletedProcess(
                    command, 0, board_info(mac=mac), ""
                )
            return subprocess.CompletedProcess(
                command, 0, board_info() + "flash complete\n", ""
            )

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            elf = root / "qualification.elf"
            partition = root / "partition.csv"
            prefix = root / "board-a-flash"
            elf.write_bytes(b"elf")
            partition.write_text("partition\n")
            with self.assertRaisesRegex(
                host.PostWriteEvidenceError, "unverified target"
            ):
                host.flash_qualification_image(
                    expected_usb_serial=MAC_A,
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=prefix,
                    elf=elf,
                    partition_table=partition,
                    ioreg_reader=lambda: ioreg,
                    command_runner=runner,
                    path_is_character_device=lambda _path: True,
                )
            self.assertEqual(commands, ["board-info", "flash", "board-info"])
            self.assertFalse(
                host._evidence_path(prefix, ".flash.verified.json").exists()
            )

    def test_backup_output_cannot_alias_any_evidence_path(self) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")
        calls = 0

        def runner(*_args, **_kwargs):
            nonlocal calls
            calls += 1
            return subprocess.CompletedProcess([], 0, board_info(), "")

        with tempfile.TemporaryDirectory() as directory:
            prefix = Path(directory) / "board-a"
            alias = host._evidence_path(prefix, ".ioreg-before.txt")
            with self.assertRaisesRegex(host.QualificationError, "aliases"):
                host.read_full_flash(
                    expected_usb_serial=MAC_A,
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=prefix,
                    output=alias,
                    ioreg_reader=lambda: ioreg,
                    command_runner=runner,
                    path_is_character_device=lambda _path: True,
                )
            self.assertEqual(calls, 0)

    def test_qualification_flash_inherited_inputs_are_read_only(self) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")

        def runner(command: list[str], **kwargs) -> subprocess.CompletedProcess[str]:
            if command[1] == "board-info":
                return subprocess.CompletedProcess(command, 0, board_info(), "")
            for descriptor in kwargs["pass_fds"]:
                with self.assertRaises(OSError) as denied:
                    os.write(descriptor, b"unauthorized mutation")
                self.assertIn(denied.exception.errno, (errno.EBADF, errno.EACCES))
            return subprocess.CompletedProcess(
                command, 0, board_info() + "flash complete\n", ""
            )

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            elf = root / "qualification.elf"
            partition = root / "partition.csv"
            prefix = root / "board-a-flash"
            elf.write_bytes(b"elf")
            partition.write_text("partition\n")
            host.flash_qualification_image(
                expected_usb_serial=MAC_A,
                expected_mac=MAC_A,
                expected_flash_bytes=16 * 1024 * 1024,
                evidence_prefix=prefix,
                elf=elf,
                partition_table=partition,
                ioreg_reader=lambda: ioreg,
                command_runner=runner,
                path_is_character_device=lambda _path: True,
            )
            self.assertTrue(
                host._evidence_path(prefix, ".flash.verified.json").is_file()
            )

    def test_deleted_post_flash_input_is_reported_as_post_write_failure(self) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")

        def runner(command: list[str], **_kwargs) -> subprocess.CompletedProcess[str]:
            if command[1] == "board-info":
                return subprocess.CompletedProcess(command, 0, board_info(), "")
            host._evidence_path(prefix, ".flash-input.elf").unlink()
            return subprocess.CompletedProcess(
                command, 0, board_info() + "flash complete\n", ""
            )

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            elf = root / "qualification.elf"
            partition = root / "partition.csv"
            prefix = root / "board-a-flash"
            elf.write_bytes(b"elf")
            partition.write_text("partition\n")
            with self.assertRaisesRegex(
                host.PostWriteEvidenceError, "flash was attempted"
            ):
                host.flash_qualification_image(
                    expected_usb_serial=MAC_A,
                    expected_mac=MAC_A,
                    expected_flash_bytes=16 * 1024 * 1024,
                    evidence_prefix=prefix,
                    elf=elf,
                    partition_table=partition,
                    ioreg_reader=lambda: ioreg,
                    command_runner=runner,
                    path_is_character_device=lambda _path: True,
                )
            self.assertFalse(
                host._evidence_path(prefix, ".flash.verified.json").exists()
            )

if __name__ == "__main__":
    unittest.main()
