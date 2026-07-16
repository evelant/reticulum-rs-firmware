# Reticulum Rust firmware

Bare-metal Rust firmware for a standalone Reticulum LoRa node. The first
target is the ESP32-S3 and SX1262/KCT8103L combination on the Heltec Wireless
Tracker V2.3, but protocol and application code will remain portable across
boards and radios.

The repository is in **Phase 1: receive-only vertical slice**. It does not yet
provide a complete Reticulum or LXMF node. The default firmware binary remains
a deliberately RF-disabled safe-idle image. A separate, explicitly configured
lab binary now owns the Tracker SX1262 through an opaque RX-only wrapper; it has
no transmit API and hands complete PHY frames from a sole radio task to a
separate ingress owner through a depth-2, non-blocking drop-new queue. Timed
RNode reassembly, endpoint-only Rete admission, periodic protocol maintenance
and unconditional action suppression are target-linked behind that owner. A
clean, matching normal/pressure and eight-artifact closure pair is preserved at
`artifacts/hil/phase1-rx/20260716T000006Z-fdd6d9e-*-bundle`. A later clean
`bf23cc5` normal image was flashed to board E9:44, read back byte-for-byte, and
ran a 125-second supplemental smoke recorded in
`artifacts/board-flashes/2026-07-16-e944-bf23cc5-rx-refresh/RESULTS.md`, but it
has no matching `bf23cc5` closure bundle and is not formal powered
qualification. Powered heap/stack,
electrical, RX/RF, fault, retention, and soak evidence therefore remain open.
Lab-only startup stack watermarking and retained reset-storm quarantine are now
linked. A separately named,
compile-gated lab artifact provides a one-shot deterministic depth-2 queue-
pressure stimulus without changing the normal lab image. Deterministic RNode
1.86 peer/malformed/backpressure/returned-fault stimuli are checked in as a
19-scenario corpus and replayed through the Rust ingress in CI; a separate
host-tested generator creates encrypted local DATA for one exact ephemeral
Tracker boot.

Separately from the target-linked receive-only image, the portable node-core
now registers caller-owned 500-byte packet buffers, retains fixed dispatch
metadata for them, and prepares outbound DATA directly into one supplied
buffer. `PrepareDataRequest` rejects an owner deadline at or before its current
monotonic sample before any reservation, entropy use, or RNS mutation. Success
resolves the preserved RNS target against an enabled-interface snapshot and
returns a unique routed `TxJob`; multi-interface fan-out is deterministic,
serialized, and reuses that same buffer.

The portable typestate now also covers opaque non-`Copy` permit requests and
replies, deadline-aware authorization, one-shot byte access through
`AuthorizedTx::frame(now)`, completion, and retained recovery. Permit issuance
is the conservative linearization point: it irrevocably records that RF may
have started, even if the reply arrives too late to expose bytes. Exact proofs
or timeouts remain fixed in-place terminal tombstones until explicit
acknowledgement, and a missing owner is never fabricated or force-reused.

The separate `reticulum-tx-handoff` crate now carries these unique static
owners through bounded Embassy channels without exposing raw channel handles
or an owner-taking async send. The firmware-excluded `reticulum-tx-dispatch`
crate now owns those ports in an RF-inert persistent packet-interface state
machine, provides the node-side permit server, and owns a node DATA machine
that validates boot seeds into a fixed per-slot owner table. The DATA machine
reconciles completions through node-core, parks recovered owners until exact
generation-scoped acknowledgement, and retains/retries serialized `Next` jobs
unchanged under pressure. It also synchronously selects the lowest available
parked owner, prepares DATA into that exact buffer, and either queues the fresh
job or restores/retains its exact owner on rejection or handoff failure. Known
returns and continuations take priority, and queue preflight avoids consuming
entropy or mutating node state under pressure. Synchronous steps retain every
owning value, while short waits store a ready return before completing or wait
for `Next` capacity without moving the job into a future. Its only byte consumer
is an internal scalar inspector: it has no executor, clock, TX-capable
driver/HAL, device-API, or pluggable byte-sink dependency and cannot transmit.
Node-core's transitive portable RX/framing edge supplies no TX capability.

