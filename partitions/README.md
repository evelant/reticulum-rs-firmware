# Partition tables and storage-HIL runbook

`heltec-vision-master-e290-node.csv` is the first 16 MiB permanent-node layout:

| Partition | Range | Size | Product state |
| --- | --- | ---: | --- |
| `nvs` | `0x009000..0x00f000` | 24 KiB | ESP/NVS reserve |
| `phy_init` | `0x00f000..0x010000` | 4 KiB | ESP PHY reserve |
| `factory` | `0x010000..0x610000` | 6 MiB | Permanent node image |
| `node_identity` | `0x610000..0x612000` | 8 KiB | Wired immutable identity mirrors |
| `announce_clock` | `0x612000..0x614000` | 8 KiB | Wired boot-epoch mirrors |
| `device_config` | `0x614000..0x630000` | 112 KiB | Reserved, not wired |
| `node_journal` | `0x630000..0x730000` | 1 MiB | Checked boot provision/mount/recovery probe; live owner deferred |
| `message_store` | `0x730000..0x930000` | 2 MiB | Reserved, not wired |
| unpartitioned | `0x930000..0x1000000` | 6.8125 MiB | OTA/layout decision |

The journal and message-store offsets are unchanged. `device_config` is now
the 112 KiB standard-NVS range after the two new durability partitions. The
journal and message store use ESP-IDF's standard `data,undefined` subtype until
their formats are integrated. `node_identity` and `announce_clock` also use
that standard subtype, but their exact application-owned formats are now
implemented; no unsupported numeric subtype is used to imply ownership.

`node_identity` contains the same plaintext 64-byte Reticulum private material
in two commit-last, SHA-256-protected 4 KiB mirrors. Its complete preflight is
read-only: blank/recognized-torn media is vacant, matching valid media is
committed, and unknown data without authority, sole committed corruption, or
conflicting valid keys fails closed. A normal committed reload performs zero
writes and zero erases. Blank first provisioning uses three program calls per
mirror and no erase; repair mutates only the peer and never erases the sole
valid copy.

`announce_clock` is two 4 KiB append-log sectors. Before identity provisioning
or repair, the product reserves the next 20-bit boot epoch in both sectors. A
20-bit per-boot ordinal supplies the lower half of the 40-bit local announce
time. Existing identity plus missing clock high-water fails closed without
mutation; only a mutation-free vacant-identity preflight permits first clock
provisioning. Normal boot appends one commit-last record per sector (four
program calls total and normally no erase). Full or repairable sectors rotate
one at a time while the other preserves high-water. Power loss can consume or
skip an epoch, but retries scan committed state and never reuse one.

While identity remains vacant, the product can establish or resume only the
canonical empty A1 journal trajectory before committing identity. Provisioning
never erases; after identity is committed, only strict mount is allowed. Boot
drives the submission runtime through complete conservative recovery and then
returns the temporary journal borrow before protocol construction. Resident
journal mutation, device configuration, and message storage remain deferred.
The product does not start protocol service unless clock reservation, journal
mount/recovery, and redundant identity coverage all succeed. LoRa is the
primary first transport slice;
USB/BLE/Wi-Fi client service and additional Reticulum transports remain
deferred.

Raw full-flash dumps now contain a private key after provisioning. Set
`umask 077` before creating them and retain them only with restricted
permissions on encrypted storage. After the required backup and before the
first product boot, perform either a full-chip erase or an exact, readback-
verified erase of `0x610000..0x730000`. The unpadded merged image does not
initialize those data partitions. Subsequent upgrades must preserve every
product store; do not repeat the provisioning erase. The exact guarded sequence is in
[`docs/e290-node.md`](../docs/e290-node.md). This table must not be used through
the workspace's 8 MiB runner. Both connected modules are confirmed
`HT-RA62-HF`; neither this permanent-node layout nor its host/build checks
qualifies the unflashed permanent image on powered hardware.

