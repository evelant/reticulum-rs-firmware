# Phase 1 Tracker RX hardware qualification

**Status:** deterministic RNode peer/corpus and clean-tree bundle tooling ready;
preserved qualification runs and powered evidence not yet complete<br>
**Target:** Heltec Wireless Tracker V2.3, ESP32-S3FN8, 863–928 MHz RF variant<br>
**Image:** `reticulum-heltec-tracker-v2-lab-rx`

This procedure qualifies the explicitly configured receive-only lab image. It
does not authorize transmission from the Tracker, establish production RF
settings or qualify another Tracker revision. Stop if the board revision or RF
matching variant is uncertain.

This runbook does not yet close every Phase 1 gate. Host tests cover the
returned-SPI fail point and the four electrical command traces. CI strictly
checks every selector and runs the public clean-tree closure command to build,
inspect and verify all four electrical selections, both returned-fault policies
and representative corrupt/torn journal selectors. That ephemeral bundle is a
software smoke test of GitHub's merge commit; CI does not preserve it as
qualification evidence. A clean local bundle run and every powered capture
remain evidence that must actually be collected. A source build, host test, CI
closure run or ad hoc flash is not qualification evidence.

## Safety and equipment

Required:

- a Tracker V2.3 with a suitable antenna attached before power is applied;
- a separately controlled RNode/LoRa transmitter configured within the local
  regulatory limits;
- an independent on-air observer in a conducted, shielded or calibrated spatial
  arrangement that can distinguish the Tracker from the transmitting peer;
- a logic analyzer capable of decoding SPI mode 0 at 1 MHz and enough digital
  channels for the safety pins;
- current measurement appropriate for the board; and
- a development host with the pinned Rust/ESP toolchain.

The Tracker image has no TX operation, but the peer does. Frequency, peer
power, antenna gain and airtime remain the operator's regulatory
responsibility. Do not hard-short GPIOs to inject faults. Use a protected jig
or explicit mock-only fault hook.

Record the observer bandwidth, noise floor, detection threshold, calibration
source and physical/coupled arrangement. An observer that merely sees the
peer's expected packets cannot establish whether the Tracker also transmitted.

## Preserve the exact artifact

Qualification evidence requires a clean commit and two immutable bundles. The
preparation commands reject staged, unstaged and untracked source changes,
refuse to overwrite an existing output directory, resolve output-parent
symlinks before enforcing the Git-ignore boundary, prove the archived source
tree matches the recorded commit, and build from that archive outside the
workspace with an isolated Cargo home. They invoke only `cargo +esp build` and
host-side `espflash save-image`; dependency fetching may require network access,
but no command accepts a serial port or can flash, monitor or transmit.

Export the complete explicit radio profile and select a new result path. The
seven-second pressure stall is also explicit and has no default:

```sh
source ~/export-esp.sh
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
hil_bundle="artifacts/hil/phase1-rx/${stamp}-normal-pressure-bundle"
closure_bundle="artifacts/hil/phase1-rx/${stamp}-closure-bundle"
run="artifacts/hil/phase1-rx/${stamp}-powered-evidence"
export RETICULUM_LAB_RX_FREQUENCY_HZ=915000000
export RETICULUM_LAB_RX_SPREADING_FACTOR=7
export RETICULUM_LAB_RX_BANDWIDTH_HZ=125000
export RETICULUM_LAB_RX_CODING_RATE_DENOMINATOR=5
export RETICULUM_LAB_RX_PREAMBLE_SYMBOLS=18
export RETICULUM_LAB_RX_EXPLICIT_HEADER=1
export RETICULUM_LAB_RX_CRC=1
export RETICULUM_LAB_RX_IQ_INVERTED=0

cargo run --locked -p xtask -- phase1-rx-hil-artifacts prepare \
  --output "$hil_bundle" \
  --backpressure-stall-us 7000000

cargo run --locked -p xtask -- phase1-rx-hil-artifacts verify \
  --bundle "$hil_bundle"

cargo run --locked -p xtask -- phase1-rx-closure-artifacts prepare \
  --output "$closure_bundle" \
  --journal-corrupt-slot 0 \
  --journal-corrupt-word 4 \
  --journal-torn-write-ordinal 9

cargo run --locked -p xtask -- phase1-rx-closure-artifacts verify \
  --bundle "$closure_bundle"

cargo run --locked -p xtask -- phase1-rx-powered-evidence init \
  --normal-pressure-bundle "$hil_bundle" \
  --closure-bundle "$closure_bundle" \
  --output "$run"
```

Preparation preserves the normal ELF and merged, unpadded address-zero image at
the normal/pressure bundle root. The distinctly named pressure binary is built
in another target directory and preserved under
`$hil_bundle/backpressure-artifact/`; the
verifier requires different ELF and image hashes for the two modes. Both builds
enable compiler stack-size records and reject an individual frame above 49,152
bytes. The machine-owned `artifact-preparation.json` uses schema
`reticulum.phase1-rx-hil.artifacts.v2` and binds the commit, its exact raw Git
root tree, source archive, tool versions, exact build environment, modes,
paths, sizes and hashes. The closure manifest binds the same commit/root-tree
pair, and powered-evidence initialization and verification require the two
immutable bundles to agree on both values.
`prepared-artifacts.sha256` covers every preparation file, while
`artifact-preparation.complete` is written only after host-side verification.

All project-source Git subprocesses used for identity, cleanliness, timestamps
and archives start from an environment-cleared policy with only `PATH`
preserved. They disable replacement objects, null system/global configuration,
override hooks, fsmonitor and external attributes, require the canonical
workspace root, reject nonstandard tracked-file index flags, and fail closed if
the Git common directory contains `info/attributes` (including an empty file or
symlink). Archive verification extracts the tar and compares its directories,
files, executable modes, symlinks and blob bytes with raw Git tree/blob objects.
Committed `export-ignore` filtering or `export-subst` modification therefore
makes preparation fail; regenerating a second equally filtered `git archive`
is not source proof.

The separate closure bundle has schema
`reticulum.phase1-rx-closure-artifacts.v2` and exactly these artifact
directories:

- `electrical-ldo-unboosted`;
- `electrical-ldo-boosted`;
- `electrical-dcdc-unboosted`;
- `electrical-dcdc-boosted`;
- `returned-fault-one-boot`;
- `returned-fault-repeat-until-quarantine`;
- `reset-journal-corrupt-slot0-word4`; and
- `reset-journal-torn-write9`.

The two journal entries are representative selectors only: slot 0/word 4 and
write ordinal 9. This bundle is not evidence for the full slot/word/ordinal
matrix. Qualification of another selector requires separately reviewed bundle
support; manually changing an environment variable is not a substitute.

The verifier is bundle-read-only. It rechecks source provenance, hashes,
lengths, address records, mode identity, size/link/stack/TX-symbol contracts,
and regenerates each merged image in a temporary directory for an exact byte
comparison with its preserved ELF-derived image. It still performs no hardware
operation. The image recipe explicitly fixes ESP32-S3, DIO, 80 MHz, 8 MB, a
40 MHz crystal, minimum chip revision 0.0 and ESP-IDF format while isolating
local and global `espflash.toml`. DIO matches both preserved, bootable Tracker
V2 baselines; a QIO-built powered smoke image watchdog-reset in ROM flash
mapping before firmware startup, strongly implicating the mode mismatch pending
the controlled DIO smoke. Preparation and verification inspect the bootloader
and partition-table-selected factory-app headers and reject any image that does
not encode DIO and 8 MB/80 MHz. There is intentionally no dirty-worktree or
overwrite bypass.

Both v2 preparations start Cargo and its build scripts after clearing the
process environment. They copy only `PATH` and the resolved `RUSTUP_HOME`, set
an isolated Cargo home and controlled `TMPDIR`, and set `SOURCE_DATE_EPOCH` to
the source commit timestamp multiplied by 1,000,000 because the pinned ESP
bootloader consumes microseconds. Encoded Rust flags remap the entire nonce
build root and Rustup home to stable virtual prefixes, so source, target and
registry paths cannot make otherwise identical ELFs differ.

Preparation also performs one proportionate independent-build guard instead
of doubling every artifact build. The normal/pressure bundle rebuilds the
normal image from a second source-archive extraction with a fresh target and
Cargo home. The closure bundle does the same for its
`electrical-ldo-unboosted` canary. Each guard requires the rebuilt ELF and
merged flash image to match byte-for-byte, then records the matching hashes and
fresh-root properties in the manifest. This is a canary for the deterministic
tooling boundary; it does not claim that every closure selector was compiled
twice.

