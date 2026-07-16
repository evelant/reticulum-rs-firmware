# Physical submission journal

**Status:** physical format, portable implementation, and portable sole-owner
adapter complete; host fault injection implemented; isolated RF-inert ESP32-S3
clean-path/software-reset HIL passed on board E9:44; actor-on-firmware,
controlled power cuts, endurance/soak, at-rest encryption, and product-runtime
integration remain open

## Boundary

`reticulum-storage-journal` is the physical backend for schema-1 durable
submission records. It is a `no_std`, allocation-free crate over
`embedded-storage` raw NOR traits. It consumes and reproduces the canonical
records from `reticulum-storage-model`; it does not define a second semantic
format.

The journal crate implements format, complete mount/replay, idempotent append,
and two-bank compaction. The separate `reticulum-storage-actor` now consumes
projector requests, owns the live replay index and sole projector, serializes
one exact pending mutation, and applies results only after durable append or
exact readback. It is a portable synchronous aggregate, not yet the permanent
Embassy task that coordinates flash with OTA, watchdogs, other stores and radio
timing. The separate portable authenticated adapter maps actor results to the
logical device API, but has no framing/session/firmware transport. The dedicated
Heltec HIL calls the journal directly and is a storage qualification image, not
actor/adapter qualification or product firmware. See
[Portable sole storage actor](storage-actor.md).

The journal protects against torn writes and accidental corruption with
domain-separated SHA-256 chains and commit-last markers. These hashes are not
keyed authentication and provide no confidentiality. A party able to rewrite
raw flash can forge a new internally consistent plaintext journal; production
at-rest protection remains a separate provisioning and security decision.

## Version-1 geometry

The physical format accepts exactly a 1 MiB partition with 4-byte reads,
4-byte programs, and 4 KiB erases. Mutating operations additionally require
the backend's `MultiwriteNorFlash` contract. All offsets below are relative to
the start of that partition.

| Region | Offset | Size | Contents |
| --- | ---: | ---: | --- |
| manifest A sector | `0x00000` | 4 KiB | generation-A manifest and source handoff |
| manifest B sector | `0x01000` | 4 KiB | generation-B manifest and source handoff |
| bank A | `0x02000` | 508 KiB (`0x7f000`) | 812 fixed slots plus a 512-byte erased tail |
| bank B | `0x81000` | 508 KiB (`0x7f000`) | 812 fixed slots plus a 512-byte erased tail |

Each 640-byte slot is exactly:

| Field | Size | Rule |
| --- | ---: | --- |
| physical header | 64 bytes | magic, physical/schema versions, bank, kind, length, generation, ordinal, and duplicated logical key |
| canonical semantic body | 512 bytes | encoded record followed by erased padding |
| integrity digest | 32 bytes | SHA-256 over the domain, header, complete padded body, and previous committed digest |
| commit marker | 32 bytes | programmed separately and last |

A manifest uses 96 bytes of versioned data, a 32-byte digest, and a 32-byte
commit marker. Its optional handoff immediately follows and uses 80 bytes of
data, a 32-byte digest, and a 32-byte commit marker. Everything else in the
4 KiB manifest sector must remain erased. A manifest records its bank geometry,
generation, semantic schema, copied baseline count and copied chain tail.

There are 812 physical slots per generation. Schema 1 reserves at most five
semantic records for each accepted submission, so the hard physical acceptance
ceiling is `floor(812 / 5) = 162` submissions. At that ceiling 810 records are
reserved and two slots remain. The runtime's compile-time
`SubmissionIndex<SUBMISSIONS>` capacity can be lower; a product profile must
choose enough RAM index entries for every acceptance it permits. Compaction
retains all committed records, so this journal has no culling or capacity
reclamation for completed submissions.

## Format, mount, and append

Formatting is explicit. `format_erased()` first reads the complete partition
and succeeds only if every byte is erased, then writes the generation-1 bank-A
manifest. The journal never automatically erases or reformats an unknown or
corrupt partition. An erased unformatted partition and a programmed
unformatted partition are distinct errors.

