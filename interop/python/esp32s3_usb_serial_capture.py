#!/usr/bin/env python3
"""Reset-minimizing, receive-only ESP32-S3 native-USB serial capture.

This tool is intentionally narrower than a terminal. It never reads stdin,
never writes to the serial device and never flushes bytes that were already
buffered by the host. It clears DTR and RTS together immediately after opening
the POSIX TTY, then records the device byte stream verbatim on stdout.

Opening a native ESP32-S3 USB CDC device can itself reset the target. POSIX has
no API that presets DTR/RTS before open(2), so this tool is reset-minimizing,
not passive. Its output is supplemental post-boot evidence and must not be used
to claim that the original cold-power-on boot was observed.

An explicit hard-reset-after-open mode is available for tests that need one
counted normal boot. It preserves all bytes received before the reset, reports
their exact length as the counted-reset offset, and toggles only the modem
control lines on the already-open exclusive descriptor. It never writes serial
data to the target.
"""

from __future__ import annotations

import argparse
import array
from datetime import datetime, timezone
import errno
import fcntl
import math
import os
from pathlib import Path
import platform
import select
import sys
import termios
import time
from typing import BinaryIO, Callable, Sequence, TextIO


EXPECTED_PYTHON = (3, 13, 7)
BAUDRATE = 115_200
READ_SIZE = 4_096
SELECT_TIMEOUT_SECONDS = 0.1
DEFAULT_PRE_RESET_DRAIN_SECONDS = 1.0
HARD_RESET_STEP_SECONDS = 0.1
USB_SERIAL_JTAG_HARD_RESET_MODE = "usb_serial_jtag_hard_reset"
MODEM_INACTIVE_MASK = termios.TIOCM_DTR | termios.TIOCM_RTS
MODEM_FLOW_CONTROL_NAMES = (
    "CRTSCTS",
    "CCTS_OFLOW",
    "CRTS_IFLOW",
    "CDTR_IFLOW",
    "CDSR_OFLOW",
    "CCAR_OFLOW",
    "MDMBUF",
)


class SerialConfigurationError(RuntimeError):
    """The opened TTY could not be placed in the required receive-only state."""


class CaptureOutputError(RuntimeError):
    """The evidence sink could not preserve the captured byte stream."""


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds").replace(
        "+00:00", "Z"
    )


def validate_runtime() -> None:
    if platform.python_implementation() != "CPython":
        raise RuntimeError("capture requires CPython")
    if sys.version_info[:3] != EXPECTED_PYTHON:
        expected = ".".join(str(value) for value in EXPECTED_PYTHON)
        actual = ".".join(str(value) for value in sys.version_info[:3])
        raise RuntimeError(f"capture requires CPython {expected}, found {actual}")


def _clear_modem_lines(
    fd: int,
    *,
    ioctl_fn: Callable[..., object] = fcntl.ioctl,
) -> None:
    mask = array.array("i", [MODEM_INACTIVE_MASK])
    ioctl_fn(fd, termios.TIOCMBIC, mask, True)


def _clear_modem_line(
    fd: int,
    line: int,
    *,
    ioctl_fn: Callable[..., object] = fcntl.ioctl,
) -> None:
    mask = array.array("i", [line])
    ioctl_fn(fd, termios.TIOCMBIC, mask, True)


def _set_modem_line(
    fd: int,
    line: int,
    *,
    ioctl_fn: Callable[..., object] = fcntl.ioctl,
) -> None:
    mask = array.array("i", [line])
    ioctl_fn(fd, termios.TIOCMBIS, mask, True)


def _read_modem_lines(
    fd: int,
    *,
    ioctl_fn: Callable[..., object] = fcntl.ioctl,
) -> int:
    state = array.array("i", [0])
    ioctl_fn(fd, termios.TIOCMGET, state, True)
    return state[0]