Both verifiers enforce an exact directory tree. Treat `$hil_bundle` and
`$closure_bundle` as immutable after preparation: do not write flash logs,
readbacks, serial captures, peer manifests or notes into either directory.
All mutable powered evidence belongs under the distinct sibling
`$run/captures`; adding even one file to a bundle makes verification fail.

### Exploratory full-selector builds

The following commands document the exact compile-time selections and are
useful for source/static development and broader selector checks only. They do
not add their outputs to either qualification bundle above. Use a distinct
target directory for every radio-bearing variant so that an ELF cannot be
mistaken for another mode.
The eight ordinary lab-profile variables from the preceding command block must
remain exported.

Build all four regulator/receive-gain combinations:

```sh
for regulator in ldo dcdc; do
  for gain in unboosted boosted; do
    CARGO_TARGET_DIR="/tmp/phase1-electrical-${regulator}-${gain}" \
    RUSTFLAGS='-C link-arg=-nostartfiles -Z emit-stack-sizes' \
    RETICULUM_LAB_RX_REGULATOR="$regulator" \
    RETICULUM_LAB_RX_GAIN="$gain" \
    cargo +esp build --locked --release \
      -p reticulum-heltec-tracker-v2 \
      --bin reticulum-heltec-tracker-v2-lab-rx-electrical-hil \
      --no-default-features --features lab-rx-electrical-hil \
      --target xtensa-esp32s3-none-elf
  done
done
```

Each ELF identifies itself as
`lab-rx-electrical-hil;regulator=<ldo|dcdc>;rx_gain=<unboosted|boosted>`.
LDO is represented by reset-default omission of `SetRegulatorMode`; DC-DC must
emit `0x96 0x01` after standby and before `0x9d 0x01`. The gain register write
must be `0x0d 0x08 0xac 0x94` for unboosted or end in `0x96` for boosted. Host
tests prove that the remaining command trace is identical and contains no TX
command. Powered current, reliability and sensitivity comparisons remain open.

Build both returned-fault policies:

```sh
for policy in one-boot repeat-until-quarantine; do
  CARGO_TARGET_DIR="/tmp/phase1-returned-${policy}" \
  RUSTFLAGS='-C link-arg=-nostartfiles -Z emit-stack-sizes' \
  RETICULUM_LAB_RX_RETURNED_FAULT_TRIGGER=get-irq-status-after-set-rx \
  RETICULUM_LAB_RX_RETURNED_FAULT_POLICY="$policy" \
  cargo +esp build --locked --release \
    -p reticulum-heltec-tracker-v2 \
    --bin reticulum-heltec-tracker-v2-lab-rx-returned-fault-hil \
    --no-default-features --features lab-rx-returned-fault-hil \
    --target xtensa-esp32s3-none-elf
done
```

The returned-fault identity includes the exact trigger and policy. Its
decorator is below the board-owned TX-opcode firewall, forwards a real `SetRx`,
then rejects the first `GetIrqStatus` before physical SPI. Host integration
tests prove the resulting `Receive / Radio(Spi)` error and inert cleanup; only a
powered run can prove retained reset behavior and pin/RF containment.

The retained-journal binaries are deliberately RF-inert and require no radio
profile. The corruption selector is slot `0|1` plus word `0..=8`; the torn
transaction selector is write ordinal `1..=9`. For example:

```sh
CARGO_TARGET_DIR=/tmp/phase1-journal-corrupt-s0-w4 \
RETICULUM_LAB_RX_RESET_JOURNAL_SLOT=0 \
RETICULUM_LAB_RX_RESET_JOURNAL_WORD=4 \
cargo +esp build --locked --release \
  -p reticulum-heltec-tracker-v2 \
  --bin reticulum-heltec-tracker-v2-lab-rx-reset-journal-corrupt \
  --no-default-features --features lab-rx-reset-journal-corrupt-hil \
  --target xtensa-esp32s3-none-elf

CARGO_TARGET_DIR=/tmp/phase1-journal-torn-w9 \
RETICULUM_LAB_RX_RESET_JOURNAL_WRITE_ORDINAL=9 \
cargo +esp build --locked --release \
  -p reticulum-heltec-tracker-v2 \
  --bin reticulum-heltec-tracker-v2-lab-rx-reset-journal-torn \
  --no-default-features --features lab-rx-reset-journal-torn-hil \
  --target xtensa-esp32s3-none-elf
```

These images establish reset/FEM-low owners before touching retained state and
construct no SPI, radio, executor timer or supervisor watchdog. Static ELF
inspection has confirmed the single 72-byte `.rtc_fast.persistent` journal and
absence of owned Rust radio/SPI/runtime definitions for representative
selectors. The next-boot `CorruptOrTornJournal` quarantine still requires a
preserved two-boot powered capture. The closure command above preserves only
the two exact representative journal selectors; none of these exploratory
commands can satisfy a gate.

Use `inspect-elf` with the mode that exactly matches the selected environment:

| Selection | Inspector `--mode` |
| --- | --- |
| electrical LDO / unboosted | `lab-rx-electrical-hil-ldo-unboosted` |
| electrical LDO / boosted | `lab-rx-electrical-hil-ldo-boosted` |
| electrical DC-DC / unboosted | `lab-rx-electrical-hil-dcdc-unboosted` |
| electrical DC-DC / boosted | `lab-rx-electrical-hil-dcdc-boosted` |
| returned / one boot | `lab-rx-returned-fault-hil-one-boot` |
| returned / repeat | `lab-rx-returned-fault-hil-repeat-until-quarantine` |
| journal corruption | `lab-rx-reset-journal-corrupt-hil` |
| journal torn write | `lab-rx-reset-journal-torn-hil` |

For example:

```sh
elf=/tmp/phase1-electrical-dcdc-boosted/xtensa-esp32s3-none-elf/release/reticulum-heltec-tracker-v2-lab-rx-electrical-hil
cargo run --locked -p xtask -- phase1-rx-hil-artifacts inspect-elf \
  --mode lab-rx-electrical-hil-dcdc-boosted \
  --elf "$elf"
```

The inspector checks identity, size, retained/stack sections and owners, and
named TX definitions appropriate to the mode. It reads one ELF; it does not
archive source, create a merged image, prove distinct variants, or produce a
qualification bundle.

Every qualifying `esp-rtos`-based Tracker ELF—safe-idle, normal, pressure,
electrical and returned-fault—must contain the exact
`esp-rtos-0.3.0-cpu0-cpu1-main-stack-words-v2` runtime identity. The RF-inert
reset-journal binaries use `esp_hal::main` and are intentionally exempt; do not
misclassify their missing marker as a failure.

## Result manifest

`phase1-rx-powered-evidence init` creates a schema-versioned evidence directory
with these machine-owned and operator-owned surfaces:

- `$run/powered-evidence.json` binds canonical absolute paths and manifest
  digests for both immutable bundles, their common Git commit and radio profile,
  and every normal, pressure and closure ELF/image digest;
- `$run/records/operator.json` uses operator schema v2 to record the operator,
  board samples, paths and digests for copied peer image/source/corpus/tool
  artifacts, numeric peer power and airtime authorization, calibrated observer
  and equipment identity;
- `$run/records/scenarios/*.json` contains one typed gate record for every
  traffic, fault, electrical and soak scenario; and
- `$run/captures/` is the only open subtree for flash/readback bytes and logs,
  serial logs, peer manifests/transcripts, observer output, analyzer source and
  decodes, current measurements and other run artifacts.

Do not edit `powered-evidence.json`, rename generated records or add files
outside `captures/`. Complete the operator record and each scenario record with
canonical whole-second UTC timestamps, board sample IDs, a truthful `pass`,
`fail` or `not-run` status, the exact generated scenario-schema-v2 check
inventory, and relative paths below `captures/`. Each check object has its own `status`,
`evidence_files` and optional narrow `observation`. A passing or failed attempted
check must bind at least one non-empty capture that is both in the scenario's
complete evidence inventory and classified as a serial, peer, logic-analyzer,
RF-observer or current-measurement capture, or as a required flash readback. A
check with no capture is `not-run`; it cannot be marked `fail` with an empty
placeholder. Passing common checks additionally require their matching typed
role: analyzer checks bind analyzer evidence, RF checks bind observer evidence,
the serial check binds serial evidence, and the artifact check binds serial
activation evidence plus every artifact readback.

