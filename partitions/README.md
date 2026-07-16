# Partition tables and storage-HIL runbook

`heltec-tracker-v2-storage-hil.csv` is the explicit 8 MiB development layout
for the RF-inert physical-journal HIL:

| Partition | Range | Purpose |
| --- | --- | --- |
| `nvs` | `0x009000..0x00f000` | development NVS reservation |
| `phy_init` | `0x00f000..0x010000` | PHY-init reservation |
| `factory` | `0x010000..0x670000` | single HIL application slot |
| `retlog` | `0x670000..0x770000` | writable plaintext 1 MiB journal under test |
| unpartitioned | `0x770000..0x7f0000` | reserved for later product-layout work |
| `coredump` | `0x7f0000..0x800000` | retained 64 KiB crash reservation |

This table is not the final OTA/product layout. It deliberately retains one
large factory application rather than claiming that the final image, A/B OTA,
full LXMF store, SPA, and other data all fit this no-PSRAM board. It must always
be supplied explicitly to `espflash`; it is not an ambient default.

The HIL firmware independently requires an MD5-valid table, exactly 8 MiB of
flash, disabled flash encryption, exactly one writable/plaintext `retlog` entry
with the range above, and no other partition overlapping that range. It holds
the SX1262 and front-end controls inactive before it logs or accesses flash and
has no radio/LoRa/RNS dependency. See
[`docs/storage-journal.md`](../docs/storage-journal.md) for the format and
expected test sequence.

## Guarded E9:44 runbook

The selected storage-test board is the device whose full MAC is
`44:1B:F6:F8:E9:44`. The other attached board, `44:1B:F6:F8:E0:40`, is the
external derived-RNode peer and must not be erased or flashed by this runbook.
Serial device names can change after reset, so a cached `/dev/cu.*` path is not
board identity.

The commands below are a reviewable operator runbook, not evidence that the HIL
has already passed. Run them from the repository root. Replace the candidate
port only after `board-info` prints the E9:44 MAC; stop on any mismatch, unknown
security state, unexpected flash size, parse error, verification failure, or
RF-interlock failure.

### 1. Identify and preserve the board

Create a new ignored evidence directory, inspect a candidate port, and promote
it to `PORT` only after checking the printed MAC and 8 MiB flash size:

```sh
RUN="artifacts/storage-hil/$(date -u +%Y%m%dT%H%M%SZ)-e944"
mkdir -p "$RUN"

CANDIDATE=/dev/cu.usbmodem101
espflash board-info --port "$CANDIDATE" --non-interactive 2>&1 \
  | tee "$RUN/board-info.txt"

# Set this only after the output above says 44:1B:F6:F8:E9:44.
PORT="$CANDIDATE"
```

The path shown is the current lab hint, not an authorization to skip the MAC
check. Confirm the board is the known development unit with flash encryption
disabled; the HIL repeats that encryption check before constructing its raw
partition view.

The initial full-board backups remain the recovery baseline. Also preserve an
immediate pre-run full image and hashes before changing this board:

```sh
espflash read-flash --port "$PORT" --chip esp32s3 --after no-reset \
  0 0x800000 "$RUN/flash-before.bin"
espflash read-flash --port "$PORT" --chip esp32s3 --after no-reset \
  0x670000 0x100000 "$RUN/retlog-before.bin"
shasum -a 256 "$RUN/flash-before.bin" "$RUN/retlog-before.bin" \
  > "$RUN/hashes-before.sha256"
```

### 2. Build one explicit image

Build the release ELF with the installed ESP toolchain and generate one merged,
unpadded image containing this exact partition table:

