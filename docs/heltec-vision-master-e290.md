# Heltec Vision Master E290 product target

**Status:** selected and memory-qualified as the primary full-stack prototype
target. Both supplied boards have 16 MiB flash and 8 MiB mapped octal PSRAM.
The independent E290 HT-RA62 radio owner now passes host command-log tests and
generic bare-metal plus ESP32-S3 target checks. Its MAC-gated same-image
semantic HIL has now passed on both physically confirmed `HT-RA62-HF` boards;
see [`e290-semantic-hil.md`](e290-semantic-hil.md). The first permanent
LoRa-only E290 node graph is composed/build-verified and now has bounded powered
boot/erased-credential/ordinary-TX smoke on both boards; full qualification
remains open. See [`e290-node.md`](e290-node.md). The Tracker V2
pair remains the second known-good Reticulum/RNode radio regression fixture.

## Cutover decision

The permanent firmware graph has moved to the Vision Master E290 now that both
boards' flash and PSRAM have been qualified. Portable ordinary-action ownership
and logical-packet dispatch remain board-independent. The E290 uses its own
HT-RA62 owner rather than inheriting the Tracker's external-FEM policy.

This is an early hardware cutover, before storage-task, USB/BLE/Wi-Fi, LXMF,
NomadNet, or display integration. It avoids shaping the full runtime around the
Tracker V2's lack of PSRAM while preserving the already-qualified Tracker pair
for regression testing.

## LoRa-first, multi-interface boundary

LoRa is the first and primary Reticulum interface, and the E290 LoRa path gets
the full concrete implementation and HIL effort first. It is not the node's
global transport abstraction. RNode framing, SX1262 configuration, CAD,
regional frequency policy, RF airtime accounting, and radio deadlines belong
inside one LoRa interface actor.

The permanent node and Rete owner instead operate on stable interface IDs and
interface-neutral packet targets. Each future LoRa, Wi-Fi, BLE, or USB adapter
owns bounded ingress and egress queues and reports its MTU, reachability,
online state, bitrate/cost, and capabilities. Routing selects one or more
interface IDs; only the selected LoRa actor translates a packet into RNode
frames and requests a LoRa airtime reservation. This preserves Reticulum's
ability to route over simultaneous heterogeneous links without requiring
speculative Wi-Fi or BLE implementations before the LoRa vertical slice works.

## Confirmed board wiring

The supplied V0.3.1 schematic and pin map bind the internal peripherals as
follows:

| Function | ESP32-S3 GPIO |
| --- | ---: |
| E-Ink SDI | 1 |
| E-Ink clock | 2 |
| E-Ink chip select | 3 |
| E-Ink D/C | 4 |
| E-Ink reset | 5 |
| E-Ink busy | 6 |
| Battery ADC | 7 |
| HT-RA62 / SX1262 NSS | 8 |
| HT-RA62 / SX1262 SCK | 9 |
| HT-RA62 / SX1262 MOSI | 10 |
| HT-RA62 / SX1262 MISO | 11 |
| HT-RA62 / SX1262 reset | 12 |
| HT-RA62 / SX1262 busy | 13 |
| HT-RA62 / SX1262 DIO1 | 14 |
| Native USB D- | 19 |
| Native USB D+ | 20 |
| Active-low user key | 21 |
| QuickLink SCL | 38 |
| QuickLink SDA | 39 |
| UART TX | 43 |
| UART RX | 44 |

The radio's seven MCU signals match the Tracker V2 numerically, but the RF
topology does not. The HT-RA62 contains its own switch and oscillator control.
Its TXEN, RXEN, DIO2, and DIO3 module pads are not routed to ESP32 GPIOs on the
E290.