The operator-v2 fields `peer_firmware_image_path`,
`peer_firmware_source_path`, `peer_corpus_path` and `peer_tool_path` must name
non-empty regular files below `$run/captures/`; do not point them back into the
working tree or an immutable bundle. Preserve a copy of the exact flashed peer
image and a self-contained Git bundle with the complete object history
reachable from its corresponding official release tag, then copy the project-owned
`interop/vectors/rnode-hil-v1.json` and `interop/python/rnode_hil.py` files into
the evidence tree. The verifier hashes all four copies and requires each value
to equal its matching operator digest. It clones and strictly checks the source
bundle's Git object graph, requires the pinned official commit to be reachable
from a preserved ref and requires its exact root tree. It also requires the
corpus and tool copies to equal the project-owned files in the qualifying
bundle's verified `source.tar`, not the current checkout, so replacing both a
copy and its operator digest with unrelated, validly hashed bytes does not
qualify.

A passing record must bind each required artifact to its declared and
activation-observed mode and an exact flash readback whose digest equals the
prepared image. It must also place serial, logic-analyzer and RF-observer paths
in their typed capture lists; peer-driven cases require typed peer
manifest/transcript paths, and cold-boot, electrical and soak cases require
typed current-measurement paths. Every typed path must also occur in the
record's complete `evidence_files` inventory. Scenario board IDs must occur in
the operator record; a passing electrical matrix mechanically requires at
least two samples. The validator reads only the structured duration/counter
observations described below; it does not parse arbitrary serial, analyzer or
observer formats. The typed checks remain operator attestations about capture
content, and the sealer proves their binding, policy consistency and byte
integrity, not measurement authenticity.

Every peer-driven passing scenario record is also machine-checked at the
generated-record boundary. Each required invocation must put its
`peer-manifest.json` and sibling `peer-transcript.jsonl` in
`peer_capture_files` and the record-wide `evidence_files`. The applicable
check directly binds the manifest; that manifest's `transcript_sha256`
indirectly binds its required sibling transcript. The verifier parses every listed peer manifest,
requires a finished timestamp, `status` equal to
`enqueued_not_rf_verified`, and `error` equal to `null`, and hashes the listed
sibling transcript to reproduce `transcript_sha256`. It rejects a missing,
unlisted or mismatched transcript. The transcript is strict, newline-terminated
JSONL: sequence numbers, canonical whole-second UTC timestamps, monotonic time,
KISS escaping and direction are checked together with the exact request/reply,
device/configuration, READY-loop and `CMD_DATA` state machine and the expected
scenario payloads. Unsolicited, reordered or extra traffic is rejected except
that an exact physical-timing report matching the manifest may repeat in the
explicitly permitted configuration and initial-READY windows. The
manifest interval must lie inside its powered scenario record and every
transcript entry must lie inside the manifest interval. It also binds the full
corpus scenario and enqueued-step count, exact target-artifact declaration,
bundle radio profile, operator region and power/airtime values, peer
firmware/device report, and corpus/tool digests. A syntactically valid manifest
for a different scenario, mode, profile or artifact copy is not interchangeable
evidence. Repeated invocations need distinct transcript digests and time pairs;
cross-record reuse is rejected except for the same preserved `rnode-exact-500`
result intentionally shared by the split and malformed records.

For a passing run, the peer-manifest inventory bound to the named check is
exactly the following, except for the explicitly extensible soak row:

| Powered scenario | Required peer-tool invocations | Manifest `target_artifact_mode` | Binding check |
| --- | --- | --- | --- |
| `single-physical-frame` | one each of `raw-header-only`, `raw-single-1`, `raw-single-253`, `raw-single-254` | `lab-rx` | `all-corpus-cases-run` |
| `split-packet` | one each of `rnode-split-255`, `rnode-split-256`, `rnode-split-499`, `rnode-exact-500` | `lab-rx` | `all-corpus-cases-run` |
| `fragment-expiry-and-replacement` | one each of `raw-orphan-split`, `raw-split-replacement`, `raw-nonsplit-discards-pending` | `lab-rx` | `all-corpus-cases-run` |
| `physical-over-rns-boundary` | one `rnode-501-through-508` | `lab-rx` | `all-corpus-cases-run` |
| `malformed-and-semantic-rejection` | one each of `raw-duplicate-first-half`, `raw-reordered-same-sequence`, `released-python-announce`, `released-python-announce-duplicate`, one generated `boot-local-data`, plus the exact preserved `rnode-exact-500` result from `split-packet` | `lab-rx` | `all-corpus-cases-run`; also bind `rnode-exact-500` to `all-output-actions-suppressed` |
| `bounded-backpressure` | one `raw-backpressure-four-frame` | `lab-rx-backpressure-hil` | `feature-bound-corpus-run` |
| `returned-radio-fault` one-boot lane | one `raw-returned-fault-trigger` | `lab-rx-returned-fault-hil;trigger=get-irq-status-after-set-rx;policy=one-boot` | `one-boot-fault-trace` |
| `returned-radio-fault` repeat lane | three separate `raw-returned-fault-repeat-until-quarantine` invocations | `lab-rx-returned-fault-hil;trigger=get-irq-status-after-set-rx;policy=repeat-until-quarantine` | `repeat-policy-three-fault-quarantine` |
| `receive-soak-24h` | at least one each of `raw-single-1`, `rnode-split-256`, `raw-duplicate-first-half`; additional ordinary pinned-corpus `lab-rx` scenarios are allowed | `lab-rx` | `mixed-valid-and-hostile-traffic` |

Ordinary invocations must use the operator's copied pinned corpus, whose digest
also appears in each manifest. `boot-local-data` is the sole custom-corpus
exception: keep its generated corpus below `captures/`, list it in the malformed
record and bind it to both `all-corpus-cases-run` and
`boot-local-data-processed`. Its manifest corpus digest must match those exact
custom bytes, and the verifier validates the one exact `boot-local-data`
scenario and `lab-rx` target. The verifier reads the shared generator source
from the qualifying bundle, enforces its schema-freeze digest, regenerates the
corpus from the recorded target identity and base corpus, and compares the raw
JSON bytes exactly. Because the packet is generated for one ephemeral Tracker
boot, that custom corpus is deliberately not required to equal the operator's
pinned-corpus digest.

For example, the pressure-counter check records the firmware-reported deltas
and binds the exact serial and peer captures from which the operator transcribed
them:

```json
"offered-three-queued-two-dropped-one": {
  "status": "pass",
  "evidence_files": [
    "captures/backpressure/tracker-serial.log",
    "captures/backpressure/peer/raw-backpressure-four-frame/peer-manifest.json"
  ],
  "observation": {
    "kind": "backpressure-counters",
    "offered_during_stall": 3,
    "queued_during_stall": 2,
    "dropped_during_stall": 1
  }
}
```

After the run, seal and independently verify the evidence:

```sh
cargo run --locked -p xtask -- phase1-rx-powered-evidence finalize \
  --evidence "$run"
cargo run --locked -p xtask -- phase1-rx-powered-evidence verify \
  --evidence "$run"
```

`finalize` rejects symlinks, special files, unexpected paths, partial passing
records and unbound/tampered readbacks. It validates the exact tree, bundle
bindings and complete operator/scenario payload before creating an inventory,
then repeats those checks around the installed inventory before staging a seal.
The canonical sorted `artifacts.sha256` covers every manifest, record and
capture while excluding itself and lifecycle markers. Only after verifying it
does `finalize` commit the seal with one same-directory rename of
`powered-evidence.incomplete` to `powered-evidence.sealed`. The temporary
inventory is created and synced inside the evidence directory before it is
renamed, and the directory is synced at each durability boundary. That marker
rename is the only commit point: before it the filename still declares the run
incomplete, and after the following directory sync returns the seal is durable.
A crash during that boundary
recovers as either the retryable incomplete state or the committed sealed
state. Stop all record and capture writers, then rerun `finalize`: under an
incomplete marker it removes only the reserved temporary/final inventory files
and rebuilds them, while an already sealed run is fully verified and returned
unchanged. Both/neither lifecycle markers, unknown files and symlinked or
special recovery metadata fail closed.

Finalization is serialized by a persistent sibling lock named
`<evidence>.phase1-powered-evidence-finalize.lock`. It is coordination metadata
outside the exact evidence tree, so retaining it is expected and it must not be
copied into the evidence directory. The lock coordinates finalizers; it does
not make concurrent operator writes safe.

Sealing a failed or incomplete run preserves its evidence but reports
`qualification_status=fail` or `qualification_status=not-run`; only an all-pass
record set reports `pass`. `verify` is evidence-directory read-only and
re-verifies both source bundles, the binding, records, inventory and seal.

The seal is an unsigned internal root, not an external trust anchor. Preserve
the printed qualification status plus the SHA-256 of `powered-evidence.sealed`
in an independent signed or write-once run log before treating the directory as
archival evidence. A party able to rewrite the whole directory can otherwise
replace both records and their unsigned integrity metadata coherently.

