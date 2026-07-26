# ADR 0019: Secure BLE appliance onboarding

- **Status:** accepted and powered for the alpha success path. Credential-free
  discovery, the production display actor, GPIO21-bound secure link pairing,
  durable one-bond storage, the Expo BLE central/UI, Rust-native fileless
  credential publication, retained reconnect, and two-profile add/switch flows
  are implemented and powered-qualified on the two E290s. Fault-injected
  negative cases, factory-reset/recovery UX, independently revocable
  multi-phone authority, Android hardware, and background mobile lifecycle
  qualification remain pending
- **Date:** 2026-07-25
- **Extends:** [ADR 0009](0009-device-api-credential-store-and-pairing.md),
  [ADR 0010](0010-device-api-live-pairing-protocol.md), and
  [ADR 0015](0015-universal-expo-client-and-generated-bindings.md)

## Context

Before this ADR, the alpha phone workflow imported a copy of an already
activated 96-byte credential. It proved the authenticated BLE/LXMF product
path, but it was not acceptable appliance onboarding: the user had to
provision over USB, transfer a secret file, and later remove the transfer copy.
The app also could not identify an unprovisioned board before it owned a
credential.

BLE advertisements solve only candidate discovery. An advertised service UUID,
local name, RSSI, or platform identifier is neither authenticated appliance
identity nor proof that a board is unprovisioned. The current live-pairing
protocol is also not safe to tunnel unchanged over an unencrypted GATT link:
its successful Begin response contains the long-lived client PSK.

The E290 has a 296-by-128 monochrome e-paper panel and GPIO21 physical-presence
button. The panel can show a temporary Bluetooth passkey, but a full refresh is
slow and physically persists after power loss. Display work therefore cannot
block the executor that drives BLE, LoRa, and the node core, and every terminal
path must replace the visible secret.

## Decision

### Discovery is explicit and unauthenticated

Before a credential exists, the native Expo app may run one bounded foreground
scan for the generated appliance service. It:

- collects at most 64 distinct platform identifiers;
- coalesces duplicate advertisements, preserving a useful nonblank name and
  strongest observed RSSI;
- sorts the result deterministically;
- never connects, subscribes, initializes an authenticated session, or changes
  credential-derived targeting; and
- requires an explicit user selection rather than choosing the first result.

The UI calls the results **nearby appliances**, not unpaired boards. A
provisioned and an unprovisioned E290 currently advertise the same service
shape. The selected candidate remains unauthenticated UI state until the
following ceremony completes.

### Bluetooth security precedes device authorization

The BLE firmware profile enables pinned Trouble `0.6.0` security, seeds its
CSPRNG from the existing hardware RNG, declares `DisplayOnly`, and marks the RX
write as requiring authenticated security. After SMP, the firmware accepts
onboarding records only when Trouble reports
`SecurityLevel::EncryptedAuthenticated`; encrypted Just Works is a downgrade
and must disconnect.

Trouble supplies the six-digit `PassKeyDisplay` value. The E290 renders it, and
the phone's operating system owns entry. Fresh onboarding uses this order:

1. the selected app connects and discovers only the public onboarding surface;
2. a GPIO21 hold binds the sole current GATT epoch and opens a short onboarding
   window without yet making the connection bondable;
3. firmware requests SMP only for that presence-bound epoch, and the phone
   operating system presents its pairing prompt;
4. firmware treats `PassKeyDisplay` and `PairingComplete` as onboarding events
   only for that bound epoch; and
5. after pairing, firmware requires authenticated encryption and a nonempty
   bond durably committed by the sole flash owner before starting device
   authorization; the indication subscription and retained-link readiness
   marker are deliberately public so they cannot initiate SMP early.

The app polls a public `WAIT`/`RDY1` retained-link marker while this ceremony is
in progress. Reading that marker cannot initiate SMP, so an early Continue tap
is harmless. Firmware publishes `RDY1` only after it has consumed the exact
connection's pairing completion and durably committed the resulting bond.

