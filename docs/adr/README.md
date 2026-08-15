# Architecture decision records

ADRs explain why the system owns responsibilities where it does. Later records
refine earlier ones; implementation and qualification status may therefore be
newer than the original decision context.

| ADR | Decision |
| --- | --- |
| [0001](0001-phase-0-scaffold.md) | Phase-0 scaffold and dependency boundaries |
| [0002](0002-rete-provisional-foundation.md) | Adopt Rete as the provisional RNS foundation |
| [0003](0003-lora-first-interface-fabric.md) | LoRa-first heterogeneous Reticulum interface fabric |
| [0004](0004-sole-flash-coordinator.md) | Sole flash coordinator with operation-scoped store access |
| [0005](0005-active-data-durability-fail-stop.md) | Interface-local fail-stop for an active DATA durability fault |
| [0006](0006-authenticated-local-api-bearer.md) | Authenticated local device-API bearer |
| [0007](0007-device-api-credential-authority.md) | Immutable device-API credential authority |
| [0008](0008-durable-authorization-provenance.md) | Durable authorization provenance and journal schema 2 |
| [0009](0009-device-api-credential-store-and-pairing.md) | Device-API credential store and initial pairing policy |
| [0010](0010-device-api-live-pairing-protocol.md) | Wired developer pairing protocol |
| [0011](0011-durable-rns-inbox-qualification.md) | Durable raw-RNS inbox qualification slice |
| [0012](0012-application-event-and-resource-ownership.md) | Application-event ownership and bounded RNS Resource admission |
| [0013](0013-bounded-lxmf-wire-boundary.md) | Bounded LXMF wire and service ownership |
| [0014](0014-durable-lxmf-message-ownership.md) | Durable LXMF message ownership |
| [0015](0015-universal-expo-client-and-generated-bindings.md) | Universal Expo client and generated TypeScript/native boundaries |
| [0016](0016-bound-link-data-lxmf-ingress.md) | Bound Link DATA ownership for direct LXMF |
| [0017](0017-reticulum-peer-discovery-and-proximity-bootstrap.md) | Reticulum peer discovery and proximity bootstrap |
| [0018](0018-durable-lxmf-delivery-policy.md) | Durable LXMF delivery policy and direct-Link support |
| [0019](0019-secure-ble-appliance-onboarding.md) | Secure BLE appliance onboarding |
| [0020](0020-wifi-station-reticulum-tcp-border-interface.md) | Wi-Fi station and Reticulum TCP border interface |
| [0021](0021-owner-controlled-announces-public-tcp-bootstrap-and-rmap-discovery.md) | Owner-controlled announces, public TCP bootstrap, and RMAP discovery |
| [0022](0022-local-message-notifications-and-display-indicator.md) | Durable local message notifications and appliance indicator |
| [0023](0023-receiver-local-ingress-evidence-and-reticulum-proof-probe.md) | Receiver-local ingress evidence and Reticulum proof probe |
| [0024](0024-atomic-reboot-applied-lora-profile-and-rmap-import.md) | Atomic reboot-applied LoRa profile and RMAP import |
| [0025](0025-durable-packet-correlated-radio-tracing.md) | Durable packet-correlated radio tracing |
| [0026](0026-sideband-compatible-lxmf-message-location.md) | Sideband-compatible LXMF message location |
| [0027](0027-board-owned-durable-lxmf-retry.md) | Board-owned durable LXMF delivery loop |

For the implementation as it exists now, start with the
[current architecture](../architecture/overview.md). For user-visible
qualification and remaining gaps, see [current status](../status.md).