Canonical absolute bundle paths are intentional: later verification requires
both byte-identical sibling bundles to remain at their recorded paths. For
archival storage, preserve the common parent tree and restore or mount it at the
same absolute path before verification; v2 has no relocation override. None of
the powered-evidence commands accepts a serial port or performs a flash,
monitor or RF operation.

## Flash and monitor

Find the actual USB serial port:

```sh
espflash list-ports
export ESPFLASH_PORT=/dev/cu.usbmodemYOUR_PORT
```

Qualification must flash the preserved, hash-checked merged image. Do not use
`cargo run` or `espflash flash`: either can regenerate or select bytes other
than the preserved image.

```sh
set -euo pipefail
normal_artifact="$hil_bundle"
normal_run="$run/captures/normal"
mkdir -p "$normal_run"
cargo run --locked -p xtask -- phase1-rx-hil-artifacts verify \
  --bundle "$hil_bundle"
(cd "$normal_artifact" && shasum -a 256 -c firmware.sha256)
(cd "$normal_artifact" && shasum -a 256 -c flash-image.sha256)
test "$(cat "$normal_artifact/flash-image-address.txt")" = 0x00000000
test "$(cat "$normal_artifact/flash-image-bytes.txt")" = \
  "$(wc -c < "$normal_artifact/flash-image.bin" | tr -d ' ')"

espflash write-bin \
  --port "$ESPFLASH_PORT" \
  --chip esp32s3 \
  --non-interactive \
  0x00000000 "$normal_artifact/flash-image.bin" 2>&1 | tee "$normal_run/flash.log"

espflash read-flash \
  --port "$ESPFLASH_PORT" \
  --chip esp32s3 \
  --non-interactive \
  0x00000000 "$(cat "$normal_artifact/flash-image-bytes.txt")" \
  "$normal_run/flash-readback.bin" 2>&1 | tee "$normal_run/readback.log"
cmp "$normal_artifact/flash-image.bin" "$normal_run/flash-readback.bin"
shasum -a 256 "$normal_run/flash-readback.bin" \
  > "$normal_run/flash-readback.sha256"
```

The exact merged image and equal readback make the bootloader and partition
bytes part of the evidence, which matters when judging ROM/boot pin
transients. Record both digests, the byte count and address in the manifest.

Attach without resetting the ephemeral identity and retain the complete
session. Start this recorder before an externally controlled reset when the
cold-boot trace matters:

```sh
espflash monitor \
  --port "$ESPFLASH_PORT" \
  --chip esp32s3 \
  --non-interactive --no-reset \
  --elf "$normal_artifact/firmware.elf" 2>&1 | tee -a "$normal_run/serial.log"
```

Each boot generates a new identity. The boot record reports the SoC reset
reason; a preattached recorder is still required to retain the fatal line that
precedes an immediate digital-core software reset.
The first activation snapshot and every 60-second heartbeat repeat the public
identity, destination hash, heap use and bounded radio/ingress counters.

## Pinned RNode peer and corpus

The project-owned peer tool uses the small published RNode KISS protocol; it
does not copy the GPL RNode implementation. The committed corpus pins official
RNode Firmware 1.86 at revision
`9b39b6ce5962007fafefc22034082f354eff3374`. Preserve and hash the peer source,
built image, flash log and, where supported, flash readback separately. A
reported `1.86` version alone does not prove that source revision or peer
binary.

Before the first peer invocation, place regular-file copies of the exact peer
image, a self-contained Git bundle rooted at the official `1.86` release tag,
and the project-owned corpus and peer tool below
`$run/captures/peer-provenance/`. Obtain the corpus and tool from the already
verified normal/pressure bundle's `source.tar`, not from the mutable checkout.
The source bundle must retain a ref that reaches the pinned commit and its exact
root tree. Preserve that tag's complete history without bundling unrelated
local branches or private refs:

```sh
: "${RNODE_SOURCE_REPO:?path to the official RNode_Firmware Git repository}"
: "${RNODE_FIRMWARE_IMAGE:?path to the exact RNode image that will be flashed}"

peer_revision=9b39b6ce5962007fafefc22034082f354eff3374
peer_root_tree=12f583c5f0fd8ae83c59a391267f0fe9ce184d86
test "$(git -C "$RNODE_SOURCE_REPO" rev-parse --is-shallow-repository)" = false
test "$(git -C "$RNODE_SOURCE_REPO" rev-parse \
  'refs/tags/1.86^{commit}')" = "$peer_revision"
test "$(git -C "$RNODE_SOURCE_REPO" rev-parse "$peer_revision^{tree}")" = \
  "$peer_root_tree"

peer_provenance="$run/captures/peer-provenance"
mkdir -p "$peer_provenance"
peer_provenance="$(cd "$peer_provenance" && pwd -P)"
peer_image="$peer_provenance/RNode_Firmware-1.86.bin"
peer_source="$peer_provenance/RNode_Firmware-1.86.bundle"
peer_corpus="$peer_provenance/rnode-hil-v1.json"
peer_tool="$peer_provenance/rnode_hil.py"
cp "$RNODE_FIRMWARE_IMAGE" "$peer_image"
git -C "$RNODE_SOURCE_REPO" bundle create "$peer_source" refs/tags/1.86
git -C "$RNODE_SOURCE_REPO" bundle verify "$peer_source"

project_source="$(mktemp -d)"
tar -xf "$hil_bundle/source.tar" -C "$project_source" \
  interop/vectors/rnode-hil-v1.json interop/python/rnode_hil.py
cp "$project_source/interop/vectors/rnode-hil-v1.json" "$peer_corpus"
cp "$project_source/interop/python/rnode_hil.py" "$peer_tool"
rm -rf "$project_source"
```

Enter those four relative capture paths and SHA-256 values in the operator-v2
record. The powered-evidence verifier recomputes all four hashes, clones the
source bundle with hooks and ambient Git configuration disabled, runs strict
object verification, requires the pinned commit to be reachable from a
preserved ref, and checks the exact root tree above. It independently compares
the copied corpus and tool with the archived project-owned bytes. A tar file
with forged revision metadata is not sufficient source evidence.

The verifier does not mechanically prove that an opaque upstream image was
built from the preserved source graph. Preserve the peer build/release log,
flash log and, where supported, readback, and record the operator's binary-to-
source basis. The image digest, source graph and runtime version reply close
different links in the chain; none alone proves their equivalence.

The tool has no radio defaults. Its `list` and `plan` commands never open a
serial device. Verify the generated corpus, Python KISS tests and Rust replay
through the complete receive-only RNode/Rete ingress before a run:

```sh
PYTHON=python3.13 cargo run --locked -p xtask -- check-rnode-hil-vectors
tooling_run="$run/captures/tooling"
plan_run="$run/captures/plans"
mkdir -p "$tooling_run" "$plan_run"
python3.13 --version | tee "$tooling_run/python-version.txt"
python3.13 -c 'import serial; print(serial.__version__)' \
  | tee "$tooling_run/pyserial-version.txt"
python3.13 "$peer_tool" --corpus "$peer_corpus" list

scenario=raw-header-only
python3.13 "$peer_tool" --corpus "$peer_corpus" plan "$scenario" \
  > "$plan_run/$scenario-plan.json"
```

Qualification uses exactly CPython 3.13.7 and pyserial 3.5; the send path
enforces and records both plus explicit 8N1/no-flow-control serial settings.

Use one fresh Tracker boot and one freshly reset or power-cycled RNode for each
corpus scenario. This is required state isolation: Rete deduplication survives
within a Tracker boot, while RNode has no queue-empty command. Start the
Tracker log, logic analyzer and independent RF observer before either reset,
retain the new activation record, and do not send any other `CMD_DATA` to the
peer. A fresh Tracker boot ensures that the first packet in
`released-python-announce-duplicate` is processed before its deliberate
duplicate; running the standalone announce first on the same boot would change
that expected delta.

Copy `maximum_frame_airtime_us` and `fragment_timeout_us` exactly from that
Tracker activation record. The tool checks the Phase-1 relationship between
them and includes one maximum emitted-frame airtime, including the peer's
longer preamble, before a receiver-timeout wait because its clock starts at
host enqueue rather than receiver capture. This does not bound CSMA delay, so
the target expiry counter remains authoritative. Select
the two airtime limits from the operator's regulatory plan; they are unsigned
basis points (`500` means 5 percent and explicit `0` disables that lock).