A separate firmware-excluded `reticulum-tx-supervisor` crate now owns
node-core, the DATA machine, permit server, RF-inert dispatcher, authorization
policy, and monotonic clock contract in one permanent aggregate. Its async
runner samples the clock separately before maintenance and every machine lane,
waits for the exact next node-owner deadline or permit-recovery grace, yields
after at most 16 immediately productive passes and every selected wake, and races only
phase-compatible cancellation-safe waits. `RfInertTxPolicy` denies every RF
authorization. Retained faults stop fresh preparation and further policy calls
while DATA and dispatcher stepping continue to drain exact owners where their
APIs permit.

This is still not the final product node owner. The new portable
`reticulum-storage-model` defines strict canonical submission records,
principal-scoped idempotency, fail-closed complete replay, lifecycle validation,
and opaque preflighted mutations. `reticulum-submission-projector` now enforces
the durable `Queued -> Preparing` barrier and withholds exact terminal and
recovered-owner acknowledgements until their corresponding transition or audit
is known committed. Neither crate writes flash: the selected project-owned
two-bank journal is not implemented, its physical reservation/fault budgets
remain to be proved, and the device-API adapter, sole runtime owner, and firmware
edge remain open. The supervisor still has no radio/HAL or RF path.
Every project firmware graph remains TX-free, and all project radio-bearing
firmware artifacts remain RX-only. The separately derived RNode image on the
second board is an external development peer, not part of that project graph;
its radio authorization remained off during the recorded project smoke.

## Read first

- [Architecture](docs/firmware-architecture.md)
- [Phase-0 scaffold decision](docs/adr/0001-phase-0-scaffold.md)
- [Rete provisional-foundation decision](docs/adr/0002-rete-provisional-foundation.md)
- [Phase-0 validation contract](docs/phase-0-acceptance.md)
- [Phase-1 receive-only slice](docs/phase-1-rx-slice.md)
- [Phase-1 Tracker RX hardware qualification](docs/phase-1-rx-hil.md)
- [Device API v1 logical protocol](docs/api/device-api-v1.md)
- [Bounded node-core external-buffer DATA dispatch](docs/node-core-outbox.md)
- [Owning async TX handoff](docs/async-tx-handoff.md)
- [RF-inert permanent TX supervisor](docs/tx-supervisor.md)
- [Durable submissions and persist-before-ack projection](docs/durable-submissions.md)
- [Rete upstream hardening backlog](docs/rete-upstream-backlog.md)
- [Dependency provenance](docs/provenance.md)

## Toolchains

Host tools and portable crates use the Rust version pinned by
`rust-toolchain.toml`. ESP32-S3 builds use Espressif's separately installed
Xtensa toolchain:

```sh
espup install --targets esp32s3 \
  --toolchain-version 1.95.0.0 \
  --name esp
source ~/export-esp.sh
```

The export step is required for the Xtensa GCC linker. Check the local setup:

```sh
cargo run -p xtask -- doctor
```

## Initial checks

```sh
cargo test --locked
cargo test --locked -p reticulum-device-api --features host-sim
cargo run --locked -p reticulum-conformance-rete
cargo check --locked \
  -p reticulum-rns-conformance \
  -p reticulum-rns-rete \
  -p reticulum-rns-rete-rx \
  -p reticulum-device-api \
  -p reticulum-node-core \
  -p reticulum-storage-model \
  -p reticulum-submission-projector \
  -p reticulum-tx-handoff \
  -p reticulum-tx-dispatch \
  -p reticulum-tx-supervisor \
  -p reticulum-radio-interface \
  -p reticulum-board-heltec-tracker-v2 \
  --target riscv32imac-unknown-none-elf
cargo +esp check --locked \
  -p reticulum-device-api \
  -p reticulum-node-core \
  -p reticulum-storage-model \
  -p reticulum-submission-projector \
  -p reticulum-tx-handoff \
  -p reticulum-tx-dispatch \
  -p reticulum-tx-supervisor \
  --target xtensa-esp32s3-none-elf
cargo +esp build --locked --release \
  -p reticulum-heltec-tracker-v2 \
  --target xtensa-esp32s3-none-elf
```

