# Over-the-air firmware updates

> Status: active implementation. The A/B layout, bounded coordinator,
> identified-Link authorization, ordinary PRNS Resource transfer, native
> sender, slot validation, activation, explicit remote reboot, rollback-enabled
> bootloader, and post-boot health confirmation are present. Generated app
> bindings/UI and powered transfer and rollback evidence remain open; this is
> not yet a production update claim.

The goal is to update an E290 node in the field without physical access. Nodes
are deployed on towers, often without Wi-Fi or nearby operators, so the design
takes the update to the node over the transports that are already there rather
than requiring a laptop and USB cable.

## Approach summary

| Decision | Choice |
| --- | --- |
| On-device mechanism | ESP-IDF A/B OTA via `esp-bootloader-esp-idf` plus a reproducible rollback-enabled ESP-IDF second-stage bootloader |
| Flash layout | Two 5 MiB OTA slots plus `otadata`; no `factory` slot |
| Update authenticity | Reticulum identity allow-list; no bespoke image signing |
| Operator tooling | Native PRNS staging/reboot API and capability-gated mobile Settings flow implemented; desktop recovery remains open |
| App-side Reticulum node | Native in-process PRNS node; the web build uses `appliance-service` |
| Transport semantics | One PRNS application protocol over Bluetooth Auto, LoRa, TCP, or a routed combination |

## Why these choices

Neither `RNode_Firmware` nor `microReticulum_Firmware` in `reference/`
implements device-side OTA. Their `firmware_update_mode` command only lights a
display icon; the actual flash is always host-driven over USB serial through
`rnodeconf` and `esptool`. microReticulum's remote provisioning (web console and
the `rnstransport.remote.management` link) is configuration-only and never
carries firmware bytes. This design is therefore new work, not an adaptation of
the reference material.

The pieces it builds on already exist in the repository:

- `esp-bootloader-esp-idf` 0.5.0 is pinned in `firmware/e290` and ships the
  `ota` and `ota_updater` modules that write an image to the inactive slot,
  flip the boot slot, and inspect or set OTA image state. The product packages
  a separately built ESP-IDF v5.5.5 bootloader with application rollback
  enabled; it does not use `espflash`'s rollback-disabled default binary.
- PRNS has the Python-compatible Resource implementation, application
  acceptance callback, retries, cancellation, segment assembly, and hash
  verification. Its PSRAM-backed ESP32-S3 profile deliberately retains one
  incoming Resource with an 8 KiB sealed-transfer ceiling. OTA therefore sends
  bounded application chunks instead of asking PRNS to retain a whole image.
- The management/OTA application can carry the same bounded Resource protocol
  over Bluetooth Auto, LoRa, or TCP. OTA therefore needs no second BLE-specific
  control plane.

## Flash layout

The E290 keeps its 16 MiB flash and moves product state to the end to make room
for two equal OTA slots.

| Partition | Range | Size | Owner |
| --- | --- | ---: | --- |
| `nvs` | `0x009000..0x00f000` | 24 KiB | ESP NVS reserve |
| `phy_init` | `0x00f000..0x010000` | 4 KiB | ESP PHY reserve |
| `ota_0` | `0x010000..0x510000` | 5 MiB | firmware slot A |
| `ota_1` | `0x510000..0xa10000` | 5 MiB | firmware slot B |
| `otadata` | `0xa10000..0xa12000` | 8 KiB | ESP OTA boot selection |
| `product_state` | `0xa12000..0xe80000` | 4.43 MiB | generic application-state arena |
| `prns_state` | `0xe80000..0x1000000` | 1.5 MiB | PRNS routes, ratchets, and journal |

The two state arenas together occupy 5.93 MiB. Application formats share the
single `product_state` arena instead of adding physical partitions for each app
combination; PRNS exclusively owns its independent protocol journal. The
application image is about 2.3 MiB today, so a 5 MiB slot accepts roughly
double that before the layout needs to change again.

There is no `factory` slot. On a fresh install the merged provisioning image
targets `ota_0`, and with `otadata` erased the bootloader boots `ota_0`.
Verified-before-activate prevents selecting malformed or truncated images. The
packaged bootloader changes a first-boot candidate from `New` to
`PendingVerify`; after PRNS composition, authorization seeding, service
announcement, and 30 seconds of continued product-loop operation, the
application commits `Valid` and reads it back. A reset before confirmation, or
a confirmation write/readback failure followed by the application's deliberate
reset, leaves the bootloader to reject the candidate on the next boot. Powered
evidence remains required before calling that path anti-brick protection.

This change requires fresh provisioning: the partition contract, the build
contract checks in `firmware/e290/build.rs`, and the guide must all change in
the same commit. Smaller boards follow the same invariant (two slots sized to
the per-board image budget) in their own partition CSVs; the gateway image does
not fit A/B OTA on an 8 MiB board and that constraint is documented rather than
worked around.

## Update security

The Reticulum path authenticates originators through the network itself rather
than a separate signature scheme:

- Reticulum links are encrypted end to end, and PRNS reports the peer identity
  after the remote Link initiator identifies itself.
- The node's OTA destination accepts links and resources only from identity
  hashes recorded in the generic product metadata allow-list.
- The Resource protocol verifies every chunk against its advertised Resource
  hash, and the coordinator recomputes the full image SHA-256 against the
  release manifest before activation.

The allow-list starts empty, which disables OTA. During an explicit physical-
presence enrollment window, an app identifies a Reticulum Link with its own
identity and the product durably adds that identity hash. The same authorization
then applies over Bluetooth Auto, LoRa, and TCP. Multiple identities are
supported; serial recovery remains physical access.

## On-device OTA coordinator