```sh
: "${RNODE_PORT:?set the transmitting peer serial port}"
: "${PEER_TX_POWER_DBM:?set an authorized conducted TX power}"
: "${PEER_SHORT_AIRTIME_LIMIT_BP:?set the explicit short-term limit}"
: "${PEER_LONG_AIRTIME_LIMIT_BP:?set the explicit long-term limit}"
: "${TRACKER_MAXIMUM_FRAME_AIRTIME_US:?copy maximum_frame_airtime_us from serial.log}"
: "${TRACKER_FRAGMENT_TIMEOUT_US:?copy fragment_timeout_us from serial.log}"
: "${REGION_BASIS:?record the operator's regulatory basis}"

peer_run="$run/captures/peer/$scenario"
mkdir -p "$(dirname "$peer_run")"
python3.13 "$peer_tool" --corpus "$peer_corpus" send "$scenario" \
  --port "$RNODE_PORT" \
  --target-artifact-mode lab-rx \
  --output-dir "$peer_run" \
  --frequency-hz 915000000 \
  --bandwidth-hz 125000 \
  --spreading-factor 7 \
  --coding-rate-denominator 5 \
  --tx-power-dbm "$PEER_TX_POWER_DBM" \
  --expected-peer-preamble-symbols 24 \
  --receiver-preamble-symbols 18 \
  --short-airtime-limit-basis-points "$PEER_SHORT_AIRTIME_LIMIT_BP" \
  --long-airtime-limit-basis-points "$PEER_LONG_AIRTIME_LIMIT_BP" \
  --receiver-maximum-frame-airtime-us "$TRACKER_MAXIMUM_FRAME_AIRTIME_US" \
  --receiver-fragment-timeout-us "$TRACKER_FRAGMENT_TIMEOUT_US" \
  --post-enqueue-observation-ms 2000 \
  --expected-firmware 1.86 \
  --region-basis "$REGION_BASIS" \
  --antenna-or-load-attached \
  --fresh-peer-reset-ack I_RESET_THE_PEER_FOR_THIS_SCENARIO \
  --fresh-tracker-boot-ack I_STARTED_A_FRESH_TRACKER_BOOT_FOR_THIS_SCENARIO \
  --transmit-ack I_ACCEPT_RF_TRANSMISSION
```

Those numeric modulation values must equal this run's explicit Tracker build;
they are an example profile, not defaults or regional authorization. The two
fresh-state acknowledgments mean the operator started a new Tracker boot and
reset or power-cycled the peer immediately before this invocation; the tool
cannot infer either fact from protocol responses. It first forces and verifies
radio OFF, writes frequency, bandwidth,
SF, CR, power, explicit-header mode and short/long airtime locks, then turns
radio ON and re-queries every setting for which pinned firmware has a
non-mutating query. RNode 1.86 stores airtime limits as a float, so its echo can
be one basis point below the request; the manifest records the effective values
and rejects any less restrictive or larger discrepancy.

Likewise, `--target-artifact-mode` is an operator declaration, not a Tracker
attestation: the peer tool cannot inspect the Tracker, its activation log or its
ELF. A qualifying result binds that declaration to the exact ELF, merged image
and readback hashes in the scenario record and to the mode printed by the fresh
Tracker activation. A mismatch is a failed run even if the peer enqueued every
frame.

The peer preamble is deliberately distinct from the Tracker's configured
18-symbol packet parameter: pinned RNode 1.86 dynamically selects 24 symbols
for SF7/BW125 to meet its 24 ms target. The tool verifies that peer-reported
value and adds the six-symbol airtime difference to timeout waits. This is only
a conservative minimum because CSMA delay is not bounded by the serial
protocol; only the Tracker's exact `pending_expired` delta plus observer timing
closes the expiry scenario. The tool records all decoded and escaped KISS
frames observed while its serial session is open, and hashes its tool and the
single immutable corpus snapshot it parsed. It drains asynchronous error
reports during every wait and for the explicit post-enqueue observation
window, but reports after serial close cannot be captured. A successful
manifest deliberately ends as `enqueued_not_rf_verified`: `CMD_READY` proves
only that the peer queue can accept another item, not that it is empty, that
all queued transmissions completed or that the intended bytes appeared on
air. Close each result only with the Tracker digest/counter record and
independent observer evidence, then reset the peer before any later mode or
configuration change.

On any tool exception after the serial port opens, especially a
`failed_after_enqueue` manifest, assume queued RF can remain live. Keep the
independent observer recording, hardware-reset or power-cycle the peer, and do
not reconfigure it or reuse that failed result as qualification evidence.

Ordinary-mode scenarios supply exact RNS/interface packet bytes and let RNode
choose its sequence nibble and split framing. Promiscuous-mode scenarios supply
the complete physical LoRa frame, including the one-byte RNode header, and
RNode transmits those bytes verbatim. The pinned RNode/SX1262 cannot
deterministically emit a zero-byte physical frame or a frame above 255 bytes;
missing-header and physical-over-MTU rejection therefore remain host-only
tests. A one-byte header-only frame is physically exercisable and represents
zero RNS bytes, not a missing header.

Before opening serial, the tool validates every selected step, canonical hex,
length, digest, mode, wait and physical bound. It permits at most 16 packets
and 5,119 decoded payload bytes per fresh peer—the smallest pinned RNode queue
is 5,120 bytes, but its closing-frame path requires one byte to remain free.

### Boot-bound local DATA

The Tracker identity is deliberately ephemeral in Phase 1. While this exact
boot remains running, copy the 128-hex-character `public_key` and
32-hex-character `destination_hash` from the same activation/heartbeat record.
Generate a one-scenario corpus addressed to that destination:

```sh
: "${TARGET_PUBLIC_KEY_HEX:?copy public_key from this boot}"
: "${TARGET_DESTINATION_HASH_HEX:?copy destination_hash from the same record}"

local_data_run="$run/captures/boot-local-data"
mkdir -p "$local_data_run"
local_data_corpus="$local_data_run/boot-local-data.json"
cargo run --locked -p reticulum-phase1-rx-local-data -- \
  --target-public-key-hex "$TARGET_PUBLIC_KEY_HEX" \
  --target-destination-hash-hex "$TARGET_DESTINATION_HASH_HEX" \
  --output "$local_data_corpus"
shasum -a 256 "$local_data_corpus" \
  > "$local_data_run/boot-local-data.sha256"
python3.13 "$peer_tool" \
  --corpus "$local_data_corpus" plan boot-local-data \
  > "$local_data_run/boot-local-data-plan.json"
```

The generator independently verifies that the public key and destination name
produce the copied hash. It then uses explicitly predictable HIL-only entropy
to produce the same encrypted packet for the same public key. Its plaintext is
non-secret and the corpus records every byte and digest. This is a same-Rete-
stack safety fixture for local event/action suppression, not an independent
RNS interoperability oracle; the released-Python announce scenarios remain
the independent lane. Generation and verification share
`tools/phase1-rx-local-data/src/generator.rs`. The powered-evidence verifier
reads that source from the qualifying bundle, requires its schema-frozen digest,
regenerates from the preserved base corpus and recorded boot identity, and
requires byte-for-byte equality with the captured JSON; semantically equivalent
reformatting is not accepted.

Send it with the same explicit radio/safety arguments shown above, selecting
the custom corpus before the `send` subcommand:

```sh
scenario=boot-local-data
peer_run="$run/captures/peer/$scenario"
python3.13 "$peer_tool" --corpus "$local_data_corpus" \
  send "$scenario" \
  --port "$RNODE_PORT" \
  --target-artifact-mode lab-rx \
  --output-dir "$peer_run" \
  --frequency-hz 915000000 \
  --bandwidth-hz 125000 \
  --spreading-factor 7 \
  --coding-rate-denominator 5 \
  --tx-power-dbm "$PEER_TX_POWER_DBM" \
  --expected-peer-preamble-symbols 24 \
  --receiver-preamble-symbols 18 \
  --short-airtime-limit-basis-points "$PEER_SHORT_AIRTIME_LIMIT_BP" \
  --long-airtime-limit-basis-points "$PEER_LONG_AIRTIME_LIMIT_BP" \
  --receiver-maximum-frame-airtime-us "$TRACKER_MAXIMUM_FRAME_AIRTIME_US" \
  --receiver-fragment-timeout-us "$TRACKER_FRAGMENT_TIMEOUT_US" \
  --post-enqueue-observation-ms 2000 \
  --expected-firmware 1.86 \
  --region-basis "$REGION_BASIS" \
  --antenna-or-load-attached \
  --fresh-peer-reset-ack I_RESET_THE_PEER_FOR_THIS_SCENARIO \
  --fresh-tracker-boot-ack I_STARTED_A_FRESH_TRACKER_BOOT_FOR_THIS_SCENARIO \
  --transmit-ack I_ACCEPT_RF_TRANSMISSION
```

