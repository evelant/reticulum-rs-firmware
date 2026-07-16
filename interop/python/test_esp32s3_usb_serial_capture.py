from __future__ import annotations

import array
import errno
from io import BytesIO, StringIO
import os
from pathlib import Path
import termios
import unittest

import esp32s3_usb_serial_capture as capture_tool


ROOT = Path(__file__).parents[2]


def sample_attributes() -> list[object]:
    control_characters: list[int] = [0] * 32
    modem_flow_control = 0
    for name in capture_tool.MODEM_FLOW_CONTROL_NAMES:
        modem_flow_control |= getattr(termios, name, 0)
    return [
        0xFFFF,
        0xFFFF,
        termios.CS7
        | termios.PARENB
        | termios.PARODD
        | termios.CSTOPB
        | termios.HUPCL
        | getattr(termios, "CIGNORE", 0)
        | modem_flow_control,
        0xFFFF,
        termios.B9600,
        termios.B9600,
        control_characters,
    ]


class PartialWriter:
    def __init__(self, maximum_write: int) -> None:
        self.maximum_write = maximum_write
        self.payload = bytearray()
        self.flushes = 0

    def write(self, payload: memoryview) -> int:
        written = min(len(payload), self.maximum_write)
        self.payload.extend(payload[:written])
        return written

    def flush(self) -> None:
        self.flushes += 1


class SerialConfigurationTests(unittest.TestCase):
    def test_open_clears_both_lines_and_configures_raw_without_flush(self) -> None:
        events: list[object] = []
        configured: list[list[object]] = []

        def open_fn(port: str, flags: int) -> int:
            events.append(("open", port, flags))
            return 17

        def close_fn(fd: int) -> None:
            events.append(("close", fd))

        def ioctl_fn(fd: int, request: int, argument=None, mutate=False):
            if request == termios.TIOCMBIC:
                self.assertIsInstance(argument, array.array)
                events.append(("clear", fd, argument[0], mutate))
            elif request == termios.TIOCEXCL:
                events.append(("exclusive", fd))
            elif request == termios.TIOCMGET:
                events.append(("get-lines", fd, mutate))
                argument[0] = 0
            else:
                self.fail(f"unexpected ioctl request {request}")
            return 0

        def tcgetattr_fn(fd: int) -> list[object]:
            events.append(("tcgetattr", fd))
            return sample_attributes()

        def tcsetattr_fn(fd: int, when: int, attributes: list[object]) -> None:
            events.append(("tcsetattr", fd, when))
            configured.append(attributes)

        fd = capture_tool.open_capture_port(
            "/dev/cu.test",
            open_fn=open_fn,
            close_fn=close_fn,
            ioctl_fn=ioctl_fn,
            tcgetattr_fn=tcgetattr_fn,
            tcsetattr_fn=tcsetattr_fn,
        )

        self.assertEqual(fd, 17)
        open_flags = events[0][2]
        self.assertEqual(open_flags & os.O_ACCMODE, os.O_RDWR)
        self.assertNotEqual(open_flags & os.O_NOCTTY, 0)
        self.assertNotEqual(open_flags & os.O_NONBLOCK, 0)
        self.assertEqual(
            [event[0] for event in events],
            [
                "open",
                "clear",
                "exclusive",
                "tcgetattr",
                "tcsetattr",
                "clear",
                "get-lines",
            ],
        )
        clear_events = [event for event in events if event[0] == "clear"]
        self.assertEqual(len(clear_events), 2)
        self.assertTrue(
            all(
                event[2] == termios.TIOCM_DTR | termios.TIOCM_RTS
                for event in clear_events
            )
        )

        attributes = configured[0]
        self.assertEqual(attributes[0], 0)
        self.assertEqual(attributes[1], 0)
        self.assertEqual(attributes[3], 0)
        self.assertEqual(attributes[4], termios.B115200)
        self.assertEqual(attributes[5], termios.B115200)
        self.assertEqual(int(attributes[2]) & termios.CSIZE, termios.CS8)
        self.assertEqual(int(attributes[2]) & termios.HUPCL, 0)
        self.assertEqual(int(attributes[2]) & termios.PARENB, 0)
        self.assertEqual(int(attributes[2]) & termios.CSTOPB, 0)
        self.assertEqual(
            int(attributes[2]) & getattr(termios, "CIGNORE", 0),
            0,
        )
        for name in capture_tool.MODEM_FLOW_CONTROL_NAMES:
            self.assertEqual(
                int(attributes[2]) & getattr(termios, name, 0),
                0,
                name,
            )
        self.assertNotEqual(int(attributes[2]) & termios.CLOCAL, 0)
        self.assertNotEqual(int(attributes[2]) & termios.CREAD, 0)

    def test_active_control_line_fails_closed_and_closes_fd(self) -> None:
        closed: list[int] = []

        def ioctl_fn(_fd: int, request: int, argument=None, _mutate=False):
            if request == termios.TIOCMGET:
                argument[0] = termios.TIOCM_DTR
            return 0

        with self.assertRaisesRegex(
            capture_tool.SerialConfigurationError,
            "left DTR/RTS active",
        ):
            capture_tool.open_capture_port(
                "/dev/cu.test",
                open_fn=lambda _port, _flags: 23,
                close_fn=closed.append,
                ioctl_fn=ioctl_fn,
                tcgetattr_fn=lambda _fd: sample_attributes(),
                tcsetattr_fn=lambda _fd, _when, _attributes: None,
            )
        self.assertEqual(closed, [23])


