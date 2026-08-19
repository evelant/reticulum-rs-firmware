# Crate index

This index maps every workspace package to its owning layer and its portability
class. It is the entry point for navigating the workspace and for deciding where
a new behavior belongs. The ownership rules live in
[`docs/architecture.md`](../architecture.md); the dependency graph is
authoritative in `Cargo.toml` and `Cargo.lock`.

## Portability classes

| Class | Meaning |
| --- | --- |
| Portable | No board, HAL, executor, or bearer dependency; builds and tests on the host |
| Transport-specific | Board-neutral but tied to one bearer (LoRa, BLE, Wi-Fi/TCP) |
| Board-specific | Encodes one board's facts or drives one board's peripherals |
| Host | Client-side code that runs on a phone, desktop, or server, not on the appliance |
| Target composition | The firmware image that connects portable services to one board |

## Layer overview

| Layer | Packages |
| --- | --- |
| Reticulum protocol | `rns-rete`, `node-core`, `nomad-protocol` |
| Packet interfaces | `interface-router`, `tx-supervisor`, `tx-handoff`, `radio-interface`, `radio-lora-phy`, `radio-tx-dispatch`, `rns-interface-discovery` |
| Durable messaging | `storage-model`, `storage-journal`, `storage-actor`, `nor-flash-region`, `submission-projector`, `submission-runtime`, `lxmf-wire`, `lxmf-model`, `lxmf-store`, `lxmf-ingress`, `lxmf-durable-ingress`, `lxmf-mailbox-store` |
| Local control API | `device-api` and its framing, session, handoff, credential, pairing, BLE, and adapter packages |
| Client data and sync | `appliance-store`, `appliance-sync`, `appliance-runtime` |
| Client adapters | `appliance-native`, `appliance-service`, `host-ble`, `device-client`, `device-pairing-client` |
| E290 target | `board-e290`, `board-e290-radio`, `eink-ssd1680`, `firmware/e290` |
| Build tooling | `xtask` |

## Control bearer vs packet interface

A **packet interface** carries Reticulum traffic. LoRa is interface 1 and the
outbound TCP client is interface 2; they plug into `interface-router`,
`tx-supervisor`, and `tx-handoff`. A **control bearer** carries the authenticated
local device API to a nearby client. BLE is the current control bearer and is
not a Reticulum packet interface.

This distinction is a durable boundary, not a naming coincidence. A future
USB or RNode host-control bearer reuses the portable `device-api-framing`,
`device-api-session`, and `device-api-handoff` crates and only replaces the
bearer profile (`device-api-ble`). A future BLE or USB **packet interface**
would be a new actor beside LoRa and TCP, distinct from the BLE control bearer.
Keep the two words apart when naming new packages.

## Index

### Reticulum protocol

| Crate | Portability | Purpose |
| --- | --- | --- |
| `rns-rete` | Portable | Rete protocol foundation and Reticulum integration: identity, announces, DATA, proofs, Links, and application events |
| `node-core` | Portable | Bounded protocol orchestration and the durable TX ownership types between the node and its interfaces |
| `nomad-protocol` | Portable | State machine for one bounded NomadNet Micron page fetch |

### Packet interfaces

| Crate | Portability | Purpose |
| --- | --- | --- |
| `interface-router` | Portable | Fixed, transport-neutral outbound routing between one node owner and per-interface actors |
| `tx-supervisor` | Portable | Sole node and interface supervisor owning the router, ticket paths, and permit services |
| `tx-handoff` | Portable | Bounded permit and acknowledgement channels between node and interface actors |
| `radio-interface` | Portable | RNode/LoRa packet boundary: profiles, RX pipeline, logical packet access, and diagnostics |
| `radio-lora-phy` | Transport-specific | Board-neutral `lora-phy` SX126x owner for persistent RNode RX, CAD, and atomic TX |
| `radio-tx-dispatch` | Transport-specific | Sole-radio dispatch edge serializing DATA and ordinary permits over one LoRa radio |
| `rns-interface-discovery` | Portable | Bounded no-std interface-discovery announce encoding and stamping |

### Durable messaging and storage

