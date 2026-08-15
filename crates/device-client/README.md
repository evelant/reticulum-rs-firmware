# Reticulum device client

`reticulum-device-client` is the reusable synchronous host-side client for the
authenticated local device API. It owns credential decoding, handshake and
record mechanics, one sequential session, typed product operations, response
validation, and idempotent mutation semantics.

The caller owns byte transport, finite I/O timeouts, discovery, reconnect
policy, durable application data, and UI. A logical API error preserves the
session; framing, transport, handshake, or authentication failure consumes it.
On uncertain mutation delivery, callers retain the same idempotency key and
content, reconnect, and reconcile the resulting durable state.

Reusable device operations belong here. Product scheduling and persistence
belong in the appliance sync/runtime crates, and platform-specific bearer setup
belongs in connectors.

```sh
cargo test --locked -p reticulum-device-client --all-features
cargo clippy --locked -p reticulum-device-client --all-targets --all-features -- -D warnings
```