def _raw_115200_attributes(attributes: list[object]) -> list[object]:
    configured = list(attributes)
    configured[0] = 0  # input flags: no byte rewriting or software flow control
    configured[1] = 0  # output flags: no byte rewriting

    flow_control = 0
    for name in MODEM_FLOW_CONTROL_NAMES:
        flow_control |= getattr(termios, name, 0)
    clear_control = (
        termios.CSIZE
        | termios.PARENB
        | termios.PARODD
        | termios.CSTOPB
        | termios.HUPCL
        | getattr(termios, "CIGNORE", 0)
        | flow_control
    )
    configured[2] = (
        (int(configured[2]) & ~clear_control)
        | termios.CS8
        | termios.CLOCAL
        | termios.CREAD
    )
    configured[3] = 0  # local flags: raw, non-canonical input
    configured[4] = termios.B115200
    configured[5] = termios.B115200

    control_characters = list(configured[6])
    control_characters[termios.VMIN] = 0
    control_characters[termios.VTIME] = 0
    configured[6] = control_characters
    return configured


def configure_capture_fd(
    fd: int,
    *,
    ioctl_fn: Callable[..., object] = fcntl.ioctl,
    tcgetattr_fn: Callable[[int], list[object]] = termios.tcgetattr,
    tcsetattr_fn: Callable[[int, int, list[object]], None] = termios.tcsetattr,
) -> None:
    # POSIX cannot preset CDC control lines before open(2). Make the first
    # device operation a single ioctl that clears both lines together.
    _clear_modem_lines(fd, ioctl_fn=ioctl_fn)
    ioctl_fn(fd, termios.TIOCEXCL)

    attributes = _raw_115200_attributes(tcgetattr_fn(fd))
    tcsetattr_fn(fd, termios.TCSANOW, attributes)

    # Reassert the safe line state after termios configuration and fail closed
    # if the driver does not report both controls inactive.
    _clear_modem_lines(fd, ioctl_fn=ioctl_fn)
    active = _read_modem_lines(fd, ioctl_fn=ioctl_fn) & MODEM_INACTIVE_MASK
    if active:
        raise OSError(
            errno.EIO,
            f"serial driver left DTR/RTS active (mask=0x{active:x})",
        )


def open_capture_port(
    port: str,
    *,
    open_fn: Callable[[str, int], int] = os.open,
    close_fn: Callable[[int], None] = os.close,
    ioctl_fn: Callable[..., object] = fcntl.ioctl,
    tcgetattr_fn: Callable[[int], list[object]] = termios.tcgetattr,
    tcsetattr_fn: Callable[[int, int, list[object]], None] = termios.tcsetattr,
) -> int:
    # Darwin's TTY line-control path expects a read/write descriptor. This tool
    # deliberately makes no data write call anywhere after opening it.
    flags = os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK
    fd = open_fn(port, flags)
    try:
        configure_capture_fd(
            fd,
            ioctl_fn=ioctl_fn,
            tcgetattr_fn=tcgetattr_fn,
            tcsetattr_fn=tcsetattr_fn,
        )
    except Exception as error:
        close_fn(fd)
        raise SerialConfigurationError(
            f"could not establish reset-minimizing serial state on {port}: {error}"
        ) from error
    except BaseException:
        close_fn(fd)
        raise
    return fd


def _flush_output(output: BinaryIO) -> None:
    try:
        output.flush()
    except BrokenPipeError:
        raise
    except OSError as error:
        raise CaptureOutputError(f"capture output failed: {error}") from error


def _write_all(output: BinaryIO, payload: bytes) -> int:
    try:
        remaining = memoryview(payload)
        while remaining:
            written = output.write(remaining)
            if written is None:
                written = len(remaining)
            if written <= 0:
                raise CaptureOutputError("capture output accepted no bytes")
            remaining = remaining[written:]
        _flush_output(output)
    except BrokenPipeError:
        raise
    except OSError as error:
        raise CaptureOutputError(f"capture output failed: {error}") from error
    return len(payload)