`heltec-vision-master-e290-semantic-hil.csv` is the explicit hazardous RF HIL
layout for the qualified 16 MiB E290 pair. It reserves NVS and PHY-init ranges
and a 4 MiB low-address factory image, defines no writable application-data
partition, and intentionally leaves the rest unassigned. It is neither a
product/OTA layout nor general authorization to transmit. The modules were
confirmed `HT-RA62-HF` before the isolated semantic HIL was flashed; see
[`docs/e290-semantic-hil.md`](../docs/e290-semantic-hil.md).

`heltec-vision-master-e290-qualification.csv` is a deliberately
capacity-agnostic, low-address first-flash layout for the E290 identity/PSRAM
probe. It reserves NVS and PHY-init ranges and gives the one-shot factory image
`0x10000..0x110000`; it defines no writable test data or high-address
partition. The E290 host qualification helper derives `--flash-size` from the
exact physical capacity reported by its immediately preceding `espflash
board-info` flash-detect result, because that value is encoded into the boot
image header and observed by the firmware. This table is neither the E290
product layout nor evidence of 16 MB flash. See
[`docs/heltec-vision-master-e290.md`](../docs/heltec-vision-master-e290.md) for
the backup and qualification sequence.

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

## Latest qualifying run

The first clean powered qualification passed on board
`44:1B:F6:F8:E9:44` from source
`7b47113aeec6c7f0549cd5b264eceacef830fb4c`. The complete evidence directory
is
`artifacts/storage-hil/20260716T211318Z-e944-7b47113`.

The strict serial verifier accepted one continuous counted capture with two
boots (`CoreUsbUart` followed by the firmware-issued `CoreSw` reset): A1 format,
five appends, semantic replay, mutation-free exact retry and conflict, B2
compaction, zero-write/zero-erase B2 replay, and two final RF-inert heartbeats.
The independent raw-dump verifier mounted the preserved partition through the
production journal implementation and confirmed bank B generation 2, five
committed records in five consumed slots, one accepted submission at revision
4 `Delivered`, no pending compaction, an erased retired-A manifest, and an
erased unused B tail.

This is qualification of the isolated journal clean path and software-reset
replay only. It is not controlled power-cut, endurance/soak, at-rest encryption,
async storage-actor, device-API, product-runtime, or RF evidence.

## Guarded E9:44 runbook

The selected storage-test board is the device whose full MAC is
`44:1B:F6:F8:E9:44`. The other attached board, `44:1B:F6:F8:E0:40`, is the
external derived-RNode peer and must not be erased or flashed by this runbook.
Serial device names can change after reset, so a cached `/dev/cu.*` path is not
board identity.

The commands below are a reviewable operator runbook, not evidence that the HIL
has already passed. Run every block from the repository root in the same Bash or
Zsh process. The first block enables `errexit`, `nounset`, and `pipefail`; do not
disable them or continue in a new shell. Stop on any identity mismatch, unknown
security state, unexpected flash size, parse error, verification failure,
capture discontinuity, or RF-interlock failure.

### 1. Identify and preserve the board

Create a new ignored evidence directory and map E9:44's USB serial descriptor to
its callout path without opening either attached board. The mapper deliberately
consumes the complete IORegistry stream: exiting `awk` early would send
`SIGPIPE` to `ioreg` and fail a shell running with `pipefail`.

