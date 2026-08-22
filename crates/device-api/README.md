# Reticulum device API

`reticulum-device-api` is the allocation-bounded logical protocol shared by
firmware and trusted local clients. It defines strict CBOR requests, responses,
capabilities, errors, validation, and authorization vocabulary without owning
an executor, transport, storage device, board, or PRNS runtime.

Feature flags select optional operation families while preserving one
wire authority. PRNS Links carry requests and requester identity; product
dispatch, durable authorization, and physical-presence enrollment remain above
this codec. The crate also owns the compact product OTA wire values carried by
ordinary PRNS requests and Resources.

The current protocol version and operation table are documented in the
[device API reference](../../docs/reference/device-api.md). Rust types and wire
tests are authoritative.

```sh
cargo test --locked -p reticulum-device-api --all-features
cargo clippy --locked -p reticulum-device-api --all-targets --all-features -- -D warnings
```