```sh
source ~/export-esp.sh
cargo +esp build --locked --release \
  -p reticulum-heltec-tracker-v2-storage-hil \
  --target xtensa-esp32s3-none-elf

ELF=target/xtensa-esp32s3-none-elf/release/reticulum-heltec-tracker-v2-storage-hil
espflash save-image \
  --chip esp32s3 \
  --merge --skip-padding \
  --flash-mode dio --flash-freq 80mhz --flash-size 8mb --xtal-freq 40mhz \
  --partition-table partitions/heltec-tracker-v2-storage-hil.csv \
  --target-app-partition factory \
  "$ELF" "$RUN/storage-hil.bin"
shasum -a 256 "$ELF" "$RUN/storage-hil.bin" \
  > "$RUN/hashes-image.sha256"
```

Do not substitute a default or previously generated partition table. Preserve
the ELF because it is also the symbol source for serial backtraces.

### 3. Erase only `retlog`, then flash without an intermediate boot

The journal refuses to format a programmed unknown partition. The old contents
at `retlog` must therefore be erased externally after backup and before the HIL
is allowed to boot. Keep the board in the loader between erase, verification,
write, and exact image readback:

```sh
espflash erase-region --port "$PORT" --chip esp32s3 --after no-reset \
  0x670000 0x100000
espflash read-flash --port "$PORT" --chip esp32s3 --after no-reset \
  0x670000 0x100000 "$RUN/retlog-erased.bin"
test "$(LC_ALL=C tr -d '\377' < "$RUN/retlog-erased.bin" | wc -c | tr -d ' ')" = 0

espflash write-bin --port "$PORT" --chip esp32s3 --after no-reset \
  0 "$RUN/storage-hil.bin"
IMAGE_BYTES="$(wc -c < "$RUN/storage-hil.bin" | tr -d ' ')"
espflash read-flash --port "$PORT" --chip esp32s3 --after no-reset \
  0 "$IMAGE_BYTES" "$RUN/storage-hil-readback.bin"
cmp "$RUN/storage-hil.bin" "$RUN/storage-hil-readback.bin"
```

These are the only intended destructive commands: erase the exact 1 MiB E9:44
`retlog`, then write the merged image to that same board. The raw merged write
replaces its bootloader, partition table, and factory application and can erase
intervening NVS/PHY data sectors; that is why the full pre-run image is required.
Do not use whole-chip erase and do not run any command against E0:40.

### 4. Capture the two-boot result

Start the monitor with the just-built ELF. The HIL should format and seed
generation 1, reject a conflicting retry without a write, compact to generation
2, then software-reset after 250 ms. Native USB may re-enumerate at that reset;
if the monitor disconnects, rediscover E9:44 and restart only the monitor.

```sh
espflash monitor --port "$PORT" --elf "$ELF" 2>&1 \
  | tee -a "$RUN/serial.log"
```

Do not accept the run unless the preserved log contains all of these results
without an intervening FAIL/panic:

- RF interlock PASS before flash work;
- preflight PASS with 8 MiB, flash encryption false, and the exact writable
  plaintext `retlog` range;
- format/mount and all five deterministic seed records;
- semantic replay to revision 4 `Delivered`;
- exact-retry PASS with unchanged write/erase counters;
- logical-conflict PASS with unchanged write/erase counters;
- compaction PASS selecting generation 2;
- after reset, final replay PASS for generation 2; and
- at least one 30-second RF-inert PASS heartbeat.

Exit the monitor, rediscover E9:44 if needed, and preserve the resulting
partition for independent inspection:

```sh
espflash read-flash --port "$PORT" --chip esp32s3 \
  0x670000 0x100000 "$RUN/retlog-after.bin"
shasum -a 256 "$RUN/retlog-erased.bin" "$RUN/storage-hil-readback.bin" \
  "$RUN/retlog-after.bin" > "$RUN/hashes-after.sha256"
```

The first clean run validates real raw-flash format/append/replay/compaction and
a software-reset replay. It does not by itself prove controlled power-cut
recovery, flash endurance, production encryption, the future async actor, or
any RF behavior. Add controlled cuts and longer cycling only as separately
recorded HIL scenarios; keep every such image radio-free.