| Crate | Portability | Purpose |
| --- | --- | --- |
| `storage-model` | Portable | Portable durable record model and indexes |
| `storage-journal` | Portable | Power-loss-safe fixed-slot NOR journal |
| `storage-actor` | Portable | Sole-owner storage actor serializing mutations across stores |
| `nor-flash-region` | Portable | Checked partition-relative raw NOR flash view |
| `submission-projector` | Portable | Persist-before-ack projection from volatile Reticulum TX observations |
| `submission-runtime` | Portable | Transport-neutral durable submission coordinator and board-owned retry |
| `lxmf-wire` | Portable | Bounded, allocation-free LXMF wire views and validation |
| `lxmf-model` | Portable | Allocation-free durable semantic model for inbound LXMF |
| `lxmf-store` | Portable | Append-only power-loss-safe NOR store for inbound LXMF |
| `lxmf-ingress` | Portable | Transport-neutral admission of LXMF application events |
| `lxmf-durable-ingress` | Portable | Durable LXMF ownership before local application-event acknowledgement |
| `lxmf-mailbox-store` | Portable | Durable mailbox collection watermark |
| `network-config-store` | Portable | Bounded Wi-Fi and Reticulum TCP configuration store |
| `announce-clock` | Portable | Power-loss-safe announce-emission boot epoch |
| `device-identity-store` | Portable | Power-loss-safe immutable Reticulum device identity store |

### Local control API

| Crate | Portability | Purpose |
| --- | --- | --- |
| `device-api` | Portable | Portable logical protocol: CBOR messages and common authorization policy |
| `device-api-framing` | Portable | Allocation-free record framing for byte-stream bearers |
| `device-api-session` | Portable | Bounded authenticated sessions above framing |
| `device-api-handoff` | Portable | Allocation-free owning handoff for authenticated jobs |
| `device-api-credentials` | Portable | Fixed-capacity device-owned credential authority |
| `device-api-credential-store` | Portable | Power-loss-safe raw-NOR credential authority store |
| `device-api-pairing` | Portable | Allocation-free live pairing records and possession proofs |
| `device-api-pairing-control` | Portable | Pre-authentication credential-store initialization control |
| `device-api-pairing-policy` | Portable | Physical-presence and enrollment-window policy |
| `device-api-adapter` | Portable | Authenticated adapter into durable Reticulum submissions |
| `device-api-ble` | Transport-specific | Portable GATT profile contract for the BLE control bearer |
| `ble-bond-store` | Transport-specific | Power-loss-safe one-bond raw-NOR store for embedded BLE |

### Client data, sync, and adapters

| Crate | Portability | Purpose |
| --- | --- | --- |
| `appliance-store` | Host | Durable SQLite data model for appliance clients |
| `appliance-sync` | Host | Device synchronization and messaging services |
| `appliance-runtime` | Host | Transport-neutral durable LXMF chat actor and client projections |
| `appliance-display-model` | Portable | Allocation-free semantic display state |
| `appliance-native` | Host | Native mobile binding surface (UniFFI + Expo TurboModule) |
| `appliance-service` | Host | Host service and generated TypeScript boundary |
| `device-client` | Host | Reusable authenticated host client for the device API |
| `device-pairing-client` | Host | Transport-neutral pairing session and durable credential state |
| `host-ble` | Transport-specific | macOS BLE transport for the device API |

### E290 target

| Crate | Portability | Purpose |
| --- | --- | --- |
| `board-e290` | Board-specific | Immutable E290 board facts and RF safety policy |
| `board-e290-radio` | Board-specific | Bidirectional HT-RA62/SX1262 owner for the E290 |
| `eink-ssd1680` | Board-specific | Allocation-free async SSD1680 e-paper driver |
| `peer-discovery` | Portable | Bounded history of authenticated Reticulum peer announces |
| `firmware/e290` | Target composition | The E290 image: task scheduling, memory placement, and peripheral construction |

### Build tooling

| Crate | Portability | Purpose |
| --- | --- | --- |
| `xtask` | Host | Recurring build, packaging, ELF, and doctor commands |

## Vendor and generated code

`vendor/lora-phy-3.0.1` is the reviewed local SX126x policy overlay and
`vendor/rete` is the owned Rete submodule; both are documented in
[`NOTICE`](../../NOTICE) and
[`docs/reference/dependencies.md`](dependencies.md). Generated TypeScript,
UniFFI, and native bindings are not listed here because their Rust sources are
the authoritative definition.
