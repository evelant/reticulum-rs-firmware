# Partition tables and storage-HIL runbook

For the normal E290 build and flash path, use
[`docs/getting-started/firmware-e290.md`](../docs/getting-started/firmware-e290.md).
This file is the authoritative partition-layout reference and also retains
historical storage-HIL procedures.

`heltec-vision-master-e290-node.csv` is the first 16 MiB permanent-node layout:

| Partition | Range | Size | Product state |
| --- | --- | ---: | --- |
| `nvs` | `0x009000..0x00f000` | 24 KiB | ESP/NVS reserve |
| `phy_init` | `0x00f000..0x010000` | 4 KiB | ESP PHY reserve |
| `factory` | `0x010000..0x610000` | 6 MiB | Permanent node image |
| `node_identity` | `0x610000..0x612000` | 8 KiB | Wired immutable identity mirrors |
| `announce_clock` | `0x612000..0x614000` | 8 KiB | Wired boot-epoch mirrors |
| `api_credentials` | `0x614000..0x616000` | 8 KiB | Wired boot-mounted plaintext two-sector credential store |
| `ble_bond` | `0x616000..0x618000` | 8 KiB | Wired boot-mounted authenticated BLE bond store |
| `device_config` | `0x618000..0x630000` | 96 KiB | Raw-NOR configuration arena; first 8 KiB assigned to network configuration and next 8 KiB to the LXMF collection watermark |
| `node_journal` | `0x630000..0x730000` | 1 MiB | Schema-3/physical-2 resident submission runtime |
| `message_store` | `0x730000..0x930000` | 2 MiB | Wired raw-RNS inbox qualification slot; not an LXMF store |
| `lxmf_store` | `0x930000..0xb30000` | 2 MiB | Wired append-only LXMF store with mount-gated opportunistic and responder direct admission |
| unpartitioned | `0xb30000..0x1000000` | 4.8125 MiB | OTA/layout decision |

The journal and message-store offsets are unchanged. The previously unwired
`device_config` reservation now yields dedicated 8 KiB raw-NOR ranges for
`api_credentials` and one authenticated `ble_bond`, leaving a 96 KiB raw-NOR
configuration arena. Its first 8 KiB (`0x618000..0x61a000`) is assigned to the
network-configuration store, its next 8 KiB (`0x61a000..0x61c000`) to the
power-loss-safe LXMF collection watermark, and the remaining 80 KiB stays
reserved. The security-authority and mailbox-watermark ranges are validated
and boot-mounted.
[ADR 0009](../docs/adr/0009-device-api-credential-store-and-pairing.md)
defines the credential store's implemented two-sector plaintext developer/HIL
format and pairing policy;
[ADR 0019](../docs/adr/0019-secure-ble-appliance-onboarding.md) defines the
separate bond authority. Credential recovery remains the first boot flash
mutation. BLE bond mount is strictly read-only and never auto-recovers or
provisions damaged media; a pairing-time bond commit uses the dedicated
two-sector store's commit-last exact-readback and remount contract. The journal,
message store, identity, announce clock, credential, and BLE bond ranges use
ESP-IDF's standard `data,undefined` subtype. No unsupported numeric subtype is
used to imply application ownership.

The journal's current physical format 2 keeps the same 1 MiB range and two
`0x7f000`-byte banks. Each bank holds 774 672-byte slots with a 544-byte
canonical body and a 64-byte erased tail, reserving at most 154 complete
five-record submission lifetimes. Semantic schema 3 keeps generic RNS DATA at
383 plaintext bytes and separately retains one exact complete signed
LXMF wire through 431 bytes without selecting its delivery method.
Schema-2/physical-1 media is not rewritten in place: development migration
must erase and verify only
`0x630000..0x730000`, then boot the explicit
`journal-schema3-dev-reprovision` image. The native Expo app simultaneously
moves to `reticulum-lxmf-chat-alpha-schema3.sqlite3`; its separate credential
file remains valid.

The separate `lxmf_store` range preserves every byte and meaning of
`message_store`. Boot binds it to the same eFuse-derived physical device ID,
mounts format 1 through the sole flash owner, and builds an exact 512-slot
opaque index in explicitly allocated PSRAM. Current source registers the
mount-gated `lxmf.delivery` destination, admits signed opportunistic and
responder-side direct Link DATA, and releases retained proofs only after a new
commit or a fresh `AlreadyDurable` replay. Powered evidence remains narrower
than that complete source composition.