A single coordinator owns staging regardless of transport. It validates the
manifest, writes the inactive slot, verifies the complete image, and selects it
for an explicit reboot:

1. Open a session over an authorized identified Link, validate its release manifest and inactive-slot
   bounds, and erase only the selected inactive OTA partition.
2. Accept one ordered chunk at a time, verify its Resource hash, write it to
   the inactive slot, and read it back before requesting the next chunk. No
   whole-image PSRAM buffer exists.
3. Verify the ESP image magic and the full SHA-256 against the manifest before
   the slot is made bootable.
4. Select the slot with `OtaUpdater::next_partition` (never hand-picking a
   slot, which sidesteps the pinned crate's `ota_seq == 0` edge case), write
   it, call `activate_next_partition`, mark the selected OTA record `New`, and
   wait for the separate authorized reboot request.
5. On the candidate's first boot, the rollback-enabled bootloader marks it
   `PendingVerify`. The product waits through a 30-second health window after
   its PRNS application owner is ready, commits `Valid`, and reads the state
   back. It uses ESP-IDF's existing redundant `otadata` records instead of
   inventing parallel last-known-good metadata. A powered rollback test remains
   the acceptance criterion.

The current control path reports target version, selected slot, verified byte
count, next chunk, and stable failure state. Running-slot, health, and rollback
projection through management and the display remain open.

## Transports

### Bluetooth Auto upload

The app's native PRNS node reaches the same management/OTA destination over a
Bluetooth Auto packet interface, opens an identified Link, and sends the same
bounded Resources used on every other Reticulum interface. There is no custom
GATT session, device credential, or BLE-only firmware operation. This is the
first powered deliverable because it is bench-testable and exercises the
coordinator and protocol used by every other transport.

### Reticulum transfer

The app runs a native in-process PRNS node and sends a manifest followed by
ordered application chunks over an identified Link to the node's management/
OTA destination:

- The app's node connects over a `tcp_client` interface to an operator transport
  node or reaches the E290 over Bluetooth Auto/LoRa, establishes a Link to the
  target's shared management/OTA destination, identifies itself, and sends
  Resources.
- The enrolled app retains that management destination hash as application
  state. OTA does not add a fourth default destination or another announce.
- The node accepts the session only when the identified initiator is
  allow-listed. Each Resource contains at most 7 KiB of image data and one
  exact 32-byte MessagePack `bin8` metadata value. Its 7,203-byte stream seals
  to 7,280 bytes. The E290 lane is conservatively sized for as much as 512
  metadata bytes, whose 7,760-byte sealed bound remains below PRNS's 8,192-byte
  ESP32-S3 incoming-transfer capacity.
- Only one Resource is admitted at a time. A full or disconnected PRNS lane is
  reported as an update failure or retryable command settlement; it does not
  change Resource acceptance, deduplication, or proof behavior.

This one path covers local Bluetooth Auto, off-grid LoRa nodes,
internet-connected gateways over the existing TCP uplink, and multi-hop mesh
relaying.

PRNS receives and verifies the ordinary Resources. Product code temporarily
opens the existing per-Link Resource strategy for one expected chunk, closes it
before flash mutation, copies the verified event into a bounded PSRAM lane, and
read-verifies the chunk before the client arms the next one. Device-side
staging, activation, and health confirmation are implemented; powered
qualification is still required.

### Serial and desktop

A Tauri desktop package bundles the web build with the local `appliance-service`
(which hosts the Reticulum node as in-process commands) and adds native serial
flashing through the Rust `serialport` crate. This covers provisioning and
recovery with a USB cable and is more robust than browser WebSerial, which the
web build still offers where supported. iOS Safari lacks WebSerial, so phone
users use BLE.

## Transfer-time expectations

LoRa is the limiting medium. A 2.3 MiB image is roughly:

- half an hour at 500 kHz SF7;
- one and a half to two and a half hours at the default 125 kHz SF7 per hop;
- seconds over the TCP uplink.

Off-grid nodes therefore update slowly on the default profile, which is
acceptable for an unattended overnight update. A temporary fast-profile
negotiation is a later, opt-in refinement. Long transfers share the medium; the
Resource sender paces them, but mesh throughput degrades during an update.

## Delivery order

1. **Layout and coordinator foundation** — partition restructure, fresh
   provisioning, the OTA coordinator, and the bounded application protocol.
2. **PRNS delivery** — identified-Link allow-list, native app-side node, and
   powered Bluetooth Auto, LoRa, and TCP transfer.
3. **Desktop and serial** — the Tauri package with native serial flashing and
   the web WebSerial panel.

## Verification

- Layout: host tests for the partition contract, `xtask build/package/check-elf`,
  and the per-slot OTA image outputs.
- Bluetooth Auto: a powered E290 update over the PRNS interface with a
  deliberate bad image to prove verification and self-rollback, plus native
  binding and app verification.
- Reticulum: Python RNS 1.4.2 and PRNS Resource conformance, rejection of an
  oversized chunk before flash mutation, a two-node powered update over LoRa
  and TCP, and a gateway relay test for multi-hop.
- Desktop and serial: packaging CI and a serial flash against a bricked board.

## Open items

- Management/display projection of running-slot, health, and rollback state.
- Powered qualification of the packaged rollback-enabled bootloader as the
  actual anti-brick fallback, including reset or power loss before
  confirmation.
- Live per-chunk app progress while the native transfer is running, host UI,
  and desktop recovery.
- Powered Bluetooth Auto, LoRa, TCP, interruption, bad-image, and rollback
  evidence.
- Temporary fast-profile negotiation to shorten LoRa transfers.
- A Rust/Wasm PRNS client if a future pure-browser node is justified; the
  supported web build currently reaches a native node through
  `appliance-service`.