```sh
set -euo pipefail

RUN="artifacts/storage-hil/$(date -u +%Y%m%dT%H%M%SZ)-e944"
test ! -e "$RUN"
mkdir -p "$RUN/hardware" "$RUN/provenance" "$RUN/flash"

TRACKER_USB_SERIAL=44:1B:F6:F8:E9:44
map_tracker_port() {
  ioreg -r -c IOUSBHostDevice -l -w0 |
  awk -v target="$TRACKER_USB_SERIAL" '
    /"kUSBSerialNumberString" = / {
      wanted = index($0, "\"" target "\"") != 0
    }
    wanted && /"IOCalloutDevice" = / && !emitted {
      line = $0
      sub(/^.*"IOCalloutDevice" = "/, "", line)
      sub(/".*$/, "", line)
      print line
      emitted = 1
    }
  '
}
record_tracker_port() {
  destination="$1"
  PORT="$(map_tracker_port)"
  test -n "$PORT"
  test -c "$PORT"
  printf 'usb_serial=%s port=%s\n' "$TRACKER_USB_SERIAL" "$PORT" \
    > "$destination"
}

source ~/export-esp.sh
git rev-parse HEAD > "$RUN/provenance/git-head.txt"
git status --porcelain=v2 > "$RUN/provenance/git-status.txt"
git diff --binary HEAD > "$RUN/provenance/worktree.patch"
git ls-files --others --exclude-standard \
  > "$RUN/provenance/untracked-files.txt"
test ! -s "$RUN/provenance/untracked-files.txt"
git archive --format=tar HEAD > "$RUN/provenance/source-head.tar"
cp Cargo.lock "$RUN/provenance/Cargo.lock"
cp partitions/heltec-tracker-v2-storage-hil.csv \
  "$RUN/provenance/partition-table.csv"
cp interop/python/esp32s3_usb_serial_capture.py \
  "$RUN/provenance/esp32s3_usb_serial_capture.py"
cp interop/python/verify_storage_hil_log.py \
  "$RUN/provenance/verify_storage_hil_log.py"
{
  rustc +esp --version
  cargo +esp --version
  xtensa-esp32s3-elf-gcc --version
  espflash --version
  python3.13 --version
} > "$RUN/provenance/tool-versions.txt"

record_tracker_port "$RUN/hardware/e944-port-before-board-info.txt"
espflash board-info \
  --port "$PORT" --chip esp32s3 \
  --after no-reset --non-interactive --skip-update-check 2>&1 \
  | tee "$RUN/hardware/e944-board-info.txt"
rg -qi '^MAC address:[[:space:]]+44:1b:f6:f8:e9:44$' \
  "$RUN/hardware/e944-board-info.txt"
rg -q '^Flash size:[[:space:]]+8MB$' \
  "$RUN/hardware/e944-board-info.txt"
rg -q '^Flash Encryption: Disabled$' \
  "$RUN/hardware/e944-board-info.txt"
```

`board-info` is explicitly left in `no-reset` state. Omitting that option boots
the previously installed application before the storage evidence begins. The
firmware repeats the MAC, encryption, capacity, and partition checks before
constructing its raw partition view, but the host identity check remains
mandatory.

The initial full-board backups remain the recovery baseline. Also preserve an
immediate pre-run full image and an independently read `retlog`. Revalidate the
passive E9:44 mapping before each connection and prove the separate partition
read equals the corresponding full-image slice:

```sh
record_tracker_port "$RUN/hardware/e944-port-before-full-backup.txt"
espflash read-flash \
  --port "$PORT" --chip esp32s3 \
  --before default-reset --after no-reset \
  --non-interactive --skip-update-check \
  0 0x800000 "$RUN/flash/flash-before.bin" 2>&1 \
  | tee "$RUN/flash/flash-before.log"
test "$(wc -c < "$RUN/flash/flash-before.bin" | tr -d ' ')" = 8388608

record_tracker_port "$RUN/hardware/e944-port-before-retlog-backup.txt"
espflash read-flash \
  --port "$PORT" --chip esp32s3 \
  --before default-reset --after no-reset \
  --non-interactive --skip-update-check \
  0x670000 0x100000 "$RUN/flash/retlog-before.bin" 2>&1 \
  | tee "$RUN/flash/retlog-before.log"
test "$(wc -c < "$RUN/flash/retlog-before.bin" | tr -d ' ')" = 1048576
dd if="$RUN/flash/flash-before.bin" \
  of="$RUN/flash/retlog-before-from-full.bin" \
  bs=4096 skip=1648 count=256 \
  2> "$RUN/flash/retlog-before-from-full.log"
cmp "$RUN/flash/retlog-before.bin" \
  "$RUN/flash/retlog-before-from-full.bin"
shasum -a 256 \
  "$RUN/flash/flash-before.bin" \
  "$RUN/flash/retlog-before.bin" \
  "$RUN/flash/retlog-before-from-full.bin" \
  > "$RUN/flash/hashes-before.sha256"
```

