# ADR 0019: Secure BLE appliance onboarding

- **Status:** accepted; credential-free candidate discovery, the board-neutral
  display model, portable SSD1680 driver, powered display-only HIL, and opt-in
  E290 production display actor are implemented. A bounded integrated
  BLE-plus-display startup passed with esp-radio/PHY ownership established
  before display initialization. Secure link pairing, bond durability, BLE
  live-pairing transport, and native credential installation remain pending
- **Date:** 2026-07-25
- **Extends:** [ADR 0009](0009-device-api-credential-store-and-pairing.md),
  [ADR 0010](0010-device-api-live-pairing-protocol.md), and
  [ADR 0015](0015-universal-expo-client-and-generated-bindings.md)

## Context

The alpha phone workflow imports a copy of an already activated 96-byte
credential. It proved the authenticated BLE/LXMF product path, but it is not
acceptable appliance onboarding: the user must provision over USB, transfer a
secret file, and later remove the transfer copy. The app also cannot identify
an unprovisioned board before it owns a credential.

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

The BLE firmware profile will enable pinned Trouble `0.6.0` security, seed its
CSPRNG from the existing hardware RNG, declare `DisplayOnly`, and mark the RX
write plus TX CCCD as requiring authenticated security. After SMP, the
firmware accepts onboarding records only when Trouble reports
`SecurityLevel::EncryptedAuthenticated`; encrypted Just Works is a downgrade
and must disconnect.

Trouble supplies the six-digit `PassKeyDisplay` value. The E290 renders it, and
the phone's operating system owns entry. Fresh onboarding uses this order:

1. the selected app connects and discovers only the public onboarding surface;
2. a GPIO21 hold binds the sole current GATT epoch and opens a short onboarding
   window without yet making the connection bondable;
3. Android explicitly requests bonding through the native BLE manager, while
   iOS accesses the authentication-required characteristic/CCCD to trigger its
   system pairing prompt;
4. firmware treats `PassKeyDisplay` and `PairingComplete` as onboarding events
   only for that bound epoch; and
5. after pairing, firmware requires authenticated encryption, a nonempty bond
   durably committed by the sole flash owner, and authenticated subscription
   before starting device authorization.

TypeScript never receives the passkey, Bluetooth keys, live-pairing PSK, or
credential artifact. Trouble's pinned SMP implementation has a fixed
30-second inactivity timeout, so the display refresh and human entry must fit
inside that bound. App-level setup/teardown may use a longer human-interaction
envelope, but it cannot extend the Trouble timeout.

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

### Native Rust owns live pairing and credential publication

The existing pairing client will be factored from serial-port ownership into a
transport-neutral state machine over a bounded byte stream. A separate native
onboarding owner—not the normal authenticated connector—will own the selected,
secured GATT link, run initialization/Begin/ProofStart/Activate in Rust, and
create-only publish the canonical app-private credential. Expo receives only
bounded progress and public identity summaries.

This separation prevents the missing-credential connector from racing the
onboarding connection and keeps secret material outside JavaScript, logs,
platform callback payloads, and generated serialized DTOs.

## Implementation order

1. **Complete:** credential-free candidate discovery and explicit selection.
2. **Complete:** semantic display model and coalescing firmware handoff.
3. **Implemented with bounded evidence:** the asynchronous E290 e-paper HIL
   passed on Board A, and one integrated BLE-plus-display boot passed after
   establishing esp-radio/PHY ownership before display initialization.
   Repeated boots and live pairing remain open.
4. GPIO21 binding of the selected GATT epoch, followed by Trouble authenticated
   pairing, passkey display, nonempty-bond enforcement, and downgrade rejection
   for device authorization on only that epoch.
5. Sole-owner atomic bond persistence and recovery.
6. A distinct device-API BLE pairing bearer and transcript binding over the
   secured, presence-bound epoch.
7. Rust-native onboarding owner and app-private credential publication.
8. Forced reconnect into the ordinary authenticated suite-3 session.

## Acceptance

The first complete powered proof starts with erased phone bond/credential state
and an erased board credential/bond state. The user selects one of two
advertising E290s, holds GPIO21 to bind that connection, enters only the
passkey then shown on that board, and reaches a normal authenticated app
session without handling a file. A message and Nomad page must still cross the
two-board LoRa path.

The proof then reboots the board and cold-launches the app to demonstrate
restored bond plus credential reconnect without another passkey. Negative cases
include wrong/expired entry, Just Works downgrade, bond-store power-cut
recovery, phone-forgot/board-retained, board-forgot/phone-retained, and
selection of the other nearby appliance.

## Consequences

- Candidate discovery can ship before credential creation because it transfers
  no authority and makes no identity claim.
- E-paper is useful for secure confirmation. The isolated full refresh
  completed in about 1.56 seconds, but the complete production
  passkey-to-system-prompt interaction still must be tested inside Trouble's
  fixed timeout.
- Bluetooth and device-API key recovery are coupled in UX but remain separate
  storage and revocation domains.
- Trouble's lack of pre-SMP admission permits transient non-bonding work from
  an unbound peer. It does not grant authority, but connection-level denial of
  service remains until a pinned admission hook is justified.
- The credential-file importer remains a clearly labelled development fallback
  until the complete powered first-run proof passes.
