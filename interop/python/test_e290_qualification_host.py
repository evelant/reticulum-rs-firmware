from __future__ import annotations

import hashlib
from pathlib import Path
import subprocess
import tempfile
import unittest

import e290_qualification_host as host


MAC_A = "aa:bb:cc:dd:ee:01"
MAC_B = "aa:bb:cc:dd:ee:02"


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

        def runner(command: list[str], **_kwargs) -> subprocess.CompletedProcess[str]:
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


class BoundActionTests(unittest.TestCase):
    def test_merged_verification_is_identity_gated_and_hash_bound(self) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")
        commands: list[list[str]] = []

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image = root / "merged.bin"
            image.write_bytes(bytes((0xE9, 0x03, 0x02, 0x4F)) + b"merged-image")
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
                return subprocess.CompletedProcess(command, 0, "read complete\n", "")

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

    def test_merged_verification_rejects_mismatched_readback(self) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")

        def runner(command: list[str], **_kwargs) -> subprocess.CompletedProcess[str]:
            if command[1] == "board-info":
                return subprocess.CompletedProcess(command, 0, board_info(), "")
            Path(command[-1]).write_bytes(b"wrong")
            return subprocess.CompletedProcess(command, 0, "read complete\n", "")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image = root / "merged.bin"
            image.write_bytes(bytes((0xE9, 0x03, 0x02, 0x4F)) + b"merged-image")
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

    def test_merged_flash_is_identity_gated_and_verified_by_readback(self) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")
        commands: list[list[str]] = []

        def runner(command: list[str], **_kwargs) -> subprocess.CompletedProcess[str]:
            commands.append(command)
            if command[1] == "board-info":
                return subprocess.CompletedProcess(command, 0, board_info(), "")
            if command[1] == "write-bin":
                self.assertEqual(
                    command[command.index("--before") + 1], "no-reset"
                )
                self.assertEqual(command[command.index("--after") + 1], "no-reset")
                self.assertEqual(command[-2], "0x0")
                return subprocess.CompletedProcess(command, 0, "write complete\n", "")
            self.assertEqual(command[1], "read-flash")
            self.assertEqual(command[command.index("--before") + 1], "no-reset")
            self.assertEqual(command[command.index("--after") + 1], "no-reset")
            Path(command[-1]).write_bytes(Path(commands[-2][-1]).read_bytes())
            return subprocess.CompletedProcess(command, 0, "read complete\n", "")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image = root / "merged.bin"
            image.write_bytes(bytes((0xE9, 0x03, 0x02, 0x4F)) + b"merged-image")
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
                ["board-info", "write-bin", "read-flash"],
            )
            evidence = host._evidence_path(prefix, ".flash-image.verified.json")
            self.assertTrue(evidence.is_file())
            self.assertEqual(
                host._sha256_file(host._evidence_path(prefix, ".readback.bin")),
                digest,
            )

    def test_merged_flash_requires_exact_hf_module_confirmation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "merged.bin"
            image.write_bytes(bytes((0xE9, 0x03, 0x02, 0x4F)))
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
            image.write_bytes(bytes((0xE9, 0x03, 0x02, 0x4F)))
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

        def runner(command: list[str], **_kwargs) -> subprocess.CompletedProcess[str]:
            if command[1] == "board-info":
                return subprocess.CompletedProcess(command, 0, board_info(), "")
            if command[1] == "read-flash":
                Path(command[-1]).write_bytes(b"wrong")
            return subprocess.CompletedProcess(command, 0, "complete\n", "")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image = root / "merged.bin"
            image.write_bytes(bytes((0xE9, 0x03, 0x02, 0x4F)) + b"image")
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
            return subprocess.CompletedProcess(command, 0, "read complete\n", "")

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
            self.assertEqual([command[1] for command in commands], ["board-info", "read-flash"])

    def test_flash_derives_header_size_from_qualified_capacity(self) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")
        commands: list[list[str]] = []

        def runner(command: list[str], **_kwargs) -> subprocess.CompletedProcess[str]:
            commands.append(command)
            if command[1] == "board-info":
                return subprocess.CompletedProcess(command, 0, board_info(), "")
            self.assertEqual(command[1], "flash")
            self.assertEqual(command[command.index("--before") + 1], "no-reset")
            self.assertEqual(command[command.index("--flash-size") + 1], "16mb")
            return subprocess.CompletedProcess(command, 0, "flash complete\n", "")

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
            self.assertEqual([command[1] for command in commands], ["board-info", "flash"])
            flash_command = commands[1]
            self.assertNotEqual(Path(flash_command[-1]), elf)
            self.assertNotEqual(
                Path(flash_command[flash_command.index("--partition-table") + 1]),
                partition,
            )
            self.assertTrue(Path(flash_command[-1]).is_file())

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

    def test_post_flash_input_copy_mutation_is_not_verified(self) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")

        def runner(command: list[str], **_kwargs) -> subprocess.CompletedProcess[str]:
            if command[1] == "board-info":
                return subprocess.CompletedProcess(command, 0, board_info(), "")
            preserved_elf = Path(command[-1])
            preserved_elf.chmod(0o644)
            preserved_elf.write_bytes(b"changed after flash")
            return subprocess.CompletedProcess(command, 0, "flash complete\n", "")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            elf = root / "qualification.elf"
            partition = root / "partition.csv"
            prefix = root / "board-a-flash"
            elf.write_bytes(b"elf")
            partition.write_text("partition\n")
            with self.assertRaisesRegex(
                host.PostWriteEvidenceError, "flash completed"
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

    def test_deleted_post_flash_input_is_reported_as_post_write_failure(self) -> None:
        ioreg = ioreg_device(MAC_A, "/dev/cu.usbmodem101")

        def runner(command: list[str], **_kwargs) -> subprocess.CompletedProcess[str]:
            if command[1] == "board-info":
                return subprocess.CompletedProcess(command, 0, board_info(), "")
            Path(command[-1]).unlink()
            return subprocess.CompletedProcess(command, 0, "flash complete\n", "")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            elf = root / "qualification.elf"
            partition = root / "partition.csv"
            prefix = root / "board-a-flash"
            elf.write_bytes(b"elf")
            partition.write_text("partition\n")
            with self.assertRaisesRegex(
                host.PostWriteEvidenceError, "flash completed"
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
