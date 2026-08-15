# Current alpha status

The project has a usable two-node proof of concept: an E290 can stay powered as
a standalone Reticulum LoRa node, a phone can pair over BLE without importing a
credential file, and the Expo client can exchange and retain basic LXMF
messages. The architecture deliberately remains broader than the currently
powered-qualified LoRa transport. The selectable `wifi-tcp-proof` profile now
has a bounded powered bring-up and stream proof for its station uplink and
second Reticulum interface. On board `3E:88`, the diagnostic image initialized
BLE, restored its saved bond, advertised, associated with Wi-Fi, acquired DHCP,
connected to the configured public TCP peer, accepted native Reticulum ingress,
and completed local announce writes. After the scoped point-to-point announce
reflection fix, one 420-second diagnostic run completed without a reset,
transmit failure, socket close, or reconnect. This is not yet full
LoRa-to-TCP/TCP-to-LoRa border-routing, loss-recovery, RMAP-presentation, or
long-soak qualification.

## Capability matrix

| Capability | Current state |
| --- | --- |
| E290-HF board support | Powered on two ESP32-S3R8 boards with 16 MiB flash and 8 MiB mapped PSRAM |
| HT-RA62-HF/SX1262 LoRa | Powered NA915 CAD, receive, transmit, RNode framing, and continuous receive |
| LoRa radio profile | Authenticated atomic persistence of frequency, bandwidth, SF, coding rate, and +14/+17/+20/+22 dBm requested output with reboot-to-apply; the app separates running from after-restart state and locally previews one copied RMAP `RNodeInterface` block before explicit save; configurable modulation and +22 dBm range remain unpowered |
| Radio and route diagnostics | API 1.12 diagnostics retained in the current unreleased API 1.18 contract and the app's foreground Network panel expose bounded interface, LoRa, Reticulum-counter, and retained-route state; API 1.15 labels the aggregate last TX as DATA or ordinary and retains DATA packet length/SHA/interface evidence across later ordinary TX, including pre-authorization failures, while last RX remains final-hop accepted-packet evidence and routes/local LRU use do not imply reachability or last-heard time |
| Packet-correlated RF trace | API 1.16 exposes a boot-aware three-event page over a 32-event firmware ring containing durable-submission route selection, terminal DATA dispatch and per-frame `TxDone`, logical LoRa RX signal, and proof/timeout terminal evidence. The app incrementally imports it into the schema-7 trace tables retained by current SQLite schema 9, correlates it with message attempts and queue-time phone location, presents global and per-message views, and exports complete JSON or RFC 4180 CSV snapshots. Source/host tests remain to be followed by powered field qualification. |
| Reticulum | Announces, path discovery, encrypted DATA/proofs, responder Links, and bounded initiator Links; path-request clocks now start only after a complete interface transmission |
| Opportunistic LXMF | Powered, durable, and available through the app; current source keeps one accepted LXMF submission durably `Preparing` across serialized volatile RNS attempts, unknown-route discovery, receipt timeout, and reboot. Board retry uses 5s/15s/60s/5m/15m-capped backoff plus deterministic <=20% jitter, permits one automatic retry globally, prefers fresh sends, and wakes on the exact destination path becoming usable. Source/host qualification passes; the autonomous retry/reboot path remains to be powered-qualified. |
| Direct LXMF | Powered for a one-packet Link DATA subset, including reuse and stale-Link recovery |
| LXMF persistence | Append-only receiver store plus durable outbound submission journal; physical format 2 retains optional first-arrival interface/signal evidence while reading legacy format-1 records, an identity-bound collection watermark records the highest message durably imported by the client, and SQLite schema 9 retains that evidence, schema-6 closed per-attempt phone-location stamps, schema-7 packet-correlated trace side tables, schema-8 authenticated message location, and schema-9 first-import receiver-phone location plus phone altitude accuracy |
| Per-message ingress evidence | Unreleased API 1.14 summary key 10 and the app's message details preserve an inbound message's receiver-local first-arrival interface and optional paired RSSI/SNR; old records, outbound rows, and non-radio transports may have no signal, and source/host tests pass without a powered field qualification |
| Reticulum path measurement | Unreleased API 1.14 operations `0xf012`/`0xf013` and each conversation's **Measure path** action drive one boot-scoped standard path-and-proof probe through normal routing; source/host tests pass, while powered field and third-party responder qualification remain open |
| Contacts and message requests | The app can rename phone-local contacts without changing their authenticated destination; an authenticated inbound sender does not need to be a reciprocal contact and appears as an unsaved message request that can be opened, replied to, or saved, while outbound-only unsaved conversations remain labeled separately |
| Message activity | A global Activity surface and each message's details page show the same newest-first durable app journal, including submission/message identifiers, lifecycle status, packet length/hash evidence when the device supplied it, and opt-in queue-time phone-location state for initial and explicit manual submissions. Board-owned carrier retries remain one `Preparing` submission and are observed through packet-correlated radio-trace events rather than app-created rearm rows. |
| LXMF message location | API 1.17 optionally carries one typed phone fix on `experimental.lxmf.basic_send`; the board encodes Sideband `FIELD_TELEMETRY` (`0x02`) into the signed LXMF payload, and the app keeps that location immutable across retries. Sent and received timelines show the location and complete details with an OpenStreetMap action. This is an LXMF application field, not Reticulum routing metadata or board GNSS; source/host tests pass without a powered interoperability qualification. |
| NomadNet/Micron | One static page over an anonymous direct request |
| BLE local API | Authenticated foreground bearer with durable bonding and fileless onboarding |
| E-paper | Powered readiness, pairing/passkey, board suffix, configured-service views, and a durable `NEW n` uncollected-message indicator; burst updates are coalesced and pairing/recovery views retain priority |
| Phone notifications | Native iOS/Android builds reconcile durable inbound-import activity into deduplicated local notifications while foregrounded or resuming; taps select the owning appliance and conversation, while locked-phone background BLE wake remains deferred |
| Expo web | Builds and runs against the same-origin HTTP service |
| Expo iOS | Physical foreground BLE onboarding, reconnect, switching, messaging, and page browse qualified |
| Expo Android | Native build path exists; BLE hardware behavior is not yet powered-qualified |
| Network configuration | Authenticated BLE management, durable configuration for four Wi-Fi profiles, one outbound IPv4-or-DNS TCP peer, independent Wi-Fi/ordinary-announce/RMAP policies, one complete LoRa profile, redacted reads, compare-and-swap updates, and reboot-to-apply behavior are host/build-qualified; semantic formats 1--3 are read and format 4 is written |
| Wi-Fi station | `wifi-tcp-proof` has powered BLE-controller, saved-bond restore, advertising, WPA2-Personal association, IPv4 DHCP, ARP, and bidirectional LAN-unicast evidence on `3E:88`; current source removes TCP routing eligibility on physical-link or IP-configuration loss, while powered loss recovery and long soak remain open |
| Reticulum TCP interface | `wifi-tcp-proof` has a powered public-peer connection, native Reticulum ingress, successful local announce writes, and a 420-second no-reset/no-failure diagnostic run on interface 2. Current source marks LoRa interface 1 `Internal` and point-to-point TCP interface 2 `Boundary`; host regressions prove that the exact announce learned on TCP cannot enter LoRa while local/LoRa announces can still reach TCP. Ordinary DATA, proofs, Links, and path requests retain their normal routing. Powered boundary-policy, upstream-loss, and complete LoRa↔TCP qualification remain open. |
| Announce controls | Authenticated **Announce now** queues and coalesces one spacing-aware primary/LXMF/Nomad cycle; automatic ordinary announces have an independent durable switch |
| Nearby network visibility | While the app is foregrounded and its Nearby surface is open, it polls the bounded authenticated `lxmf.delivery` observation table every ten seconds and advances displayed ages between reads; it summarizes up to 32 observed peers independently of contacts, grouped by LoRa or Reticulum TCP interface with hops and LoRa RSSI/SNR when available, but is not a complete route table |
| Public endpoint presets | The app catalog contains three source-linked DNS presets for the single active peer; endpoint availability and advertised transport identity are not runtime-qualified |
| RMAP discovery | Opt-in `rnstransport.discovery.interface` payload, signature, cost-16 proof-of-work, public-uplink-gated initial publication, manual retry, and six-hour scheduling are source/build-qualified; public ingestion/presentation is not powered-qualified |
| Phone location | Public RMAP location remains an explicit one-shot foreground capture with privacy rounding and location-free-by-default policy. Separately, private **Field location telemetry** requests high-accuracy foreground fixes and schema 6 durably stamps each new attempt locally; its enabled preference survives app restarts and appliance switches until explicitly disabled, while collection remains foreground-only. Schema-7 radio-trace queries and exports join that stamp to correlated events, and schema 9 retains the latest available receiver-phone fix with a message's first inbox import. The Map draws an inbound sender-to-receiver line when both endpoints exist, reporting horizontal distance, both phone elevations, elevation delta, optional three-dimensional separation, and final-hop signal. Attempt locations are queue-time phone positions; receiver locations are inbox-import-time phone positions, not exact RF times, board GNSS, or routed RF paths. Recipient-visible LXMF message location is the distinct opt-in capability described above. |
| Wi-Fi SoftAP local API | Separate build/host-qualified `wifi-api-proof` development surface; normal appliance management remains BLE-first |
| USB local API | Developer/control workflows exist; the BLE appliance profile keeps USB as diagnostics rather than its normal client bearer |
| Additional Reticulum interfaces | LoRa interface 1 is powered-qualified; TCP interface 2 has bounded powered connection and packet-boundary evidence only in `wifi-tcp-proof`, not complete border-routing qualification; BLE and USB remain management bearers rather than Reticulum packet interfaces |
| Onboard GNSS | Reserved as future work; the current optional RMAP coordinate comes from a one-shot phone capture |