Do not reset the Tracker between corpus generation and transmission. Verify a
`Processed` disposition, one suppressed event, the exact admitted-packet
SHA-256 and no radio output action or Tracker-originated RF. Preserve the
custom `boot-local-data.json`, peer manifest and sibling transcript under
`captures/`, list all three in the malformed scenario record, and bind them to
the relevant checks. The custom-corpus hash must equal the manifest's corpus
digest. It is intentionally exempt from equality with the pinned operator
corpus, but the verifier still requires the exact generated `boot-local-data`
scenario and `lab-rx` target contract.

### Deterministic backpressure artifact

The queue-pressure hook exists only in the separately named
`lab-rx-backpressure` feature and must never be used as the normal lab image.
The corpus labels its sequential CI result as `unstalled_reference_deltas` and
records the deliberately different, feature-bound result under
`target_expectations`; neither result is allowed to masquerade as the other.
The peer tool also fingerprints the complete feature-bound scenario and rejects
altered custom pressure corpora; custom ordinary corpora remain available for
the boot-bound local-DATA fixture.
For the example SF7/BW125 profile, seven seconds is longer than the compiled
fragment timeout and leaves 23 seconds of the 30-second watchdog period. The
const policy rejects a duration at or below the active fragment timeout or
above 25 seconds; the environment value has no default.

The preparation command has already built the separately named
`reticulum-heltec-tracker-v2-lab-rx-backpressure` binary in an isolated target
directory and preserved it below `backpressure-artifact/`. Reverify the whole
bundle, then hash-check and flash only those preserved pressure bytes:

```sh
bp_artifact="$hil_bundle/backpressure-artifact"
bp_run="$run/captures/backpressure"
mkdir -p "$bp_run"
cargo run --locked -p xtask -- phase1-rx-hil-artifacts verify \
  --bundle "$hil_bundle"
(cd "$bp_artifact" && shasum -a 256 -c firmware.sha256)
(cd "$bp_artifact" && shasum -a 256 -c flash-image.sha256)
test "$(cat "$bp_artifact/flash-image-address.txt")" = 0x00000000
test "$(cat "$bp_artifact/flash-image-bytes.txt")" = \
  "$(wc -c < "$bp_artifact/flash-image.bin" | tr -d ' ')"

espflash write-bin \
  --port "$ESPFLASH_PORT" --chip esp32s3 --non-interactive \
  0x00000000 "$bp_artifact/flash-image.bin" 2>&1 | tee "$bp_run/flash.log"
espflash read-flash \
  --port "$ESPFLASH_PORT" --chip esp32s3 --non-interactive \
  0x00000000 "$(cat "$bp_artifact/flash-image-bytes.txt")" \
  "$bp_run/flash-readback.bin" 2>&1 | tee "$bp_run/readback.log"
cmp "$bp_artifact/flash-image.bin" "$bp_run/flash-readback.bin"
shasum -a 256 "$bp_run/flash-readback.bin" \
  > "$bp_run/flash-readback.sha256"
```

Start the Tracker log and independent observer before the fresh boot. The
activation must say `mode=lab-rx-backpressure-hil`, hook enabled and
`configured_stall_us=7000000`. Copy its timing values, freshly reset the peer,
then send the feature-bound four-frame scenario with the same RF/safety values
used above:

```sh
scenario=raw-backpressure-four-frame
peer_run="$bp_run/peer/$scenario"
python3.13 "$peer_tool" --corpus "$peer_corpus" send "$scenario" \
  --port "$RNODE_PORT" \
  --target-artifact-mode lab-rx-backpressure-hil \
  --output-dir "$peer_run" \
  --frequency-hz 915000000 \
  --bandwidth-hz 125000 \
  --spreading-factor 7 \
  --coding-rate-denominator 5 \
  --tx-power-dbm "$PEER_TX_POWER_DBM" \
  --expected-peer-preamble-symbols 24 \
  --receiver-preamble-symbols 18 \
  --short-airtime-limit-basis-points "$PEER_SHORT_AIRTIME_LIMIT_BP" \
  --long-airtime-limit-basis-points "$PEER_LONG_AIRTIME_LIMIT_BP" \
  --receiver-maximum-frame-airtime-us "$TRACKER_MAXIMUM_FRAME_AIRTIME_US" \
  --receiver-fragment-timeout-us "$TRACKER_FRAGMENT_TIMEOUT_US" \
  --post-enqueue-observation-ms 2000 \
  --expected-firmware 1.86 \
  --region-basis "$REGION_BASIS" \
  --antenna-or-load-attached \
  --fresh-peer-reset-ack I_RESET_THE_PEER_FOR_THIS_SCENARIO \
  --fresh-tracker-boot-ack I_STARTED_A_FRESH_TRACKER_BOOT_FOR_THIS_SCENARIO \
  --transmit-ack I_ACCEPT_RF_TRANSMISSION
```

The first split half triggers one async ingress-only stall. During it, the
remaining three frames must produce exact deltas `offered=3`, `queued=2` and
`dropped=1`. Completion must report `expiry_observed=true` before queued-frame
service; the two queued captures must subsequently be rejected by the existing
expiry watermark, with no completed packet or Rete ingress call. Any different
delta is a failed pressure run, not permission to tune queue depth. Verify all
four peer frames and no Tracker transmission independently. Reset the peer and
restore the preserved normal lab image before every other corpus scenario.
The passing `configured-stall-seven-seconds` check must record
`{"kind":"configured-stall-microseconds","microseconds":7000000}`, and the
passing counter check must use the exact `backpressure-counters` observation
shown in the result-manifest example. These structured values are checked
directly; their bound serial/peer captures remain the reviewable source.

## Analyzer channel map

Capture at least:

| Signal | GPIO | Requirement |
| --- | ---: | --- |
| KCT8103L CSD | 4 | low at boot; rises only after VFEM settle; low on teardown |
| KCT8103L CTX | 5 | must never rise |
| VFEM power | 7 | low at boot and before reset; provisional settle evidence |
| SX1262 NSS | 8 | SPI chip select |
| SPI SCK | 9 | mode 0, 1 MHz |
| SPI MOSI | 10 | command decoder source |
| SPI MISO | 11 | status/data decoder source |
| SX1262 RESET | 12 | low at boot and fault teardown |
| SX1262 BUSY | 13 | initialization and command timing |
| SX1262 DIO1 | 14 | receive IRQ timing |
| KCT8103L PA_CPS | SX1262 DIO2 / accessible C92-1 point | remains low throughout RX-only operation; DIO2 is the sole driver |

If channel count is limited, perform synchronized safety-pin and SPI captures
with a common reset/marker and document the split. The supplied V2.3 schematic
hidden netlist connects `PA_CPS` only between `U12-12` (SX1262 DIO2), `U10-5`
(KCT8103L CPS) and `C92-1`; net `46` separately joins `U6-52` to header
`P3-17`. The evidence files are
`reference/heltec_tracker_v2.3_schematic.pdf` with SHA-256
`148672bdc7ca8646d9de5d3e9a9e58c647b1c46bd5b0b68616efa80dbd225ea7`
and `reference/heltec_tracker_v2.3_pin_map.png` with SHA-256
`81b2e47d94dd0d3a3749c9b89ba46f22f343a8eab5d979bff721454bf4a0a5a3`.
Probe the actual `PA_CPS` net with a high-impedance probe and never drive it as
a test. An optional unpowered non-continuity check from the GPIO46 header pad
to `PA_CPS` may corroborate board revision, but GPIO46 is neither connected to
the RF path nor a powered firmware-interlock gate.

Decode every MOSI transaction from reset through initialization, receive,
malformed input and fault handling. Required or allowed commands include:

- `0x97 0x02`: DIO3 TCXO control at 1.8 V;
- `0x9d 0x01`: DIO2 RF-switch control;
- `0x8e`: `SetTxParams`, used by pinned `lora-phy` initialization without
  transmitting; and
- private SX126x sync word `0x1424`.

The following must never appear:

- `0x83` `SetTx`;
- `0xd1` continuous wave;
- `0xd2` continuous preamble; or
- `0x0e` `WriteBuffer`.

An opcode byte observed as payload or register data is not a command. Decode
transaction boundaries and the first command byte after NSS assertion.

## Traffic scenarios

Run each scenario with a labeled peer timestamp and retain the exact bytes the
peer submitted. For every RNS-admitted completed packet, compare SHA-256 of the
exact reassembled bytes with the per-packet `last_raw_packet_sha256` Tracker
record before sending the next stimulus. Packets rejected at 501–508 bytes do
not produce a digest because they never cross the RNS admission boundary.

1. **Cold boot and silence**
   - Verify inert pin levels precede all allocation, entropy and Rete setup.
   - Verify the configured profile and radio constants in the activation log.
   - Observe at least two 60-second heartbeats with stable current heap use.
