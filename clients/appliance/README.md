# Reticulum appliance client

This directory contains the universal Expo application for the Reticulum
appliance. One authored TypeScript/React Native surface targets web, iOS, and
Android. Native projects are generated with Expo Continuous Native Generation
and are deliberately not committed.

For prerequisites and exact build/install commands, use the
[app getting-started guide](../../docs/getting-started/app.md). For the
fileless BLE setup flow, use the [pairing guide](../../docs/getting-started/pairing.md).

## Contributor loop

From this directory:

```sh
bun install --frozen-lockfile
bun run verify
bun run web
```

After installing a native Debug build, use `bun run start` for the normal
Metro/Fast Refresh loop. Expo Go cannot run this project because the app
contains a custom native TurboModule.

The platform wrappers own the complete native preparation sequence:

```sh
bun run ios
bun run android
```

Each wrapper regenerates the selected Rust/UniFFI bindings, performs a clean
Expo prebuild, compiles the native app, installs it, and launches it. Do not run
binding generation or prebuild first unless you specifically need one of those
intermediate products.

For a self-contained Release that is built, installed, launched, and
artifact-verified on a selected device:

```sh
bun run release:ios
bun run release:android
```

Each command opens Expo's device picker when `--device` is omitted. Pass an
exact target with `bun run release:ios -- --device <UDID>` or
`bun run release:android -- --device <device-name>`. Release wrappers compile the
embedded Rust bridge with Cargo's release profile as well as selecting the
native platform's Release configuration.

## Generated boundaries

- `src/generated/api.ts` is generated from serialized Rust DTOs. Run
  `bun run api:generate` after changing the Rust wire contract.
- `modules/appliance-native` packages the Rust application core through
  UniFFI and a React Native TurboModule.
- `bun run native:bindings:ios` and `bun run native:bindings:android`
  regenerate one native bridge without running the app.
- `bun run native:verify` checks both platform bindings and the Rust bridge;
  it requires both Apple and Android toolchains.
- `bun run build:web` updates the tracked embedded assets under
  `../../crates/lxmf-chat-service/assets`; it does not produce a conventional
  standalone `dist` deployment.

Never hand-edit generated TypeScript, C++, Kotlin, Objective-C++, CMake,
Gradle, podspec, framework, or JNI output. Project-owned scripts remain
TypeScript executed by the exact Bun version in `package.json`.

## Source layout

```text
src/app/                 Expo Router screens
src/generated/           Rust-generated serialized API types
src/lib/                 Client state, transport adapters, and tests
modules/appliance-native Native Rust/UniFFI/TurboModule package
scripts/                 Bun TypeScript build and verification tools
```

## Messaging and activity

The Messages workspace lists saved contacts, authenticated inbound message
requests, and outbound-only unsaved conversations separately. An inbound sender
does not need to be saved or have a reciprocal contact before its conversation
can be opened and replied to. Saving or renaming a contact changes only the
phone-local display name; the authenticated LXMF destination remains fixed.

The Activity workspace and each message's **Details and actions** sheet query
the same bounded per-profile journal. It records durable inbound imports and
outbound queue, acceptance, status, and retry transitions. Times are when the
app store observed the mutation—not RF or remote timestamps. Packet length and
SHA-256 evidence may be available for outbound material. An inbound message's
details also show its immutable first-arrival interface and paired RSSI/SNR
when the appliance retained them. These are receiver-local final-hop values and
may describe a relay. Outbound messages and older records correctly show no
receiver observation; the app never substitutes a Nearby announce reading.

Each message composer has an **Attach phone location** switch. Activity also
stores the phone-local default used when a new composer opens; each draft can
override it. When enabled, queueing requests a fresh high-accuracy foreground
fix and remains visibly unsent if permission or capture fails. API 1.17 carries
the fixed-point sample to the board, which encodes Sideband-compatible LXMF
`FIELD_TELEMETRY` (`0x02`) and signs it into the immutable message. Automatic
board-owned carrier retries and explicit terminal-row replacements reuse that
original location and message identity instead of sampling again. Received and
sent timeline rows show the attached location;
the details sheet shows coordinates, altitude, speed, bearing, accuracy, update
time, and an **Open Map** action backed by OpenStreetMap.

This recipient-visible location is an LXMF application field, not Reticulum
routing metadata, board GNSS, the position of a relay, or the exact point of RF
emission. Arbitrary LXMF fields, attachments, and Resources remain future work.

Each conversation also exposes **Measure path** when the connected appliance
advertises API 1.14 probe support. It begins one volatile Reticulum
path-and-proof request and reports round-trip time, route hops, return
interface, and optional final-hop signal. This validates Reticulum reachability
to an enabled responder only—not LXMF service, application throughput, or the
RSSI at which the remote node received the request. Public nodes may disable
the responder.