Every mount selects a valid manifest and scans all 812 slots in the selected
bank. It does not stop at the first erased or torn slot, so a later committed
slot cannot be hidden behind a power-loss hole. The scan:

- accepts an erased slot without consuming semantic ordinal state;
- treats a monotonically programmed partial commit marker as a torn,
  non-visible slot while preserving its physical consumption;
- validates every committed slot's generation, ordinal, duplicated logical
  metadata, canonical re-encoding, digest chain, and commit marker;
- rejects committed corruption, physical duplicate logical keys, invalid
  semantic transitions, baseline mismatch, and a programmed bank tail; and
- exposes the live semantic index only after the physical scan and complete
  semantic replay both succeed.

Append performs that complete scan before writing. It preflights the requested
record against replayed semantics and the lifetime reservation, writes the
608-byte protected prefix, reads it back exactly, then writes the 32-byte
commit marker and reads back the complete slot. Retrying an already committed
identical `(submission, revision)` returns `AlreadyEquivalent` without a
program or erase. Different content at that key returns `LogicalConflict`
without mutation.

If power fails while writing the prefix or marker, the partial slot is a hole
and the next append uses the first safe slot after the last programmed slot. If
the physical write completed but its reply was lost, complete scanning makes
the retry equivalent instead of duplicating the semantic record. A marker that
cannot be a monotonic prefix of the expected marker, or a fully committed
record whose protected content is invalid, fails closed. When holes consume
the physical tail, append reports `NeedsCompaction`.

## Two-bank compaction and recovery

Compaction is a record-bank-preserving handoff with an explicit manifest
authority transition:

1. Fully mount and replay the selected source generation.
2. Commit and exactly read back a handoff in the source manifest sector. The
   handoff binds source and target banks, both generations, committed record
   count, and source chain tail. New appends are blocked while it is pending.
3. Erase and verify only the inactive target manifest sector and target bank.
4. Stream each committed source record in logical order into packed target
   slots, recomputing the target generation's chain and exactly reading back
   every commit.
5. Commit the target manifest last with the copied count and chain tail. A
   valid target seal makes that consecutive newer generation authoritative,
   even while the superseded manifest still exists.
6. Erase and verify the superseded manifest sector. This retires the old
   generation without erasing its record bank. Appends to the new generation
   remain blocked until retirement completes.
7. Remount and require the complete new generation to reproduce the same
   accepted-submission count and record count.

Until the target manifest commits, mount continues to select the complete
source. A torn or committed source handoff makes compaction pending and can be
resumed; target erase/copy work starts again from a known erased target. Once
the target seal is valid, mount selects that newer generation and reports
compaction pending until the old manifest sector is completely erased. A power
loss in this retirement window does not advance the generation again:
`compact()` retries only the old manifest-sector erase and returns the same
new generation. A normal compaction therefore makes exactly three raw erase
calls (target manifest, target record bank, old manifest); a retirement-only
retry makes exactly one. The old record bank remains physically intact until a
later opposite-direction compaction uses it as the inactive target.

Retiring the old manifest removes it as a fallback authority before append is
allowed on the new generation. If the active manifest is then corrupted,
mount fails closed instead of selecting the intact-but-unmanifested old record
bank. Records appended after retirement likewise can never be hidden by a
fallback to the copied baseline. Product recovery must not treat an older
record bank as a mountable generation or assume it contains records appended
after the handoff.

This ordering is designed to cover power loss during the handoff, target erase,
record copy, target manifest commit, or superseded-manifest retirement without
selecting a copied prefix or rolling back a post-retirement suffix. Host tests
use a 1 MiB NOR model that enforces one-to-zero programming and injects partial
or lost-reply program/erase failures across append and compaction phases. They
also corrupt the sole active manifest after retirement to prove fail-closed
mount and verify that retirement never erases the old record bank. Those tests
are implementation evidence for injected faults, not powered-cut or flash-
endurance evidence. The clean powered run below separately validates ordinary
raw-flash operations and software-reset replay; it does not turn the host fault
matrix into a powered-cut claim.