The hardware-independent
`reticulum-board-heltec-vision-master-e290` crate now makes those 21 internal
assignments one exhaustive, collision-tested ownership table. It also binds
explicit lab-profile validation to the complete fitted HT-RA62-HF 863--928 MHz
channel range and fixes the pre-initialization radio state at reset low/NSS
high. Native USB ownership covers the one shared GPIO19/GPIO20 connector path,
whether a product profile selects the hard-wired Serial/JTAG controller or the
OTG controller; those controllers cannot be treated as simultaneous owners.
The active-low GPIO21 user key is a separate physical-presence input suitable
for a later pairing policy, not an authentication decision by itself. The
crate has no HAL, driver, executor, compiled flash-capacity constant, or
qualified-memory claim.

## Radio owner contract

The E290 now has a separate
`reticulum-board-heltec-vision-master-e290-radio` crate. It does not depend on
or wrap the Tracker product owner. Board-neutral `lora-phy` state ownership is
factored into `reticulum-radio-lora-phy`; the E290 and Tracker wrappers retain
only their distinct admitted configurations, chip/PA selection and physical
RF-path/reset policy:

- use the stock high-power `Sx1262` variant, without the Tracker external-FEM
  gain/PA override;
- enable the SX1262 DC-DC regulator;
- enable the internal DIO2 RF-switch output;
- configure DIO3 TCXO control for 1.8 V; Heltec's executable SX1262
  initialization uses a 5 ms wake time, while the pinned driver currently uses
  a conservative 10 ms;
- start with receive boost disabled and an opaque NA915 development power
  selection;
- use the shared core's `Option<Active>` cancellation behavior, but fail closed
  by asserting only the SX1262 reset rather than manipulating FEM pins;
- retain the final-DIO1-observation timestamp capture used by timed RNode
  reassembly; and
- expose CAD, physical-frame TX, bounded RX, shutdown, and active-state APIs
  through the board-neutral `SoleRnodeRadio` contract used by the permanent
  E290 LoRa actor.

The opaque first profile is 915 MHz, SF7/BW125/CR4/5, 24-symbol preamble,
explicit header, CRC, normal IQ, private sync word `0x1424`, 248 receive-search
symbols and requested 14 dBm output. Semtech SX1261/2 Data Sheet Rev. 2.2
Table 13-21 realizes that optimal SX1262 row with PA values `02/02/00/01` and
raw `SetTxParams(+22)`; this is not a calibrated antenna-path claim. The shared sole-radio path must be constructed with
`IrqTimestampCapture::new_monotonic_us` in the same clock domain as channel
access; the legacy tick constructor is only for the historical direct receive
surface and is rejected fail-closed by `SoleRnodeRadio` operations.

Seven host tests require DC-DC mode (`0x96, 0x01`), DIO2 RF-switch control
(`0x9d, 0x01`), the pinned 10 ms 1.8 V DIO3 TCXO command
(`0x97, 0x02, 0x00, 0x02, 0x80`), the private sync word and the Rev. 2.2
optimal 14 dBm PA/raw-command pair before exercising RX, CAD and one/two-frame
TX. They reject the
Tracker OCP-register write and cover reset/SPI/cancellation containment,
receive timeout/success and partial second-frame progress. Maximum-power work
must separately verify OCP behavior; initial semantic HIL does not need to
start at the module's 21 +/- 1 dBm rated maximum.

US915 operation requires the `HT-VME290-HF` / `HT-RA62-HF` variant. Its fitted
range is 863--928 MHz and the US915 operating range is 902--928 MHz. The generic
SX1262 claim of 150--960 MHz does not override the module matching-network
variant. Board/SKU marking and a 915 MHz antenna are therefore hard gates before
NA915 transmission.

## Memory and flash qualification

The design floor is **8 MB PSRAM**, not 16 MB:

- the schematic names the MCU `ESP32-S3R8` and shows no discrete PSRAM;
- Espressif defines `ESP32-S3R8` as 8 MB in-package octal-SPI PSRAM; and
- the Heltec datasheet's `16M*PSRAM` field does not identify bits versus bytes
  and conflicts with the fitted MCU designation.