### 2. Build one explicit image

Build the release ELF with the installed ESP toolchain and generate one merged,
unpadded image containing this exact partition table:

```sh
source ~/export-esp.sh
cargo +esp build --locked --release \
  -p reticulum-heltec-tracker-v2-storage-hil \
  --target xtensa-esp32s3-none-elf 2>&1 \
  | tee "$RUN/provenance/build.log"

cargo build --locked --release \
  -p reticulum-storage-hil-verify 2>&1 \
  | tee "$RUN/provenance/storage-hil-verify-build.log"

BUILT_ELF=target/xtensa-esp32s3-none-elf/release/reticulum-heltec-tracker-v2-storage-hil
cp "$BUILT_ELF" "$RUN/provenance/firmware.elf"
ELF="$RUN/provenance/firmware.elf"
BUILT_DUMP_VERIFIER=target/release/reticulum-storage-hil-verify
cp "$BUILT_DUMP_VERIFIER" \
  "$RUN/provenance/reticulum-storage-hil-verify"
DUMP_VERIFIER="$RUN/provenance/reticulum-storage-hil-verify"
espflash save-image \
  --chip esp32s3 \
  --merge --skip-padding \
  --flash-mode dio --flash-freq 80mhz --flash-size 8mb --xtal-freq 40mhz \
  --partition-table "$RUN/provenance/partition-table.csv" \
  --target-app-partition factory \
  "$ELF" "$RUN/flash/storage-hil.bin" 2>&1 \
  | tee "$RUN/provenance/save-image.log"
wc -c < "$RUN/flash/storage-hil.bin" | tr -d ' ' \
  > "$RUN/flash/storage-hil-bytes.txt"
shasum -a 256 \
  "$RUN/provenance/source-head.tar" \
  "$RUN/provenance/worktree.patch" \
  "$RUN/provenance/Cargo.lock" \
  "$RUN/provenance/partition-table.csv" \
  "$RUN/provenance/esp32s3_usb_serial_capture.py" \
  "$RUN/provenance/verify_storage_hil_log.py" \
  "$DUMP_VERIFIER" "$ELF" "$RUN/flash/storage-hil.bin" \
  > "$RUN/provenance/hashes-image-and-source.sha256"
```

Do not substitute a default or previously generated partition table. The copied
ELF is the image input and retained symbol source; the HEAD archive plus the
recorded worktree patch identifies the actual source used for the build.

### 3. Erase only `retlog`, then flash without an intermediate boot

The journal refuses to format a programmed unknown partition. The old contents
at `retlog` must therefore be erased externally after backup and before the HIL
is allowed to boot. Keep the board in the loader between erase, verification,
write, and exact image readback:

