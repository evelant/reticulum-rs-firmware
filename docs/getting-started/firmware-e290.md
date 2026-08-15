# Build and flash the E290 appliance firmware

This is the canonical procedure for the current Heltec Vision Master E290-HF
appliance images. Choose one profile before building:

| Profile | Cargo feature | Build target directory | Merged image | Qualification |
| --- | --- | --- | --- | --- |
| BLE/LoRa appliance | `ble-api-proof` | `target/e290-ble` | `e290-node-ble.bin` | Powered on both development boards |
| Wi-Fi/TCP border candidate | `wifi-tcp-proof` | `target/e290-wifi-tcp` | `e290-node-wifi-tcp.bin` | Bounded powered BLE/Wi-Fi startup, association/DHCP, public-TCP connection, native ingress, local announce writes, and a 420-second diagnostic run; full border routing is not qualified |

`ble-api-proof` remains the normal app-usable profile. It includes LoRa, the
BLE local API, secure onboarding, and the e-paper display.
`wifi-tcp-proof` includes that complete profile, then adds the Wi-Fi station
task and one outbound Reticulum TCP packet interface in slot 2. It is available
for bounded powered use, but is not yet a fully qualified replacement for the
BLE/LoRa image because complete LoRa↔TCP forwarding, recovery, and long-soak
gates remain open.

Both wireless appliance profiles initialize the USB Serial/JTAG logger at
`Info`. Keep a USB cable attached during alpha testing so startup, BLE, Wi-Fi,
DNS, TCP, and fault diagnostics are available without rebuilding a special
image. The legacy no-wireless profile is the exception: it owns the same USB
Serial/JTAG byte stream for framed RDA1 records and therefore does not
initialize the logger. Raw log text must never be multiplexed into that framed
stream.

