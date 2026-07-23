# Reticulum LXMF host appliance alpha

`reticulum-lxmf-chat-service` turns one paired E290 and its authenticated local
device API into a small always-running host application. The shared
`reticulum-lxmf-chat-runtime` actor exclusively owns the authenticated session
and SQLite connection, reconciles the durable outbox, polls the device inbox,
and publishes immutable transport-neutral snapshots. This crate supplies the
USB Serial/JTAG connector, a macOS CoreBluetooth connector, USB managed
onboarding, and a loopback-only HTTP API serving the bundled Expo web export.

This is an appliance-development bridge, not the final standalone UI. The web
export is compiled into the **host executable** and the computer must remain
running; the E290 does not yet serve HTTP over USB or Wi-Fi. The same Expo
source can target iOS and Android, but those native builds cannot yet reach the
loopback-only host boundary. The node firmware continues to own its Reticulum
identity, radio, routing, durable submissions, and LXMF inbox.

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

The preferred managed mode owns the private per-device paths and enables
first-run initialization, pairing, and recovery. First list attached
native-USB ESP32-S3 candidates and select one stable descriptor serial:

```sh
target/debug/reticulum-lxmf-chat-service --discover

target/debug/reticulum-lxmf-chat-service \
  --usb-serial AC:A7:04:E1:3E:88 \
  --profile-root "$HOME/.local/share/reticulum-lxmf-chat"
```

The profile root and each per-device directory must be owner-only. The service
derives the credential and SQLite paths from the normalized USB serial, so
neither secret paths nor ephemeral `/dev` names become client inputs. Managed
profiles currently fail closed on non-Unix hosts until equivalent owner-only
filesystem checks are implemented.

Open the printed URL and start setup when the client reports that pairing is
needed. The device requires a release followed by a continuous two-second hold
of the E290's middle button labelled `21`, between `RST` and `BOOT`, for each
physical-presence operation. Follow the current stage shown by the client
rather than holding the button continuously. After successful activation,
reset or unplug/replug the board: the service requires a real USB disappearance
and reappearance before it permits a new authenticated epoch.

Interrupted work is classified from the private credential artifact. A known,
canonical Pending artifact may be resumed or explicitly aborted; an ambiguous
Begin may only be explicitly aborted with physical presence. An activation-
ambiguous artifact deliberately has no automatic recovery because the device
may already have committed Active state. Credential paths, PSKs, protocol
transcripts, and detailed pairing errors never cross the browser API.

The required USB serial is the stable 48-bit descriptor value, with or without
colon or hyphen separators. The service discovers the current device path by
that exact serial; on macOS it treats the `/dev/cu.*` and `/dev/tty.*` entries
for one CDC interface as aliases and prefers the callout path. `--port` is an
explicit diagnostic override, but it must still report the configured USB
serial and cannot bypass device selection. `--http-port 0`, the default, asks
the operating system for an unused loopback port.

An already-activated profile can instead use the E290 BLE GATT bearer on
macOS:

```sh
target/debug/reticulum-lxmf-chat-service \
  --ble \
  --usb-serial AC:A7:04:E1:3E:88 \
  --profile-root "$HOME/.local/share/reticulum-lxmf-chat"
```

In BLE mode, `--usb-serial` is the stable per-device profile key; the service
does not open a USB interface. It reloads the profile credential, rejects a
credential whose E290 device-ID EUI does not match that profile key, derives
the one exact `reticulum-e290-*` advertised name, connects over CoreBluetooth,
and then authenticates suite 3 before exposing any device operation.
`--ble-peripheral-id` can additionally narrow selection to an opaque platform
identifier for diagnostics, but it cannot replace the EUI check, name
selection, or authentication.

BLE mode intentionally does not start the USB pairing/onboarding owner. Pair
and activate the board over the qualified USB managed workflow first, then
boot the board normally and select `--ble`. `--port` is USB-only. The direct
host BLE adapter is currently macOS-only; other host platforms report the
bearer as unavailable instead of silently falling back. Wireless onboarding
and additional host BLE backends remain future work.

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

## Web source and generated assets