```sh
record_tracker_port "$RUN/hardware/e944-port-before-retlog-erase.txt"
espflash erase-region \
  --port "$PORT" --chip esp32s3 \
  --before default-reset --after no-reset \
  --non-interactive --skip-update-check \
  0x670000 0x100000 2>&1 \
  | tee "$RUN/flash/retlog-erase.log"

record_tracker_port "$RUN/hardware/e944-port-before-erased-readback.txt"
espflash read-flash \
  --port "$PORT" --chip esp32s3 \
  --before default-reset --after no-reset \
  --non-interactive --skip-update-check \
  0x670000 0x100000 "$RUN/flash/retlog-erased.bin" 2>&1 \
  | tee "$RUN/flash/retlog-erased-readback.log"
test "$(wc -c < "$RUN/flash/retlog-erased.bin" | tr -d ' ')" = 1048576
test "$(LC_ALL=C tr -d '\377' < "$RUN/flash/retlog-erased.bin" \
  | wc -c | tr -d ' ')" = 0

record_tracker_port "$RUN/hardware/e944-port-before-image-write.txt"
espflash write-bin \
  --port "$PORT" --chip esp32s3 \
  --before default-reset --after no-reset \
  --non-interactive --skip-update-check \
  0 "$RUN/flash/storage-hil.bin" 2>&1 \
  | tee "$RUN/flash/image-write.log"

IMAGE_BYTES="$(cat "$RUN/flash/storage-hil-bytes.txt")"
record_tracker_port "$RUN/hardware/e944-port-before-image-readback.txt"
espflash read-flash \
  --port "$PORT" --chip esp32s3 \
  --before default-reset --after no-reset \
  --non-interactive --skip-update-check \
  0 "$IMAGE_BYTES" "$RUN/flash/storage-hil-readback.bin" 2>&1 \
  | tee "$RUN/flash/image-readback.log"
test "$(wc -c < "$RUN/flash/storage-hil-readback.bin" | tr -d ' ')" \
  = "$IMAGE_BYTES"
cmp "$RUN/flash/storage-hil.bin" \
  "$RUN/flash/storage-hil-readback.bin"
shasum -a 256 \
  "$RUN/flash/retlog-erased.bin" \
  "$RUN/flash/storage-hil.bin" \
  "$RUN/flash/storage-hil-readback.bin" \
  > "$RUN/flash/hashes-flashed.sha256"
```

These are the only intended destructive commands: erase the exact 1 MiB E9:44
`retlog`, then write the merged image to that same board. The raw merged write
replaces its bootloader, partition table, and factory application and can erase
intervening NVS/PHY data sectors; that is why the full pre-run image is required.
Do not use whole-chip erase and do not run any command against E0:40.

### 4. Capture the counted two-boot result

Do not use `espflash monitor`. In espflash 4.5.0 even `--no-reset` connects
through the ROM loader before monitoring, while interactive mode does not start
the application until an unrecorded Ctrl-R. An external `probe-rs reset` is not
valid after the flash operations leave the target in the ROM loader: on the
live board it produced `boot:0x0 (DOWNLOAD)` instead of a normal application
boot. The project-owned recorder must instead own the counted reset.

Opening the ESP32-S3 native-USB TTY can itself reset the target before DTR and
RTS can be cleared. This image therefore emits a five-second
`stage=capture-guard` and performs no `FlashStorage`/`retlog` access or flash
mutation during that interval. Instruction fetches remain ordinary flash
reads, so the precise evidence fields are `retlog_access=false` and
`flash_mutation=false`. The recorder opens and exclusively retains the same
serial descriptor, drains one second of attachment evidence, durably records
its byte offset, and performs espflash's normal-boot USB-Serial/JTAG DTR/RTS
sequence on that already-open descriptor. It makes no serial data writes. Only
bytes at or after
`counted-reset-serial-offset.txt` belong to the qualifying two-boot attempt;
earlier bytes are attachment evidence, not a storage result.

