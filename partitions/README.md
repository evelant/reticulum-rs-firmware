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
PROBE_SELECTOR="303a:1001:$TRACKER_USB_SERIAL"
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
  probe-rs --version
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
probe-rs list > "$RUN/hardware/probes-before.txt"
rg -Fi "$PROBE_SELECTOR" "$RUN/hardware/probes-before.txt"
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
the application until an unrecorded Ctrl-R. Instead, arm the project-owned
reset-minimizing recorder first, wait for its `opened=` status, record the
current serial byte offset, and only then reset the exact E9:44 JTAG probe.

Opening the ESP32-S3 native-USB TTY can itself reset the target before DTR and
RTS can be cleared. This image therefore emits a five-second
`stage=capture-guard` and performs no `FlashStorage`/`retlog` access or flash
mutation during that interval. Instruction fetches remain ordinary flash
reads, so the precise evidence fields are `retlog_access=false` and
`flash_mutation=false`. The coordinator below issues the counted JTAG reset
inside that guard. Only bytes at or after
`counted-reset-serial-offset.txt` belong to the qualifying two-boot attempt;
earlier bytes are attachment evidence, not a storage result.

```sh
capture="$RUN/capture"
test ! -e "$capture"
mkdir "$capture"
RECORDER="$RUN/provenance/esp32s3_usb_serial_capture.py"
shasum -a 256 "$RECORDER" > "$capture/serial-recorder.sha256"

record_tracker_port "$capture/e944-port-before-open.txt"
printf 'probe_selector=%s\n' "$PROBE_SELECTOR" \
  > "$capture/e944-probe-selector.txt"
probe-rs list > "$capture/probes-before-reset.txt"
rg -Fi "$PROBE_SELECTOR" "$capture/probes-before-reset.txt"

recorder_pid=""
cleanup_recorder() {
  if [[ -n "$recorder_pid" ]] && kill -0 "$recorder_pid" 2>/dev/null; then
    kill "$recorder_pid"
  fi
  if [[ -n "$recorder_pid" ]]; then
    wait "$recorder_pid" 2>/dev/null || true
  fi
}
trap cleanup_recorder EXIT INT TERM

python3.13 "$RECORDER" \
  --port "$PORT" --duration-seconds 90 \
  > "$capture/serial.log" \
  2> "$capture/serial-recorder.log" &
recorder_pid=$!
printf '%s\n' "$recorder_pid" > "$capture/serial-recorder.pid.txt"

opened=false
attempt=0
while (( attempt < 100 )); do
  if rg -F "opened=$PORT " "$capture/serial-recorder.log" > /dev/null; then
    opened=true
    break
  fi
  if ! kill -0 "$recorder_pid" 2>/dev/null; then
    break
  fi
  attempt=$((attempt + 1))
  sleep 0.1
done
test "$opened" = true
kill -0 "$recorder_pid"

# Drain any attachment-reset boot prefix, but remain inside the 5 s guard.
sleep 1
if rg -F 'storage-hil stage=capture-guard status=COMPLETE' \
  "$capture/serial.log" > /dev/null; then
  printf '%s\n' 'capture guard completed before counted reset' \
    > "$capture/coordinator-failure.txt"
  exit 1
fi

SERIAL_OFFSET="$(wc -c < "$capture/serial.log" | tr -d ' ')"
printf '%s\n' "$SERIAL_OFFSET" \
  > "$capture/counted-reset-serial-offset.txt"
date -u +%Y-%m-%dT%H:%M:%SZ \
  > "$capture/counted-reset-requested-at.txt"
if probe-rs reset \
  --chip esp32s3 --protocol jtag \
  --probe "$PROBE_SELECTOR" --non-interactive \
  > "$capture/counted-reset.log" 2>&1; then
  reset_status=0
else
  reset_status=$?
fi
printf '%s\n' "$reset_status" > "$capture/counted-reset.exit-status.txt"
test "$reset_status" -eq 0

if wait "$recorder_pid"; then
  recorder_status=0
else
  recorder_status=$?
fi
recorder_pid=""
trap - EXIT INT TERM
printf '%s\n' "$recorder_status" \
  > "$capture/serial-recorder.exit-status.txt"
test "$recorder_status" -eq 0
rg -F 'completed=true duration_seconds=90.0' \
  "$capture/serial-recorder.log"

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

The 90-second recorder must remain continuously open across the firmware's own
software reset. If native USB re-enumerates, the recorder fails instead of
following a possibly reassigned path. Do not reopen it or append another boot:
the attempt is invalid, and a new clean attempt must return to the external
`retlog` erase step with a new evidence directory.

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

The first clean run validates real raw-flash format/append/replay/compaction and
a software-reset replay. It does not by itself prove controlled power-cut
recovery, flash endurance, production encryption, the future async actor, or
any RF behavior. Add controlled cuts and longer cycling only as separately
recorded HIL scenarios; keep every such image radio-free.