def usb_serial_jtag_hard_reset(
    fd: int,
    *,
    ioctl_fn: Callable[..., object] = fcntl.ioctl,
    sleep_fn: Callable[[float], None] = time.sleep,
) -> None:
    """Perform espflash's normal-boot USB-Serial/JTAG hard-reset pulse.

    The descriptor was already configured with both controls inactive. Clear
    DTR again immediately before the pulse, hold it inactive while asserting
    RTS for 100 ms, then release RTS. These are modem-control ioctls only; no
    serial data is written.
    """

    _clear_modem_line(fd, termios.TIOCM_DTR, ioctl_fn=ioctl_fn)
    sleep_fn(HARD_RESET_STEP_SECONDS)
    _set_modem_line(fd, termios.TIOCM_RTS, ioctl_fn=ioctl_fn)
    sleep_fn(HARD_RESET_STEP_SECONDS)
    _clear_modem_line(fd, termios.TIOCM_RTS, ioctl_fn=ioctl_fn)
    active = _read_modem_lines(fd, ioctl_fn=ioctl_fn) & MODEM_INACTIVE_MASK
    if active:
        raise OSError(
            errno.EIO,
            f"hard reset left DTR/RTS active (mask=0x{active:x})",
        )


def stream_capture_fd(
    fd: int,
    output: BinaryIO,
    *,
    duration_seconds: float | None = None,
    select_fn: Callable[..., tuple[list[int], list[int], list[int]]] = select.select,
    read_fn: Callable[[int, int], bytes] = os.read,
    monotonic_fn: Callable[[], float] = time.monotonic,
) -> int:
    bytes_written = 0
    deadline = (
        None if duration_seconds is None else monotonic_fn() + duration_seconds
    )
    while True:
        timeout = SELECT_TIMEOUT_SECONDS
        if deadline is not None:
            remaining = deadline - monotonic_fn()
            if remaining <= 0:
                return bytes_written
            timeout = min(timeout, remaining)
        readable, _, _ = select_fn([fd], [], [], timeout)
        if not readable:
            continue
        try:
            payload = read_fn(fd, READ_SIZE)
        except BlockingIOError:
            continue
        if not payload:
            raise OSError(errno.EIO, "serial device disconnected")
        bytes_written += _write_all(output, payload)


def capture(
    port: str,
    output: BinaryIO,
    status: TextIO,
    *,
    duration_seconds: float | None,
    hard_reset_after_open: bool = False,
    pre_reset_drain_seconds: float = DEFAULT_PRE_RESET_DRAIN_SECONDS,
    open_fn: Callable[[str], int] = open_capture_port,
    close_fn: Callable[[int], None] = os.close,
    stream_fn: Callable[[int, BinaryIO, float | None], int | None] | None = None,
    hard_reset_fn: Callable[[int], None] = usb_serial_jtag_hard_reset,
) -> None:
    fd = open_fn(port)
    try:
        print(
            f"{utc_now()} opened={port} baud={BAUDRATE} data=8N1 "
            "flow=none dtr=false rts=false receive_only=true reconnect=false",
            file=status,
            flush=True,
        )
        def stream(duration: float | None) -> int | None:
            if stream_fn is None:
                return stream_capture_fd(
                    fd,
                    output,
                    duration_seconds=duration,
                )
            return stream_fn(fd, output, duration)

        if hard_reset_after_open:
            drained = stream(pre_reset_drain_seconds)
            if drained is None or drained < 0:
                raise RuntimeError(
                    "pre-reset capture did not report a valid byte count"
                )
            # This flush is deliberately separate from the per-read flushes:
            # the evidence bytes must be durable before their offset is armed.
            _flush_output(output)
            print(
                f"{utc_now()} counted_reset_offset={drained} "
                f"reset_mode={USB_SERIAL_JTAG_HARD_RESET_MODE} "
                f"pre_reset_drain_seconds={pre_reset_drain_seconds} "
                f"counted_reset_status=armed duration_seconds={duration_seconds} "
                "duration_scope=post_reset",
                file=status,
                flush=True,
            )
            hard_reset_fn(fd)
            print(
                f"{utc_now()} counted_reset_offset={drained} "
                f"reset_mode={USB_SERIAL_JTAG_HARD_RESET_MODE} "
                "counted_reset_status=completed",
                file=status,
                flush=True,
            )
        stream(duration_seconds)
        duration_scope = " duration_scope=post_reset" if hard_reset_after_open else ""
        print(
            f"{utc_now()} completed=true duration_seconds={duration_seconds}"
            f"{duration_scope}",
            file=status,
            flush=True,
        )
    finally:
        close_fn(fd)


