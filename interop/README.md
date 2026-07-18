# Interoperability fixtures

`peers.toml` pins the upstream implementations used to generate and verify
wire behavior. Generated vectors belong under `vectors/` and must include:

- generator source revision;
- command and Python version;
- protocol/release lane;
- whether bytes came from creation, parsing or a captured exchange;
- expected result and any normalization applied.

Do not copy ad-hoc bytes from a moving `master` checkout without recording its
commit. Secrets and real user identities must never enter fixtures.

## Released Reticulum lane

Create a CPython 3.13.7 environment, install the exact released source revision
and regenerate or check the deterministic foundation corpus:

```sh
python3.13 -m pip install \
  --target artifacts/phase0/rns-1.3.8-python \
  -r interop/python/requirements-rns-1.3.8.txt
PYTHONPATH=artifacts/phase0/rns-1.3.8-python \
  PYTHON=python3.13 \
  cargo run --locked -p xtask -- check-rns-vectors
```

The committed corpus deliberately excludes generated ciphertext: Reticulum
uses a fresh ephemeral key and IV, so byte equality would not be reproducible.
Separate semantic tests will decrypt Python ciphertext and encrypt Rust data
for Python as the Link/Resource lanes are added.

## Local device-API session lane

`vectors/device-api-session-v1.json` freezes the non-secret inputs, exact hello
encodings, transcript hash, HKDF outputs, full proofs, direction-separated
record tags and COBS wire records for the USB Serial/JTAG qualification suite.
Its generator uses only Python's standard-library SHA-256 and HMAC and imports
no Rust, Rete or Reticulum implementation. Check both the committed corpus and
the independent mutation tests with:

```sh
python3 interop/python/generate_device_api_session_vectors.py --check
PYTHONPATH=interop/python python3 -m unittest -v \
  interop/python/test_device_api_session_vectors.py
cargo test --locked -p reticulum-device-api-session
```

These are deterministic software vectors. They do not claim that an
authenticated USB bearer or permanent E290 session composition exists yet.
Suite 1 authenticates and integrity-protects records but deliberately does not
hide their payloads; it is forbidden on BLE and Wi-Fi.

## Wired developer-pairing lane

`vectors/device-api-pairing-v1.json` freezes ADR 0010's public test inputs,
eight successful pre-authentication records, transcript hash, full client proof,
activation confirmation and exact COBS wire bytes. The independent generator
uses only Python's standard-library SHA-256 and HMAC. Check the committed corpus,
mutation tests and allocation-free Rust core with:

```sh
python3 interop/python/generate_device_api_pairing_vectors.py --check
python3 interop/python/test_device_api_pairing_vectors.py
cargo test --locked -p reticulum-device-api-pairing
```

This integrity-only developer ceremony assumes the physically connected USB
host after GPIO21 confirmation. It provides no payload confidentiality and is
not enabled for BLE or Wi-Fi. Durable credential mutation and powered bearer
composition remain separate qualification gates.

## Phase-1 RNode receive lane

`vectors/rnode-hil-v1.json` is the deterministic schema-3 receive corpus for
official RNode Firmware 1.86 at the revision pinned in `peers.toml`. Its 19
scenarios cover ordinary and promiscuous RNode framing, single/split MTU
boundaries, timeout/replacement/malformed cases, queue pressure and the
feature-bound returned-fault stimuli. Regenerate it with the project-owned
generator and check the generator, corpus, KISS peer tool and Rust replay with:

```sh
python3.13 interop/python/generate_rnode_hil_vectors.py
PYTHON=python3.13 \
  cargo run --locked -p xtask -- check-rnode-hil-vectors
```

`interop/python/rnode_hil.py` keeps planning and transmission separate. The
check above and its `list` and `plan` subcommands do not open a serial device or
transmit. RF use requires the explicit acknowledgements and complete radio,
airtime and safety arguments on its `send` subcommand; follow
[`docs/phase-1-rx-hil.md`](../docs/phase-1-rx-hil.md) and preserve the peer
image, source bundle, copied corpus/tool and per-invocation manifest/transcript
as powered evidence. The attached bench already satisfies the antenna and
NA915 authorization requirements; these flags preserve per-run provenance and
configuration rather than requesting new approval. The host-generated
boot-local DATA corpus is deliberately
ephemeral and belongs in that ignored evidence tree, not in `vectors/`.

## ESP32-S3 native-USB log capture

`python/esp32s3_usb_serial_capture.py` is a receive-only POSIX recorder for
supplemental Tracker development logs. It clears DTR and RTS together at the
first ioctl after opening, configures raw 115200 8N1, preserves already-buffered
input and exposes no serial write path. Run it with the pinned CPython 3.13.7:

```sh
python3.13 interop/python/esp32s3_usb_serial_capture.py \
  --port /dev/cu.usbmodemYOUR_PORT > serial.log 2> serial-recorder.log
```

For a counted normal boot from an already opened exclusive descriptor, opt in
to the same USB-Serial/JTAG hard-reset pulse used by `espflash`:

```sh
python3.13 interop/python/esp32s3_usb_serial_capture.py \
  --port /dev/cu.usbmodemYOUR_PORT \
  --hard-reset-after-open --pre-reset-drain-seconds 1 \
  --duration-seconds 90 > serial.log 2> serial-recorder.log
```

That mode preserves the pre-reset bytes, reports their exact
`counted_reset_offset`, verifies DTR and RTS are inactive after the pulse, and
then records for the requested post-reset duration. It changes modem-control
lines but still never writes serial data to the target.

The tool is reset-minimizing, not passive. Its descriptor is read/write because
Darwin's TTY control path requires that mode, but the recorder makes no serial
write call or host-input read. POSIX cannot set CDC line controls before
`open(2)`, and opening the Tracker's native USB can reset the ESP32-S3. The tool
does not follow re-enumeration, since the path could then name another attached
Tracker. Opening does not guarantee a reset: a capture can attach to an already
running activation, start in the middle of a buffered record and omit the boot
lines. Plain mode is suitable for supplemental post-boot heartbeats. The
explicit hard-reset mode can prove the sequence beginning at its recorded
offset, but not a preceding cold-power-on boot. Follow the independent RX-only
UART0 procedure in the Phase-1 HIL runbook whenever the original power-on is
evidence-critical.