The complete `message_store` range is now bound to the deliberately temporary
[ADR 0011](../docs/adr/0011-durable-rns-inbox-qualification.md) format-1
qualification store. It may retain one decrypted RNS DATA item of at most 383
bytes in one 576-byte commit-last record at relative offset zero; every later
byte in the exact 2 MiB range must remain erased. Mount is read-only and any
torn, corrupt, unknown, wrongly bound, or noncanonical media disables inbox
service without repair. The format stores destination and payload in plaintext
and provides no acknowledgement, deletion, erase, reclamation, or garbage
collection. It is not the final message queue, an LXMF store, or evidence that
the eventual product format fits this encoding.

The final post-audit API 1.2 merged image, SHA-256
`ba10b04408368c3f5cbcc91f5d514f454595a7812986764c1e95ef528cc71f03`,
matched exact address-zero readbacks on both E290s. Its bounded powered inbox
run committed one maximum-size item, returned it through authenticated peek
before and after hard reset, and preserved it while dropping a newer valid
packet. The final exact 2,097,152-byte `message_store` readback had SHA-256
`f50dab680d46ef20cd875eff778296a3b92f9d7eef34684f29eedc10b468d724`;
the first 576 bytes were the canonical record and every remaining byte was
`0xff`. This is qualification evidence for the temporary one-entry format, not
authorization to treat the range as a general or LXMF message store.

## E290 raw-inbox fault fixtures

The host-only `e290-rns-inbox-fixture` command generates one complete,
deterministic 2 MiB `message_store` image. It first creates a canonical record
through the public `reticulum-rns-inbox-store` mount/admission API, then applies
one reviewed physical-state transformation. It does not open or write a board.
The parser requires an absent output path, one 12-character lowercase MAC
without separators, and exactly one mode. Output is create-new, mode `0600`,
synchronized before success, and summarized only by mode, length, and SHA-256.

```sh
umask 077
cargo +stable run --locked -p xtask -- e290-rns-inbox-fixture \
  --output /secure/absent-message-store.bin \
  --source-mac aca704e13e88 \
  interrupted-commit
```

`--source-mac` is the physical binding encoded in the record, not necessarily
the board on which a deliberate foreign-binding test will program it. For MAC
`ac:a7:04:e1:3e:88`, the fixed nonsecret fixture record produces:

| Mode | Exact state | 2 MiB SHA-256 |
| --- | --- | --- |
| `interrupted-claim` | First 16 bytes of the 32-byte claim programmed; every later byte `0xff` | `4b9e6dad1415850588c001b17053e893ab1316aaa1b6d584082170d049f871f0` |
| `interrupted-commit` | Exact claim, body, and digest at `0..544`; commit and remainder at `544..0x200000` entirely `0xff` | `a8a8d40f63a69c7e3df59f4af1960f241f464566a5ae9251c12209eb3334c66a` |
| `invalid-digest` | Exact committed record with one programmed digest bit monotonically cleared | `bb24e892d435a0b6888cc16f8733f096015a36f0f19dcd8a22e0978602e55ad5` |
| `committed` | Canonical matching committed record | `dee21d3c72a914ac00627c49a119631999dc9e986ce18897b9a171254c79561b` |

`committed` mounts occupied only under the matching device/range binding. The
powered foreign-binding case generated this `3e:88` image and deliberately
programmed it on `ac:a7:04:e1:3f:88`. A fixture generated for another MAC has a
different encoded binding, digest, and whole-image hash.

The raw port-based fixture programming used to collect the 2026-07-19 powered
matrix is historical evidence, not a current executable procedure. Those
bounded runs targeted only `message_store` and captured a complete partition
readback before boot, but their mutable port handoff does not meet the current
identity-attribution standard. The old command block is intentionally omitted
so it cannot be mistaken for an approved rerun path.

A future powered rerun must fail closed until the project-owned E290
qualification helper gains a separately reviewed, identity-bound `write-region`
operation. The current helper intentionally has no arbitrary `write-region`;
do not substitute direct `espflash board-info`, `write-bin`, or `read-flash`
commands. The new operation must:

- resolve an exact uppercase native-USB serial for every phase and require the
  intended eFuse MAC, ESP32-S3, 16 MiB flash, disabled secure boot, and disabled
  flash encryption;
- copy, hash, and retain a read-only descriptor for the exact 2 MiB fixture,
  then restrict the write to `message_store` without touching identity,
  announce clock, credentials, configuration, or journal;
- validate the write action's own DeviceInfo, then capture unchanged USB
  mapping and loader-preserving post-write board information;
- create a private retained-inode readback of the exact range, validate the read
  action identity and mapping, and require exact byte count, fixture hash, and
  full-file equality before publishing durable verified evidence.

