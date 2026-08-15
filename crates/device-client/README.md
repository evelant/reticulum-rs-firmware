# Reticulum device client

This crate is the reusable, synchronous host-side foundation for the
authenticated Reticulum device API. It owns protocol mechanics, not product UI:

- strict decoding of the existing 96-byte activated-credential state image;
- the authenticated USB Serial/JTAG handshake;
- a sequential, multi-request authenticated session over any `Read + Write`
  byte stream;
- typed identity, submission-status, LXMF enumeration, verified LXMF read,
  durable mailbox status/acknowledgement, nearby-peer discovery, source-free
  basic-send with optional API-1.17 message location, and NomadNet operations;
- authenticated `node_diagnostics`, cursor-based `route_diagnostics_page`, and
  boot-aware `radio_trace_page` reads across all device-api feature
  combinations;
- typed `reticulum_probe_start` and `reticulum_probe_poll` operations for the
  unreleased API 1.14 one-shot path-and-proof measurement;
- a capability-gated manual ordinary service announce operation, exposed as
  both `manual_service_announce` and the ergonomic `announce_now` alias;
- typed redacted network configuration, compare-and-swap mutation, and live
  network-status operations behind the `experimental-network-config` feature.

Serial discovery/configuration, pairing-state file security, pairing ceremony,
reconnect policy, evidence files, local conversation storage, and UI remain
outside this crate. A blocking transport must impose finite read/write timeouts;
the client deadline cannot interrupt an indefinitely blocked `Read` or `Write`.
Typed logical API errors, including API-1.18 `RetryLater`, preserve the
authenticated session. Product runtimes decide the retry cadence; transport,
framing, or authentication failures still consume the session.

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

`BasicLxmfSend::with_location` attaches one typed, immutable phone-location
snapshot. The client requires the connected device to report API minor 17 or
newer before sending that extension, preventing an older minor-version decoder
from ignoring the optional key. The board, rather than the caller, encodes the
snapshot as Sideband-compatible LXMF telemetry and signs it into the message.
Callers that retry semantic material must retain the same location together
with the destination, timestamp, title, and content.

`announce_now` first performs a fresh `system.capabilities` request. It sends
the mutating manual-announce request only when capability key `20` reports
`Available`, and returns the device's `Queued` or `AlreadyPending`
disposition. Manual service announces cover the ordinary primary, LXMF, and
NomadNet destinations; RMAP interface discovery remains a separate opt-in.

`node_diagnostics()` returns the fixed-capacity API 1.12 interface, LoRa, and
Reticulum snapshot directly. `route_diagnostics_page(request)` returns up to
four lexicographically ordered route records; pass its `next_cursor()` through
`RouteDiagnosticsRequest::new` for the next exclusive page. These operations
require the established authenticated session but no persisted permission bit
or capability preflight. A profile that uses a minimal dispatcher reports the
normal typed `UnsupportedOperation` API error.

`radio_trace_page(request)` returns up to three retained API 1.16 trace events
covering logical LoRa RX, terminal DATA TX, durable route selection, and
terminal delivery-attempt state. Continue with
`RadioTracePageRequest::new(page.next_cursor())`; the typed cursor binds its
exclusive sequence to the current boot, so clients cannot silently continue a
pre-reboot page. Packet SHA-256 and optional attempt tokens correlate events
without returning packet content, while the page also carries the exact
boot-applied LoRa profile and bounded TxDone/RSSI/SNR evidence. This read is
authenticated but needs no persisted permission bit. A product dispatcher
without the diagnostics port returns `UnsupportedOperation`.

`reticulum_probe_start(request)` and `reticulum_probe_poll(id)` use the
established authenticated session. The start requires the device-side
experimental-submit permission; the poll does not add another permission.
Probe IDs are principal- and boot-scoped. Success reports Reticulum
round-trip/hop evidence plus the local interface receiving the returning proof
and optional receiver-local final-hop signal. It does not test LXMF service or
throughput and does not expose the remote receiver's request RSSI.

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
cargo +stable test -p reticulum-device-client --all-features
cargo +stable clippy -p reticulum-device-client --all-targets --all-features -- -D warnings
```