The target boundary keeps the passkey, Bluetooth keys, live-pairing PSK, and
credential artifact out of TypeScript. Trouble's pinned SMP implementation has
a fixed 30-second inactivity timeout, so the display refresh and human entry
must fit inside that bound. App-level setup/teardown may use a longer
human-interaction envelope, but it cannot extend the Trouble timeout.

The first native-owner implementation has one explicit alpha deviation from
that target: the existing `react-native-ble-manager` adapter relays opaque GATT
fragments through TypeScript. TypeScript has no secret-bearing typed field and
must not parse, log, or persist those fragments, but a successful Begin response
still transits JavaScript memory. Moving the byte pump directly from
Swift/Kotlin into Rust is required before claiming a strictly secret-free
JavaScript runtime.

Trouble `0.6.0` has no public pre-SMP admission hook. It automatically handles
an inbound Pairing Request before publishing a connection event, and
`set_bondable(false)` prevents bond creation rather than preventing the SMP
exchange. Therefore firmware does not claim that GPIO21 stops all pre-consent
SMP bytes: an unbound peer may transiently complete a non-bonding exchange.
Such an event has no durable bond, is never admitted to device authorization,
and causes an immediate disconnect. This preserves the authority and bounded
bond-capacity boundary while accepting a bounded connection/UX denial-of-
service risk for the first alpha. If that risk becomes material, the reviewed
hardening path is a minimal pinned Trouble pairing-admission hook that rejects
the first inbound Pairing Request before creating its pairing state machine.

A restored known-bond connection may encrypt without another GPIO21 hold for
an ordinary session.

Bluetooth link security is necessary but not sufficient authorization. Only
the presence-bound BLE connection epoch may acquire pairing exclusivity and
reach initialization or live-pairing records.

### BLE receives a distinct pairing bearer binding

ADR 0010's bearer code `1` remains USB Serial/JTAG. The BLE ceremony must add a
distinct stable bearer code and bind it into Begin, ProofStart, challenge, and
activation transcripts. BLE records must never claim the USB bearer merely
because they reuse the same allocation-free codecs and durable credential
lifecycle.

No normal authenticated session begins while the connection owns pairing
exclusivity. After durable activation, the onboarding link closes and the app
opens a fresh suite-3 BLE session using the newly installed credential.

### Display state owns secrets; the hardware actor owns pixels

`reticulum-appliance-display-model` is the allocation-free, board-neutral
semantic owner. Its pairing passkey is validated as exactly six digits,
implements neither `Copy`, `Clone`, nor `Debug`, and zeroizes on replacement
or drop. Pairing views carry a nonzero expiry window. Success, failure, timeout,
ordinary view replacement, and reboot all transition to a non-secret view.

The E290 firmware exposes a bounded latest-value `Signal` handoff. Each request
owns a strictly increasing ID and each completion reports that same ID plus
`Rendered` or `Faulted`; boot clear has a separate readiness acknowledgement.
The display actor consumes only the newest complete view so stale intermediate
refreshes cannot queue behind a multi-second panel operation. The model
contains no SPI, GPIO, font, framebuffer, or controller knowledge.

The implemented optional actor is the sole SPI3/GPIO1--6/GPIO18 owner. It keeps
one 4,736-byte 296-by-128 one-bit framebuffer in validated external PSRAM, uses
asynchronous SPI and timeout-bounded BUSY handling, clears the panel before
accepting semantic views, coalesces again before transferring pixels, and
enters deep sleep before switching the display rail off after every refresh.
An initialization or refresh fault disables display-dependent fresh pairing
for that boot while the LoRa/node owners continue. A synchronous refresh on
the shared node executor remains forbidden.

The separate display-only HIL powered the panel on Board A, initialized and
cleared the SSD1680, rendered a fixed non-secret demo, entered deep sleep, and
switched GPIO18 low. The retained output passed visual inspection for text,
layout, polarity, and landscape orientation. This validates the panel driver
and full-frame lifecycle, not the optional actor inside the permanent image,
production pairing content, partial refresh, or repeated sleep/wake behavior.
See [the powered display HIL record](../e290-display-hil.md).

