# E290 BLE API qualifier

This host-only tool proves that an E290 running the opt-in `ble-api-proof`
firmware exposes the shared GATT profile, carries the unchanged ordered RDA1
stream, completes the bearer-bound suite-3 handshake, and answers the safe
`identity.summary` request.

The board must already contain the same active credential supplied to the
tool. The qualifier does not pair, provision, or mutate device state.

```console
cargo run --locked -p reticulum-e290-ble-api -- \
  --credential /path/to/credential.rdpkey \
  --name-suffix e13e88
```

Use `--peripheral-id <id>` to select the opaque CoreBluetooth peripheral ID
printed by a previous run. Without either selector, exactly one advertising
`reticulum-e290-*` peripheral must be visible. Successful and failed runs emit
one JSON evidence object; diagnostics go to stderr.

The direct adapter is intentionally compiled only on macOS. Other hosts build
a clear diagnostic stub rather than acquiring D-Bus or Android/JNI build
requirements through `btleplug`.

## Browser-assisted fallback

If the launching terminal or Codex process does not have macOS CoreBluetooth
permission, run the portable loopback bridge:

```console
cargo run --locked -p reticulum-e290-ble-api -- \
  --browser \
  --credential /path/to/credential.rdpkey
```

The tool prints an exact URL resembling
`http://127.0.0.1:8329/session/<random-token>/`. Open that URL in a current
Chrome or Edge window and click **Choose E290 and connect**. The Web Bluetooth
device chooser requires this user click. The browser validates the shared
service, RX write-with-response, TX indication support, and the 20-byte value
bound before reporting readiness.

The browser never receives the device credential and does not implement RDA1,
authentication, session suite 3, or `identity.summary`. It only moves opaque
GATT fragments over a versioned local WebSocket envelope. Rust emits the
machine-readable evidence to stdout and sends that same completed JSON object
to the page for display. Failure to cleanly disconnect after both sides have
acknowledged that proof is reported as a diagnostic warning; it does not
rewrite the completed proof into contradictory failure evidence.

The helper binds only IPv4 localhost, uses an unguessable per-run path, accepts
one WebSocket client, permits one write in flight, and enforces finite
connection/operation deadlines. Its indication path has a 64 KiB socket
ceiling plus a 32-fragment/704-byte pre-readiness buffer for the short interval
between CCCD subscription and Rust receiving the browser's readiness control.
Every other queue is bounded; overflow or an ambiguous write response is
terminal.

Use `--browser-bind 127.0.0.1:<port>` to select another local port, or port `0`
to let the OS choose one. `--browser-connect-timeout-ms` changes the default
five-minute user-selection deadline.

The browser source is TypeScript and the checked `web/dist/app.js` is generated
with Bun:

```console
cd tools/e290-ble-api/web
bun install --frozen-lockfile
bun run typecheck
bun test
bun run build
```

`http://127.0.0.1` is a potentially trustworthy local origin in supporting
browsers, so the fallback does not require a development TLS certificate.
Web Bluetooth remains an experimental, browser-limited API; Safari is not a
supported fallback.

## Transport invariants

The transport deliberately writes each ordered stream fragment with an ATT
response and caps it at the profile's initial 20-byte value size. It maintains
one bounded receive queue. Any overflow, malformed indication, write timeout,
or disconnect is terminal, so the authenticated client cannot accidentally
retry an ambiguously acknowledged write.

On macOS 11 or later, the terminal or host application launching the binary
must be allowed under **System Settings > Privacy & Security > Bluetooth**.
This is the command-line permission model documented by `btleplug` 0.11; an
application-bundled binary instead needs
`NSBluetoothAlwaysUsageDescription`.
