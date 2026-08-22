# Crate index

This index describes the PRNS product architecture. The dependency graph in
`Cargo.toml` and `Cargo.lock` remains authoritative. The retired alpha network,
custom control-bearer, and duplicate storage packages are no longer present.

## Portability classes

| Class | Meaning |
| --- | --- |
| Portable | No board, HAL, executor, or packet-interface dependency; host-testable |
| Interface-specific | Board-neutral or target code tied to one packet interface |
| Board-specific | Encodes E290 facts or drives an E290 peripheral |
| Host | Runs on a phone, desktop, or server |
| Target composition | Connects PRNS, applications, stores, and hardware into one firmware image |

## Network and applications

PRNS is an exact git dependency, not a local wrapper crate. It owns the
Reticulum engine, routing, Links, requests, Resources, persistence, and packet
interfaces. Product crates begin at the application boundary.

| Crate | Class | Purpose |
| --- | --- | --- |
| `device-api` | Portable | Management and OTA application wire types, CBOR codecs, paths, and generated DTO source |
| `nomad-protocol` | Portable | Bounded NomadNet page protocol |
| `lxmf-wire` | Portable | Python-compatible LXMF wire views, parsing, signatures, composer, and message IDs |
| `lxmf-model` | Portable | Durable LXMF application model and signature state |
| `lxmf-ingress` | Portable | Parsing of ordinary borrowed PRNS delivery carriers |
| `lxmf-durable-ingress` | Portable | Product persistence and deduplication after PRNS delivery |

## Product storage

| Crate | Class | Purpose |
| --- | --- | --- |
| `nor-flash-region` | Portable | Checked partition-relative raw NOR view |
| `network-config-store` | Portable | Bounded LoRa, Wi-Fi, TCP, name, and feature configuration |
| `lxmf-store` | Portable | Append-only power-loss-safe inbound LXMF store |
| `lxmf-mailbox-store` | Portable | Durable client collection watermark |

The E290 product owner contains mirrored Reticulum identity material, the
management allow-list, and the outbound LXMF journal. They are typed quotas
inside the common `product_state` arena, not physical partitions or one
package per application.

## Client data and adapters

| Crate | Class | Purpose |
| --- | --- | --- |
| `appliance-store` | Host | Identity-bound SQLite product data |
| `appliance-sync` | Host | Typed management requester and durable message synchronization |
| `appliance-runtime` | Host | Long-running chat actor and product projections |
| `appliance-native` | Host | Persisted mobile PRNS node, product clients, UniFFI, JNI, and Expo boundary |
| `appliance-service` | Host | Persisted host PRNS node and web gateway |
| `appliance-display-model` | Portable | Allocation-free semantic display state |

The native and service packages issue ordinary PRNS path, Link, request,
Resource, and DATA operations. TypeScript presents their product DTOs; it does
not own a Reticulum engine or opaque custom BLE session.

## E290 target

| Crate | Class | Purpose |
| --- | --- | --- |
| `eink-ssd1680` | Board-specific | Allocation-free async e-paper driver |
| `firmware/e290` | Target composition | E290 board facts, PRNS SX126x configuration, identity storage, one PRNS node, product applications, storage owner, and hardware tasks |
| `xtask` | Host | Recurring E290 build, package, ELF, and doctor commands |

`firmware/e290` is the product composition boundary. E290-specific wiring and
policy stay there unless another PRNS board can reuse the same abstraction.

## Generated code

Rust is the source of truth for shared API and native types. The following are
generated outputs and must be regenerated with their source change:

- `clients/appliance/src/generated/api.ts`;
- UniFFI/C++/Kotlin/Objective-C++ and TurboModule bindings under
  `clients/appliance/modules/appliance-native`;
- Android PRNS bridge outputs; and
- `crates/appliance-service/assets/app.js` and its manifest.

Do not hand-edit generated files.