The board-facts crate consequently exposes only an explicitly named 8 MiB
*design floor*. It provides no `FLASH_BYTES`, `HAS_PSRAM`, or qualified-capacity
constant; the connected-board procedure below remains the authority for all
operational memory and partition decisions.

The schematic's W25Q128 part and Heltec datasheet revision support **16 MB
external flash**, while current Heltec Arduino board metadata still selects an
8 MB layout. The connected-board procedure below resolved that conflict:
`espflash` independently detected 16,777,216 bytes on both supplied boards,
and two complete reads of each factory image compared byte-for-byte. The
project may therefore use a 16 MB partition map on these identified boards,
while other E290 units must still pass their own physical-capacity gate.

The first bring-up image must report and verify:

1. eFuse base MAC and chip revision;
2. physical flash-detect capacity captured before flashing, plus the capacity
   encoded in the flashed image header;
3. PSRAM interface mode, mapped start, and mapped byte count;
4. a bounded PSRAM pattern test across the reported range; and
5. internal and external heap baselines/high-water marks.

The qualification image uses only a low-address partition map until that
physical capacity check passes. Its flash-capacity API reflects the boot-image
configuration and is logged as a consistency check, not treated as independent
hardware detection. Any later paired-sector alias test is performed only after
the full detected flash contents have been backed up and hashed.

The dedicated
`reticulum-heltec-vision-master-e290-qualification` binary contains no
executor, radio driver, display driver, Wi-Fi/BLE stack, RNS stack, or storage
writer. It holds SX1262 reset low and NSS high, uses PSRAM autodetection at
conservative 40 MHz flash/RAM settings, and performs address-derived,
bit-inverted, and final-zero volatile passes over every mapped PSRAM word before
registering that memory with `esp-alloc`. Capability-specific allocations must
then prove that internal storage lies outside the mapped PSRAM interval and
external storage lies inside it. A mapped range below 8 MiB, above the 32 MiB
safety ceiling, unaligned, corrupt, or incapable of those allocations fails the
image.

### Powered result for the first pair

Both supplied boards passed the complete qualification on 2026-07-17 UTC:

| Label | USB serial | eFuse base MAC | Flash | Mapped PSRAM |
| --- | --- | --- | ---: | ---: |
| A | `AC:A7:04:E1:3E:88` | `ac:a7:04:e1:3e:88` | 16 MiB | 8 MiB |
| B | `AC:A7:04:E1:3F:88` | `ac:a7:04:e1:3f:88` | 16 MiB | 8 MiB |

Both are ESP32-S3 revision 0.2 with secure boot and flash encryption disabled.
Each complete 16 MiB factory image was captured twice and compared exactly.
Board A's two captures share SHA-256
`eeb7d7903cb81e253ad9fabac96e266f97a48edf7d7204025f06dc616ba8dab2`;
board B's share
`b6a4813a3cf4d29ff18870a29d2e10cb99250666cca440ab4e9c830518e4b274`.

The same qualification ELF, SHA-256
`d912b7a88c82d21badb86424c6017db363a7204ab388521adf86593ff2581627`,
and partition CSV, SHA-256
`e53c24c14c63588612a3090e92103ebf926ca49fe3e74aad6ff12309b88876ec`,
were identity-gated and flashed to both. Each complete powered record reports
the SX1262 still held at reset low/NSS high, a 16 MiB image-header capacity,
octal PSRAM mapped at `0x3c020000..0x3c820000`, full-range address/inverted/
zero pattern passes, correct internal and external allocations, 266,240 bytes
peak allocator use, zero use after release, and final `stage=complete
status=PASS`. The preserved local evidence is under
`artifacts/e290-qualification/20260717T042037Z-first-pair/`.

This binary is release-only in practice: the HAL's PSRAM support requires a
release build. Do not use the workspace `cargo run` runner for it because that
runner still encodes the Tracker's 8 MB flash size. Build the ELF explicitly,
then pass the capacity reported for the exact physical board to `espflash`:

