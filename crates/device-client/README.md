# Reticulum device client

This crate is the reusable, synchronous host-side foundation for the
authenticated Reticulum device API. It owns protocol mechanics, not product UI:

- strict decoding of the existing 96-byte activated-credential state image;
- the authenticated USB Serial/JTAG handshake;
- a sequential, multi-request authenticated session over any `Read + Write`
  byte stream;
- typed identity, submission-status, LXMF enumeration, verified LXMF read,
  nearby-peer discovery, and source-free basic-send operations.

Serial discovery/configuration, pairing-state file security, pairing ceremony,
reconnect policy, evidence files, local conversation storage, and UI remain
outside this crate. A blocking transport must impose finite read/write timeouts;
the client deadline cannot interrupt an indefinitely blocked `Read` or `Write`.

## Live serial ownership

The application opens and owns the USB Serial/JTAG port, then passes that
already-configured stream to `DeviceClient::connect`. The current E290 baseline
is 115,200 baud, a 100 ms transport I/O timeout, DTR asserted, RTS cleared, a
250 ms settle after open, and stale input cleared before the handshake. Keep
those serial details in the executable so another transport can use this crate
without pretending to be a serial port.

One `DeviceClient` represents one sequential authenticated session. Do not use
it concurrently. Logical device-API errors preserve the session, but an I/O,
framing, handshake, or response-authentication error makes the session
unavailable. On disconnect, device reboot, or `SessionUnavailable`, discard the
client, reopen and reconfigure the stream, clear stale input, and perform a
fresh `connect` handshake with the same activated credential. Reuse the same
idempotency key when retrying a basic send whose acceptance is uncertain, then
query the returned submission ID until it reaches a terminal state. Automatic
port discovery, retry backoff, and reconnect loops belong in the client app.

## Workspace integration

The crate is a root-workspace member. Reusable client operations should move
here while product-specific policy stays at the application boundary. Remaining
xtask integration is intentionally separate:

1. add `reticulum-device-client = { path = "../crates/device-client" }` to
   `xtask/Cargo.toml`;
2. migrate the reusable handshake/exchange/LXMF code from
   `xtask/src/e290_authenticated_usb.rs`, leaving serial-port setup, CLI output,
   and evidence handling in xtask;
3. move or share the activated-state decoder currently private to
   `xtask/src/e290_pairing_live.rs` so there is one format authority;
4. extend the root dependency/closure policy for this host-only crate.

Validate this package with:

```sh
cargo +stable test -p reticulum-device-client
cargo +stable clippy -p reticulum-device-client --all-targets -- -D warnings
```