The receive-only lab binary has no frequency or modulation defaults. A known
host/RNode-compatible build example is:

```sh
export RETICULUM_LAB_RX_FREQUENCY_HZ=915000000
export RETICULUM_LAB_RX_SPREADING_FACTOR=7
export RETICULUM_LAB_RX_BANDWIDTH_HZ=125000
export RETICULUM_LAB_RX_CODING_RATE_DENOMINATOR=5
export RETICULUM_LAB_RX_PREAMBLE_SYMBOLS=18
export RETICULUM_LAB_RX_EXPLICIT_HEADER=1
export RETICULUM_LAB_RX_CRC=1
export RETICULUM_LAB_RX_IQ_INVERTED=0

cargo +esp build --locked --release \
  -p reticulum-heltec-tracker-v2 \
  --bin reticulum-heltec-tracker-v2-lab-rx \
  --no-default-features --features lab-rx \
  --target xtensa-esp32s3-none-elf
```

Those settings authorize only a local receive experiment with a matching peer;
they are not a regional transmit profile. Missing, malformed, out-of-hardware-
range and currently unverified LDRO combinations fail the build before any
radio-bearing image is produced.

## Qualification artifacts

Phase-1 powered work uses two immutable clean-tree bundles: normal plus
backpressure, and the eight closure artifacts covering four electrical modes,
both returned-fault policies and representative retained-journal selectors
slot 0/word 4 and write ordinal 9. After exporting the eight radio-profile
variables shown above, prepare absent output directories from a clean commit
and verify them before flashing:

```sh
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
normal_pressure_bundle="artifacts/hil/phase1-rx/${stamp}-normal-pressure-bundle"
closure_bundle="artifacts/hil/phase1-rx/${stamp}-closure-bundle"
powered_evidence="artifacts/hil/phase1-rx/${stamp}-powered-evidence"

cargo run --locked -p xtask -- phase1-rx-hil-artifacts prepare \
  --output "$normal_pressure_bundle" \
  --backpressure-stall-us 7000000
cargo run --locked -p xtask -- phase1-rx-hil-artifacts verify \
  --bundle "$normal_pressure_bundle"

cargo run --locked -p xtask -- phase1-rx-closure-artifacts prepare \
  --output "$closure_bundle" \
  --journal-corrupt-slot 0 \
  --journal-corrupt-word 4 \
  --journal-torn-write-ordinal 9
cargo run --locked -p xtask -- phase1-rx-closure-artifacts verify \
  --bundle "$closure_bundle"

cargo run --locked -p xtask -- phase1-rx-powered-evidence init \
  --normal-pressure-bundle "$normal_pressure_bundle" \
  --closure-bundle "$closure_bundle" \
  --output "$powered_evidence"
```

Both artifact manifests and their tool inventories bind the project commit and
its exact raw Git root tree. Source identity and archive Git subprocesses clear
ambient repository/configuration variables, disable replacement objects and
external attributes, and reject a common-directory `info/attributes`. Archive
verification reconstructs files, modes and symlinks from raw tree/blob objects;
it does not accept a second identically filtered `git archive` as proof.

Both verifiers enforce exact directory trees. Never write flash logs,
readbacks or captures into a bundle; store all mutable evidence in the sibling
`$powered_evidence/captures` directory and complete the generated operator and
scenario records. Each scenario-schema-v2 check is an object that binds its
status to specific classified capture paths; both passing and failed attempted
checks require non-empty evidence, while a check with no capture remains
`not-run`. The soak duration, pressure-stall duration and pressure counters use
narrow machine-readable observations. Once a run is over, seal and then
independently verify the exact evidence inventory:

```sh
cargo run --locked -p xtask -- phase1-rx-powered-evidence finalize \
  --evidence "$powered_evidence"
cargo run --locked -p xtask -- phase1-rx-powered-evidence verify \
  --evidence "$powered_evidence"
```