class HardResetTests(unittest.TestCase):
    def test_usb_serial_jtag_reset_uses_exact_normal_boot_sequence(self) -> None:
        events: list[object] = []

        def ioctl_fn(fd: int, request: int, argument, mutate: bool):
            events.append(("ioctl", fd, request, argument[0], mutate))
            if request == termios.TIOCMGET:
                argument[0] = 0
            return 0

        capture_tool.usb_serial_jtag_hard_reset(
            29,
            ioctl_fn=ioctl_fn,
            sleep_fn=lambda seconds: events.append(("sleep", seconds)),
        )

        self.assertEqual(
            events,
            [
                ("ioctl", 29, termios.TIOCMBIC, termios.TIOCM_DTR, True),
                ("sleep", 0.1),
                ("ioctl", 29, termios.TIOCMBIS, termios.TIOCM_RTS, True),
                ("sleep", 0.1),
                ("ioctl", 29, termios.TIOCMBIC, termios.TIOCM_RTS, True),
                ("ioctl", 29, termios.TIOCMGET, 0, True),
            ],
        )

    def test_active_line_after_reset_fails_closed(self) -> None:
        def ioctl_fn(_fd: int, request: int, argument, _mutate: bool):
            if request == termios.TIOCMGET:
                argument[0] = termios.TIOCM_RTS
            return 0

        with self.assertRaisesRegex(OSError, "left DTR/RTS active"):
            capture_tool.usb_serial_jtag_hard_reset(
                29,
                ioctl_fn=ioctl_fn,
                sleep_fn=lambda _seconds: None,
            )

    def test_ioctl_error_aborts_reset_sequence(self) -> None:
        events: list[object] = []

        def ioctl_fn(_fd: int, request: int, argument, _mutate: bool):
            events.append((request, argument[0]))
            if request == termios.TIOCMBIS:
                raise OSError(errno.EIO, "control transfer failed")
            return 0

        with self.assertRaisesRegex(OSError, "control transfer failed"):
            capture_tool.usb_serial_jtag_hard_reset(
                30,
                ioctl_fn=ioctl_fn,
                sleep_fn=lambda seconds: events.append(("sleep", seconds)),
            )
        self.assertEqual(
            events,
            [
                (termios.TIOCMBIC, termios.TIOCM_DTR),
                ("sleep", 0.1),
                (termios.TIOCMBIS, termios.TIOCM_RTS),
            ],
        )


