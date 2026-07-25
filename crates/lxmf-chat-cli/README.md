# LXMF chat alpha

`reticulum-lxmf-chat` is the first persistent host client for the E290's
authenticated LXMF API. It turns the already-qualified device operations into
a small, usable command-line workflow:

- local contacts;
- a SQLite conversation database;
- commit-before-send outbound messages;
- authenticated method-neutral basic LXMF submission over USB, with the
  appliance's current `Auto` policy choosing opportunistic delivery when
  eligible;
- inbox synchronization with message-ID deduplication;
- restart-safe outbox reconciliation and status refresh; and
- a timestamp-ordered conversation timeline.

This is an alpha client, not NomadNet and not the final phone/web experience.
It does not browse Micron pages, run in the background, discover serial ports,
or provide a graphical UI. The separate
[API 1.4 powered record](../../docs/e290-api14-lxmf-poc.md) qualifies the device
send/list/read path. The later
[chat-alpha powered record](../../docs/e290-lxmf-chat-alpha-proof.md) qualifies
one persistent SQLite-backed exchange in each direction with this CLI; it is
not evidence of soak, disconnect, or 128-message powered qualification.

## Current capacity, not the historical proof limit

The current E290 source profile retains **128 accepted submissions** in its
external-PSRAM runtime and projector. A 129th novel request is rejected with
`CapacityExhausted` before a NOR write; exact replay of an accepted idempotency
key remains available at capacity. The append-only physical journal has a
separate **154-acceptance lifetime ceiling** under semantic schema 3 / physical
format 2, and finalized submissions are not yet reclaimed. Therefore 128 is a
bounded current profile, not an indefinitely-running product capacity.

Earlier repository artifacts used one-entry and then 16-entry profiles. Those
artifacts remain valid evidence for their exact source and image, but 16 is not
the current source limit. Conversely, the historical powered runs do not prove
that the current 128-entry profile has been filled, remounted, or pressure
tested on hardware. A board still running an older image retains that image's
older limit until current firmware is built and flashed.

| Boundary | What is established |
| --- | --- |
| Current source/host test | 128 novel acceptances, mutation-free rejection of number 129, exact replay at capacity |
| Physical journal format | 774 slots per bank and at most 154 complete five-record accepted-submission lifetimes; no reclamation |
| Historical powered artifacts | Small bidirectional LXMF exchanges on their then-current profiles, not a capacity fill |
| Long-running product | Retention, export, reclamation, and migration policy still open |

The current PSRAM-resident durable runtime is 375,544 bytes on the 32-bit
Xtensa target and 375,568 bytes in the 64-bit host fixture. That total includes
an actor-owned replay scratch index which keeps boot, append validation, and
compaction replay off the CPU stack while preserving the live index until a
durable result. The final linked-path sums are mount/append/compact
79,376/54,320/54,112 bytes for the default image and
54,352/54,656/54,448 bytes for the runtime-measurement HIL; every path must also
fit a 4,096-byte ROM flash-read/interrupt reserve. The initially flashed
128-entry image failed this expanded gate and was not qualified. The corrected
source passes the static gate and a bounded two-message powered run; a
128-message fill and pressure run remains open.

The 128-entry generic E290 host fixture is intentionally much larger than the
target's in-place PSRAM owner and exceeds Rust's default host test-thread stack.
The qualified host invocation gives that fixture a 16 MiB thread stack and runs
the package serially:

```sh
RUST_MIN_STACK=16777216 \
  cargo +stable test --locked \
  -p reticulum-heltec-vision-master-e290-node -- --test-threads=1
```

All 163 E290 library tests pass under that host-fixture setting. A default-stack
host abort is not target stack evidence; the firmware initializes the resident
runtime in place in external PSRAM. Powered fill and pressure qualification are
still open independently.

## Prerequisites

1. Build and flash the current permanent E290 node image with the repository's
   16 MiB partition table. Both LoRa radios need antennas before transmission.
