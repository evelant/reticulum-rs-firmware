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

Complete connectors now include the host USB Serial/JTAG adapter in
`reticulum-lxmf-chat-service` and the Expo native BLE path backed by the shared
Rust appliance/session core. The BLE suite-3 binding has a bounded installed-iOS
powered proof; this does not yet qualify every mobile platform or lifecycle.
The opt-in native raw-TCP Wi-Fi proof connector and separately transcript-bound
suite-2 E290 SoftAP endpoint are implemented and host-qualified; powered field
qualification remains open.

Focused checks:

```sh
cargo test --locked -p reticulum-lxmf-chat-runtime
cargo clippy --locked -p reticulum-lxmf-chat-runtime --all-targets -- -D warnings
```
