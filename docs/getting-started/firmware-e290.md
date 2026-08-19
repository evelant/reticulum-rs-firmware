# Build and flash E290 firmware

These instructions target the Heltec Vision Master E290-HF with 16 MiB flash,
at least 8 MiB mapped PSRAM, and the HT-RA62-HF radio. Keep a suitable antenna
attached. Do not use this image on the LF radio variant.

The repository provides two product profiles:

| Profile | Selection | Contents |
| --- | --- | --- |
| Gateway | default, or `--profile gateway` | LoRa, BLE API/onboarding, display, Wi-Fi station, and one outbound Reticulum TCP interface |
| Appliance | `--profile appliance` | LoRa, BLE API/onboarding, and display without the Wi-Fi/TCP uplink |

Both product profiles emit startup, lifecycle, BLE, radio, storage, display,
and fault logs over native USB Serial/JTAG at `Info` level. The gateway also
logs Wi-Fi and TCP state. USB is diagnostics-only; the authenticated local
device API is exposed over BLE.

## Toolchains

The host toolchain is pinned by `rust-toolchain.toml`. The Espressif Xtensa
fork is pinned separately and is the effective minimum supported Rust version
for firmware; it tracks upstream with a delay, so the workspace MSRV is lower
than the host toolchain. See
[Toolchains](../development/verification.md#toolchains) for the exact numbers
and why they differ. Install the matching Espressif toolchain and espflash:

```sh
cargo install espup --version 0.16.0 --locked

export ESPUP_EXPORT_FILE="$HOME/export-esp.sh"
espup install --targets esp32s3 \
  --toolchain-version 1.95.0.0 \
  --skip-version-parse \
  --crosstool-toolchain-version 15.2.0_20250920 \
  --name esp
source "$HOME/export-esp.sh"

cargo install espflash --version 4.5.0 --locked
cargo run --locked -p xtask -- doctor
```

Source the generated export file in each new shell before building for the
ESP32-S3.

## Build and package

The repository wrapper selects the target, release flags, partition table, and
output layout consistently. The default profile is `gateway`:

```sh
cargo run --locked -p xtask -- build
cargo run --locked -p xtask -- check-elf
cargo run --locked -p xtask -- package \
  --output target/e290-gateway.bin
```

Build the LoRa/BLE-only appliance instead with:

```sh
cargo run --locked -p xtask -- build --profile appliance
cargo run --locked -p xtask -- package --profile appliance \
  --output target/e290-appliance.bin
```

`package` builds first and produces one merged address-zero image for the
checked 16 MiB partition map, DIO flash mode, 80 MHz flash frequency, and the
application in `factory`. The file is not padded through product data, so an
ordinary address-zero write preserves state above the application partition.
`check-elf` verifies the linked startup stack and compiler-emitted frame sizes;
PSRAM cannot compensate for an oversized CPU startup stack.

### Direct Cargo and espflash equivalent

Use the wrapper for routine work. When diagnosing the build pipeline, these are
the important direct inputs for the default gateway:

```sh
source "$HOME/export-esp.sh"

CARGO_TARGET_DIR=target/e290-gateway \
RUSTFLAGS='-C code-model=large -C link-arg=-nostartfiles -Z emit-stack-sizes' \
cargo +esp build --locked --release \
  -p reticulum-e290-firmware \
  --target xtensa-esp32s3-none-elf

ELF=target/e290-gateway/xtensa-esp32s3-none-elf/release/reticulum-e290-firmware

espflash save-image --skip-update-check \
  --chip esp32s3 --merge --skip-padding \
  --flash-mode dio --flash-freq 80mhz --flash-size 16mb \
  --xtal-freq 40mhz \
  --partition-table partitions/e290.csv \
  --target-app-partition factory \
  "$ELF" target/e290-gateway.bin
```

For the appliance profile, add `--no-default-features --features appliance` to
the Cargo command and use a distinct target directory. `-C code-model=large`,
`-nostartfiles`, and emitted stack sizes are required for the current image.

## Select the board

If several boards are connected, unplug the others while identifying the
target. Enter the ROM loader by holding `BOOT`, tapping `RST`, waiting about one
second, and releasing `BOOT`.

List the board and record its current port and eFuse MAC:

```sh
espflash board-info --chip esp32s3 --after no-reset
```

Set the exact port returned for the intended board. Port names can change after
every reset, so repeat `board-info` before a later flash rather than treating a
`/dev/cu.*`, `/dev/tty*`, or `COM*` name as board identity.

```sh
PORT=/dev/cu.usbmodemXXXX
espflash board-info --chip esp32s3 --port "$PORT" --after no-reset
```

Stop if the command does not report an ESP32-S3 and 16 MiB flash, or if the
physical radio module is not HT-RA62-HF.

## Fresh provisioning

Fresh provisioning deletes the Reticulum identity, BLE bond, device
credentials, network configuration, journals, and all messages. It is required
before the first installation of the current storage formats. It is also the
correct recovery when the installed layout or format is unknown; do not try to
mount incompatible product data.

Erase the complete flash, then write the packaged image:

```sh
espflash erase-flash --chip esp32s3 --port "$PORT" \
  --after no-reset --non-interactive --skip-update-check

IMAGE=target/e290-gateway.bin
espflash write-bin --chip esp32s3 --port "$PORT" \
  --after no-reset --non-interactive --skip-update-check \
  0x0 "$IMAGE"
```

The current store layout is defined in
[`partitions/README.md`](../../partitions/README.md). Double-check `$PORT`
immediately before the destructive command.

## Upgrade while preserving state

Use this only after the board has already been provisioned with every format
version listed in [`partitions/README.md`](../../partitions/README.md). Do not
erase product data:

```sh
IMAGE=target/e290-gateway.bin
espflash write-bin --chip esp32s3 --port "$PORT" \
  --after no-reset --non-interactive --skip-update-check \
  0x0 "$IMAGE"
```

The merged image ends before product data at `0x610000`. A future incompatible
storage change must update this guide and require fresh provisioning or an
explicit migration; reflashing the application alone does not make
incompatible persistent data safe.

To package and flash directly from the ELF instead of writing a merged image,
retain every geometry input explicitly:

```sh
espflash flash --chip esp32s3 --port "$PORT" \
  --flash-size 16mb --flash-mode dio --flash-freq 80mhz \
  --target-app-partition factory \
  --partition-table partitions/e290.csv \
  --after no-reset --non-interactive --skip-update-check \
  "$ELF"
```

## Boot and monitor

The flash commands leave the chip in the loader. Release `BOOT`, tap `RST`, and
wait for the display to progress from `STARTING` to `READY`.

The application may enumerate on a different port. Find it again, then attach
without resetting the running board:

```sh
PORT=/dev/cu.usbmodemXXXX
espflash monitor --chip esp32s3 --port "$PORT" \
  --elf "$ELF" --skip-update-check --non-interactive --no-reset
```

Continue with [BLE pairing](pairing.md). Configure Wi-Fi, the TCP peer, announce
policy, RMAP, and the LoRa profile from the app. Material network and radio
changes are saved atomically and apply after reboot.

## Radio configuration

The erased default is 915 MHz, 125 kHz, SF7, coding rate 4/5, and requested
+14 dBm. The app can save a complete compatible tuple and requested +14, +17,
+20, or +22 dBm output for the next boot. All direct peers need matching
frequency, bandwidth, spreading factor, and coding rate.

The RMAP importer previews one copied `RNodeInterface` block but does not make
that configuration appropriate for the E290 or the operator's region. Review
every field before saving. See [E290 hardware](../hardware/e290.md) and use the
[range-test procedure](../development/range-testing.md) before drawing range
conclusions.

## Troubleshooting

If the display remains at `STARTING`, confirm that the board left the loader,
the image was packaged with the checked 16 MiB partition table, and the target
has at least 8 MiB mapped PSRAM. Capture USB logs before changing state.

If the app cannot find the board, wait for `READY`, grant Bluetooth permission,
and close other clients that may own the single GATT connection. Use the
[board-only recovery flow](pairing.md#recover-a-stale-or-unavailable-bluetooth-bond)
when the retained bond belongs to another phone.

If Wi-Fi or TCP is unhealthy, keep BLE connected if possible and inspect the
Network panel plus USB logs for association, DHCP, DNS, socket, and reconnect
state. Wi-Fi/BLE controller allocations require internal RAM and cannot be
offloaded to PSRAM.