API 1.16 adds a separate packet-correlated RF trace below the Activity journal
and inside each message's details. The runtime imports the board's boot-aware
bounded pages into the per-appliance SQLite database, so route selection,
terminal LoRa DATA dispatch, physical-frame `TxDone`, logical RX RSSI/SNR, and
delivery-proof or timeout evidence survive after collection. Correlated rows
include the exact board attempt token and the app-created submission's opt-in
queue-time phone-location stamp. Use **Export JSON** for a lossless complete snapshot or **Export CSV**
for analysis. Exports exclude message bodies and credentials but can include
precise coordinates, peer identities, packet hashes, and timing.

The separate **Field location telemetry** switch is a phone-local durable
diagnostic preference: once enabled it remains enabled across app restarts and
appliance switches
until explicitly turned off. Collection remains foreground-only, and the
preference file stores only that boolean—not coordinates or observations.
Available phone fixes retain platform-reported horizontal accuracy, altitude,
and vertical accuracy. When the app first imports an inbound message it also
stores the latest available receiver-phone fix with that message; duplicate
imports never replace or backfill the original observation.

Board event time is monotonic since boot, app import time is a separate wall
clock, and phone location is captured when the app-created submission was
queued. Later board-owned carrier attempts reuse that stamp. An outgoing trace
cannot report the remote receiver's RSSI. If the UI reports incomplete
history, events were already missing from at least one bounded board ring; the
export preserves that warning rather than presenting a partial capture as
complete.

## Transmission map

The Map workspace renders the same per-profile activity and RF evidence with
MapLibre on web, iOS, and Android. It shows one marker for each retained
app-created outbound submission that has a phone-location sample, plus
sender-attached locations from the loaded message activity history. Selecting
a marker loads its complete message-scoped RF trace when available.

For a newly imported inbound message that has both a sender-attached location
and a retained receiver-phone fix, the map draws a solid sender-to-receiver
line. Its label shows horizontal endpoint distance and both reported phone
elevations; details also show horizontal and vertical accuracy, elevation
difference, three-dimensional endpoint separation, and receiver-local
final-hop RSSI/SNR when available. The receiver fix is the phone position when
the foreground app imported the board inbox—not necessarily the board's
position or the exact RF-arrival position. On a relayed route the line is still
end-to-end phone separation while signal values describe only the final hop.

Dashed lines separately connect chronological outbound queue observations for
visual comparison; they are not RF paths, Reticulum routes, board GNSS tracks,
or traveled tracks. Older records created before receiver-location capture
remain honest and do not gain an inferred reception line.

The default online basemap is OpenFreeMap's Liberty style. Set
`EXPO_PUBLIC_MAP_STYLE_URL` while building to use another MapLibre-compatible
style. The observations and their details remain browsable if the basemap is
offline, but map tiles require network access. Adding or changing MapLibre
requires a native rebuild; Expo Go cannot load the native map module.

## Message notifications

Native builds can present a local phone notification when the foreground app,
or an app returning to the foreground, discovers a newly imported LXMF
message. Notification taps select the owning appliance profile and open the
message's conversation. A small per-profile ledger in the app document
directory records the durable activity-event watermark; the first observation
establishes a baseline rather than replaying historical messages.

This is intentionally not yet a locked-phone BLE guarantee. BLE byte delivery
is currently owned by foreground JavaScript. Reliable delivery while suspended
or after an OS-eligible process relaunch requires a native BLE mailbox
characteristic watcher, Core Bluetooth restoration/AccessorySetupKit on iOS,
and Android companion-device background integration. The current notification
ledger and tap payload are isolated so that native phase can reuse them.

## Radio and route diagnostics

The Network workspace includes a collapsible **Radio & Routes** panel for the
API-1.12 node snapshot and complete bounded retained-route read. It shows
applied LoRa profile/power, RX/TX/CAD observations and counters, registered
interfaces, Reticulum counters, route resolution, and observed/retained/usable
counts. Requested +14/+17/+20/+22 dBm selection lives in the same panel and
retains the network configuration's reboot-to-apply behavior.

With API 1.15, the panel distinguishes the latest terminal DATA and ordinary
jobs. The latest DATA row includes its selected interface, complete encoded
packet length, and selectable SHA-256, using the same length/hash definition as
message details. It is prepared-packet correlation evidence, including for
channel-access rejection before authorization; it must not be read as proof of
RF transmission or remote delivery. Ordinary traffic can replace the aggregate
last-TX row without hiding the separately retained latest DATA evidence.

Polling runs only while the app is foregrounded, the Network workspace is
visible, and the active appliance session is ready. The controller permits one
read at a time, keeps the last good snapshot after a transient failure, and
rejects results from a previous appliance or activation generation. Rust owns
route paging and restarts a bounded read when the revision or total count
changes between pages.

These values must not be presented as a general RF scan or peer-presence list.
Last LoRa RX is conservative whole-packet metadata for the most recently
accepted logical packet (the field-wise weaker RSSI and SNR across both frames
for a split packet), not arbitrary recent RF energy or the latest physical
frame. Retained routes are routing evidence rather than connected/reachable
peers, and **local LRU use** means this node's route-table access—not when that
peer was last heard.

The web target uses the same-origin HTTP service. Native builds use the Rust
profile/database owner and foreground BLE by default. USB and Wi-Fi connector
surfaces remain explicit development or future-work boundaries; they do not
silently replace BLE.
