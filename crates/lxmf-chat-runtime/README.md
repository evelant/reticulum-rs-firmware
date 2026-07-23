# Transport-neutral LXMF chat runtime

`reticulum-lxmf-chat-runtime` owns the long-running, single-writer application
actor shared by host and native clients. It opens the SQLite conversation
store, serializes contact and durable-outbox mutations, reconciles one
authenticated device session, polls the device inbox, and publishes immutable
snapshots.

The runtime does not discover serial ports, choose a mobile platform API, or
serve HTTP. Callers provide a `Connector`; successful connections carry
transport-neutral metadata and an optional opaque lease retained for the
session lifetime. Connection state names the actual bearer, endpoint, and
device label. Retryable, unavailable-in-this-build, and permanent connector
failures have distinct states, so reserved USB OTG, BLE, and Wi-Fi connectors
can remain honest stubs without retrying continuously.

The shared Serde and `ts-rs` request/response types are the semantic client
contract. Both the loopback HTTP service and the Expo native UniFFI facade use
the same validation, JSON-safe integer policy, contacts, timelines, and durable
send outcomes. The host service separately projects ready state into its
historical HTTP-v1 `port` and `usb_serial` names; that compatibility shape does
not leak back into this runtime.

The only complete connector today is the host USB Serial/JTAG adapter in
`reticulum-lxmf-chat-service`. The portable session adapter still uses the
current authenticated `DeviceClient` error vocabulary, whose qualified
credential suite is USB Serial/JTAG. Adding BLE or Wi-Fi therefore requires a
real bearer binding and qualification; the enum variants here do not imply
that those transports work.

Focused checks:

```sh
cargo test --locked -p reticulum-lxmf-chat-runtime
cargo clippy --locked -p reticulum-lxmf-chat-runtime --all-targets -- -D warnings
```