## Next gateway qualification milestone

The bounded station and public-TCP startup trial now passes. The next milestone
is complete gateway qualification, not more endpoint breadth:

- complete an authenticated app session while Wi-Fi remains online;
- resolve a configured public hostname and recover from DNS, peer, and
  access-point loss without taking LoRa offline;
- repeat the two-node lossy-message trial with an unreachable configured access
  point and prove path request TxDone, response reception, route learning, DATA,
  and delivery proof in order;
- validate an opt-in interface-discovery entry, including a location-free run
  and a separately consented phone-location run, on [rmap.world][rmap-info];
- exercise ordinary manual and automatic announce policy on hardware; and
- demonstrate and inspect traffic in both LoRa-to-TCP and TCP-to-LoRa
  directions.

Even a successful packet exchange will not by itself qualify a production
border node. The product layer now implements the required
**Boundary**-to-**Internal** announce block, but pinned Rete and the product do
not yet implement the complete interface-mode matrix, recursive-discovery
rules, or announce-cap behavior described by Reticulum's
[interface-mode documentation][rns-interface-modes].

## Important limits

- The firmware and app are an alpha, not a production-secure messenger.
  Current message and identity stores are plaintext at rest. The local API is
  authenticated, but its current wireless application record suites should not
  be treated as an independently confidential transport for sensitive data.