```sh
capture="$RUN/capture"
test ! -e "$capture"
mkdir "$capture"
RECORDER="$RUN/provenance/esp32s3_usb_serial_capture.py"
shasum -a 256 "$RECORDER" > "$capture/serial-recorder.sha256"

record_tracker_port "$capture/e944-port-before-open.txt"
if python3.13 "$RECORDER" \
  --port "$PORT" \
  --hard-reset-after-open \
  --pre-reset-drain-seconds 1 \
  --duration-seconds 90 \
  > "$capture/serial.log" \
  2> "$capture/serial-recorder.log"; then
  recorder_status=0
else
  recorder_status=$?
fi
printf '%s\n' "$recorder_status" \
  > "$capture/serial-recorder.exit-status.txt"
test "$recorder_status" -eq 0

OPENED_COUNT="$(awk -v port="$PORT" '
  index($0, "opened=" port " ") &&
    /receive_only=true reconnect=false$/ { count++ }
  END { print count + 0 }
' "$capture/serial-recorder.log")"
test "$OPENED_COUNT" -eq 1

ARMED_COUNT="$(awk '
  /counted_reset_offset=[0-9]+ reset_mode=usb_serial_jtag_hard_reset pre_reset_drain_seconds=1\.0 counted_reset_status=armed duration_seconds=90\.0 duration_scope=post_reset$/ {
    count++
  }
  END { print count + 0 }
' "$capture/serial-recorder.log")"
test "$ARMED_COUNT" -eq 1

SERIAL_OFFSET="$(awk '
  /counted_reset_offset=[0-9]+ reset_mode=usb_serial_jtag_hard_reset pre_reset_drain_seconds=1\.0 counted_reset_status=armed duration_seconds=90\.0 duration_scope=post_reset$/ {
    for (field = 1; field <= NF; field++) {
      if ($field ~ /^counted_reset_offset=[0-9]+$/) {
        split($field, parts, "=")
        print parts[2]
      }
    }
  }
' "$capture/serial-recorder.log")"
case "$SERIAL_OFFSET" in
  ''|*[!0-9]*) exit 1 ;;
esac

COMPLETED_COUNT="$(awk -v offset="$SERIAL_OFFSET" '
  /counted_reset_offset=[0-9]+ reset_mode=usb_serial_jtag_hard_reset counted_reset_status=completed$/ {
    total++
  }
  index($0, "counted_reset_offset=" offset " reset_mode=usb_serial_jtag_hard_reset counted_reset_status=completed") {
    matching++
  }
  END {
    if (total == 1 && matching == 1) print 1
    else print 0
  }
' "$capture/serial-recorder.log")"
test "$COMPLETED_COUNT" -eq 1

RESET_MARKER_COUNT="$(awk '
  /counted_reset_offset=/ { count++ }
  END { print count + 0 }
' "$capture/serial-recorder.log")"
test "$RESET_MARKER_COUNT" -eq 2

CAPTURE_COMPLETED_COUNT="$(awk '
  /completed=true duration_seconds=90\.0 duration_scope=post_reset$/ {
    count++
  }
  END { print count + 0 }
' "$capture/serial-recorder.log")"
test "$CAPTURE_COMPLETED_COUNT" -eq 1
printf '%s\n' "$SERIAL_OFFSET" \
  > "$capture/counted-reset-serial-offset.txt"

dd if="$capture/serial.log" \
  of="$capture/serial-after-counted-reset.log" \
  bs=1 skip="$SERIAL_OFFSET" \
  2> "$capture/serial-after-counted-reset-dd.log"
test -s "$capture/serial-after-counted-reset.log"

LOG_VERIFIER="$RUN/provenance/verify_storage_hil_log.py"
python3.13 "$LOG_VERIFIER" \
  --byte-offset "$SERIAL_OFFSET" \
  "$capture/serial.log" \
  > "$capture/storage-hil-log-verification.json" \
  2> "$capture/storage-hil-log-verification.log"
test -s "$capture/storage-hil-log-verification.json"
```

The 90-second duration starts after the counted reset; it excludes the one-second
pre-reset drain and reset pulse. The recorder must remain continuously open
across that counted reset and the firmware's own software reset. If native USB
re-enumerates, the recorder fails instead of following a possibly reassigned
path. Do not reopen it or append another boot. An invalid attempt must use a new
evidence directory and externally re-erase `retlog` before retrying, unless an
external full-partition readback proves that `retlog` remained entirely erased.

Do not accept the run unless `serial-after-counted-reset.log` contains one
coherent sequence, without an intervening FAIL or panic:

- exactly two boot records whose `base_mac` is E9:44;
- on each boot, RF-interlock PASS followed by capture-guard ARMED and COMPLETE
  (`duration_ms=5000`, `retlog_access=false`, `flash_mutation=false`) before
  `FlashStorage` or `retlog` access;
- preflight PASS with 8 MiB, flash encryption false, and the exact writable
  plaintext `retlog` range;
