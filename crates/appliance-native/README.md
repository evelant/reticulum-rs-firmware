# Reticulum appliance native boundary

This host-only crate is the Rust source of truth for the Expo application's
native callable surface. It is deliberately separate from firmware dependency
graphs. Its contract query binds a compiled mobile application to the exact
device-API version and bounds it understands, while `NativeAppliance` owns the
transport-neutral durable chat runtime and its SQLite database.

The selected integration is UniFFI `0.31.0` through the pinned
`uniffi-bindgen-react-native` Expo TurboModule. Android and iOS development
builds compile this crate and have executed its immutable contract query.
Platform packaging belongs to the Expo client, while this crate remains
independent of React Native, iOS, Android, and application UI lifecycles.
Contacts, saved-or-unsaved conversation peers, timelines with optional
receiver-local first-arrival evidence and authenticated message location,
bounded durable message-activity queries, and idempotent outbox writes work
offline immediately.
Nearby-peer refreshes page the authenticated device's volatile discovery
projection through the same single-owner actor. Rust handles boot-scoped
cursors and LXMF announce metadata, then returns only semantic JSON to Expo.
The same actor owns redacted network-configuration reads, live network status,
and compare-and-swap Wi-Fi/Reticulum-TCP mutations. Secret-bearing mutation
JSON and its parsed runtime DTO are zeroized at the native boundary; generated
bindings expose only an opaque JSON string rather than a second copy of the
protocol model.
Message location crosses the native boundary only through the generated shared
request and timeline DTOs. Rust durably binds it to the outbox material; the
TypeScript composer supplies the validated phone fix but does not duplicate the
device-API or persistence wire model.

`radio_routes_status_json` returns the same generated, bounded diagnostics
projection used by the loopback HTTP service. Rust aggregates stable route
pages; TypeScript receives lowercase public hashes and semantic local state,
not pagination cursors or claims that retained routes are connected peers.
The generated `reticulum_probe_start_json` and `reticulum_probe_poll_json`
methods serialize one volatile API 1.14 probe through the same actor-owned
authenticated session. They do not write the chat or activity stores; a
successful return is Reticulum proof evidence, not an LXMF or throughput test.
`radio_trace_json` exposes the runtime's validated API-1.16 durable trace query
without duplicating its DTOs across the native bridge. It is a local SQLite
read; background session synchronization owns board pagination and import.
USB serial/JTAG and USB OTG remain explicit unavailable connector stubs: their
stable variants and errors reserve the boundary without claiming that a bearer
works or silently selecting another one.

`NativeProfileStore` owns the mobile app's private storage layout. Canonical
Active credentials are decoded in Rust, keyed by their validated 16-byte
device ID, and stored beside a distinct identity-bound SQLite database under
that device's profile. A small atomic metadata record selects one active
profile. Generated bindings expose only public credential/profile summaries
and an explicit activation operation; credential bytes and credential/database
paths do not cross into TypeScript. On first open, the store migrates the
previous single `reticulum-device-credential.rdpkey` and
`reticulum-lxmf-chat-alpha-schema3.sqlite3` artifacts into the matching profile
when the credential is canonical. Invalid legacy state remains in place for an
explicit recovery flow.

`NativeAppliance::open_wifi` loads an app-private activated credential, opens a
finite-timeout raw TCP stream, and delegates framing, the
Wi-Fi-transcript-bound suite-2 handshake, logical device operations, and LXMF
validation to the portable Rust client crates.

The mobile adapter uses `NativeAppliance::open_wifi_profile` and
`NativeAppliance::open_ble_profile`; the path-taking constructors remain for
host tests and compatibility callers.

`NativeAppliance::open_ble` owns the same composition for the BLE-bound suite-3
handshake while the platform owns scanning, connection, subscription, and GATT
I/O. Its bounded generation-aware byte bridge exposes opaque writes one at a
time and advances Rust I/O only after write-with-response succeeds. Indications
feed the same RDA1 stream without a second fragmentation protocol. Link loss,
overflow, an ambiguous write deadline, and session-lease release all wake
blocked operations and require explicit GATT teardown before another generation
can replace the link. The platform reports one conservative single-write
capability. The native bridge retains that negotiated value, caps it at the
generated 248-byte profile maximum, and keeps 20 bytes as the mandatory ATT
fallback. On iOS the capability is the lower of CoreBluetooth's with-response
and without-response maxima; on Android it follows the requested ATT MTU.

`NativeBleOnboarding` is a separate native owner with a separate BLE byte hub,
so the ordinary authenticated appliance actor cannot claim a pre-authentication
link. It runs initialization and BLE-bearer-bound pair, resume, or confirmed
abort through the transport-neutral pairing client, keeps its durable recovery
artifact below the app-private profile root, and publishes an activated
credential directly into the matching device-keyed profile. Generated bindings
expose only coarse progress/failure enums, a link generation, and public profile
summaries; neither a credential path nor a typed PSK/passkey field crosses the
boundary.

The current alpha platform adapter still moves opaque GATT fragments through
TypeScript because `react-native-ble-manager` delivers indications and accepts
writes there. A successful Begin response therefore transits JavaScript memory
even though TypeScript never parses, logs, or persists it and the generated
typed API has no secret-bearing DTO. A future direct Swift/Kotlin-to-Rust byte
pump is required before claiming that live-pairing material never enters the
JavaScript runtime.

Initial platform setup registers the subscribed generation and calls the
non-destructive `ensure_connected`. Explicit replacement first closes and
reports the old generation, calls destructive `reconnect` while no replacement
can be claimed, then registers the fresh generation and calls
`ensure_connected`. Preserving that order prevents the actor from racing ahead
and acquiring a new link that a late destructive reconnect would immediately
release. Automatic foreground recovery uses the same non-destructive ensure
path: retries coalesce during GATT setup and retain an already usable generation
while the actor is still reaching ClientHello. Only an explicit operator
reconnect replaces that generation.

These proof suites provide authentication and integrity, not confidentiality.
Future adapters should preserve this composition rather than reimplementing
protocol or LXMF handling in TypeScript, Swift, or Kotlin.