- Basic LXMF title/content messages and one typed Sideband-compatible location
  field are supported. Propagation nodes, Resource transfer, attachments,
  arbitrary nonempty fields, stamps/tickets, and a complete LXMF router are not.
- The Nomad slice serves one small static Micron page. Forms, files, dynamic
  content, Resource responses, and general NomadNet client/server behavior are
  not implemented.
- The current outbound runtime retains 128 submissions, while the physical
  journal admits 154 complete submission lifetimes before migration or
  reclamation is required.
- Accepted LXMF submissions remain durably `Preparing` while the board performs
  path discovery and fresh serialized carrier attempts. Receipt timeout is an
  attempt outcome, not `Failed(DeliveryTimeout)` for the logical message; the
  old attempt must be exactly acknowledged before the next is admitted. The
  board uses 5/15/60-second, 5-minute, and capped 15-minute base delays with
  deterministic additive jitter no greater than 20 percent, allows one
  automatic retry globally, prefers fresh sends, and wakes an exact
  destination when its path changes from unusable to usable. Boot restores
  `Preparing` LXMF after 15 seconds plus jitter. The signed wire and message ID
  remain fixed while each attempt receives fresh ciphertext and a fresh token.
  The app performs no automatic rearm on timers, startup, reconnect, Sync, or
  Nearby observations. Explicit same-row **Retry now** remains a transitional
  action for legacy or permanently terminal rows. Raw RNS DATA keeps its
  conservative one-shot timeout and ambiguous-reset behavior. These paths are
  source/host-qualified but not yet powered-qualified.
