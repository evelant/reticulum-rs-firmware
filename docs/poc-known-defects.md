# Usable-firmware POC: known limits and deferred work

This file records deliberate proof-of-concept limits that must not be mistaken
for the intended final product boundary. The architecture remains
transport-neutral and the full feature set is not constrained to boards without
PSRAM. These items may be refined after the first USB-controlled, two-node LXMF
proof unless a failing test promotes one into a release blocker.

## Durable submission and message storage

- Current source retains 128 outbound submissions in external PSRAM. The 129th
  novel request reports capacity exhaustion before a NOR write, while replay of
  an already accepted idempotency key remains available at capacity. The
  append-only journal and terminal projector do not reclaim finalized entries,
  and the physical journal has a separate 162-acceptance lifetime ceiling.
  Source/host tests exercise the 128-plus-one boundary; there is not yet a
  powered 128-message fill, remount, pressure, or timing qualification.
- The generic 128-entry E290 host fixture exceeds Rust's default test-thread
  stack because it owns the large fake runtime by value. Qualify that package
  with `RUST_MIN_STACK=16777216 cargo +stable test --locked -p
  reticulum-heltec-vision-master-e290-node -- --test-threads=1`. This host
  harness requirement is not target stack evidence; firmware constructs the
  resident runtime in place in external PSRAM.
- Earlier one-entry and 16-entry profiles and their proof artifacts remain
  valid only for their named revisions. The 16-entry result is historical
  evidence, not the current device limit and not evidence that the current
  128-entry PSRAM profile has been filled on hardware.
- The first corrected 128-entry A/B chat-alpha run exposed an asymmetric
  sender-lifecycle deadlock after on-demand path discovery. Board B's
  submission 3 was received and committed verbatim by Board A, but repeated
  authenticated status reads left the sender at `Preparing`; submission 4
  consequently remained `Queued`. The direct-path A-to-B submission reached
  `Delivered`. Root cause was scheduler inversion: an ordinary packet queued
  behind a physically transmitted DATA frame, while the storage lane waited
  for ordinary quiescence before making that DATA frame's observation durable.
  Current source prioritizes the exact retained frame and has a regression that
  recreates the owner cycle. The subsequent
  [powered repeat](e290-lxmf-chat-alpha-proof.md) reached `Delivered` and exact
  peer import in both directions; long-running contention and pressure remain
  unqualified.
- The durable LXMF inbox has no delete, acknowledgement, compaction, or
  retention-policy operation. Its stable handles are designed to survive a
  future compactor, but the current store only appends.
- Received normalized LXMF and raw RNS inbox plaintext are stored unencrypted
  in their dedicated flash partitions. The selected outbound LXMF carrier and
  destination are likewise retained as plaintext intent in the durable
  submission journal. API authentication protects access over a bearer; it does
  not provide encryption at rest.
- LXMF enumeration and reads are currently global to every authenticated
  principal. They require no persisted permission bit and provide no
  per-principal mailbox ACL or ownership filtering.
- A host read verifies the final normalized-wire SHA-256 from committed
  metadata. Individual flash chunks revalidate their extent headers but do not
  independently hash the complete message on every read.

## Basic outbound LXMF subset

- The first semantic send operation composes only Python-compatible basic LXMF:
  binary title and content, an empty fields map, no stamp, and opportunistic
  delivery. Direct, propagated, Link/Resource, ticket, stamp, attachment, and
  nonempty-fields sends remain deferred. Empty title and empty content together
  are supported and match the independent Python vector.
- The signing source is always the node's registered inbound Single
  `lxmf.delivery` destination. No API caller may choose a source hash or obtain
  private identity material.
- Python LXMF's exact opportunistic selection limit can produce a 391-byte
  carrier. The existing durable generic-RNS intent holds at most 383 bytes, so
  the product API currently rejects otherwise-valid prepared carriers from 384
  through 391 bytes without accepting journal work. Rete has a qualified
  Header-1-only 391-byte path; durable intent reconstruction and routed
  Header-2 fallback need a separate design before the full boundary is usable.
- Basic composition currently uses allocation-backed Rete LXMF packing and
  signing before copying into caller storage. The E290 POC must measure heap
  high-water behavior. A bounded `encoded_len`/`pack_into` composer remains
  desirable for smaller targets and fallible-allocation handling.
- The client supplies one Unix timestamp in whole milliseconds in the exact
  product range `1..=8_796_093_022_207_999` and must retain it across retries.
  This is a deliberately narrower, JavaScript-friendly subset of Python LXMF's
  binary64 timestamp. The firmware has no trusted wall clock yet.

## Discovery and multiple transports

- LoRa is the first active Reticulum transport, but submission, inbox, API, and
  routing ports do not name LoRa or SX1262. USB is connected only as the local
  client bearer; BLE, Wi-Fi, USB packet transport, and simultaneous
  multi-interface routing have not yet been connected as Reticulum interfaces.
- Local secondary-destination path responses currently use a temporary wrapper
  in `crates/rns-rete`. A cleaner uncommitted implementation exists only in the
  reference Rete checkout and is deliberately not combined with the pinned
  workaround. Moving ownership into Rete requires a separately reviewed
  upstream/local commit and a deliberate dependency-pin update.
- The wrapper suppresses rebroadcast of locally generated path responses and
  is qualified for the current one-interface product. It does not yet retain
  Python-style per-interface pending path-request forwarding state, so it must
  be revisited before enabling simultaneous LoRa, Wi-Fi, and BLE routing.

## Client and product surface