```sh
source ~/export-esp.sh
cargo +esp build --locked --release \
  -p reticulum-heltec-vision-master-e290-qualification \
  --target xtensa-esp32s3-none-elf
```

## First-board qualification procedure

Before connecting, label the boards A and B and photograph each board/module
SKU marking and attached 915 MHz antenna. After connection, record the USB port
delta and run the following read-only command separately for each new port:

```sh
espflash board-info --chip esp32s3 --port "$PORT" \
  --after no-reset --non-interactive --skip-update-check
```

The output must identify an ESP32-S3, a unique eFuse MAC, a detected flash
capacity, `Secure Boot: Disabled`, and `Flash Encryption: Disabled`. This is a
hard pre-write fuse gate: a readable MAC or capacity does not establish that a
plaintext development image is safe to install. `espflash` 4.5 does not print
the raw JEDEC manufacturer/device ID, and an unknown flash ID can produce a
warning followed by a 4 MB fallback. Preserve complete stdout and stderr and
reject any detection warning or fallback; the printed capacity is the evidence
this procedure actually uses.
Stop if the board/module is not the HF variant, the antenna is absent, or the
two boards report inconsistent capacities; do not infer capacity from the
schematic or from the later firmware log.

Before the first write, read the complete reported flash twice and compare its
hashes. `$FLASH_BYTES` is the exact byte count from `board-info`, not a fixed
project default. `$EXPECTED_MAC` and `$EXPECTED_USB_SERIAL` are immutable for
one labeled board. Preserve that initial association in the evidence directory;
the USB serial normally resembles the base MAC, but the runbook does not assume
they are equal. Before **each** `read-flash` or `flash`, the project-owned helper
rediscovers the port by that USB identity, reruns `board-info`, preserves its
complete streams, and rejects a missing/different MAC, chip, capacity, security
state, or detection result. Never reuse a mutable `$PORT` captured for the
other board or before a reset/re-enumeration:

```sh
python3.13 interop/python/e290_qualification_host.py read-flash \
  --usb-serial "$EXPECTED_USB_SERIAL" --expected-mac "$EXPECTED_MAC" \
  --expected-flash-bytes "$FLASH_BYTES" \
  --evidence-prefix "$RUN/hardware/board-a-before-backup-1" \
  --output "$BACKUP_1"
python3.13 interop/python/e290_qualification_host.py read-flash \
  --usb-serial "$EXPECTED_USB_SERIAL" --expected-mac "$EXPECTED_MAC" \
  --expected-flash-bytes "$FLASH_BYTES" \
  --evidence-prefix "$RUN/hardware/board-a-before-backup-2" \
  --output "$BACKUP_2"
shasum -a 256 "$BACKUP_1" "$BACKUP_2"
cmp "$BACKUP_1" "$BACKUP_2"
```

