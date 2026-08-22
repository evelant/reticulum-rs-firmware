# Roadmap

The current development image is composed around PRNS with one generic,
allocation-free Embassy accepted-announce observer. The source contains the
PRNS E290 node, product applications, generic storage arena, native client node,
host web gateway, and bounded OTA transfer. The legacy alpha graph and custom
control bearer have been removed. It is not yet a qualified release: mobile,
network, and rollback claims require powered evidence.

## Implemented architecture

| Area | Current implementation |
| --- | --- |
| Hardware | E290-HF with 16 MiB flash, mapped PSRAM, SX1262 LoRa, and e-paper composition |
| Reticulum | Exact PRNS revision owns routing, proofs, Links, requests, Resources, persistence, and interfaces; the generic Embassy observer reports already accepted announces without changing admission |
| Interfaces | PRNS LoRa, Bluetooth Auto, and optional outbound TCP lanes in the product recipe |
| Applications | Shared management/OTA, `lxmf.delivery`, `nomadnetwork.node`, and opt-in announce-only RMAP destinations |
| Management | Identified-Link request authorization and physical-presence enrollment backed by a durable identity allow-list |
| LXMF | Python-compatible parsing and signature states, durable inbound store, persist-before-accept outbound journal, and board-owned retry |
| Client | Persisted native/host PRNS nodes feeding the existing Rust SQLite and sync actors |
| OTA | A/B layout, bounded PRNS Resource chunks, flash readback, complete digest/image validation, activation, native staging UI, and explicit reboot |
| Storage | One generic `product_state` arena plus independent PRNS persistence; no physical partition per application |

This table describes code present in the migration worktree, not powered proof
or a release claim.

## Migration completion priorities

1. Pass formatting, workspace tests, clippy, app verification, Python RNS/LXMF
   interop, and both E290 profiles from a clean clone.
2. Continue powered qualification of PRNS's native SX126x and Bluetooth Auto
   paths on the E290, including recovery and sustained traffic.
3. Exercise iOS, Android, and host Bluetooth Auto enrollment, reconnect,
   process death, sync, and explicit foreground/background behavior.
4. Qualify LoRa/Internal and TCP/Boundary routing, multi-hop behavior,
   reconnect, route/ratchet restore, and app-absent operation.
5. Complete OTA health projection and live native transfer progress, then
   qualify the packaged rollback-enabled
   bootloader and 30-second application confirmation on powered hardware. Prove
   valid, malformed, interrupted, unauthorized, flash-failure, startup-failure,
   and pre-confirmation reset cases over Bluetooth Auto, LoRa, and TCP.

## Product priorities after migration

1. Add storage retention, compaction, export, and reset/recovery UX within the
   generic application registry.
2. Complete native background notification lifecycles on iOS and Android.
3. Expand NomadNet beyond the bounded static page subset.
4. Add management revocation, multi-user policy, secure backup, and at-rest
   protection.
5. Improve RF range through controlled antenna, power, placement, modulation,
   and packet-trace testing.
6. Add other boards and applications through PRNS public boundaries without
   application-combination partition schemes.

## Important limitations

- The migration is an alpha reset: earlier board and app state is discarded.
- PRNS immediate proof can precede durable LXMF persistence; the documented
  crash window is intentionally not hidden by a deferred-proof extension.
- Direct LXMF Link delivery remains open until it is implemented through
  unmodified PRNS public APIs or a genuinely generic attribution gap is proven.
- The packaged bootloader and application confirmation implement ESP-IDF's
  rollback state machine, but they are not an anti-brick claim until a powered
  candidate failure selects the previous image without USB intervention.
- One outbound TCP peer is supported. Endpoint presets are convenience
  metadata, not trust anchors or availability guarantees.
- Nearby announces and retained routes are evidence, not connected-peer or
  delivery guarantees.
- Receiver RSSI/SNR describes the final hop into that appliance, not complete
  end-to-end history.
- Product and PRNS state are not encrypted at rest in the alpha image.
- The E290 exposes requested SX1262 output power, not calibrated conducted
  power or antenna EIRP.

Add roadmap work only when it fits the PRNS/product ownership boundary and has
an evidence-backed acceptance test.
