# E290 Expo iOS BLE-to-LoRa powered proof

**Date:** 2026-07-23

**Status:** complete for one signed, self-contained Expo Release application
installed on the physical iOS device labelled `MetalbeardMobile`, one imported
activated credential, an exact authenticated BLE/GATT session to one E290, and
one durable basic LXMF exchange in each direction over LoRa with a second E290
owned concurrently by the macOS host BLE service. Both senders reached
Reticulum `Delivered`, and each receiver retained the same message identifier,
timestamp, endpoints, title, and content as its sender.

The keyboard obstruction exposed during that exchange was corrected in a
separately hashed follow-up Release. That artifact cold-launched, automatically
reconnected the credential-bound E290, and passed a physical keyboard,
scrolling, and action-reachability check. No additional LXMF send is claimed
under the follow-up artifact.

This is the first physical Expo iOS qualification of the native Rust bridge,
credential-import path, foreground BLE central, authenticated device session,
and E290 LoRa chat path together. It extends the constituent
[native Rust bridge](expo-native-rust-bridge-proof.md) and
[host BLE-service](e290-lxmf-chat-alpha-proof.md#ble-bearer-composition-proof)
proofs. It is not a general iOS lifecycle, background BLE, multi-hop,
propagation-node, pressure, soak, NomadNet, or production credential-management
qualification.

## Signed Release artifact for the LXMF exchange

The two exact LXMF messages below used bundle identifier
`org.reticulum.appliance`, version `0.0.1` build `1`, as a thin arm64 Release
application signed for development. Its minimum deployment target was iOS
16.4. The application contained its 2,719,355-byte Metro bundle and did not
depend on a development Metro server.

| Release member | Bytes | SHA-256 |
| --- | ---: | --- |
| `main.jsbundle` | 2,719,355 | `e43900395419f6c2ae99c7a1fead48799d1530897bab312ce151f7e3ac99bd18` |
| `ReticulumAppliance` arm64 executable | 20,948,064 | `8f910d80a6ff425f152e5e30852cca8104d3cc941bf91cfee040feaaabe359f1` |

The generated application declared
`NSBluetoothAlwaysUsageDescription`. The user granted Bluetooth access on the
physical device. No physical-phone hardware identifier, signing identity,
provisioning payload, or other phone-specific personal data is retained in this
record.

## Native startup failure and fix

The first physical development launch failed with:

```text
new NativeEventEmitter() requires a non-null argument
```

React Native DevTools located the failure under Metro's namespace-import
helper and the `PushNotificationIOS` lazy getter. The native runtime loader had
dynamically imported the complete `react-native` namespace only to inspect
`Platform.OS`. Metro enumerated the namespace's lazy exports, including the
removed/extracted `PushNotificationIOS` module; that getter constructed a
`NativeEventEmitter` with a null native module.

The fix removes the namespace import. Metro now selects a native-only platform
module that uses the named import `import { Platform } from "react-native"`,
while Bun and non-native builds select a small unsupported-platform shim that
does not parse React Native's Flow entrypoint. A cold native launch no longer
reported the exception. The Release artifact above includes that fix.

## Board, credential, and transport binding

| Role | EUI-48 / public device label | BLE local name | Primary destination | LXMF delivery destination |
| --- | --- | --- | --- | --- |
| Physical iOS application's E290 | `AC:A7:04:E1:3F:88` / `ACA704E13F88` | `reticulum-e290-e13f88` | `83a09ed807a0a7c631386deaa0448fb9` | `935caba93f7cd97c7c6658350ac02b45` |
| macOS host service's E290 peer | `AC:A7:04:E1:3E:88` / `ACA704E13E88` | `reticulum-e290-e13e88` | `c99e8ff1ec8629e4e1290e14462ae8af` | `03869ee76b74d1e2a4626f0c02ae3248` |

The user selected the activated `ACA704E13F88` credential through the iOS
system document picker. Expo copied the selection into app-owned staging
without decoding credential bytes in TypeScript; Rust validated and
create-only published the 96-byte canonical app-private credential. Its public
summary selected the exact `reticulum-e290-e13f88` advertisement rather than
performing a first-match scan. No credential bytes or credential digest are
included here.

The iOS foreground central opened generated GATT profile 1.0:

| GATT element | Exact value |
| --- | --- |
| Primary service | `f3c8a0b0-5e7a-4c51-a3b9-7d2160d20a01` |
| Phone-to-E290 characteristic | `f3c8a0b1-5e7a-4c51-a3b9-7d2160d20a01`, write with response |
| E290-to-phone characteristic | `f3c8a0b2-5e7a-4c51-a3b9-7d2160d20a01`, indications |
| Initial ATT value bound | 20 bytes |
| Authenticated stream | unchanged RDA1 byte stream, BLE-bound suite 3 |

The application initially rendered `no subscribed BLE GATT link is ready`.
It subsequently reached the connected/ready state without replacing the
credential. Ready state required indication subscription and the Rust-owned
suite-3 handshake, after which the returned device binding matched the 3F EUI,
primary destination, and LXMF destination above. This run does not assign a
cause to the initial transient state.

At the same time, the macOS service selected
`reticulum-e290-e13e88`, authenticated the 3E peer over the same GATT profile,
and reached `ready`. Thus the message path used BLE independently at both
client edges:

```text
Expo iOS -> BLE/GATT -> E290 3F -> LoRa -> E290 3E -> BLE/GATT -> macOS service
macOS service -> BLE/GATT -> E290 3E -> LoRa -> E290 3F -> BLE/GATT -> Expo iOS
```

BLE is the local authenticated API bearer in this proof, not a Reticulum
network interface. LoRa remains the Reticulum interface between the two
firmware nodes.

## Physical iOS-to-peer LXMF message

The user queued this exact message in the physical application:

| Field | Value |
| --- | --- |
| Source | `935caba93f7cd97c7c6658350ac02b45` |
| Destination | `03869ee76b74d1e2a4626f0c02ae3248` |
| Timestamp ms | `1784846871231` |
| Title | `iOS peer to peer` |
| Content | `Test message\n\n\n` |
| Content bytes | 15 / `54657374206d6573736167650a0a0a` |
| LXMF message ID | `b788b7bf6d36d22ffee3fd69a2f1febf96e21fe041652d0d76e3ef74f3e631bd` |
| Sender terminal status | `Delivered` |
| Packet bytes / SHA-256 | 227 / `c692a162f706508cb5a4f1cf1b2228d1cff402c91819d3f5510c824bea9e0975` |

The three `\n` sequences above denote three literal trailing line-feed bytes;
they are not presentation whitespace. The physical phone's app-private SQLite
outbox retained the row at terminal `Delivered`. The 3E host database
independently imported the same message ID, timestamp, source, local
destination, 16-byte title, and exact 15-byte content.

This closes the physical Expo UI -> Rust-owned durable phone outbox ->
authenticated BLE -> 3F firmware -> LoRa -> 3E durable firmware inbox ->
authenticated host BLE -> peer SQLite path.

## Peer-to-physical-iOS return LXMF message

After the authenticated connection became ready, the macOS service sent this
exact message to the physical application. It preceded the user's later
iOS-to-peer submission:

| Field | Value |
| --- | --- |
| Source | `03869ee76b74d1e2a4626f0c02ae3248` |
| Destination | `935caba93f7cd97c7c6658350ac02b45` |
| Timestamp ms | `1784845361000` |
| Title | `ios-release-proof-return` |
| Content | `peer 3E88 to MetalbeardMobile through E290 LoRa` |
| LXMF message ID | `dd29a6bb4940ed3228857e7a97f57f05501f86b76790226c002d95dc0313c4d5` |
| Sender terminal status | `Delivered` |
| Packet bytes / SHA-256 | 275 / `809485d782593f914ec33cd8c8bb0b01424d15ee3cc6dfd2c2c7f7d17012e828` |

The host outbox retained the return row at terminal `Delivered`. A read-only
copy of the physical application's SQLite database independently contained the
same message ID, timestamp, source, local destination, 24-byte title, and
47-byte content in its inbound table. This closes the reverse service ->
authenticated BLE -> 3E firmware -> LoRa -> 3F durable firmware inbox ->
authenticated phone BLE -> Rust-owned phone SQLite path.

The host proof database remains local under
`/private/tmp/reticulum-phone-ble-proof-20260723.mHHLHN`. A point-in-time,
read-only copy of the phone SQLite database had SHA-256
`a31f2033b5ae36f9b2e0b272a5e43b29bdc9bda4aaa0b341e44d148f0b82ccb5`.
Neither evidence reference contains the activated credential.

## Keyboard defect and post-fix physical qualification

The LXMF-exchange Release exposed a significant mobile UX defect before the
outbound send: when the keyboard was visible, it covered form inputs and
actions, preventing the user from reliably seeing entered text or interacting
with the obscured controls. The message was ultimately queued and delivered,
but that workaround was not acceptable product UX.

A scrollable, keyboard-aware layout was then built as a separate signed,
self-contained Release:

| Post-fix Release member | Bytes | SHA-256 |
| --- | ---: | --- |
| `main.jsbundle` | 2,720,302 | `845f08ba485fe6b68ac661478926e4c756ab847e94a0f3539a9a050efa16a393` |
| `ReticulumAppliance` arm64 executable | 20,948,064 | `7848682c38614ec5efe4ba56f3a7d5d3809937db7bf8a4ae6c17fbcb4e4c1083` |

This follow-up retained bundle identifier `org.reticulum.appliance` and the
iOS 16.4 minimum deployment target. Strict, deep code-signature verification
passed before installation. On `MetalbeardMobile`, it cold-launched,
automatically selected and connected `reticulum-e290-e13f88`, and reached the
subscribed GATT state without another credential import.

With the keyboard open, the user physically verified title and body entry,
composer scrolling, and reachability of **Send**, and reported that the
interaction worked correctly. This resolves the observed obstruction for that
tested foreground interaction. It does not rewrite the earlier artifact
record: both exact LXMF messages and their `Delivered` results remain qualified
by the original hashes, and no new LXMF send is attributed to the post-fix
hashes.

## What this run establishes

This bounded run establishes that:

- an Expo iOS Release can embed its own JavaScript and native Rust runtime,
  install, and cold-launch without Metro;
- the system picker and Rust create-only importer can activate the intended
  E290 credential in a fresh physical app;
- the credential-derived exact local name can select the intended E290, enable
  the generated indication/write GATT link, and complete suite-3
  authentication;
- Rust-owned SQLite state, the foreground iOS BLE central, the E290 device API,
  and firmware LoRa routing compose without an HTTP or USB client bearer;
- one sequential basic LXMF message in each direction reaches sender
  `Delivered` and exact durable peer import; and
- the separately hashed post-fix Release preserves cold-launch exact-board
  BLE/GATT readiness while making the title, body, scroll surface, and
  **Send** action usable with the physical iOS keyboard open.

## Limits and follow-up

This is one foreground, near-field, direct LoRa exchange in each direction.
It does not qualify Bluetooth permission denial or later revocation, repeated
process restarts, screen lock, backgrounding, suspension, restoration, Fast
Refresh, radio loss during a command, disconnect during database mutation,
repeated reconnect, credential corruption, long traffic, storage pressure,
simultaneous half-duplex sends, multi-hop routing, propagation nodes, or
electrical power loss. It also does not qualify Android hardware, Wi-Fi,
phone-side USB, multiple simultaneous Reticulum transports, E290-served UI,
NomadNet, Micron, or GNSS.

The imported file clones transferable authentication authority. Phone-native
pairing, identity preview and confirmation, credential replacement/revocation,
Keychain storage, backup exclusion, deletion of the operator's source file,
and full recovery UX remain deferred. The BLE session authenticates and
integrity-protects RDA1 records but does not add application-layer
confidentiality. The initial transient not-ready state and the process-global
BLE manager's cross-instance ownership epoch also remain to qualify.

The keyboard qualification covers the one tested foreground layout. Other
screen sizes, rotation while editing, accessibility text sizes, external
keyboards, Android keyboard variants, interrupted composition, and an actual
LXMF send from the post-fix artifact remain outside this result.