The current firmware graph pins `esp-hal`, `esp-radio`, `esp-rtos`, and the
other patched esp-rs crates to exact upstream revision
`b50efcb0dcd94b58ec337e511891057aa1f2e8fb`. That revision includes
[esp-hal #5776](https://github.com/esp-rs/esp-hal/pull/5776), which pairs
ESP32-S3 combo-PHY initialization with Wi-Fi RX enable/disable. The Wi-Fi
station controller also explicitly limits its maximum TX power to 60 quarter-
dBm, or 15 dBm; this setting is unrelated to the independently configurable
LoRa transmit power.

Current upstream still has a known Wi-Fi TX-credit lifecycle risk: a send that
races station disconnect can consume a global in-flight credit without its
completion callback returning that credit. The project uses three TX credits,
and enough such races can block both Wi-Fi TX and RX while association and DHCP
still appear healthy. Treat reconnect and repeated DNS/TCP failures as a
regression signal until this path is corrected and hardware-qualified.

The procedure assumes the fitted `HT-RA62-HF` radio, an antenna appropriate for
the selected frequency, 16 MiB flash, and at least the ESP32-S3R8's 8 MiB
PSRAM. Do not use this image on an E290-LF radio variant.

Once booted, this image automatically transmits announces and routed traffic.
Exactly erased configuration starts with the historical 915 MHz, 125 kHz, SF7,
CR 4/5, requested +14 dBm default. An authenticated client can save a complete
frequency/BW/SF/CR/power tuple for the next boot. Power it only with a suitable
antenna and where the complete selected operating profile is permitted.

## 1. Install and check the toolchains

The host Rust version is pinned by `rust-toolchain.toml`. ESP32-S3 Xtensa builds
use the same `espup` and crosstool revisions as CI:

```sh
cargo install espup --version 0.16.0 --locked
test "$(espup --version)" = "espup 0.16.0"

export ESPUP_EXPORT_FILE="$HOME/export-esp.sh"
espup install --targets esp32s3 \
  --toolchain-version 1.95.0.0 \
  --skip-version-parse \
  --crosstool-toolchain-version 15.2.0_20250920 \
  --name esp
source "$HOME/export-esp.sh"

test "$(rustc +esp --version)" = \
  "rustc 1.95.0-nightly (95e5bda86 2026-04-15) (1.95.0.0)"
test "$(cargo +esp --version)" = \
  "cargo 1.95.0-nightly (f2d3ce0bd 2026-03-21) (1.95.0.0)"
test "$(xtensa-esp32s3-elf-gcc --version | head -n 1)" = \
  "xtensa-esp-elf-gcc (crosstool-NG esp-15.2.0_20250920) 15.2.0"
```

Install the exact `espflash` version expected by repository policy and ensure
CPython 3.13 is available for the identity-bound flash helper:

```sh
cargo install espflash --version 4.5.0 --locked
python3.13 --version
cargo run --locked -p xtask -- doctor
```

`doctor` verifies host Rust 1.97.0, Espressif Rust 1.95.0.0, `espflash`
4.5.0, and the Xtensa GCC linker. Source `~/export-esp.sh` in every new shell
before an ESP build.

## 2. Select and build the appliance image

From the repository root, choose exactly one variable block.

For the powered BLE/LoRa appliance:

```sh
FIRMWARE_FEATURE=ble-api-proof
BUILD_TARGET=e290-ble
IMAGE_NAME=e290-node-ble.bin
```

For the bounded powered Wi-Fi/TCP candidate:

```sh
FIRMWARE_FEATURE=wifi-tcp-proof
BUILD_TARGET=e290-wifi-tcp
IMAGE_NAME=e290-node-wifi-tcp.bin
```

Keep these three variables in the same shell through packaging and flashing.
Then build the selected profile:

```sh
source "$HOME/export-esp.sh"

case "$FIRMWARE_FEATURE:$BUILD_TARGET:$IMAGE_NAME" in
  ble-api-proof:e290-ble:e290-node-ble.bin | \
  wifi-tcp-proof:e290-wifi-tcp:e290-node-wifi-tcp.bin) ;;
  *) printf '%s\n' 'profile, build target, and image name do not match' >&2; exit 1 ;;
esac

cargo test --locked \
  -p reticulum-heltec-vision-master-e290-node \
  --lib --no-default-features --features "$FIRMWARE_FEATURE"
cargo run --locked -p xtask -- graph-policy

CARGO_TARGET_DIR="target/$BUILD_TARGET" \
RUSTFLAGS='-C code-model=large -C link-arg=-nostartfiles -Z emit-stack-sizes' \
cargo +esp clippy --locked --release \
  -p reticulum-heltec-vision-master-e290-node \
  --bin reticulum-heltec-vision-master-e290-node \
  --no-default-features --features "$FIRMWARE_FEATURE" \
  --target xtensa-esp32s3-none-elf -- -D warnings

CARGO_TARGET_DIR="target/$BUILD_TARGET" \
RUSTFLAGS='-C code-model=large -C link-arg=-nostartfiles -Z emit-stack-sizes' \
cargo +esp build --locked --release \
  -p reticulum-heltec-vision-master-e290-node \
  --bin reticulum-heltec-vision-master-e290-node \
  --no-default-features --features "$FIRMWARE_FEATURE" \
  --target xtensa-esp32s3-none-elf
```

The full alpha appliance has crossed the Xtensa `l32r` literal-reach limit.
`-C code-model=large` makes Espressif LLVM intersperse literal pools through
the text sections so the final ELF can link. It is required for a flashable
image, not an optional optimization. Setting `RUSTFLAGS` replaces the target
configuration's flags, so these commands also repeat `-nostartfiles`; the
startup inspection additionally requires `-Z emit-stack-sizes`.

The selected output ELF is:

```sh
ELF="target/$BUILD_TARGET/xtensa-esp32s3-none-elf/release/reticulum-heltec-vision-master-e290-node"
test -f "$ELF"

cargo +stable run --locked -p xtask -- \
  e290-runtime-measurement inspect-startup-elf --elf "$ELF"
```

The startup inspection is a required packaging gate, not an optional diagnostic.
It reads the linked stack bounds and compiler-emitted frame sizes from this exact
ELF and rejects an image whose startup construction path cannot fit the E290's
internal CPU stack. PSRAM does not replace that stack; omitting
`-Z emit-stack-sizes` makes the inspection fail instead of silently accepting
an unaudited image.

## 3. Package the exact flash image

Package bootloader, partition table, and application into one unpadded merged
image:

```sh
ELF="target/$BUILD_TARGET/xtensa-esp32s3-none-elf/release/reticulum-heltec-vision-master-e290-node"
IMAGE="target/$BUILD_TARGET/$IMAGE_NAME"

espflash save-image --skip-update-check \
  --chip esp32s3 --merge --skip-padding \
  --flash-mode dio --flash-freq 80mhz --flash-size 16mb \
  --xtal-freq 40mhz \
  --partition-table partitions/heltec-vision-master-e290-node.csv \
  --target-app-partition factory \
  "$ELF" "$IMAGE"

IMAGE_BYTES="$(wc -c < "$IMAGE" | tr -d ' ')"
IMAGE_SHA256="$(shasum -a 256 "$IMAGE" | cut -d ' ' -f 1)"
test "$IMAGE_BYTES" -le $((0x610000))
printf 'image=%s bytes=%s sha256=%s\n' \
  "$IMAGE" "$IMAGE_BYTES" "$IMAGE_SHA256"
```

Every option in this command matters. In particular, omitting the checked-in
partition table or allowing an implicit 8 MiB/40 MHz image can leave the panel
at `STARTING`. The workspace intentionally has no board-wide flash runner:
do not use `cargo run`, a bare `espflash flash`, or implicit `espflash
save-image` defaults for the E290 product.

## 4. Put one board in the ROM loader

Keep the antenna attached. The board has two different buttons:

- `BOOT` is ESP32-S3 GPIO0 and selects the ROM loader.
- the middle button labelled `21` is GPIO21 and is used later for pairing.

To enter download mode, hold `BOOT`, tap `RST`, wait about one second, and
release `BOOT`. If multiple boards are attached, disconnect the others while
identifying the target.

Run the read-only probe and record the eFuse MAC, detected 16 MiB flash, and
current USB serial:

```sh
espflash board-info --chip esp32s3 --after no-reset
```

On the qualified E290 pair, the uppercase native-USB serial is the same MAC
shown by `board-info`, while `--expected-mac` uses lowercase. Set both values
explicitly; port names such as `/dev/cu.usbmodem101` are ephemeral and are not
board identities:

```sh
EXPECTED_USB_SERIAL=AC:A7:04:E1:3E:88
EXPECTED_MAC=ac:a7:04:e1:3e:88
```

The repository's identity-bound helper currently uses macOS IORegistry to map
that serial to its active callout device. Image creation is portable, but this
verified flash workflow is currently macOS-only.

## 5. Choose fresh provisioning or an upgrade

Create a private, unique evidence directory for this operation:

```sh
umask 077
RUN="${TMPDIR:-/tmp}/e290-flash-$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -m 700 "$RUN"
```

Every helper `--evidence-prefix` must be new. Reusing a prefix fails closed.

### Fresh provisioning or factory reset

Use this path for a factory board, a deliberately wiped board, or a board whose
entire appliance identity and message state should be discarded.

The erase destroys the Reticulum identity, device credentials, BLE bond,
configuration, journal, and both message stores:

```sh
python3.13 interop/python/e290_qualification_host.py erase-region \
  --usb-serial "$EXPECTED_USB_SERIAL" \
  --expected-mac "$EXPECTED_MAC" \
  --expected-flash-bytes 16777216 \
  --evidence-prefix "$RUN/product-data-erase" \
  --offset 0x610000 --length 0x520000

LOADER_PORT="$(
  python3.13 interop/python/e290_qualification_host.py flash-merged \
    --usb-serial "$EXPECTED_USB_SERIAL" \
    --expected-mac "$EXPECTED_MAC" \
    --expected-flash-bytes 16777216 \
    --evidence-prefix "$RUN/product-image-flash" \
    --image "$IMAGE" \
    --expected-image-sha256 "$IMAGE_SHA256" \
    --confirmed-radio-module HT-RA62-HF
)"
```

The single blanking range covers every current product-owned store. A separate
first-LXMF migration is not needed for a truly fresh install.

### Routine upgrade that preserves state

Do not erase any region for a routine upgrade:

```sh
LOADER_PORT="$(
  python3.13 interop/python/e290_qualification_host.py flash-merged \
    --usb-serial "$EXPECTED_USB_SERIAL" \
    --expected-mac "$EXPECTED_MAC" \
    --expected-flash-bytes 16777216 \
    --evidence-prefix "$RUN/product-image-flash" \
    --image "$IMAGE" \
    --expected-image-sha256 "$IMAGE_SHA256" \
    --confirmed-radio-module HT-RA62-HF
)"
```

The merged image ends before `0x610000`, so this preserves identity, pairing,
bonds, journals, and messages. `flash-merged` validates both ESP32-S3 headers,
the 16 MiB DIO/80 MHz profile, the embedded canonical partition table, the
protected boundary, board identity, write result, and exact readback before it
succeeds.

Current firmware appends LXMF physical-format-2 records while retaining
read-only compatibility with legacy format-1 records. After any format-2
append, do not roll back to format-1 firmware without first exporting or
deliberately erasing the LXMF store; the older firmware cannot safely mount the
mixed store.

Current firmware also reads network-configuration semantic formats 1 through 3
and writes format 4 after a material configuration change. Firmware predating
format 4 cannot mount that snapshot. Before such a downgrade, preserve any
needed settings and deliberately erase the 8 KiB network-configuration store at
`0x618000..0x61a000`; this loses saved Wi-Fi, TCP-peer, announce/RMAP, location,
and LoRa-profile settings. The ordinary `flash-merged` path preserves this
store, so reflashing an older application image alone is not sufficient.

`flash-merged` is an intentional exception to commands that pass a partition
CSV at flash time: it accepts only the already-merged address-zero image and
verifies its embedded table byte-for-byte before touching hardware.

If preserving a deployed identity matters, first follow the encrypted
full-flash backup and before/after product-data comparison in the
[detailed E290 procedure](../e290-node.md#detailed-connected-board-identity-backup-and-flash-procedure).
A full dump contains plaintext private keys, device credentials, BLE bond
material, and messages. Never put it in the repository, ordinary build
artifacts, an issue, or unencrypted sync storage.

## 6. Boot the application

The verified helper deliberately leaves the chip in the ROM loader. Release
`BOOT`, then tap `RST`, or power-cycle the board without holding `BOOT`.

For the powered-qualified `ble-api-proof` profile, the display should progress
from `STARTING` to `READY`. Both `ble-api-proof` and `wifi-tcp-proof` emit
application and framework logs over USB Serial/JTAG at `Info`. After the
application re-enumerates, identify its current callout port and attach a
monitor without resetting the running board:

```sh
PORT=/dev/cu.usbmodemXXXX
espflash monitor --chip esp32s3 --port "$PORT" \
  --elf "$ELF" --skip-update-check --non-interactive --no-reset
```

`--no-reset` preserves the exact runtime state being diagnosed and captures
only lines emitted after the monitor attaches. The ROM loader and application
may enumerate under different ephemeral port paths, so rediscover by USB
serial rather than assuming `$LOADER_PORT` is still current. An empty monitor
is expected only from the legacy no-wireless profile whose USB FIFO carries
framed RDA1 records.

The `wifi-tcp-proof` image has powered evidence for BLE-controller
startup, saved-bond restore and advertising, Wi-Fi association and DHCP, a
public-TCP connection, native Reticulum ingress, and local announce writes. A
420-second post-fix run completed without reset, transmit failure, socket
close, or reconnect. Treat each new image as a regression run: this bounded
evidence does not qualify complete LoRa↔TCP forwarding, recovery, or long soak.

The 2026-08-14 upstream-stack checkpoint additionally flashed e13e88 without
changing its product-data partitions. On three consecutive boots the saved
`rmap.world` hostname resolved through DHCP DNS in 49, 72, and 33 ms and TCP
interface 2 came online. The first run received a public-network announce nine
hops away; the third remained online through 25 bonded BLE link cycles and
ongoing LoRa transmissions without a logged Wi-Fi/TCP failure. This closes the
earlier basic-DNS gate but does not qualify an induced AP outage or the known
upstream TX-credit disconnect race described above.

With no enabled Wi-Fi profile or TCP peer, the candidate keeps interface 2
offline. Configure the board from the app's Connectivity workspace, then reboot
to apply the new durable revision. The initial profile supports up to four
WPA2-Personal networks and one literal-IPv4-or-DNS outbound Reticulum TCP peer.

For the exact unattended watchdog-reset procedure, see the
[detailed E290 runbook](../e290-node.md#detailed-connected-board-identity-backup-and-flash-procedure).

## 7. Configure the LoRa profile

After pairing, open the app's **Connectivity** workspace and find **Radio
compatibility**. It shows the immutable **Running** tuple separately from the
saved **After restart** tuple. Choose **Configure** to edit center frequency,
bandwidth, spreading factor, coding rate, and requested transmit power as one
atomic profile. The NA915 default preset is the project's 915 MHz, 125 kHz,
SF7, CR 4/5 compatibility tuple, not a Reticulum-wide standard; selecting it
does not change power.

To reuse a profile copied from RMAP.world:

1. Copy exactly one Reticulum `RNodeInterface` block.
2. Choose **Paste config**, paste the block, and choose **Preview imported
   values**. This only replaces the unsaved draft.
3. Review every field. If the block omits `txpower`, the current draft power is
   retained.
4. Choose **Save for next restart** explicitly.

The app normalizes supported rounded RNode bandwidth labels, but neither RMAP
nor the importer is an authority for this hardware or region. The E290 and app
reject a profile unless the complete occupied channel fits the HT-RA62-HF
863--928 MHz path, SF is 7 through 12, coding rate is 4/5 through 4/8, power is
one of +14, +17, +20, or +22 dBm, and the bandwidth/SF combination is qualified
against the current RNode low-data-rate-optimization behavior. The firmware
repeats the authoritative validation before committing.

All LoRa nodes that should exchange frames directly must use matching frequency,
bandwidth, SF, and coding rate; their power settings may differ. The operator is
responsible for confirming that frequency, bandwidth, duty cycle, antenna, and
EIRP are legal at the place of operation. Product validation is not regulatory
authorization.

Saving persists the complete tuple but never reconfigures the active radio.
Reset or power-cycle after the app reports a pending profile, reconnect, and
confirm that **Running** matches **After restart**. The four power choices are
the exact admitted SX1262 high-power PA rows; there is no separate +21 dBm row.
They describe requested chip output, not measured EIRP or a range promise.
+22 dBm requires at least 3.3 V at the PA supply and an adequate module current
budget; use a sound cable, regulator, battery, or powered hub. See the
[board radio contract](../heltec-vision-master-e290.md#radio-owner-contract) for
the command rows and electrical limits. Profiles other than the default
modulation remain source/host-qualified rather than powered field-qualified.

### Controlled two-board range check

Use the [instrumented E290 range-test
runbook](../development/e290-range-testing.md) before drawing a range conclusion
from a timeout or a farthest single success. It requires a LoRa-only
interface-1 route, paired sender/receiver counter deltas, per-attempt location
and accuracy, controlled antenna-height and power A/B runs, and explicit stop
conditions. Those records distinguish DATA that never reached LoRa dispatch,
RF no-reception, receive/framing loss, and proof-return failure.

## Troubleshooting

### The display remains at `STARTING`

1. Confirm that the board was deliberately reset after flashing and is no
   longer in the ROM loader.
2. Re-run the exact packaging command and `flash-merged`; do not substitute
   default `espflash` geometry.
3. Confirm the image came from `target/$BUILD_TARGET`, its filename matches
   `$IMAGE_NAME`, and it was built with `--features "$FIRMWARE_FEATURE"`.
4. For a deliberately fresh board, confirm that
   `0x610000..0xb30000` was blanked and readback-verified before first boot.

### The USB port changed or disappeared

That is normal across loader/application resets. Re-identify the board by its
USB serial and eFuse MAC; never retain a `/dev/cu.usbmodem*` path as identity.

### The app cannot find the board

Confirm the display reached `READY`, Bluetooth is enabled, the app has
Bluetooth permission, and no other client owns the GATT connection. Continue
with the [pairing guide](pairing.md).

If the retained Bluetooth bond belongs to an unavailable phone, do not erase
or reflash the appliance:

1. Hold the middle GPIO21 button before pressing `RST`.
2. Keep holding GPIO21 continuously for at least three seconds, then release
   it.
3. Let the board finish booting and use the app's **Repair Bluetooth** or
   **Add appliance** flow.
4. On the phone in hand, forget any stale operating-system Bluetooth entry for
   this board before retrying if one exists.

This board-only procedure clears only the dedicated BLE bond store. It does not
change the Reticulum identity or BLE discovery name, and it preserves
application credentials, network configuration, messages, journals, and other
product data. The previous phone is not required. Do not hold `BOOT`; GPIO0
would enter the ROM loader instead of running Bluetooth recovery. The complete
flow, including the separate post-connection GPIO21 hold, is documented under
[Replace an unavailable phone's Bluetooth bond](pairing.md#replace-an-unavailable-phones-bluetooth-bond).

For `wifi-tcp-proof`, also confirm that the source retains the qualified
strict-internal-memory Wi-Fi profile. Wi-Fi/BLE station builds provide 120 KiB
of internal heap and use the pinned driver's default ten persistent static RX
buffers and receive block-ack window of six. The former 72 KiB profile required
a reduced four-buffer, two-window workaround; PSRAM cannot satisfy these
strict-internal controller allocations.
