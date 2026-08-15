# Reticulum device API

`reticulum-device-api` is the allocation-bounded logical protocol shared by
firmware and trusted local clients. It defines strict CBOR requests, responses,
capabilities, errors, validation, and authorization vocabulary without owning
an executor, transport, storage device, board, or Rete runtime.

Feature flags select optional operation families while preserving one
wire authority. Framing, authenticated sessions, pairing, dispatch adapters,
and physical bearers live in separate crates.

The current protocol version and operation table are documented in the
[device API reference](../../docs/reference/device-api.md). Rust types and wire
tests are authoritative.

```sh
cargo test --locked -p reticulum-device-api --all-features
cargo clippy --locked -p reticulum-device-api --all-targets --all-features -- -D warnings
```
