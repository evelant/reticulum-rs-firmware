# Host appliance service

`reticulum-appliance-service` is the supported gateway for the Expo web client.
It owns one persisted host PRNS node and Reticulum identity, the shared Rust
synchronization actor, an identity-bound SQLite database, and a loopback HTTP
service containing the deterministic Expo web bundle. The board does not serve
this HTTP application.

## Build

```sh
cd clients/appliance
bun run build:web
cd ../..
cargo build --locked -p reticulum-appliance-service
```

## Run

Select one exact verified management destination and an owner-private state
root:

```sh
target/debug/reticulum-appliance-service \
  --state-root "$HOME/.local/share/reticulum-appliance" \
  --management-destination <32-hex-destination>
```

The service loads or creates its PRNS identity and runtime persistence beneath
the state root. The selected destination gets an isolated SQLite database at
`profiles/<management-destination>/chat.sqlite3`.

If this host identity is not already authorized, open the appliance's GPIO21
physical-presence window and start once with `--enroll`. The service establishes
and identifies a normal PRNS Link, commits authorization on the appliance, and
then verifies the privileged management path. There is no device credential
file or custom BLE session.

Use `--http-port 0` for an ephemeral loopback port. Open the capability URL
printed by the service. The host process and at least one usable PRNS route must
remain available while the browser uses the appliance.

The HTTP listener is loopback-only and validates its capability URL and Host
header. It is a local product gateway, not a public Reticulum TCP server.

```sh
cargo test --locked -p reticulum-appliance-service
cargo clippy --locked -p reticulum-appliance-service --all-targets -- -D warnings
```

Real host Bluetooth Auto behavior remains a powered migration gate; a host
build alone does not prove attachment to an E290.