- first-boot raw counters `0/0`, format A1 at `2/0`, mount A1 empty at `2/0`,
  and seed indices 0 through 4 at writes `4,6,8,10,12`, all with zero erases;
- semantic replay to revision 4 `Delivered`;
- exact-retry and logical-conflict PASS at unchanged counters `12/0`;
- compaction PASS selecting B2 with five records and counters `26/3`;
- the exact `software-reset` ARMED and ISSUED markers for
  `reason=post-compaction source_generation=1 target_generation=2`, with the
  ARMED marker reporting `delay_ms=250` and the ISSUED marker reporting
  `flush_ms=100` before reset;
- second-boot mount/final replay of B2 with five records, one accepted
  submission, no pending compaction, and counters `0/0`; and
- at least one 30-second RF-inert PASS heartbeat with counters `0/0`.

The copied `verify_storage_hil_log.py` is the fail-closed machine check for this
contract. It reads the complete byte capture, applies the recorded offset,
requires every normalized `storage-hil` event above in exact order, rejects
fatal output or any extra project event other than final B2 heartbeats, and
records capture/segment byte counts and SHA-256 digests in
`storage-hil-log-verification.json`. Retain the complete and offset-extracted
logs as its independently reviewable inputs.

After the capture has closed, obtain a fresh passive E9:44 mapping, confirm the
MAC again, and preserve the resulting partition for independent inspection.
Both operations explicitly leave the board in the loader; this avoids an
unrecorded application boot after the qualifying capture.

```sh
record_tracker_port "$RUN/hardware/e944-port-after-capture.txt"
espflash board-info \
  --port "$PORT" --chip esp32s3 \
  --after no-reset --non-interactive --skip-update-check 2>&1 \
  | tee "$RUN/hardware/e944-board-info-after.txt"
rg -qi '^MAC address:[[:space:]]+44:1b:f6:f8:e9:44$' \
  "$RUN/hardware/e944-board-info-after.txt"

record_tracker_port "$RUN/hardware/e944-port-before-final-retlog-read.txt"
espflash read-flash \
  --port "$PORT" --chip esp32s3 \
  --before default-reset --after no-reset \
  --non-interactive --skip-update-check \
  0x670000 0x100000 "$RUN/flash/retlog-after.bin" 2>&1 \
  | tee "$RUN/flash/retlog-after.log"
test "$(wc -c < "$RUN/flash/retlog-after.bin" | tr -d ' ')" = 1048576
"$DUMP_VERIFIER" "$RUN/flash/retlog-after.bin" \
  > "$RUN/flash/retlog-after-verification.txt" \
  2> "$RUN/flash/retlog-after-verification.log"
test -s "$RUN/flash/retlog-after-verification.txt"
shasum -a 256 \
  "$RUN/flash/retlog-erased.bin" \
  "$RUN/flash/storage-hil-readback.bin" \
  "$RUN/flash/retlog-after.bin" \
  > "$RUN/flash/hashes-after.sha256"

(
  cd "$RUN"
  find . -type f ! -name evidence.sha256 -print \
    | LC_ALL=C sort \
    | while IFS= read -r path; do
        shasum -a 256 "$path"
      done \
    > evidence.sha256
  shasum -a 256 -c evidence.sha256
)
```

The copied `reticulum-storage-hil-verify` binary mounts the raw dump through the
production journal implementation and fails unless it independently proves the
expected B2 manifest, five committed records, one accepted fixture submission,
revision-4 Delivered lifecycle, erased retired A manifest, and erased unused B
tail. Its preserved stdout is therefore the semantic counterpart to the raw
partition hash; the final evidence manifest covers both verifier copies, both
results, and all of their inputs.

The qualifying run recorded above validates real raw-flash
format/append/replay/compaction and a software-reset replay. It does not by
itself prove controlled power-cut recovery, flash endurance, production
encryption, the future async actor, or any RF behavior. Add controlled cuts and
longer cycling only as separately recorded storage-HIL scenarios; keep those
images radio-free so their evidence remains isolated from the radio stack.