- LXMF storage is append-only. Durable collection acknowledgement now exists,
  but delete, human read/unread state, compaction, retention, and migration
  policies remain open. The current acknowledgement is appliance-global rather
  than per controlling principal. Current firmware reads physical-format-1
  records and appends format-2 records; after any format-2 append, rolling back
  to format-1 firmware cannot safely mount that store.
- Network-configuration semantic formats 1 through 3 remain readable, but every
  material current mutation writes format 4 with the complete LoRa tuple.
  Firmware predating format 4 cannot mount that store. Erase network
  configuration deliberately before such a downgrade; the normal merged-image
  flash preserves it.
- Message activity timestamps are best-effort times when this app's durable
  store observed a state mutation. They are not RF transmit/receive times,
  remote delivery times, or peer-supplied timestamps. The activity journal is
  capped at 10,000 immutable events. Opening a pre-schema-4 database cannot
  reconstruct earlier transitions, and retention pruning removes the oldest
  events; either condition marks the history as incomplete without invalidating
  the current message state. Schema-5 migration leaves earlier inbound rows
  without ingress evidence rather than inventing it. Schema-6 migration marks
  legacy attempt locations `not_observed` and history incomplete instead of
  fabricating historical fixes.
- Field location telemetry must be enabled explicitly once in Activity; the
  phone-local preference then survives app restarts and appliance switches
  until it is turned off. It never adds coordinates to LXMF or RMAP. The durable stamp is the
  latest phone sample when the app queued the initial submission or an explicit
  manual replacement, not the board's position or exact RF emission time.
  Autonomous board attempts reuse the submission's existing observation; the
  app does not wake in the background to resample location. The app shows
  capture time and sample age. Correlated radio-trace JSON/CSV exports include
  the stamp; the app does not yet provide a field-trial map.
- Sender-attached message location has a separate durable default and per-draft
  switch. If selected, a fresh foreground phone fix must succeed before the
  message is queued. The resulting Sideband-compatible field is sent to the
  recipient, contributes to the LXMF signature and message ID, and remains
  unchanged across every retry. Its fields map occupies at most 52 bytes rather
  than the one-byte empty map, reducing available title/content by up to 51
  bytes under the existing one-packet limits. The receiver UI can open that
  coordinate in OpenStreetMap; it must not present it as route or RF position.
- Message details correlate an inbound LXMF row with its immutable
  first-arrival interface and optional RSSI/SNR. The signal is measured by this
  receiving appliance on the final hop and may be from a relay. It does not
  provide end-to-end hop history, sender-side signal, or outbound receiver
  telemetry. The separate radio trace now provides local route, dispatch,
  `TxDone`, proof-ingress, and terminal evidence for correlated outbound
  attempts. An outgoing trace still cannot invent the receiving appliance's
  RSSI, and Nearby announce readings are never attributed to messages.
- The one-shot probe proves only Reticulum path-and-proof reachability to an
  enabled `rnstransport.probe` responder. It does not prove LXMF availability,
  estimate throughput, or expose remote request RSSI. Its returning-proof
  signal is local final-hop evidence and may measure a relay; public nodes may
  disable the responder. The packet-correlated ring is centered on durable
  message attempts and logical LoRa RX; durable probe history remains deferred.
- BLE is foreground-only in the app. Local message alerts reconcile while the
  app is active or resumes, but reliable locked-phone arrival requires a native
  background BLE watcher and platform restoration/companion-device lifecycle.
  The complete disconnect/resume matrix, multi-phone authority/revocation, and
  Keychain/Keystore-backed credentials remain open.