Stop all record and capture writes before `finalize`. Finalization takes a
persistent single-writer sibling lock named
`<evidence>.phase1-powered-evidence-finalize.lock`; that coordination file is
outside the exact evidence tree and may be retained. An interrupted finalize
remains explicitly incomplete and can be rerun: it recovers only its reserved
temporary/final inventory files, rebuilds and syncs the inventory, then commits
the seal with one same-directory rename from `powered-evidence.incomplete` to
`powered-evidence.sealed`. Repeating `finalize` on an already sealed directory
is idempotent and performs full verification.

These commands are host-only: they accept no serial port and perform no flash,
monitor or RF operation. Sealing preserves honest `pass`, `fail` and `not-run`
results; it reports `pass` only when every generated gate record passes, every
required readback is bound to its prepared image, every check has classified
evidence of the required role, the soak records at least 86,400 seconds, the
pressure record contains exactly 7,000,000 microseconds and `3/2/1`
offered/queued/dropped deltas, and the electrical matrix names at least two
board samples. The validator does not parse arbitrary instrument formats;
operators and reviewers remain responsible for the content of the hash-sealed
captures. Peer-driven passing records additionally require operator-schema-v2
paths to a regular copied peer image, a self-contained peer-source Git bundle,
the pinned corpus and peer-tool files below `captures/`; the verifier hashes all
four, binds those bytes to the operator digests, verifies the official peer
commit and root tree from the Git object graph, and requires the copied corpus
and tool to equal the files in the qualification bundle's verified `source.tar`
rather than the current checkout.
Every required peer invocation must list a parsed `peer-manifest.json` and its
sibling `peer-transcript.jsonl`; verification parses the strict JSONL request,
reply, READY and DATA state machine and binds its digest, interval and payloads
with the manifest's successful enqueue status, exact scenario, target mode,
radio profile, firmware, corpus, tool and step count to the powered record. The
boot-local custom corpus is regenerated byte-for-byte by the shared,
schema-frozen generator. The unsigned seal proves internal
consistency, not measurement authenticity; externalize its SHA-256 in a signed
or write-once run log for archival trust. The manifest records canonical
absolute bundle paths, so both immutable sibling bundles must remain at those
paths for later verification. CI runs the host/negative tests, strict target
selector checks and the public eight-build closure prepare/verify pipeline for
the GitHub merge commit. It does not preserve that ephemeral smoke bundle as
qualification evidence. See the
[hardware qualification runbook](docs/phase-1-rx-hil.md) before any powered or
RF operation.

To regenerate and independently check the released-Python wire corpus, use
CPython 3.13.7, install `interop/python/requirements-rns-1.3.8.txt` in an
isolated environment and set `PYTHON` to that environment's interpreter:

```sh
python3.13 -m pip install \
  --target artifacts/phase0/rns-1.3.8-python \
  -r interop/python/requirements-rns-1.3.8.txt
PYTHONPATH=artifacts/phase0/rns-1.3.8-python PYTHON=python3.13 \
  cargo run --locked -p xtask -- check-rns-vectors
PYTHON=python3.13 \
  cargo run --locked -p xtask -- check-rnode-hil-vectors
```

The second command verifies the deterministic RNode 1.86 HIL corpus, its
project-owned KISS peer tool and the same corpus replayed through the Rust
receive-only RNode/Rete ingress tests. It does not transmit; RF requires the
separate explicit `send` command documented in the Phase-1 HIL runbook.

Every project Tracker binary remains TX-disabled until a board revision,
antenna, region, frequency and conservative power profile are explicitly
selected and the later guarded transmit milestone is qualified. There is
intentionally no default LoRa frequency. The external derived RNode peer is
governed separately and remains radio-off unless explicitly authorized.

## Source layout

```text
crates/          portable contracts, the provisional Rete foundation, and board data
comparisons/     separately licensed RNS oracle/fallback graphs
firmware/        target binaries
interop/         pinned peer revisions and generated-vector provenance
tools/           host conformance runners
xtask/           reproducible development commands and environment checks
reference/       ignored research checkouts; never a build dependency
```

Project-owned code is licensed under either MIT or Apache-2.0. Separately
licensed fallback and future derived-code boundaries are documented in
`docs/provenance.md`.