- USB Serial/JTAG is the first usable authenticated bearer. The session and
  logical API are bearer-neutral, but BLE and Wi-Fi provisioning, stronger
  wireless authentication policy, and full recovery UX are deferred. Managed
  host profiles now expose secret-free initialization, pairing, Pending
  resume/abort, and reset progress through the Expo client. The powered managed
  first-run path, real reset, retained-profile service restart, and one
  Expo-to-LoRa-to-peer message now pass on the E290 pair; activation-ambiguous
  repair and the alternate Pending recovery paths remain to qualify.
- With several identical attached boards, the app shows the selected USB serial
  but the physical E290 has no corresponding identify cue. Until a display or
  LED identify action exists, an operator may have to press the middle button
  labelled `21` on every candidate board. Only the selected serial owns the
  pairing session, but this is not acceptable final multi-device UX.
- The current session authenticates records but does not encrypt the USB
  transcript. The diagnostic and chat-alpha CLIs accept title/content bytes in
  process arguments, where shell history or same-host process inspection can
  expose them. Do not use these POC paths for sensitive messages.
- The foreground [LXMF chat alpha](../crates/lxmf-chat-cli/README.md) provides a
  local SQLite conversation database, contacts, a durable outbox, one-shot
  inbox synchronization, reconciliation, and timelines over authenticated USB.
  The newer
  [host appliance service](../crates/lxmf-chat-service/README.md) adds exact USB-
  serial discovery, a sole serial/database actor, automatic reconnect/backoff,
  continuous one-step inbox/status work, immutable state invalidations, and a
  bundled loopback Expo web export. The shared Expo application now also
  compiles and runs as Android and iOS development builds, and its first
  callable UniFFI round trip reaches Rust. It remains a computer-side
  companion: there is still no client served by the E290, Wi-Fi or BLE client
  bearer, native device transport, display UI, NomadNet client, or Micron
  client.
- The native Rust bridge currently exports immutable contract metadata only.
  Native screens still use the interim HTTP adapter, while the host service's
  loopback/origin policy is deliberately not a phone transport. Cancellation,
  Rust panic/error translation, module cleanup, reload idempotence,
  background/foreground lifecycle, and BLE disconnect/resume remain
  unqualified. Native generation and application builds are not yet in CI.
  The iOS XCFramework supports arm64 device and Apple-Silicon simulator only.
  The Android proof used a large multi-ABI debug APK; release stripping,
  splits, startup, and memory remain unmeasured.
- The host service has no operating-system single-instance lock, notification
  service, account migration, database encryption, activation-ambiguous repair,
  or cross-platform disconnect/host-suspend matrix. Managed profiles currently
  fail closed outside Unix until equivalent private-file semantics exist. Do
  not run it concurrently with another service or the foreground CLI against
  the same database/device.
  Current firmware accepts a canonical replacement handshake when the old
  session is idle, but a busy owner is never displaced and a terminal session
  fault still requires USB reset/re-enumeration.
- SQLite schema 2 can bind a database to the authenticated device ID, primary
  destination, and local LXMF destination. The host service performs and
  enforces that binding; the foreground CLI does not yet. A schema-1 migration
  starts unbound because the old rows cannot prove their source, so migrate only
  a database already known to belong to that board. One database per paired
  board remains mandatory.
- GNSS/location integration is intentionally a stub. Location must later be an
  optional service and must not couple core Reticulum routing to this board.
- The current API exposes normalized LXMF bytes plus authenticated metadata.
  The chat alpha adds host-local contacts and timelines, but device-synchronized
  contacts, threading, delivery receipts at the LXMF layer, message deletion,
  propagation-node selection, attachments, and human-friendly error recovery
  remain application-layer work.
- Unlike `submit-and-wait` and the raw-inbox qualification commands, current
  LXMF list/read/send, chat-alpha, and appliance-service operations do not write
  a structured evidence sidecar. The
  [completed powered proof](e290-api14-lxmf-poc.md) manually retained the
  authenticated stdout records, private read files, exact retry inputs, and
  independently recorded hashes; product tooling should generate that bundle
  atomically.
- The identity-safe flash helper intentionally leaves native-USB E290s in the
  ROM loader after exact readback. Starting the application is still a separate
  operator step using the dependency set pinned in
  `interop/python/requirements-esptool-5.3.0.txt`: clear only
  `RTC_CNTL_OPTION1.FORCE_DOWNLOAD_BOOT`, then request an RTC-watchdog full-chip
  reset. Host USB re-enumeration is not a CPU reset, and `espflash 4.5.0 reset`
  cannot perform that exact transition. `probe-rs debug` halts the target again
  when its CLI disconnects, while `probe-rs reset` can lose its sole native-USB
  JTAG path during the ESP32-S3 reset-and-halt sequence. Neither probe-rs command
  is a supported run-and-detach mechanism. Integrate the qualified launch step
  into the identity-safe helper before treating flashing as a one-command
  product installer.

## Hardware profile

- The full E290 profile assumes external PSRAM. The resident submission runtime
  is initialized in place there. The current 128-entry target runtime is
  375,544 bytes; its 64-bit host fixture is 375,568 bytes. That includes an
  actor-owned replay scratch index which keeps boot, append validation, and
  compaction replay off the CPU stack while preserving the live index until a
  durable outcome.
- The final linked-path gate records default mount/append/compact sums of
  53,072/52,816/52,704 bytes and runtime-measurement-HIL sums of
  53,248/53,040/52,928 bytes. Each path must additionally fit a 4,096-byte
  ROM flash-read/interrupt reserve. The initially flashed 128-entry image
  failed this expanded gate and was not qualified. Current source passes the
  static gate and a corrected two-message powered run, but still needs
  allocator, stack-watermark, fill/pressure, and timing qualification before a
  release claim.
- Non-PSRAM ESP32 boards may compile reduced profiles with services disabled.
  They do not define the maximum product feature set, and fitting the complete
  stack on the Tracker V2 is not a requirement.