## RF-inert Heltec storage HIL

`reticulum-heltec-tracker-v2-storage-hil` is a synchronous, dedicated test
image for the Heltec Wireless Tracker V2.3. It has no executor, radio driver,
LoRa PHY, Rete/RNS, Wi-Fi, BLE, or TX dependency. Before logging or touching
flash it drives the SX1262 reset low, FEM power/CSD/CTX low, SX1262 NSS high,
Vext low, and the battery divider low.

The image then requires:

- exactly 8 MiB of flash and disabled flash encryption;
- an MD5-valid partition table;
- exactly one writable, plaintext `retlog` data partition at
  `0x670000..0x770000`, with no differently named overlapping partition; and
- project-owned partition-relative raw NOR access, not the sector-rewriting
  byte-storage API.

On a completely erased `retlog`, the clean-run fixture formats generation 1,
appends one submission's complete five-record lifetime, verifies semantic
replay, proves an exact retry causes no program/erase call, proves conflicting
content at the same key is rejected without mutation, compacts to generation
2 with exactly three raw erase calls, and software-resets. The second boot must
replay the same delivered state from generation 2 and emit an RF-inert PASS
heartbeat every 30 seconds. A generation-1 boot with a pending handoff resumes
the full compaction. A generation-2 boot with retirement pending verifies the
new generation, retires only the superseded manifest in exactly one erase call
without advancing the generation, verifies replay again, and resets before the
final PASS boot. Unexpected generations, non-erased unformatted contents, and
integrity or semantic errors fail closed.

This clean sequence passed on board `44:1B:F6:F8:E9:44` from source
`7b47113aeec6c7f0549cd5b264eceacef830fb4c`; the qualifying evidence is
preserved at
`artifacts/storage-hil/20260716T211318Z-e944-7b47113`. The strict serial
verifier accepted one continuous two-boot capture (`CoreUsbUart` then
`CoreSw`) containing A1 format, five appends, semantic replay, no-mutation exact
retry/conflict, B2 compaction, B2 replay at raw counters `0/0`, and two final
heartbeats. The independent raw-dump verifier confirmed bank B generation 2,
five committed records in five consumed slots, one accepted submission at
revision 4 `Delivered`, no pending compaction, an erased retired-A manifest,
and an erased unused B tail.

This result qualifies only the isolated journal clean path and software-reset
replay. It does not qualify the portable actor on hardware, controlled power-
cut recovery, erase endurance or soak, at-rest protection, the device API, or
the product runtime. Follow the guarded runbook in
[`partitions/README.md`](../partitions/README.md) for later runs and preserve
each image, readback, hash set, and continuous serial log independently.

## Modularity and remaining product work

The physical journal is deliberately narrower than the full appliance store.
It can be linked wherever schema-1 durable submissions are enabled without
bringing in Rete, LXMF, a radio, an executor, networking, or a UI. A constrained
Tracker profile may omit onboard LXMF/NomadNet clients, SPA assets, BLE, or
propagation service while retaining this exact durability format. A PSRAM/full
appliance profile may raise the RAM index and enable those modules without
changing journal semantics.

Conversely, the journal is not a general message or blob database. LXMF
messages, propagation payloads, identities, configuration, attachments, OTA,
and telemetry need separately bounded stores and quotas. Before product use,
the project still needs:

- one permanent Embassy task around the implemented portable actor, with the
  checked product `esp-storage` partition adapter and boot service gating;
- framing/session/firmware integration for the implemented authenticated
  device-API persist-before-accept/status adapter, plus safe projector-slot
  retirement;
- coordination with watchdogs, OTA, other flash users, and radio timing;
- on-target stack, boot-scan, latency, erase-endurance, and power-cut evidence;
- a migration/export decision before physical or semantic format changes; and
- a separately reviewed confidentiality/tamper-resistance design if required.
