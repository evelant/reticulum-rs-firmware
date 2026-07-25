#!/usr/bin/env python3
"""Passively follow a USB serial device across disconnect/re-enumeration.

Unlike a flashing monitor, this helper never writes to the device or requests
a reset. It is intended for native-USB firmware whose serial device vanishes
briefly during a physical reset.
"""

from __future__ import annotations

import argparse
import sys
import time

import serial


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", required=True)
    parser.add_argument("--baud", type=int, default=115_200)
    parser.add_argument("--retry-seconds", type=float, default=0.05)
    return parser.parse_args()


def open_passively(port: str, baud: int) -> serial.Serial:
    connection = serial.Serial()
    connection.port = port
    connection.baudrate = baud
    connection.timeout = 0.1
    connection.write_timeout = 0
    connection.dtr = False
    connection.rts = False
    connection.open()
    return connection


def main() -> int:
    args = parse_args()
    while True:
        try:
            connection = open_passively(args.port, args.baud)
        except serial.SerialException:
            time.sleep(args.retry_seconds)
            continue

        try:
            while True:
                chunk = connection.read(4_096)
                if chunk:
                    sys.stdout.buffer.write(chunk)
                    sys.stdout.buffer.flush()
        except (OSError, serial.SerialException):
            pass
        finally:
            connection.close()


if __name__ == "__main__":
    raise SystemExit(main())