A later powered integrated A/B found that ESP32-S3 esp-radio/PHY ownership must
be established before SPI3/e-paper initialization. The display-first order
stalled in `esp_phy::enable_phy` registration/calibration; constructing and
retaining the real `BleConnector` first passed display boot clear, advertising,
the exact `Ready` rendered-completion gate, visual `READY`, and composition
readiness. This is a startup ownership/order invariant, not a memory ceiling.

### Bonds and appliance credentials are separate durable authorities

Both phone and board must agree on Bluetooth bond state after reboot. The
firmware will retain versioned, integrity-checked bond records through the sole
flash owner and restore them into Trouble before advertising. Before declaring
the connection bondable, firmware must preflight that durable bond storage is
available. Because the negotiated LTK exists only after SMP completes, the
phone may retain its half before the board can atomically persist the matching
record. On persistence failure firmware must disconnect, withhold appliance
credential activation, and enter an explicit asymmetric-bond recovery flow
that tells the phone to forget the device before retrying. Interrupted writes,
replacement, capacity, and explicit forget/recovery need tests. The first alpha
may retain one bond while the format permits later bounded expansion.

The permanent E290 map assigns that first store the exact plaintext raw-NOR
range `0x616000..0x618000` (8 KiB, `data,undefined`) immediately after
`api_credentials`. `device_config` moves to `0x618000..0x630000` and shrinks to
96 KiB; `node_journal` and every later range remain unchanged. Boot mount is
strictly read-only and performs no automatic recovery. Pairing-time commit
alternates two 4 KiB sectors, verifies the exact programmed successor, and
remounts before reporting durable success.

Queue pressure that retains the exact bond command on the BLE task side until
the handoff deadline is scrubbed and remains immediately retryable. Once the
command crosses to the flash owner, either an explicit persistence failure or
a missing terminal reply requires BLE to remain disabled until reboot so the
read-only boot mount can re-establish the sole durable authority.

The alpha supports exactly one durable Bluetooth bond per board. A new
authenticated pairing admitted by the GPIO21 physical-presence ceremony
commits the next generation and replaces that one bond; it does not create a
second peer slot. A bond-store mount failure suppresses only BLE for that boot,
while LoRa and other independent transports continue. A restored authenticated
bond can reconnect without repeating SMP, then use a fresh GPIO21 hold to open
the separate five-minute application-pairing window when it needs to initialize
or add an appliance credential.

Trouble `0.6.0` does not treat key material as a secret Rust type:
`LongTermKey` is copyable and debuggable, `BondInformation` is debuggable, and
neither zeroizes on drop. The firmware must keep Trouble's `log` and `defmt`
features disabled for the security-bearing build, wrap bond events immediately
in a local non-formatting owner, and never format the upstream key-bearing
types. This contains accidental disclosure but cannot retroactively zeroize
the unavoidable upstream copies; that residue remains a pinned-dependency
limitation to reassess when Trouble is updated.

The Bluetooth bond authenticates the nearby radio link. The device-API
credential independently grants revocable appliance operations. Neither is a
substitute for the other.

The peripheral's static-random local address is derived from the durable
Reticulum node identity hash, while its human-readable local name retains the
eFuse-MAC suffix as an unauthenticated selection hint. An ordinary reboot
reloads the same identity and therefore preserves the platform peripheral
identifier and bond relationship. A factory or full-chip erase provisions a
new node identity and therefore rotates the BLE address, so stale phone-side
bond and peripheral-cache state cannot be silently attached to the freshly
initialized appliance.

### Native Rust owns live pairing and credential publication

The existing pairing client is factored from optional serial-port ownership
into a transport-neutral state machine over a bounded byte stream. A separate
native onboarding owner—not the normal authenticated connector—owns the
selected, secured GATT link, runs initialization/Begin/ProofStart/Activate in
Rust, and create-only publishes the canonical app-private credential. Expo
receives only bounded progress and public identity summaries.

This separation prevents the missing-credential connector from racing the
onboarding connection and keeps secret material out of logs, generated
serialized DTOs, and TypeScript protocol ownership. The alpha opaque-byte relay
exception above remains until the direct native pump replaces it.

