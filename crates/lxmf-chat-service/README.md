# Reticulum LXMF host appliance alpha

`reticulum-lxmf-chat-service` turns one paired E290 and its authenticated USB
API into a small always-running host application. One actor thread exclusively
owns the serial session and SQLite connection, reconciles the durable outbox,
polls the device inbox, and publishes immutable state snapshots. A bundled
HTML/CSS/JavaScript client is served from a loopback-only HTTP API.

This is an appliance-development bridge, not the final standalone UI. The SPA
is compiled into the **host executable** and the computer must remain running;
the E290 does not yet serve HTTP over USB or Wi-Fi. The node firmware continues
to own its Reticulum identity, radio, routing, durable submissions, and LXMF
inbox.

## Run

Build the service and start it against one paired board:

```sh
cargo +stable build --locked -p reticulum-lxmf-chat-service
mkdir -p "$HOME/.local/share/reticulum-lxmf-chat"

target/debug/reticulum-lxmf-chat-service \
  --usb-serial AC:A7:04:E1:3E:88 \
  --credential /secure/e290-active.key \
  --database "$HOME/.local/share/reticulum-lxmf-chat/e290-a.sqlite3"
```

The required USB serial is the stable 48-bit descriptor value, with or without
colon or hyphen separators. The service discovers the current device path by
that exact serial; on macOS it treats the `/dev/cu.*` and `/dev/tty.*` entries
for one CDC interface as aliases and prefers the callout path. `--port` is an
explicit diagnostic override, but it must still report the configured USB
serial and cannot bypass device selection. `--http-port 0`, the default, asks
the operating system for an unused loopback port.

The Active credential is reloaded for every connection attempt. On Unix it
must be a regular, non-symlink file with no group or other permissions (for
example, mode `0600`). The SQLite database and its parent directory should
also be private; message content, contacts, and retry material are plaintext.

On success the process prints a URL of this form:

```text
Reticulum LXMF appliance: http://127.0.0.1:54321/#cap=<process-capability>
```

Open the complete printed URL in a browser on the same host. The fragment is
not sent in the initial HTTP request. The bundled client exchanges it for an
`HttpOnly`, `SameSite=Strict` API cookie and removes it from the address bar.
The capability is regenerated whenever the service starts.

## Runtime behavior

- The browser can add local contacts, inspect conversations, queue basic LXMF
  messages, request an immediate synchronization pass, and force reconnect.
- A send commits its timestamp, idempotency key, destination, title, and
  content to SQLite before any device request. The HTTP response means the
  local row is durable; the background actor performs device submission and
  status projection afterward.
- Reconciliation rotates through pending rows one operation at a time so a
  long-lived submission cannot indefinitely hide later work.
- Inbox polling processes one authenticated summary per turn. The cursor is
  session-local because the device API has no durable inbox-generation token;
  after reconnect, a full summary scan skips already-known message IDs without
  downloading their complete wire again.
- Missing or unusable serial sessions enter bounded exponential reconnect
  backoff. A database/device binding mismatch is a visible fail-closed fault,
  not a retry against another board.
- The actor command queue, HTTP request body, and EventSource client count are
  bounded. Browser events are invalidations; the SPA reloads authoritative
  snapshots and timelines rather than treating events as mutable state.

SQLite schema 2 retains one authenticated binding consisting of the device ID,
primary Reticulum destination, and local `lxmf.delivery` destination. A new or
migrated unbound database is bound on the service's first authenticated
connection. Every later connection must match all three values. Migration from
schema 1 cannot infer which board produced existing rows, so migrate only a
database already known to belong to that board.

## Loopback API boundary

The current JSON API is private to the bundled alpha client:

| Method and path | Purpose |
| --- | --- |
| `POST /api/v1/session` | Exchange the process capability for the API cookie |
| `GET /api/v1/snapshot` | Connection, device, outbox, contact, and import state |
| `GET /api/v1/contacts` | List local contacts |
| `PUT /api/v1/contacts/{destination}` | Add or rename one contact |
| `GET /api/v1/conversations/{destination}` | Read one stable timeline |
| `POST /api/v1/messages` | Durably enqueue exact outbound material |
| `POST /api/v1/sync` | Make inbox and outbox work immediately due |
| `POST /api/v1/reconnect` | Drop the current session and reconnect |
| `GET /api/v1/events` | Receive bounded snapshot invalidations over SSE |

Every route checks the exact loopback `Host`. API routes require the capability
cookie; mutations additionally require the exact loopback `Origin` and
`X-Reticulum-Client: web-alpha`. The server enables no CORS, applies a 16 KiB
body limit, sets no-store/nosniff/no-referrer headers, and serves a restrictive
Content Security Policy. These controls reduce accidental local cross-origin
access; they do not defend against another process already controlling the
same user account or host.

## Proven and deferred boundaries

The [2026-07-22 appliance-alpha proof](../../docs/e290-lxmf-appliance-alpha-proof.md)
records one message queued through this HTTP service, delivered over LoRa by
Board A, and imported verbatim from Board B. The same run exercised capability
bootstrap, schema-2 identity binding, automatic inbox import, terminal status
projection, and a same-enumeration reconnect.

Still deferred are physical unplug/replug recovery across supported operating
systems, service/host restart qualification, concurrent-process locking,
notifications, browser compatibility and accessibility testing, database
encryption, pairing and credential-management UI, device-served Wi-Fi or USB
networking, BLE/mobile clients, NomadNet/Micron, direct/Resource/propagated
LXMF, pressure/fill testing, and soak.

## Focused checks

```sh
cargo +stable test --locked -p reticulum-lxmf-chat-app
cargo +stable test --locked -p reticulum-lxmf-chat-service
cargo +stable clippy --locked -p reticulum-lxmf-chat-app --all-targets -- -D warnings
cargo +stable clippy --locked -p reticulum-lxmf-chat-service --all-targets -- -D warnings
```
