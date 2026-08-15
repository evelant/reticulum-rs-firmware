# Host appliance service

`reticulum-appliance-service` is the supported gateway for the Expo web client.
It owns one local appliance connection, the shared Rust synchronization actor,
an identity-bound SQLite database, and a loopback HTTP service containing the
deterministic Expo web bundle. The computer must remain running; the board does
not serve this HTTP application.

## Run over BLE

The host connector uses CoreBluetooth on macOS and requires an activated device
credential for the selected E290. First-run wireless onboarding belongs to the
native app; USB Serial/JTAG on the board is diagnostics-only.

Build the web assets and service:

```sh
cd clients/appliance
bun run build:web
cd ../..
cargo build --locked -p reticulum-appliance-service
```

Profile-root mode uses the credential at
`PROFILE_ROOT/devices/EUI48/credential.rdpkey` and creates or reopens the
identity-bound database beside it. `EUI48` is the board's uppercase
twelve-hex-digit EUI-48:

```sh
target/debug/reticulum-appliance-service \
  --eui48 <12-hex-board-eui48> \
  --profile-root "$HOME/.local/share/reticulum-appliance"
```

The profile root, device directory, and credential must be owner-private. Use
`--ble-peripheral-id` only to select a known CoreBluetooth identifier, and
`--http-port 0` to request an ephemeral loopback port.

Explicit storage mode is available to tools that already own a credential:

```sh
target/debug/reticulum-appliance-service \
  --eui48 <12-hex-board-eui48> \
  --credential /private/active.rdpkey \
  --database /private/appliance.sqlite3
```

Open the loopback URL printed by the service. The computer and BLE connection
must remain available while the browser uses the appliance.

The HTTP listener is loopback-only and validates its capability URL and host.
It is a local development/product gateway, not a network-facing Reticulum TCP
service.

```sh
cargo test --locked -p reticulum-appliance-service
cargo clippy --locked -p reticulum-appliance-service --all-targets -- -D warnings
```