- Wi-Fi passphrases are plaintext in the alpha configuration partition.
  Profiles are WPA2-Personal only; board-side scanning, hidden-network
  refinements, multiple TCP peers, server mode, IFAC, and live apply are
  deferred. One peer may use a literal unicast IPv4 address or bounded hostname
  resolved on each reconnect. The board first uses the built-in DHCP-configured
  resolver path, then raw-queries each DHCP-provided resolver. After those
  bounded attempts fail, globally plausible dotted names may be sent to
  `1.1.1.1` and then `9.9.9.9`; single-label and common local/private suffixes
  never use that public fallback. The Network card shows the gateway, resolver
  list, socket stage, each attempt outcome, response code, and successful
  resolution source. Material configuration changes require a reboot.
- Wi-Fi/BLE controller buffers and task stacks consume strict internal RAM;
  PSRAM cannot satisfy those allocations. The former 72 KiB profile therefore
  reduced the Wi-Fi static RX pool from ten buffers to four and the receive
  block-ack window from six to two. The current 120 KiB coexistence profile
  restores the pinned driver's ten/six defaults. A powered 618-second run kept
  authenticated BLE and public TCP online while completing 282 LoRa
  transmissions without an allocation, station, TCP, BLE, panic, or reset
  fault. Longer pressure and disconnect/recovery qualification remain open.
- The 618-second public-peer run predates the current source-level boundary
  filter: it forwarded public announces onto LoRa, completed 282 LoRa
  transmissions, and rejected 24 expired ordinary owners. Current source
  computes an exact per-ingress announce egress set from transport-neutral
  interface roles and suppresses **Boundary** TCP announces on **Internal**
  LoRa without changing ordinary DATA, proofs, Links, path requests, local
  announces, or Internal-to-Boundary announces. This source policy still needs
  powered qualification. It also does not partition Rete's bounded 64-entry
  route/identity capacity: accepted public announces can still evict useful
  LoRa-learned identities even though they no longer consume RF airtime.
  Role-aware cache protection and Python-style per-egress announce caps for
  future Full/roaming interfaces remain required follow-up work.
- Public endpoint presets are convenience metadata, not trust anchors or an
  availability promise. An upstream can observe the appliance's IP address,
  timing, traffic volume, and availability and can delay or drop traffic even
  though Reticulum protects application payloads end to end.
- RMAP discovery and location are opt-in, but disabling either does not retract
  an already propagated marker. [RMAP documents][rmap-info] that an entry can
  remain visible for up to seven days. The retained phone coordinate has no
  automatic refresh and no altitude.
- The two-board proofs are bounded. Sustained multi-hop routing, high-capacity
  fill, physical power-cut matrices, range, interference, allocation pressure,
  and long soak remain unqualified. The app can atomically save an E290-fitted
  frequency/BW/SF/CR/power tuple, but only the default modulation has powered
  evidence. Peers require matching modulation, and fitted-path/RNode checks do
  not replace the operator's regional frequency, duty-cycle, antenna, and EIRP
  obligations. The +14, +17, +20, and +22 dBm choices are requested chip output
  rather than measured EIRP. A single failed message at roughly 500 metres does
  not distinguish RF range from path-discovery timing, placement, antenna
  orientation, power integrity, or vehicle/building attenuation. Use the
  [controlled two-board range check](getting-started/firmware-e290.md#controlled-two-board-range-check)
  before making a range claim.
- The `wifi-tcp-proof` profile has bounded powered BLE/Wi-Fi startup, station,
  DHCP, public-TCP connection, ingress, local-announce-write, and 420-second
  stability evidence on one board. That is not full gateway qualification:
  authenticated BLE use during Wi-Fi operation, continued two-board LoRa
  messaging, DNS success, upstream loss/backoff, both LoRa↔TCP forwarding
  directions, and long soak remain unqualified. BLE and USB are still not
  Reticulum packet transports.

The detailed and source-linked backlog is maintained in
[POC limits and deferred work](poc-known-defects.md). Revision-bound powered
claims are indexed under [qualification history](README.md#qualification-history).

[rmap-info]: https://rmap.world/info.html
[rns-interface-modes]: https://reticulum.network/manual/interfaces.html#interface-modes