2. **Single physical frame**
   - Run `raw-header-only`, `raw-single-1`, `raw-single-253` and
     `raw-single-254`.
   - Verify physical frame/byte totals, length, RSSI/SNR and Rete disposition.
3. **Split packet**
   - Run `rnode-split-255`, `rnode-split-256`, `rnode-split-499` and
     `rnode-exact-500`.
   - Verify first-half pending state, completed length, conservative RSSI/SNR
     and exact raw SHA-256.
4. **Fragment expiry and replacement**
   - Run `raw-orphan-split`, `raw-split-replacement` and
     `raw-nonsplit-discards-pending`.
   - Verify pending/replaced/expired counters and no cross-packet splice.
5. **Physical-over-RNS boundary**
   - Run `rnode-501-through-508`; the sender uses ordinary RNode mode because
     firmware accepts the full 508-byte hardware MTU even though RNS APIs
     normally cap packets at 500.
   - Verify `packets_too_long` increments and Rete ingress does not.
6. **Malformed and semantic rejection**
   - Run `raw-duplicate-first-half`, `raw-reordered-same-sequence`,
     `released-python-announce`, `released-python-announce-duplicate` and the
     generated `boot-local-data` corpus. The `rnode-exact-500` result from step
     3 also closes its invalid-LINKREQUEST semantic expectation; do not resend
     it on the same boot.
   - List that exact preserved `rnode-exact-500` manifest/transcript pair in
     this malformed record as well as the split record, bind its manifest to
     `all-corpus-cases-run` and `all-output-actions-suppressed`, and make the
     malformed record interval encompass the shared step-3 invocation and all
     later malformed invocations. This is the sole permitted cross-record peer
     evidence reuse.
   - True missing-header RF input is physically impossible and remains covered
     by the host boundary tests.
   - Verify no output action reaches the radio and no Tracker-originated RF is
     observed.