def parse_args(arguments: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="receive-only, reset-minimizing ESP32-S3 USB capture"
    )
    parser.add_argument("--port", required=True, type=Path)
    parser.add_argument(
        "--duration-seconds",
        type=float,
        help=(
            "stop successfully after this positive stream duration; in "
            "--hard-reset-after-open mode it times only the post-reset stream, "
            "excluding the pre-reset drain and reset pulse"
        ),
    )
    parser.add_argument(
        "--hard-reset-after-open",
        action="store_true",
        help=(
            "after a bounded pre-reset drain, report its byte offset and perform "
            "one USB-Serial/JTAG normal-boot hard reset on the capture descriptor"
        ),
    )
    parser.add_argument(
        "--pre-reset-drain-seconds",
        type=float,
        help=(
            "positive bounded drain before --hard-reset-after-open "
            f"(default: {DEFAULT_PRE_RESET_DRAIN_SECONDS})"
        ),
    )
    parsed = parser.parse_args(arguments)
    if parsed.duration_seconds is not None and (
        not math.isfinite(parsed.duration_seconds) or parsed.duration_seconds <= 0
    ):
        parser.error("--duration-seconds must be finite and positive")
    if parsed.pre_reset_drain_seconds is not None and not parsed.hard_reset_after_open:
        parser.error(
            "--pre-reset-drain-seconds requires --hard-reset-after-open"
        )
    if parsed.pre_reset_drain_seconds is not None and (
        not math.isfinite(parsed.pre_reset_drain_seconds)
        or parsed.pre_reset_drain_seconds <= 0
    ):
        parser.error("--pre-reset-drain-seconds must be finite and positive")
    if parsed.hard_reset_after_open and parsed.pre_reset_drain_seconds is None:
        parsed.pre_reset_drain_seconds = DEFAULT_PRE_RESET_DRAIN_SECONDS
    return parsed


def main(arguments: Sequence[str] | None = None) -> int:
    try:
        validate_runtime()
        args = parse_args(sys.argv[1:] if arguments is None else arguments)
        print(
            f"{utc_now()} WARNING reset_minimizing=true passive=false "
            "cold_power_evidence=false native_usb_open_may_reset=true",
            file=sys.stderr,
            flush=True,
        )
        if args.hard_reset_after_open:
            print(
                f"{utc_now()} hard_reset_after_open=true "
                f"reset_mode={USB_SERIAL_JTAG_HARD_RESET_MODE} "
                f"pre_reset_drain_seconds={args.pre_reset_drain_seconds}",
                file=sys.stderr,
                flush=True,
            )
        capture(
            str(args.port),
            sys.stdout.buffer,
            sys.stderr,
            duration_seconds=args.duration_seconds,
            hard_reset_after_open=args.hard_reset_after_open,
            pre_reset_drain_seconds=(
                DEFAULT_PRE_RESET_DRAIN_SECONDS
                if args.pre_reset_drain_seconds is None
                else args.pre_reset_drain_seconds
            ),
        )
    except KeyboardInterrupt:
        print(f"{utc_now()} interrupted=true", file=sys.stderr, flush=True)
        return 130
    except BrokenPipeError:
        return 0
    except (OSError, RuntimeError) as error:
        print(f"capture failed: {error}", file=sys.stderr, flush=True)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