`e290_qualification_host.py` consumes the complete macOS IORegistry stream,
associates each serial callout with its nearest USB-device ancestor at any hub
depth, and requires exactly one matching Espressif native-USB callout. It then
requires the
expected chip, MAC, physical capacity, disabled secure boot, disabled flash
encryption, and absence of flash-detection warnings. It invokes `board-info`
with `--after no-reset`, verifies that the mapping is unchanged afterward, and
owns the allowlisted full-capacity `read-flash`, qualification-image `flash`,
exact `read-region`, verified `erase-region` erase-equivalent blanking,
hash-bound merged-image
write/readback, or read-only post-run image-range verification immediately
afterward with `--before no-reset`. The region actions additionally require the
exact uppercase USB serial and a 16 MiB expected capacity. The `erase-region`
subcommand deliberately does not use espflash 4.5's native `erase-region`,
which emits no action DeviceInfo and cannot bind the destructive target. It
instead passes an exact-length retained all-`0xff` input to identity-reporting
`write-bin`; sector-aligned operands make its erase-before-write span exactly
the requested range. Espflash may checksum-skip the physical write when the
range is already blank, which is acceptable because the invariant is logical
all-`0xff` state. Success still requires action chip/flash/MAC evidence and an
exact-length readback whose entire contents are `0xff`. It never returns a
mutable port string to an
intervening shell command. Each invocation needs a unique evidence prefix; the
helper uses exclusive file creation and refuses an existing output or evidence
path, including dotted-prefix aliases. RF-capable merged images additionally
require an exact physical `HT-RA62-HF` module acknowledgement and matching ESP
image header capacity before any hardware access. Every flash dump is created
as an owner-only `0600` regular file before `espflash` starts and is written
through a retained inherited descriptor, so replacing its visible path with a
symlink cannot redirect bytes into another file. A verified dump becomes
owner-read-only `0400`; a failed or unverified dump remains private `0600`.
Merged-image and qualification writes likewise consume retained, hash-bound
input descriptors. Their verified records require the write action's own
chip/flash/MAC observation, unchanged post-action USB mapping, and a
loader-preserving post-write `board-info`; merged-image readback and read-only
verification separately validate the read action identity and mapping.

This stock-CLI all-`0xff` path closes evidence attribution, including the
erase-B/swap-back-to-A case, because `write-bin` reports DeviceInfo on the same
open connection used for the write. It cannot reject a board swapped after
qualification *before* attempting the write, since stock `write-bin` has no
expected-MAC gate. A future project-owned wrapper around the pinned espflash
library can strengthen this to fail-before-destruction by checking DeviceInfo
and erasing on one retained `Flasher`; until then, any wrong action MAC is a
post-write failure with no verified record.

Only after both boards have stable backups should the image be flashed with the
project-owned low-address table. The helper derives the only accepted
`--flash-size` token mechanically from the just-verified physical byte count;
there is no independent `$ESPFLASH_SIZE` operator input. It retains the target
in the loader after writing so the later counted capture owns the normal-boot
reset. Before touching the board it makes read-only evidence copies of the ELF
and partition table, flashes those exact copies, and rechecks their hashes after
the write. A post-write evidence failure is reported distinctly and never
creates the action's verified JSON:

```sh
python3.13 interop/python/e290_qualification_host.py flash-qualification \
  --usb-serial "$EXPECTED_USB_SERIAL" --expected-mac "$EXPECTED_MAC" \
  --expected-flash-bytes "$FLASH_BYTES" \
  --evidence-prefix "$RUN/hardware/board-a-before-flash" \
  --partition-table partitions/heltec-vision-master-e290-qualification.csv \
  --elf target/xtensa-esp32s3-none-elf/release/reticulum-heltec-vision-master-e290-qualification
```

Capture one complete boot through `stage=complete status=PASS`, including the
firmware's independent `stage=security status=PASS` fuse observation, MAC,
revision, eFuse raw observations, image-header flash capacity, mapped PSRAM
range, full-range pattern result, and allocator ranges. Accept that PASS only
when `configured_capacity_bytes` equals the same helper-verified physical
capacity recorded in the flash action JSON. The E290 radio crate begins only
after both boards produce independently passing records.

PSRAM is capacity, not a blanket replacement for internal RAM. ESP32-S3 atomic
operations and some peripheral/DMA paths cannot safely use arbitrary external
memory. Radio buffers, synchronization primitives, atomics, interrupt-visible
state, and flash-operation-critical state remain explicitly internal. Large
protocol/application payloads and UI caches are candidates for audited PSRAM
placement.

## E-Ink integration boundary

The fitted DEPG0290BNS800F6 panel is a 128 by 296 monochrome display with full
and partial refresh support. Display integration is deliberately later than
the permanent radio/node/storage graph.

One low-priority display actor should own GPIO1--6, coalesce model updates, and
render from an application snapshot. Its multi-second full refresh must never
run inside the sole radio owner, protocol owner, storage actor, or a deadline
critical callback. The display can continue showing its last image without
power, so routine status changes should prefer partial/coalesced refresh rather
than continuous redraw.

