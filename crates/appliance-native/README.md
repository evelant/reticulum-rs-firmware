# Native appliance boundary

`reticulum-appliance-native` is the Rust source of truth for the Expo native
callable surface. It owns:

- device-keyed profiles, credentials, and SQLite databases;
- the transport-neutral appliance runtime;
- BLE byte-stream and onboarding lifecycles;
- validated JSON projections for TypeScript; and
- the compiled native contract used to reject stale generated bindings.

Platform code supplies BLE scan, GATT subscription, and opaque fragment I/O.
It does not parse the device protocol or hold a second credential model. BLE
GATT is the only connector currently exposed. The transport-neutral runtime
remains the extension point for future connectors, and the boundary never
silently substitutes another bearer.

The Expo package and generation commands live under
[`clients/appliance/modules/appliance-native`](../../clients/appliance/modules/appliance-native/README.md).
Validate the Rust surface with:

```sh
cargo test --locked -p reticulum-appliance-native
cargo clippy --locked -p reticulum-appliance-native --all-targets -- -D warnings
```