2. Initialize and pair each board using the
   [pre-authentication control](../../docs/e290-node.md#pre-authentication-usb-control-client)
   and [live-pairing](../../docs/e290-node.md#live-pairing-usb-client)
   workflows. The resulting 96-byte Active credential file contains a
   plaintext PSK; keep it owner-only and never commit it.
3. After pairing, force a real USB bus reset/re-enumeration before starting an
   authenticated client. Closing the TTY or toggling DTR is not that boundary.
4. Know the peer's 32-hex `lxmf.delivery` destination. Do not send to its
   primary node destination.

Use a separate SQLite database for each paired device identity. The shared
store now migrates to schema 2, which can retain an authenticated device ID,
primary destination, and local LXMF destination. The foreground CLI does not
yet perform that authenticated binding; only the appliance service currently
enforces it. A migrated schema-1 database also begins unbound because its old
rows contain no authoritative device identity. Sharing a CLI database across
boards can therefore still merge unrelated inboxes and outboxes without an
identity-mismatch error.

The current E290 bearer permits one active authenticated session at a time. A
single `sync` or `reconcile` command performs several authenticated requests in
that session. After a normal process exit leaves the firmware idle, a later CLI
process may send a canonical ClientHello on the same USB connection; firmware
replaces the idle session with a fresh session epoch. It never displaces an
in-flight request or reply. A malformed/authentication session fault remains
terminal until USB reset/re-enumeration, and pairing exclusivity still requires
the post-pairing reset above. Local-only contact and timeline commands do not
touch USB.

## Build

```sh
cargo +stable build --locked -p reticulum-lxmf-chat

CHAT=target/debug/reticulum-lxmf-chat
PORT=/dev/cu.usbmodemXXXX
KEY=/secure/e290-active.key
DB="$HOME/.local/share/reticulum-lxmf-chat/chat.sqlite3"
```

On Linux the port will normally resemble `/dev/ttyACM0`. Create the database's
parent directory with private permissions. The database contains plaintext
contacts, message title/content bytes, destinations, and durable retry
material; it is not encrypted.

All global options must appear before the command:

```text
reticulum-lxmf-chat \
  [--port <serial-path>] \
  [--credential <active-key>] \
  [--database <sqlite-path>] \
  [--timeout-ms <nonzero-u64>] \
  <command>
```

The default device-operation timeout is five seconds. This is appropriate for
an API exchange, not for waiting through Reticulum discovery or delivery; the
alpha performs one status refresh per invocation rather than waiting for a
terminal state.

## First conversation

Read the peer's LXMF delivery destination. This operation uses the device but
does not need the local database:

```sh
"$CHAT" --port "$PORT" --credential "$KEY" identity
```

Example output:

```text
primary=<32-hex> lxmf_delivery=<32-hex>
```

Add that delivery destination to the local address book:

```sh
PEER=<peer-lxmf-delivery-32-hex>

"$CHAT" --database "$DB" contact-add \
  --destination "$PEER" \
  --name "Field peer"

"$CHAT" --database "$DB" contacts
```

With the sending board in ordinary-session mode, send one message. A reset is
not required after a prior CLI invocation that exited with the session idle:

```sh
"$CHAT" \
  --port "$PORT" \
  --credential "$KEY" \
  --database "$DB" \
  --timeout-ms 10000 \
  send \
  --destination "$PEER" \
  --title "Greeting" \
  --content "Hello over Reticulum"
```

`send` samples the host's current millisecond clock, generates a random
16-byte idempotency key, and commits all exact material to SQLite before it
opens the device. Once the device durably accepts the message, the client
atomically records both the submission ID and LXMF message ID, then performs
one status query. A successful command therefore means durable acceptance, not
necessarily peer delivery.

On the receiving board, use its own port and Active credential, then
synchronize:

```sh
"$CHAT" \
  --port "$RECEIVER_PORT" \
  --credential "$RECEIVER_KEY" \
  --database "$RECEIVER_DB" \
  sync
```

`sync` first reconciles unfinished local outbound work. It then enumerates the
device's committed LXMF inbox, reads and verifies every normalized wire, and
deduplicates exact messages by their authenticated LXMF message ID. It is a
one-shot snapshot, not a listener.

View either local conversation without connecting a board:

```sh
"$CHAT" --database "$DB" timeline --destination "$PEER"
```

UTF-8 title/content are printed as quoted text; arbitrary inbound binary bytes
are printed with an unambiguous `hex:` prefix. Contacts are host-local labels
and are not written to the device.

## Recovery after interruption

The database is intentionally ahead of device I/O:

1. outbound material is committed locally;
2. that exact material is submitted to the device;
3. the returned submission/message identifier pair is committed locally; and
4. later device states are projected monotonically.

If a process, cable, or session fails between those steps, retain the database.
Reconnect and run:

```sh
"$CHAT" \
  --port "$PORT" \
  --credential "$KEY" \
  --database "$DB" \
  reconcile
```

Rows without acceptance IDs are resubmitted with their original timestamp and
idempotency key. The device's exact-replay behavior returns the original IDs if
the first acceptance succeeded but its response was lost. Accepted nonterminal
rows receive one status refresh. Terminal rows are left unchanged. Re-run
`reconcile` later to observe asynchronous progress; there is no polling daemon
or automatic backoff yet. An idle prior session can be replaced without a USB
reset; reset/re-enumerate only if the device remains busy or reports a terminal
session fault.

Do not delete an uncertain database row and create a new send: that discards
the exact retry key and can create a second device submission. Do not run two
chat processes against the same database concurrently; the alpha has no
application-level single-instance coordinator.

## Command summary

| Command | Database | USB port + Active credential | Behavior |
| --- | --- | --- | --- |
| `identity` | no | yes | Print primary and optional LXMF delivery destinations |
| `contact-add` | yes | no | Insert or rename one local contact |
| `contacts` | yes | no | List local contacts in destination order |
| `send` | yes | yes | Commit exact outbound material, submit, record acceptance, refresh once |
| `sync` | yes | yes | Reconcile outbound work, then verify and import the complete device inbox |
| `reconcile` | yes | yes | Resubmit unaccepted rows and refresh accepted nonterminal rows once |
| `timeline` | yes | no | Print one peer's stable local inbound/outbound timeline |

Run the binary with `--help` or `-h` to print the exact argument grammar.

## Alpha security and product gaps

- The authenticated USB records provide integrity and peer authentication, not
  transcript confidentiality.
- The Active credential and SQLite database are plaintext secrets/data at rest.
- SQLite schema 2 supports an authenticated device binding, but this legacy
  foreground CLI has not adopted the shared application/session adapter and
  does not enforce that binding. One database per board remains an operator
  rule when using this binary; the host appliance service enforces it.
- `send` accepts title and content in process arguments, which can expose them
  through shell history or same-host process inspection.
- The current send subset is method-neutral basic LXMF with empty fields.
  `Auto` currently selects opportunistic delivery when eligible; reusable
  direct-Link/Resource delivery and explicit propagated delivery remain
  unimplemented, as do stamps, tickets, attachments, and propagation-node
  selection.
- This CLI has no automatic discovery, background receive, notification,
  reconnect, message deletion, database migration UX, graphical UI, React
  Native app, NomadNet, or Micron renderer. The separate
  [`reticulum-lxmf-chat-service`](../lxmf-chat-service/README.md) now provides
  exact-serial discovery, background reconciliation/polling, reconnect backoff,
  and a bundled loopback Expo web export over the same store and device API.
- The CLI has package tests, restart-tested SQLite semantics, and a completed
  two-board powered workflow. Physical disconnect matrices and soak remain
  open.

The complete deferred-work list is maintained in
[the POC known-defects record](../../docs/poc-known-defects.md).

## Focused checks

```sh
cargo +stable test --locked -p reticulum-lxmf-chat
cargo +stable test --locked -p reticulum-lxmf-chat-core --all-features
cargo +stable clippy --locked -p reticulum-lxmf-chat --all-targets -- -D warnings
RUST_MIN_STACK=16777216 \
  cargo +stable test --locked \
  -p reticulum-heltec-vision-master-e290-node -- --test-threads=1
```