## Bring-up and migration sequence

1. **Complete:** bind both USB identities to unique eFuse MACs, verify revision
   and fuse state, and preserve two matching full factory images per board.
   Physical HF/LF marking is not machine-readable in the captured evidence and
   remains an explicit NA915 RF gate alongside the attached 915 MHz antennas.
2. **Complete:** flash the RF-inert probe and qualify 16 MiB flash plus 8 MiB
   mapped octal PSRAM on both boards.
3. **Complete:** add the separate E290 radio crate, shared
   board-neutral `lora-phy` owner and host command-log tests for the exact
   SX1262 initialization. The isolated semantic HIL now also qualifies its
   functional near-field CAD/RX/TX behavior.
4. **Complete in software:** add the minimal interface registry/router for an
   initial E290 LoRa actor without moving RNode/CAD/airtime policy into
   node-core. `NodeInterfaceSupervisor` now owns that router plus the DATA and
   ordinary coordinators and per-actor permit services.
5. **Powered PASS:** run one MAC-gated image on both boards
   at the fixed NA915 development profile. Its clear CAD before each of four
   transmissions, bounded RX/TX, signed ANNOUNCEs, encrypted DATA, and delivery
   proof provided both the radio smoke evidence and semantic checks. Both
   immediate and post-capture image-range readbacks matched.
6. **Implemented, build-verified, and bounded end-to-end powered-verified:** make
   E290 the primary permanent-node graph with one transport-neutral node task
   and one concrete LoRa task while retaining Tracker HIL as a regression
   target. The permanent image now passes boot, credential, journal/LoRa/
   interface, authenticated durable submission, controlled peer RX/DATA/proof,
   and post-re-enumeration terminal-status checks. Fault, cut, high-water,
   application-inbox, and full powered product-graph qualification remain.
7. Integrate the powered storage actor and authenticated USB device API first,
   then add an optional second Reticulum interface (with a distinct USB stream
   actor the leading candidate) to prove heterogeneous routing. Wi-Fi, BLE,
   E-Ink, GNSS/location, and richer clients follow as independent
   feature/profile slices.

## Sources

Local supplied sources (kept under the ignored `reference/` research tree):

- `HT-VME290-Datasheet.pdf`
- `HT-VME290_Schematic_Diagram_V0.3.1.pdf`
- `HT-VME290 Pin map.png`
- `DEPG0290BNS800F6_V2.1.pdf`

Primary online references:

- [Semtech SX1262 product page and current datasheet](https://www.semtech.com/products/wireless-rf/lora-connect/sx1262)
- [Semtech SX1261/2 Data Sheet Rev. 2.2 mirror](https://resource.heltec.cn/download/WiFi_LoRa_32_V4/datasheet/SX1261_2%20V2-2.pdf)
- [Heltec Vision Master E290 documentation](https://docs.heltec.org/en/node/esp32/ht_vme290/index.html)
- [Heltec HT-RA62 documentation](https://docs.heltec.org/en/node/ht-ra62/index.html)
- [Heltec HT-RA62 Rev. 1.1 datasheet](https://resource.heltec.cn/download/HT-RA62/HT-RA62%28Rev1.1%29.pdf)
- [Heltec HT-RA62 schematic](https://resource.heltec.cn/download/HT-RA62/HT-RA62_Schematic_diagram.pdf)
- [Heltec E290 Arduino board metadata](https://raw.githubusercontent.com/Heltec-Aaron-Lee/WiFi_Kit_series/master/boards.txt)
- [Espressif ESP32-S3 datasheet](https://www.espressif.com/sites/default/files/documentation/esp32-s3_datasheet_en.pdf)
- [Espressif ESP32-S3 package marking](https://docs.espressif.com/projects/esp-packaging/en/latest/esp32s3/01-marking/index_chip.html)
