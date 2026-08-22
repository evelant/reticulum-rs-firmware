# Native appliance boundary

`reticulum-appliance-native` is the Rust source of truth for the Expo native
callable surface. It owns:

- management-destination keyed profiles and SQLite databases;
- one app-wide PRNS node, identity, persistence root, and Bluetooth Auto interface;
- typed product requests over ordinary identified Reticulum Links;
- validated JSON projections for TypeScript; and
- the compiled native contract used to reject stale generated bindings.

PRNS owns interfaces, routes, Links, identification, requests, receipts, and
Bluetooth Auto. Product code verifies management announces, records only
application destination facts, and retains local application data. It does not
define a second bearer, credential model, or Reticulum state machine.

The Expo package and generation commands live under
[`clients/appliance/modules/appliance-native`](../../clients/appliance/modules/appliance-native/README.md).
Validate the Rust surface with:

```sh
cargo test --locked -p reticulum-appliance-native
cargo clippy --locked -p reticulum-appliance-native --all-targets -- -D warnings
```