The universal client is authored in `../../clients/appliance/` with Expo,
React Native, and TypeScript. Bun 1.3.13 drives the checks and Expo export;
Expo's required Metro pipeline produces a static single-page web build. The
normalizer embeds Metro image resources into `app.js` and reduces the runtime
surface to `index.html`, `app.js`, and `style.css`. Rust embeds those files in
the executable, while `assets/manifest.json` records the exact toolchain and
SHA-256 digest of every runtime asset. Files under `assets/` are generated; do
not edit them directly.

JSON DTOs in `../../clients/appliance/src/generated/api.ts` are generated from
the shared runtime and service Serde types with `ts-rs`. Change the Rust type
first and then run `bun run api:generate`; do not maintain a second handwritten
TypeScript model. Runtime snapshots use generic transport metadata. The service
keeps its existing HTTP-v1 USB field names through a generated compatibility
projection, which the HTTP client maps back into the generic application model.

The Bun version/revision and every development dependency are exact pins. To
change the client, install from the lockfile, regenerate, and run the complete
web gate:

```sh
cd clients/appliance
bun install --frozen-lockfile
bun run build:web
bun run verify
```

`verify` checks generated Rust/TypeScript bindings, Expo dependencies,
formatting and lint, strict TypeScript types, Bun tests, repeat-build
determinism, manifest hashes, and the complete tracked asset set. Cargo never
invokes Bun: checked assets keep Rust builds and firmware-oriented tooling
independent of the web toolchain.

## Runtime behavior

- The client can onboard a managed device, add local contacts, inspect
  conversations, queue basic LXMF messages, request an immediate
  synchronization pass, and force reconnect.
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
- Missing peripherals and transient local-bearer transport failures enter
  bounded exponential reconnect backoff. Credential/profile mismatches and
  handshake authentication failures instead fault for operator action. A
  connector deliberately absent from a platform settles in an explicit
  unavailable state. A database/device binding mismatch is a visible
  fail-closed fault, not a retry against another board.
- The actor command queue, HTTP request body, and EventSource client count are
  bounded. Browser events are invalidations; the Expo client reloads
  authoritative snapshots and timelines rather than treating events as
  mutable state.

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
| `GET /api/v1/onboarding` | Read the secret-free managed-profile lifecycle |
| `POST /api/v1/onboarding/start` | Start initialization and new pairing |
| `POST /api/v1/onboarding/refresh` | Reclassify local recovery state after an operator repair or transient fault |
| `POST /api/v1/onboarding/recover` | Resume or physically confirm abort of recoverable Pending state |
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

The [managed Expo first-run proof](../../docs/e290-expo-appliance-first-run-proof.md)
adds identity-bound credential-empty setup, physical-presence pairing, a
required USB reset, service restart with the retained private profile, two
simultaneous board services, and an Expo-enqueued 3F-to-3E LoRa message with
exact peer import and terminal `Delivered`.

The [BLE composition addendum](../../docs/e290-lxmf-chat-alpha-proof.md#ble-bearer-composition-proof)
records two concurrent macOS host services authenticating separate E290s over
CoreBluetooth and a sequential message in each direction reaching terminal
`Delivered` plus exact peer import over LoRa. It also retains one
simultaneous-send `failed_delivery_timeout` as a non-success case. This
qualifies the bounded host BLE service path, not an installed native Expo app
or simultaneous bidirectional scheduling.

Still deferred are activation-ambiguous repair, cross-platform host filesystem
policy and BLE backends, host restart qualification, concurrent-process
locking, notifications, broader browser compatibility and accessibility
testing, database encryption, physical installed-native-client qualification,
device-served Wi-Fi or USB networking, wireless onboarding, simultaneous
bidirectional BLE/LoRa scheduling, NomadNet/Micron,
direct/Resource/propagated LXMF, pressure/fill testing, and soak.

## Focused checks

```sh
cargo +stable test --locked -p reticulum-lxmf-chat-app
cargo +stable test --locked -p reticulum-lxmf-chat-runtime
cargo +stable test --locked -p reticulum-lxmf-chat-service
cargo +stable clippy --locked -p reticulum-lxmf-chat-app --all-targets -- -D warnings
cargo +stable clippy --locked -p reticulum-lxmf-chat-runtime --all-targets -- -D warnings
cargo +stable clippy --locked -p reticulum-lxmf-chat-service --all-targets -- -D warnings

cd clients/appliance
bun install --frozen-lockfile
bun run verify
```