Protect a fresh full-flash backup and keep the 915 MHz antenna attached for any
such rerun. The board must remain in the loader with no hard reset or ordinary
firmware boot from preflight through verified-evidence publication; reset it
deliberately only after that evidence is complete.

The 2026-07-19 powered matrix covered partial claim, exact pre-commit, invalid
digest, and foreign binding. In every boot, authenticated capabilities reported
inbox availability/max payload `0/0`; status and peek returned
`CapabilityUnavailable` (code 7); peek left its requested output absent; one
fresh direct peer DATA packet reached `Delivered` through the receiver's proof
transmission; and the post-traffic complete store remained byte-identical to the
fixture. The respective after-traffic hashes were the four hashes above. This is
target evidence for read-only fail-closed mount, API suppression, no volatile
fallback, and no repair/admission write while disabled. It is one bounded direct
DATA/decrypt/proof exchange per state, not sustained routing, forwarding,
multi-hop behavior, or a physical power cut.

Normal firmware intentionally cannot repair these states. After evidence is
captured and before returning the development board to an empty inbox, explicitly
erase and verify only `message_store`. The exact all-erased 2 MiB SHA-256 is
`4bda3a28f4ffe603c0ec1258c0034d65a1a0d35ab7bd523a834608adabf03cc5`:

```sh
set -euo pipefail
umask 077
EXPECTED_USB_SERIAL=AC:A7:04:E1:3E:88
EXPECTED_MAC=ac:a7:04:e1:3e:88
EXPECTED_FLASH_BYTES=16777216
EVIDENCE_DIR=/secure/e290-message-store-reset
ERASE_EVIDENCE_PREFIX="$EVIDENCE_DIR/message-store-erase"
READ_EVIDENCE_PREFIX="$EVIDENCE_DIR/message-store-erased-read"
ERASED="$EVIDENCE_DIR/message-store-erased.bin"
test ! -e "$EVIDENCE_DIR"
mkdir -m 700 "$EVIDENCE_DIR"

python3.13 interop/python/e290_qualification_host.py erase-region \
  --usb-serial "$EXPECTED_USB_SERIAL" --expected-mac "$EXPECTED_MAC" \
  --expected-flash-bytes "$EXPECTED_FLASH_BYTES" \
  --evidence-prefix "$ERASE_EVIDENCE_PREFIX" \
  --offset 0x730000 --length 0x200000
python3.13 interop/python/e290_qualification_host.py read-region \
  --usb-serial "$EXPECTED_USB_SERIAL" --expected-mac "$EXPECTED_MAC" \
  --expected-flash-bytes "$EXPECTED_FLASH_BYTES" \
  --evidence-prefix "$READ_EVIDENCE_PREFIX" \
  --offset 0x730000 --length 0x200000 --output "$ERASED"
test -f "${ERASE_EVIDENCE_PREFIX}.erase-region.verified.json"
test -f "${READ_EVIDENCE_PREFIX}.read-region.verified.json"
test "$(wc -c < "$ERASED" | tr -d ' ')" = 2097152
test "$(shasum -a 256 "$ERASED" | awk '{ print $1 }')" = \
  4bda3a28f4ffe603c0ec1258c0034d65a1a0d35ab7bd523a834608adabf03cc5
test "$(LC_ALL=C tr -d '\377' < "$ERASED" | wc -c | tr -d ' ')" = 0
```

The helper resolves the current port from the exact uppercase native-USB serial
for each action, requires the matching eFuse MAC and 16 MiB security-qualified
target, and refuses existing output or evidence paths. `erase-region` uses its
identity-reporting all-`0xff` write/readback workflow rather than native
`espflash erase-region`; the independent `read-region` capture remains private
and owner-read-only after verification. Both actions leave the E290 in the
loader. Reset it deliberately only after this evidence is complete and the
ordinary firmware is ready to boot.

### Same-boot terminal-commit suppression HIL

The separate non-default feature `rns-inbox-commit-fault-hil` changes no package
dependency. It wraps only an inbox admission, forwards claim and body/digest
writes, returns success without programming write three, then lets production
readback detect the absent terminal commit and execute its normal quarantine
path. The feature is mutually exclusive with
`journal-schema3-dev-reprovision`; never combine them, use `--all-features`, or
retain this image as the ordinary firmware.

Run the complete E290 build gate in
[`docs/e290-node.md`](../docs/e290-node.md). Its focused fixture/feature checks
include:

```sh
cargo +stable test --locked -p xtask e290_rns_inbox_fixture
cargo +stable test --locked \
  -p reticulum-heltec-vision-master-e290-node --lib \
  --features rns-inbox-commit-fault-hil
cargo +stable clippy --locked \
  -p reticulum-heltec-vision-master-e290-node --lib --tests \
  --features rns-inbox-commit-fault-hil -- -D warnings
cargo +stable run --locked -p xtask -- graph-policy
```