## Implementation order

1. **Complete:** credential-free candidate discovery and explicit selection.
2. **Complete:** semantic display model and coalescing firmware handoff.
3. **Powered alpha success path:** the asynchronous E290 e-paper HIL passed on
   Board A, integrated BLE-plus-display boots passed after establishing
   esp-radio/PHY ownership before display initialization, and both production
   boards completed fileless pairing and retained reconnect. Fault-injected
   pairing-failure/timeout renders and display soak remain open.
4. **Powered:** GPIO21 binding of the selected GATT epoch, followed by Trouble
   authenticated pairing, passkey display, nonempty-bond enforcement, and
   downgrade rejection for device authorization on only that epoch.
5. **Powered success path; negative injection pending:** sole-owner commit-last
   bond persistence and strict read-only boot mount are qualified; fail-closed
   remount after an ambiguous commit remains host-tested.
6. **Powered:** a distinct device-API BLE pairing bearer and transcript binding
   over the secured, presence-bound epoch.
7. **Powered:** Rust-native onboarding owner and create-only app-private
   credential publication without a credential file.
8. **Powered:** forced reconnect into the ordinary authenticated suite-3
   session, followed by a board reboot and automatic app reconnect without
   another passkey.

## Acceptance

The first complete powered proof starts with erased phone bond/credential state
and an erased board credential/bond state. The user selects one of two
advertising E290s, holds GPIO21 to bind that connection, enters only the
passkey then shown on that board, and reaches a normal authenticated app
session without handling a file. A message and Nomad page must still cross the
two-board LoRa path.

The proof then reboots the board and cold-launches the app to demonstrate
restored bond plus credential reconnect without another passkey. Still-pending
negative qualification includes wrong/expired entry, Just Works downgrade,
bond-store power-cut recovery, phone-forgot/board-retained,
board-forgot/phone-retained, and interrupted factory-reset/recovery paths.
Independent multi-phone authority and revocation also remain future work.

On 2026-07-26, Board `AC:A7:04:E1:3E:88` completed fileless iOS onboarding on
MetalbeardMobile under the source-identical USB-diagnostic profile, restored
its bond after reboot, and then automatically reconnected under the ordinary
production profile with authenticated application traffic. The acceptance
image was read back byte-for-byte from both E290s at 1,294,320 bytes and
SHA-256
`34b249ea21f9d3d7defe420a55d6acb3bf7ea507ea3ec33cc73132e909c14a5b`.
Board `AC:A7:04:E1:3F:88` then completed fresh fileless onboarding under that
exact production artifact. Both appliance profiles remained available in the
app, and switching from `E1:3F:88` to `E1:3E:88` and back completed clean BLE
disconnect, discovery, reconnect, subscription, and authenticated application
traffic in each direction. A subsequent source-documentation clarification
changed no runtime logic but produced the final 1,294,320-byte image at
SHA-256
`88d9592a0e98418a0295b7e12d91cb63755d342f143d956d48d58d0e317f691e`.
That image embeds the checked-in E290 partition table and 80 MHz DIO flash
profile, was read back byte-for-byte from both boards, and retained automatic
bonded reconnect plus authenticated application traffic.

## Consequences

- Candidate discovery can ship before credential creation because it transfers
  no authority and makes no identity claim.
- E-paper is useful for secure confirmation. The isolated full refresh
  completed in about 1.56 seconds, and the production
  passkey-to-system-prompt interaction passed inside Trouble's fixed timeout.
  Failure/timeout rendering and the full mobile lifecycle matrix remain to be
  qualified.
- Bluetooth and device-API key recovery are coupled in UX but remain separate
  storage and revocation domains.
- Trouble's lack of pre-SMP admission permits transient non-bonding work from
  an unbound peer. It does not grant authority, but connection-level denial of
  service remains until a pinned admission hook is justified.
- The credential-file importer remains a clearly labelled development/manual
  recovery fallback. Powered first-run and add-board provisioning use the
  fileless BLE flow; formal credential replacement, factory-reset/recovery, and
  multi-phone authority UX remain deferred.
