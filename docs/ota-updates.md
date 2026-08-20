# Over-the-air firmware updates

> Status: plan for review. The mechanisms described here are not implemented
> yet. Refine this document, then treat it as the source of truth for the
> implementation.

The goal is to update an E290 node in the field without physical access. Nodes
are deployed on towers, often without Wi-Fi or nearby operators, so the design
takes the update to the node over the transports that are already there rather
than requiring a laptop and USB cable.

## Approach summary

| Decision | Choice |
| --- | --- |
| On-device mechanism | ESP-IDF A/B OTA via `esp-bootloader-esp-idf` (`ota_updater`), already a firmware dependency |
| Flash layout | Two 5 MiB OTA slots plus `otadata`; no `factory` slot |
| Update authenticity | Reticulum identity allow-list; no bespoke image signing |
| Operator tooling | None — the app (mobile, Tauri desktop, web) initiates every update |
| App-side Reticulum node | In-process tokio node via the existing `rete-tokio` crate |
| Delivery order | BLE first, then Reticulum, then Tauri desktop and WebSerial |

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
  flip the boot slot, and reboot. No bootloader change is required for basic
  A/B OTA.
- `rete-transport` has a complete Resource implementation (sliding window,
  chunk retransmission, end-to-end encryption, hash verification, automatic
  splitting of files larger than 1 MiB), and `rete-tokio` already exposes
  `NodeCommand::SendResource`, `InitiateLink`, and `AcceptResource`. The
  sender side of Reticulum delivery is already written.
- The BLE device API already has a chunked-transfer precedent (`OP_LXMF_READ`)
  and a client-side loop with SHA-256 verification that a firmware upload can
  mirror.

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
| product state | `0xa12000..0x1000000` | 5.94 MiB | firmware storage coordinator |

Product state grows from its current 5.125 MiB to a 5.94 MiB arena, leaving
about 0.8 MiB of slack for configuration and store growth. The application image
is about 2.3 MiB today, so a 5 MiB slot accepts roughly double that before the
layout needs to change again.

There is no `factory` slot. On a fresh install the merged provisioning image
targets `ota_0`, and with `otadata` erased the bootloader boots `ota_0`.
Recovery from a broken image relies on verified-before-activate discipline plus
the self-rollback path described below; a future anti-brick option is a custom
bootloader built from `esp-bootloader-esp-idf` with auto-rollback enabled.

This change requires fresh provisioning: the partition contract, the build
contract checks in `firmware/e290/build.rs`, and the guide must all change in
the same commit. Smaller boards follow the same invariant (two slots sized to
the per-board image budget) in their own partition CSVs; the gateway image does
not fit A/B OTA on an 8 MiB board and that constraint is documented rather than
worked around.

## Update security

The Reticulum path authenticates originators through the network itself rather
than a separate signature scheme:

- Reticulum links are encrypted end to end, and the embedded stack reports the
  peer identity hash in `LinkIdentified` when a link is established.
- The node's OTA destination accepts links and resources only from identity
  hashes recorded in the `device_config` allow-list.
- The Resource protocol already verifies the assembled image against its
  advertised hash, and the coordinator recomputes the full SHA-256 against the
  release manifest before activation.

The allow-list starts empty, which disables Reticulum OTA. A paired app
registers its own Reticulum identity hash over the authenticated BLE session,
which is the trusted local channel. Multiple identities are supported. The BLE
path needs no allow-list because the device credential already authorizes it;
the serial path is physical access.

## On-device OTA coordinator

A single coordinator owns the update regardless of transport. It stages the
image, verifies it, writes the inactive slot, activates it, and confirms it:

1. Open a session and stream the image into a PSRAM staging buffer (using the
   existing `ExternalMemory` placement patterns), writing sectors to the
   inactive slot as they complete.
2. Verify the ESP image magic and the full SHA-256 against the manifest before
   the slot is made bootable.
3. Select the slot with `OtaUpdater::next_partition` (never hand-picking a
   slot, which sidesteps the pinned crate's `ota_seq == 0` edge case), write
   it, then `activate_next_partition` and reboot.
4. After boot, the new image runs a health window and marks itself `Valid`. The
   last-known-good slot and its digest are recorded in `device_config` so a
   failing image can self-rollback.

The coordinator also reports current version, running slot, and last-update
status through the device API and the display.

## Transports

### BLE upload

The app reads a firmware artifact and streams it over the existing authenticated
BLE device API. New `OP_FIRMWARE_*` operations (from `0xf015`) provide a session
open/chunk/commit/status exchange with about 420 bytes per chunk, mirroring
`OP_LXMF_READ`. Expected throughput is roughly 6–25 KB/s depending on the
connection interval, so a 2.3 MiB image lands in about two to six minutes. This
is the first deliverable because it is bench-testable and exercises the
coordinator that every other transport shares.

### Reticulum transfer

The app runs an in-process Reticulum node (`rete-tokio`) and sends the image as
a Resource over a link to the node's OTA management destination:

- The app's node connects over a `tcp_client` interface to an operator transport
  node (a `rete-daemon` `tcp_server` or a Python RNS node), establishes a link
  to the target's `e290.ota` destination, and calls `SendResource`.
- The target destination is announced like the existing Nomad destination. The
  app learns the target's destination hash from `OP_IDENTITY_SUMMARY` during
  BLE pairing and can trigger `OP_MANUAL_SERVICE_ANNOUNCE` for path discovery.
- The node accepts the Resource only when the originating identity is
  allow-listed, then hands the reassembled image to the coordinator.

This one path covers off-grid LoRa nodes, internet-connected gateways over the
existing TCP uplink, and multi-hop mesh relaying.

The remaining device-side gap is in the owned `rete` fork: the embedded
`rns-rete` ingress gate currently rejects every Resource-context packet
(`ResourceIngressDisabled`). Enabling resource ingress and wiring
`ResourceOffered`/`ResourceProgress`/`ResourceComplete` into the node actions,
with the allow-list check at acceptance, is the fork change to make and the
submodule pointer to bump.

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

1. **Layout and BLE foundation** — partition restructure and fresh provisioning
   path, the OTA coordinator, the `OP_FIRMWARE_*` device API surface, and the
   app upload flow.
2. **Reticulum delivery** — the `rete` fork resource-ingress change, the OTA
   management destination and allow-list, and the in-process app-side node.
3. **Desktop and serial** — the Tauri package with native serial flashing and
   the web WebSerial panel.

## Verification

- Layout: host tests for the partition contract, `xtask build/package/check-elf`,
  and the per-slot OTA image outputs.
- BLE: a powered E290 update over BLE with a deliberate bad image to prove
  verification and self-rollback, plus `bun run api:generate`/`native:bindings`
  and `bun run verify` for the generated surface.
- Reticulum: the `rns_1_3_8` conformance test for the fork, a two-node powered
  update over LoRa and over TCP, and a gateway relay test for multi-hop.
- Desktop and serial: packaging CI and a serial flash against a bricked board.

## Open items

- Custom bootloader with auto-rollback as the true anti-brick fallback.
- Temporary fast-profile negotiation to shorten LoRa transfers.
- A WASM/WebSocket transport for the app-side node if a pure-browser build is
  later required; the node stays behind a transport abstraction so this remains
  possible.
