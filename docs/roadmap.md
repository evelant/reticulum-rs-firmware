# Roadmap

The project has a usable alpha appliance: pair a phone over BLE, exchange and
retain LXMF over LoRa, inspect network evidence, browse the bounded Nomad page,
configure Wi-Fi, and connect the node to an upstream Reticulum TCP peer. The
firmware continues receiving, routing, retrying, and storing while the app is
absent.

## Working now

| Area | Current capability |
| --- | --- |
| Hardware | E290-HF with 16 MiB flash, 8 MiB mapped PSRAM, SX1262 LoRa, and e-paper |
| Reticulum | Announces, routing, path discovery, DATA/proofs, forwarding, responder Links, and bounded initiator Links |
| LXMF | Durable opportunistic and direct delivery, board-owned retry, message requests, contacts, and optional sender location |
| Diagnostics | Interfaces, routes, LoRa counters, first-arrival RSSI/SNR, probes, and packet-correlated traces |
| Local access | Authenticated BLE, fileless onboarding, persistent bond, board-only bond recovery, and multiple app profiles |
| Internet uplink | Managed Wi-Fi station and one outbound Reticulum TCP peer |
| Discovery | Nearby LXMF announces, manual/automatic service announces, and opt-in RMAP publication |
| NomadNet | Discovery and one bounded static Micron page request/response |
| Client | One Expo/TypeScript application for iOS, Android, and web with native Rust state ownership |
| Display | Readiness, pairing, identity, service state, and durable new-message indication |

## Near-term priorities

1. Validate sustained multi-hop routing and both directions of LoRa/TCP border
   traffic under disconnect and reconnect.
2. Improve RF range through controlled antenna, power, placement, modulation,
   and packet-trace testing.
3. Add storage retention, compaction, export, and reset/recovery UX.
4. Complete native background notification lifecycles on iOS and Android.
5. Expand NomadNet beyond the static one-packet page subset.
6. Harden credential management, revocation, encrypted local storage, and
   multi-user policy.
7. Add other boards and packet interfaces through the existing portable
   boundaries.
8. Add over-the-air firmware updates, starting with the BLE foundation and the
   Reticulum identity-allow-listed delivery path; see the
   [OTA plan](ota-updates.md).

## Important limitations

- The firmware and app are alpha and can require coordinated reprovisioning
  after protocol or storage changes.
- LXMF storage is bounded and append-oriented; long-term retention and
  compaction are not complete.
- One outbound TCP peer is supported. Public endpoint presets are convenience
  metadata, not trust anchors or availability guarantees.
- Wi-Fi/BLE coexistence consumes scarce internal RAM even when ample PSRAM is
  available.
- Nearby observations and retained routes are evidence, not connected-peer or
  delivery guarantees.
- Receiver RSSI/SNR describes the final hop into that appliance, not every hop
  or the complete end-to-end path.
- The Reticulum probe tests path-and-proof reachability, not LXMF service,
  throughput, or remote request signal.
- RMAP publication is public and disabling it does not immediately retract
  already propagated data.
- Locked-phone notifications, full Android/iOS background recovery, storage
  encryption, and multi-phone authorization are incomplete.
- The E290 exposes requested SX1262 output power, not calibrated conducted
  power or antenna EIRP. Range depends heavily on antenna quality, placement,
  power integrity, terrain, and matching radio profiles.

Add roadmap items only when they remain actionable in the current architecture.