```sh
source ~/export-esp.sh
CARGO_TARGET_DIR=target/e290-inbox-commit-fault-hil \
cargo +esp build --locked --release \
  -p reticulum-heltec-vision-master-e290-node \
  --no-default-features \
  --features rns-inbox-commit-fault-hil \
  --target xtensa-esp32s3-none-elf
CARGO_TARGET_DIR=target/e290-inbox-commit-fault-hil \
cargo +esp clippy --locked --release \
  -p reticulum-heltec-vision-master-e290-node \
  --bin reticulum-heltec-vision-master-e290-node \
  --no-default-features \
  --features rns-inbox-commit-fault-hil \
  --target xtensa-esp32s3-none-elf -- -D warnings
```

The isolated HIL ELF is
`target/e290-inbox-commit-fault-hil/xtensa-esp32s3-none-elf/release/reticulum-heltec-vision-master-e290-node`.
Package it only with a HIL-specific output name; do not overwrite the ordinary
default ELF or `e290-node.bin`.

The digest below identifies the retained artifact physically flashed and read
back during qualification, not a universal rebuild digest: target-directory
paths can change bytes embedded in the ELF and merged image.

The exact 762,672-byte merged HIL image had SHA-256
`e693afad19c2eac28d958f902c1b8148ae360a6b54abb14338195ef595515239`.
Starting from the verified erased store above, one 147-byte peer packet with
encoded-byte SHA-256
`0084ad098f2109b390d7c4568ba4a2dcd5285ac40062e55c9709665b2aebc73a`
reached `Delivered`. Before any reset, the ELF-bound 40-byte `RIAF` evidence at
RAM address `0x3fc8bf7c` reported three write calls, one suppressed commit, one
expected commit readback mismatch, zero unexpected failures, disabled service,
and one boot-local dropped candidate: `3/1/1/0/1/1`.

The resulting 2 MiB store SHA-256 was
`ad6d549f73681da7453870606fb34eeabad75b387f081176103562d84e5700c7`.
Its first-record SHA-256 was
`acb43e7be289c5c4f822441670ce11554b6386ca3e1cfcee47907ee82c81d7f8`:
claim/body/digest were exact and every byte from the commit marker at 544 through
the end was `0xff`. Capture the RAM evidence before any reset; those counters are
not persistent state. After this destructive HIL, explicitly erase/verify the
store and restore the default image. The restored 761,952-byte ordinary image
had exact SHA-256
`d26587a2506408ec40cd42facb9bb87cc9c32e79c2afd2e1ab09f0e1268641cb`.

This HIL is deterministic same-boot write suppression on real target storage,
not an electrical interruption, brownout, arbitrary claim/body/partial-commit
cut, backend error-after-write test, timing/high-water measurement, endurance
test, or full mailbox qualification. Neither the fixture tool nor this feature
changes ADR 0011's plaintext, capacity-one, no-acknowledgement, no-deletion, and
no-reclamation limits.

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
retains it behind the resident operation-scoped flash coordinator. Device
configuration remains deferred. The wired inbox is only the one-entry raw-RNS
qualification format above; the separate LXMF store has its own mount-gated
destination, durable admission, and retained-proof composition.
The product does not start protocol service unless clock reservation, journal
mount/recovery, and redundant identity coverage all succeed. LoRa is the
primary first transport slice.
The bounded authenticated USB and BLE client API bearers are wired; Wi-Fi
client service is implemented but not powered-qualified. These bearers do not
become Reticulum packet interfaces implicitly, and additional Reticulum
transports remain deferred.

Raw full-flash dumps now contain a private key after provisioning. Set
`umask 077` before creating them and retain them only with restricted
permissions on encrypted storage. After the required backup and before the
first product boot, perform either a full-chip erase or an exact, readback-
verified erase of `0x610000..0xb30000`, including the complete `message_store`
and `lxmf_store`. The unpadded merged image does not
initialize those data partitions. Subsequent upgrades must preserve every
product store; do not repeat the provisioning erase. The exact guarded sequence is in
[`docs/e290-node.md`](../docs/e290-node.md). This table must not be used through
the workspace's 8 MiB runner. Both connected modules are confirmed
`HT-RA62-HF`. The layout and host/build checks alone do not establish powered
behavior; the separately recorded permanent-image API 1.1 and bounded API 1.2
runs plus the 2026-07-19 cold-mount matrix and same-boot commit-suppression HIL
provide the current two-board evidence under the limits above and in that
runbook. Actual power cuts and target timing/high-water remain open.

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