class StreamTests(unittest.TestCase):
    def test_binary_input_and_partial_output_writes_are_exact(self) -> None:
        payloads = iter([b"\x00\xff\r", b"\nabc", KeyboardInterrupt()])
        readiness = iter(
            [([], [], []), ([31], [], []), ([31], [], []), ([31], [], [])]
        )
        output = PartialWriter(maximum_write=2)

        def select_fn(*_arguments):
            return next(readiness)

        def read_fn(_fd: int, _size: int) -> bytes:
            result = next(payloads)
            if isinstance(result, BaseException):
                raise result
            return result

        with self.assertRaises(KeyboardInterrupt):
            capture_tool.stream_capture_fd(
                31,
                output,  # type: ignore[arg-type]
                select_fn=select_fn,
                read_fn=read_fn,
            )
        self.assertEqual(bytes(output.payload), b"\x00\xff\r\nabc")
        self.assertEqual(output.flushes, 2)

    def test_bounded_capture_stops_at_monotonic_deadline(self) -> None:
        monotonic_values = iter([10.0, 10.0, 10.05, 10.1])
        timeouts: list[float] = []

        def select_fn(_read, _write, _error, timeout):
            timeouts.append(timeout)
            return ([], [], [])

        capture_tool.stream_capture_fd(
            33,
            BytesIO(),
            duration_seconds=0.1,
            select_fn=select_fn,
            monotonic_fn=lambda: next(monotonic_values),
        )
        self.assertEqual(len(timeouts), 2)
        self.assertAlmostEqual(timeouts[0], 0.1)
        self.assertAlmostEqual(timeouts[1], 0.05)

    def test_output_failure_closes_the_serial_descriptor(self) -> None:
        attempts = 0
        closed: list[int] = []

        def open_fn(_port: str) -> int:
            nonlocal attempts
            attempts += 1
            return 37

        def failed_stream(
            _fd: int, _output: BytesIO, _duration_seconds: float | None
        ) -> None:
            raise capture_tool.CaptureOutputError("evidence sink failed")

        with self.assertRaisesRegex(
            capture_tool.CaptureOutputError,
            "evidence sink failed",
        ):
            capture_tool.capture(
                "/dev/cu.test",
                BytesIO(),
                StringIO(),
                duration_seconds=None,
                open_fn=open_fn,
                close_fn=closed.append,
                stream_fn=failed_stream,
            )
        self.assertEqual(attempts, 1)
        self.assertEqual(closed, [37])

    def test_disconnect_is_an_error(self) -> None:
        with self.assertRaisesRegex(OSError, "disconnected") as raised:
            capture_tool.stream_capture_fd(
                41,
                BytesIO(),
                select_fn=lambda *_arguments: ([41], [], []),
                read_fn=lambda _fd, _size: b"",
            )
        self.assertEqual(raised.exception.errno, errno.EIO)

    def test_capture_closes_fd_when_interrupted(self) -> None:
        closed: list[int] = []
        with self.assertRaises(KeyboardInterrupt):
            capture_tool.capture(
                "/dev/cu.test",
                BytesIO(),
                StringIO(),
                duration_seconds=None,
                open_fn=lambda _port: 53,
                close_fn=closed.append,
                stream_fn=lambda _fd, _output, _duration: (_ for _ in ()).throw(
                    KeyboardInterrupt()
                ),
            )
        self.assertEqual(closed, [53])

    def test_counted_reset_drains_then_reports_exact_offset_before_reset(self) -> None:
        output = BytesIO()
        status = StringIO()
        events: list[object] = []
        calls = 0

        def stream_fn(
            fd: int, sink: BytesIO, duration_seconds: float | None
        ) -> int:
            nonlocal calls
            calls += 1
            events.append(("stream", fd, duration_seconds))
            if calls == 1:
                self.assertEqual(capture_tool._write_all(sink, b"pre\x00reset"), 9)
                return 9
            self.assertEqual(capture_tool._write_all(sink, b"post-reset"), 10)
            return 10

        def hard_reset_fn(fd: int) -> None:
            events.append(("reset", fd))
            self.assertIn(
                "counted_reset_offset=9 "
                f"reset_mode={capture_tool.USB_SERIAL_JTAG_HARD_RESET_MODE} "
                "pre_reset_drain_seconds=1.25 counted_reset_status=armed",
                status.getvalue(),
            )
            self.assertEqual(output.getvalue(), b"pre\x00reset")

        capture_tool.capture(
            "/dev/cu.test",
            output,
            status,
            duration_seconds=90.0,
            hard_reset_after_open=True,
            pre_reset_drain_seconds=1.25,
            open_fn=lambda _port: 61,
            close_fn=lambda fd: events.append(("close", fd)),
            stream_fn=stream_fn,
            hard_reset_fn=hard_reset_fn,
        )

        self.assertEqual(output.getvalue(), b"pre\x00resetpost-reset")
        self.assertEqual(
            events,
            [
                ("stream", 61, 1.25),
                ("reset", 61),
                ("stream", 61, 90.0),
                ("close", 61),
            ],
        )
        self.assertIn("counted_reset_status=completed", status.getvalue())
        self.assertIn("completed=true duration_seconds=90.0", status.getvalue())

    def test_reset_failure_closes_fd_without_starting_counted_capture(self) -> None:
        closed: list[int] = []
        stream_durations: list[float | None] = []
        status = StringIO()

        def stream_fn(
            _fd: int, _output: BytesIO, duration_seconds: float | None
        ) -> int:
            stream_durations.append(duration_seconds)
            return 0

        with self.assertRaisesRegex(OSError, "reset failed"):
            capture_tool.capture(
                "/dev/cu.test",
                BytesIO(),
                status,
                duration_seconds=90.0,
                hard_reset_after_open=True,
                open_fn=lambda _port: 67,
                close_fn=closed.append,
                stream_fn=stream_fn,
                hard_reset_fn=lambda _fd: (_ for _ in ()).throw(
                    OSError(errno.EIO, "reset failed")
                ),
            )
        self.assertEqual(stream_durations, [1.0])
        self.assertEqual(closed, [67])
        self.assertIn("counted_reset_status=armed", status.getvalue())
        self.assertNotIn("counted_reset_status=completed", status.getvalue())

    def test_missing_pre_reset_byte_count_fails_before_reset(self) -> None:
        reset_called = False

        def reset_fn(_fd: int) -> None:
            nonlocal reset_called
            reset_called = True

        with self.assertRaisesRegex(RuntimeError, "valid byte count"):
            capture_tool.capture(
                "/dev/cu.test",
                BytesIO(),
                StringIO(),
                duration_seconds=1.0,
                hard_reset_after_open=True,
                open_fn=lambda _port: 71,
                close_fn=lambda _fd: None,
                stream_fn=lambda _fd, _output, _duration: None,
                hard_reset_fn=reset_fn,
            )
        self.assertFalse(reset_called)