7. **Bounded backpressure**
   - Flash only the separately preserved `lab-rx-backpressure` artifact and run
     `raw-backpressure-four-frame` exactly as specified in
     [Deterministic backpressure artifact](#deterministic-backpressure-artifact).
   - Verify the first split half triggers the one-shot async ingress stall, then
     exact during-stall deltas `offered=3`, `queued=2`, `dropped=1` and
     `expiry_observed=true`. Both queued frames must be rejected by the original
     deadline watermark, with bounded heap, no completed packet and no Rete
     ingress call. Any other delta fails the scenario.
   - Restore the preserved normal `lab-rx` image before continuing. The
     instrumented artifact is not a general receive image and must not be used
     for the other scenarios or soak.
8. **Returned radio fault**
   - The compile-gated `lab-rx-returned-fault-hil` source, one-byte sticky
     evidence state, host integration tests and clean-tree preparation and
     verification support for both policies are implemented. The scenario
     remains `not-run` until the closure bundle has actually been prepared and
     verified and the images have been exercised on powered hardware. Reverify
     the complete immutable closure bundle immediately before every closure
     image flash, including each returned-fault, electrical and journal run:

     ```sh
     cargo run --locked -p xtask -- phase1-rx-closure-artifacts verify \
       --bundle "$closure_bundle"
     ```

     Then flash only the preserved image below
     `$closure_bundle/artifacts/returned-fault-one-boot` or
     `$closure_bundle/artifacts/returned-fault-repeat-until-quarantine`; put
     flash/readback, serial, peer and observer evidence only in corresponding
     sibling directories below `$run/captures`.
   - The SPI decorator must fail the first `GetIrqStatus` after a successfully
     forwarded `SetRx`, leaving the board-owned TX-opcode firewall outside the
     decorator. After one benign trigger frame, require the sticky fired record
     and `Receive / Radio(Spi)` with `cleanup=None`, then verify the committed
     pending journal record, CTX/CSD/VFEM/reset trace and digital-core software
     reset. Capture the complete ROM/boot pin transient; source/static evidence
     and the reset primitive alone are not electrical containment proof.
     Use the schema-3 `raw-returned-fault-trigger` corpus scenario only with the
     `returned-fault-one-boot` artifact. Its expectation is bound to policy
     `one-boot`; it must not be replayed against the normal, pressure or repeat
     artifact.
   - Exercise a protected retained-write failure separately. If the returned
     fault cannot be committed and verified, require
     `action=immediate_rf_inert_quarantine`, supervisor watchdog disabled and no
     `CoreSw`; firmware must not assume that the first poison word stuck or that
     the next boot will reject the old journal.
   - First use policy `one-boot` for the single-fault trace. Its pristine
     power-on boot may arm once; the following correlated `CoreSw` boot must
     acknowledge without double count and remain RF-inert before SPI, radio or
     watchdog construction instead of rearming the trigger.
   - Policy `repeat-until-quarantine` is the deliberate exception to
     fresh-Tracker-per-corpus-scenario isolation. Flash the preserved repeat
     artifact from a true-power pristine baseline, keep the Tracker powered,
     and invoke the exact
     `raw-returned-fault-repeat-until-quarantine` scenario once on each of three
     armed activations. Before each invocation, reset or power-cycle the RNode,
     start a distinct peer evidence directory and use this exact artifact-mode
     declaration with the same explicit RF, timing, authorization and safety
     arguments from the pinned send command above:

     ```sh
     scenario=raw-returned-fault-repeat-until-quarantine
     activation=1 # repeat with 2 and 3 only after the checks below
     repeat_run="$run/captures/returned-fault-repeat-until-quarantine"
     peer_run="$repeat_run/peer/activation-$activation"
     python3.13 "$peer_tool" --corpus "$peer_corpus" send "$scenario" \
       --port "$RNODE_PORT" \
       --target-artifact-mode \
         'lab-rx-returned-fault-hil;trigger=get-irq-status-after-set-rx;policy=repeat-until-quarantine' \
       --output-dir "$peer_run" \
       --frequency-hz 915000000 \
       --bandwidth-hz 125000 \
       --spreading-factor 7 \
       --coding-rate-denominator 5 \
       --tx-power-dbm "$PEER_TX_POWER_DBM" \
       --expected-peer-preamble-symbols 24 \
       --receiver-preamble-symbols 18 \
       --short-airtime-limit-basis-points "$PEER_SHORT_AIRTIME_LIMIT_BP" \
       --long-airtime-limit-basis-points "$PEER_LONG_AIRTIME_LIMIT_BP" \
       --receiver-maximum-frame-airtime-us \
         "$TRACKER_MAXIMUM_FRAME_AIRTIME_US" \
       --receiver-fragment-timeout-us "$TRACKER_FRAGMENT_TIMEOUT_US" \
       --post-enqueue-observation-ms 2000 \
       --expected-firmware 1.86 \
       --region-basis "$REGION_BASIS" \
       --antenna-or-load-attached \
       --fresh-peer-reset-ack I_RESET_THE_PEER_FOR_THIS_SCENARIO \
       --fresh-tracker-boot-ack \
         I_STARTED_A_FRESH_TRACKER_BOOT_FOR_THIS_SCENARIO \
       --transmit-ack I_ACCEPT_RF_TRANSMISSION
     ```

     After invocation 1, wait for and correlate the `CoreSw` activation, then
     require an acknowledged pending record without double count, retained
     streak/total `1/1` and a rearmed radio before setting `activation=2`.
     Repeat the same gate after invocation 2 for streak/total `2/2` before
     setting `activation=3`. After invocation 3, wait for and correlate its
     `CoreSw` activation and require streak/total `3/3` plus permanent RF-inert
     quarantine before a fourth SPI, radio or watchdog construction. Never use
     the one-boot `raw-returned-fault-trigger` scenario for this repeat-policy
     sequence. Verify that a cold power cycle clears quarantine, while
     recording that ESP32-S3 reason `0x01` also covers brownout/super-WDT and
     therefore prevents proof that power removal is the only clearing event.
     Also qualify the ten-minute healthy lease. Keep the run supervised even
     though quarantine is designed to contain the repeated fault.
   - Separately induce the configured TIMG0 supervisor watchdog without a
     pending returned-radio record. Each following boot must report
     `CoreMwdt0`, transactionally advance the same retained fault-reset streak
     before constructing SPI/radio/watchdog, and quarantine on the third
     combined returned-radio or supervisor-watchdog fault reset. An unexpected
     reset classification, a journal write that does not fail closed, or a
     fourth radio activation fails the scenario.
9. **Corrupt and torn retained journal**
   - The RF-inert `lab-rx-reset-journal-corrupt-hil` and
     `lab-rx-reset-journal-torn-hil` source artifacts are implemented with
     exact slot/word or write-ordinal selectors. They construct no SPI, radio,
     executor timer or supervisor watchdog. The closure command prepares and
     verifies the exact representative slot 0/word 4 and write-ordinal 9
     images. This scenario remains `not-run` until that bundle has actually
     been preserved and both selectors complete their two-boot powered runs.
   - From a true-power pristine baseline, run those two preserved representative
     selectors. Preserve the first boot's mutation/torn-write evidence and
     `CoreSw`, then prove that the following boot reports
     `CorruptOrTornJournal` with no history and enters the ordinary RF-inert
     quarantine before peripheral construction. A source test, linked section
     inventory or single-boot log is not retained-state evidence. No other
     slot, word or write ordinal is qualified by this bundle; adding one
     requires separately reviewed bundle support, not a manual development
     build.
10. **Electrical regulator and RX-gain matrix**
   - Exercise all four preserved electrical artifacts on at least two distinct
     Tracker V2.3 board samples. Use the same antenna/load, calibrated current
     instrument, analyzer channel map and RF-observer attribution setup for each
     selection. Record environmental or fixture changes; do not combine an
     exploratory build with this matrix.
   - For each board and each exact mode below, reverify the entire closure bundle
     immediately before flashing, hash-check the selected preserved artifact,
     read back the complete merged image and put every mutable result below that
     board/mode capture directory:

     ```sh
     set -euo pipefail
     : "${ESPFLASH_PORT:?set the Tracker serial port}"
     board_sample=tracker-v23-a # repeat with at least one other physical board
     electrical_mode=electrical-ldo-unboosted
     # Repeat exactly: electrical-ldo-boosted, electrical-dcdc-unboosted,
     # electrical-dcdc-boosted.
     electrical_artifact="$closure_bundle/artifacts/$electrical_mode"
     electrical_run="$run/captures/electrical-matrix/$board_sample/$electrical_mode"
     mkdir -p "$electrical_run"

     cargo run --locked -p xtask -- phase1-rx-closure-artifacts verify \
       --bundle "$closure_bundle"
     (cd "$electrical_artifact" && shasum -a 256 -c firmware.sha256)
     (cd "$electrical_artifact" && shasum -a 256 -c flash-image.sha256)
     test "$(cat "$electrical_artifact/flash-image-address.txt")" = 0x00000000
     test "$(cat "$electrical_artifact/flash-image-bytes.txt")" = \
       "$(wc -c < "$electrical_artifact/flash-image.bin" | tr -d ' ')"

     espflash write-bin \
       --port "$ESPFLASH_PORT" --chip esp32s3 --non-interactive \
       --after no-reset \
       0x00000000 "$electrical_artifact/flash-image.bin" \
       2>&1 | tee "$electrical_run/flash.log"
     espflash read-flash \
       --port "$ESPFLASH_PORT" --chip esp32s3 --non-interactive \
       --after no-reset \
       0x00000000 "$(cat "$electrical_artifact/flash-image-bytes.txt")" \
       "$electrical_run/flash-readback.bin" \
       2>&1 | tee "$electrical_run/readback.log"
     cmp "$electrical_artifact/flash-image.bin" \
       "$electrical_run/flash-readback.bin"
     shasum -a 256 "$electrical_run/flash-readback.bin" \
       > "$electrical_run/flash-readback.sha256"
     ```

   - Arm the serial recorder, current acquisition, logic analyzer and independent
     RF observer before a controlled true-power boot of each selection. Retain
     the complete activation/heartbeat log, analyzer source and decode, calibrated
     current trace and observer index in the mode directory above. Verify that
     the activation identity names the selected regulator/gain mode, safety-pin
     ordering matches the analyzer requirements, CTX and PA_CPS remain low, and
     no prohibited command or Tracker-originated RF occurs.
   - LDO selections must omit `SetRegulatorMode`; DC-DC selections must emit
     `0x96 0x01` after standby and before RF-switch control. The RX-gain register
     write must end in `0x94` for unboosted or `0x96` for boosted. Bind
     `all-four-selections-measured` to both current and analyzer evidence,
     `calibrated-current-measurement`, `more-than-one-board-sample` and
     `no-single-sample-policy-change` to the calibrated current evidence, and
     `safety-pin-timing-measured` to analyzer evidence. Summarize cross-board
     results below `$run/captures/electrical-matrix/`; no single sample may
     change regulator or RX-gain policy.
11. **24-hour receive soak**
   - Mix silence, valid single/split packets and hostile framing. At minimum,
     freshly invoke `raw-single-1`, `rnode-split-256` and
     `raw-duplicate-first-half` once each during the soak record's interval and
     preserve each peer manifest/transcript pair. Do not reuse any manifest or
     transcript from an earlier powered record. Additional ordinary scenarios
     from the copied pinned corpus are allowed only with target mode `lab-rx`;
     repeated invocations must have distinct transcript digests and distinct
     start/finish time pairs.
   - Retain all heartbeats and demonstrate stable heap current/max use, bounded
     counters, continuing five-second Rete maintenance and no TX opcode or
     on-air packet.
   - Run the source-attributable observer continuously. If it segments output,
     retain a gap-free timestamp/index manifest and hash every segment.
   - Record the uninterrupted elapsed time on
     `continuous-duration-at-least-24h` as
     `{"kind":"elapsed-seconds","seconds":...}` and bind that check to the
     serial capture plus the RF-observer index. A passing value must be at least
     86,400 seconds; a larger wall-clock interval does not excuse an observer
     gap, which remains a separate failed check.

The generic peer and malformed-frame inputs above are checked in and replayed
by CI. The schema-3 corpus also binds its backpressure and returned-fault
stimuli to their exact target modes; their hooks are checked as separate
software artifacts. The local-DATA builder is host-tested against a decrypting
target. Returned-fault/electrical host tests, all six radio-bearing closure
links/inspections and the two representative retained-journal links/inspections
are development evidence only. The local clean-tree commands can preserve and
verify the eight closure artifacts, but only an actual immutable bundle closes
artifact provenance, and only the corresponding powered captures close the
backpressure, fault-injection, retained-state, electrical or soak gates.

## Memory and liveness evidence

The lab feature enables `esp-alloc` internal heap statistics. Activation and
heartbeat records provide total, current, free and maximum-used heap. Pinned
`esp-alloc` does not expose the largest free block.

After it is enabled, the 30-second TIMG0 MWDT is fed only after the main owner
completes a selected wake, synchronous Rete work and diagnostics. It catches
subsequent panics and whole-main or executor stalls. The following `CoreMwdt0`
boot is counted transactionally in the retained fault-reset streak before radio
construction, preventing a repeatable watchdog loop from re-energizing the
radio indefinitely. It cannot detect a radio task waiting indefinitely for
DIO1 while the main task continues normally; radio silence is itself a valid
indefinite DIO1 wait. Each BUSY-low wait is separately bounded to 100 ms and
returns the typed radio fault/reset path.

Pinned `esp-rtos` does not expose a shared executor-stack high-water API, so
the lab image installs a strong leaf `__zero_bss` hook that paints only the
linked CPU0/shared-executor stack below the startup SP, skips the runtime guard
and leaves an exact `.noinit` marker. Each heartbeat uses the host-tested
volatile scanner to report monotonic high-water use, remaining margin and
sticky guard/scan validity. Treat `guard_intact=false`, `scan_valid=false` or
unstable soak headroom as failure. This does not cover future separately
allocated RTOS task stacks, and compiler-emitted frame inventory alone remains
non-transitive static context rather than runtime margin.

The normal image remains fixed to LDO mode and unboosted RX. The separately
named electrical binary can now produce all four compile-time combinations and
prints both selections in its artifact identity. Those variants are measurement
fixtures, not production policy. The closure command preserves and verifies
each distinct ELF and merged image, but the electrical gate remains open until
an actual immutable bundle is used for calibrated powered measurements on more
than one board. Editing board constants in a dirty worktree remains
non-evidence.

## Pass/fail record

For every scenario record `pass`, `fail` or `not-run`, the start/end timestamps,
artifact names and a short reason. Any of these is an immediate failure:

- CTX rises;
- PA_CPS rises during RX-only operation;
- a prohibited SX1262 command is decoded;
- the independent observer sees a Tracker-originated packet;
- a completed length above 500 reaches Rete;
- heap use grows without stabilizing during the soak;
- the stack guard or scanner becomes invalid, or high-water use exhausts the
  recorded usable margin;
- the watchdog resets during normal traffic/silence; or
- a required capture is missing or cannot be tied to the preserved ELF.

Do not update board timing, regulator or RX-boost policy from a single sample.
Repeat the electrical measurements on more than one Tracker V2.3 unit.