class PolicyTests(unittest.TestCase):
    def test_recorder_has_no_serial_write_or_flush_path(self) -> None:
        source = Path(capture_tool.__file__).read_text(encoding="utf-8")
        self.assertNotIn("os.write(", source)
        self.assertNotIn("tcflush", source)
        self.assertNotIn("reset_input_buffer", source)
        self.assertNotIn("sys.stdin", source)

    def test_runbook_does_not_invoke_espflash_monitor(self) -> None:
        runbook = (ROOT / "docs" / "phase-1-rx-hil.md").read_text(encoding="utf-8")
        self.assertNotIn("espflash monitor", runbook)
        self.assertIn("esp32s3_usb_serial_capture.py", runbook)

    def test_runtime_mismatch_fails_before_capture(self) -> None:
        original = capture_tool.EXPECTED_PYTHON
        try:
            capture_tool.EXPECTED_PYTHON = (0, 0, 0)
            with self.assertRaisesRegex(RuntimeError, "capture requires CPython"):
                capture_tool.validate_runtime()
        finally:
            capture_tool.EXPECTED_PYTHON = original

    def test_hard_reset_cli_defaults_to_one_second_drain(self) -> None:
        parsed = capture_tool.parse_args(
            ["--port", "/dev/cu.test", "--hard-reset-after-open"]
        )
        self.assertTrue(parsed.hard_reset_after_open)
        self.assertEqual(parsed.pre_reset_drain_seconds, 1.0)

    def test_pre_reset_drain_requires_hard_reset_mode(self) -> None:
        with self.assertRaises(SystemExit):
            capture_tool.parse_args(
                [
                    "--port",
                    "/dev/cu.test",
                    "--pre-reset-drain-seconds",
                    "1",
                ]
            )

    def test_pre_reset_drain_must_be_finite_and_positive(self) -> None:
        for invalid in ("0", "-1", "nan", "inf"):
            with self.subTest(invalid=invalid), self.assertRaises(SystemExit):
                capture_tool.parse_args(
                    [
                        "--port",
                        "/dev/cu.test",
                        "--hard-reset-after-open",
                        "--pre-reset-drain-seconds",
                        invalid,
                    ]
                )


if __name__ == "__main__":
    unittest.main()
