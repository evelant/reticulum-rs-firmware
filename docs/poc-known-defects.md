# Usable-firmware POC: known limits and deferred work

> This is the detailed engineering backlog, including source-level limits and
> historical defect narratives. Use [current alpha status](status.md) for the
> concise user-visible capability and limitation summary.

This file records deliberate proof-of-concept limits that must not be mistaken
for the intended final product boundary. The architecture remains
transport-neutral and the full feature set is not constrained to boards without
PSRAM. These items may be refined after the first USB-controlled, two-node LXMF
proof unless a failing test promotes one into a release blocker.

## Durable submission and message storage

- Current source retains 128 outbound submissions in external PSRAM. The 129th
  novel request reports capacity exhaustion before a NOR write, while replay of
  an already accepted idempotency key remains available at capacity. The
  append-only journal and terminal projector do not reclaim finalized entries,
  and the schema-3/physical-2 journal has a separate 154-acceptance lifetime
  ceiling.
  Source/host tests exercise the 128-plus-one boundary; there is not yet a
  powered 128-message fill, remount, pressure, or timing qualification.
- Three generic 128-entry E290 host cases own the product-capacity fake runtime
  and routing fixtures by value. Their thin test wrappers use an explicit 4 MiB
  worker stack, so the ordinary package test command remains reliable without
  environment overrides. This host-harness accommodation is not target stack
  evidence; firmware constructs the resident boot owners in external PSRAM and
  remains subject to the separate linked-ELF stack policy.
- Earlier one-entry and 16-entry profiles and their proof artifacts remain
  valid only for their named revisions. The 16-entry result is historical
  evidence, not the current device limit and not evidence that the current
  128-entry PSRAM profile has been filled on hardware.
- The first corrected 128-entry A/B chat-alpha run exposed an asymmetric
  sender-lifecycle deadlock after on-demand path discovery. Board B's
  submission 3 was received and committed verbatim by Board A, but repeated
  authenticated status reads left the sender at `Preparing`; submission 4
  consequently remained `Queued`. The direct-path A-to-B submission reached
  `Delivered`. Root cause was scheduler inversion: an ordinary packet queued
  behind a physically transmitted DATA frame, while the storage lane waited
  for ordinary quiescence before making that DATA frame's observation durable.
  Current source prioritizes the exact retained frame and has a regression that
  recreates the owner cycle. The subsequent
  [powered repeat](e290-lxmf-chat-alpha-proof.md) reached `Delivered` and exact
  peer import in both directions; long-running contention and pressure remain
  unqualified.
- The durable LXMF inbox now has an identity-bound, power-loss-safe
  acknowledged-through watermark used for appliance notification state. The
  app advances it only after durable local import, and boot rebaselines it when
  an erased/recreated LXMF store reuses a numeric handle for different content.
  The store still has no delete, compaction, or retention-policy operation and
  only appends. Physical format 2 reads legacy format-1 records and writes new
  first-arrival evidence in previously reserved header space. Once it appends
  any format-2 record, format-1 firmware cannot safely roll back and mount that
  mixed store; there is no downgrade migration. Collection acknowledgement is
  not human read/unread state.
- Received normalized LXMF and raw RNS inbox plaintext are stored unencrypted
  in their dedicated flash partitions. The selected outbound LXMF carrier and
  destination are likewise retained as plaintext intent in the durable
  submission journal. API authentication protects access over a bearer; it does
  not provide encryption at rest.
- LXMF enumeration, reads, and collection acknowledgement are currently global
  to every authenticated principal. They require no persisted permission bit
  and provide no per-principal mailbox ACL, watermark, or ownership filtering.
- A host read verifies the final normalized-wire SHA-256 from committed
  metadata. Individual flash chunks revalidate their extent headers but do not
  independently hash the complete message on every read.
- An inbound LXMF message whose source identity has not yet been announced
  retains its exact application-event authority so it can be admitted later.
  The former one-second reacquire loop is closed: periodic retries use a
  five-second initial base plus at most 20 percent deterministic jitter,
  exponential bases whose complete interval stays below five minutes, and an
  authenticated exact-source announce wakes matching entries
  immediately. There is intentionally still no age expiry, so sixteen hostile
  or permanently unresolved sources can occupy the bounded event profile until
  reboot; selecting a retention/expiry policy remains production hardening.

## Basic outbound LXMF subset

- The first semantic send operation composes only Python-compatible basic LXMF:
  binary title and content, no stamp, and either an empty fields map or the one
  typed API-1.17 Sideband-compatible location field. It durably
  retains one exact complete signed LXMF wire through 431 bytes without
  selecting a delivery method; generic RNS destination DATA remains a separate
  383-byte intent. The current automatic policy uses the destination-stripped
  bytes as its compatible opportunistic carrier when eligible and no ready
  matching cached Link is selected. Current source also establishes or reuses
  product-initiated outbound Links from a registry bounded to the native
  product table and prepares the exact complete wire as one Link DATA packet
  when required.
  Propagated, Resource, ticket, stamp, attachment, and arbitrary-fields sends
  remain deferred. Empty title and empty content together are supported and
  match the independent Python vector.
- The signing source is always the node's registered inbound Single
  `lxmf.delivery` destination. No API caller may choose a source hash or obtain
  private identity material.
- Python LXMF's 391-byte opportunistic carrier fits inside the distinct
  431-byte complete-wire durable intent, so the former 384-through-391 journal
  rejection is closed without raising the generic-RNS ceiling. Powered direct
  evidence now covers fresh-Link delivery plus the composed same-Link
  reuse/receiver-replay path; Resource and broader Link ownership/fault
  qualification remain open. Source tests cover path-first Link establishment,
  a snapshotted
  hop/first-interface-aware deadline starting at first actual LINKREQUEST
  dispatch, exact pending-Link abort, exact-Link single-flight from Active
  through unacknowledged Terminal, same-handle reuse after acknowledgement,
  timeout-follower parking, Link-DATA receipt projection, and the
  authorized-frame durability barrier. The
  [bounded two-board powered record](e290-direct-link-powered-proof.md) forced
  a fresh Link with a 392-byte carrier that could not fit the 391-byte
  opportunistic ceiling, committed the exact 408-byte message on the receiver,
  returned its proof, reached durable `Delivered`, and retained both sides
  across board and app restart. The
  [current-image recovery record](e290-stale-link-recovery-powered-proof.md)
  additionally qualifies exact retirement after receiver reboot and delivery
  of the next sequential submission over a fresh Link. The
  [same-Link reuse and replay record](e290-same-link-reuse-replay-powered-proof.md)
  then starts sender A from a fresh boot and delivers submissions `6` and `7`
  with different idempotency keys but one identical direct-only 408-byte LXMF
  wire and message ID beginning `9692c4`. Their 483-byte Reticulum packets have
  distinct hashes, while the receiver host projection advances exactly one row,
  from 11 rows/sequence 13 to 12 rows/sequence 14. Exact same-handle reuse and
  the receiver's `AlreadyDurable` result are source-qualified and physically
  exercised, not independently telemetered by the client API.
- The initial direct profile serializes one establishment transaction and
  retains at most four reusable product-initiated outbound Links, matching the
  E290 native Link table. Lookup and capacity checks prune only `Closed` or
  unknown entries. A `Stale` Link is retained for possible revival, is not
  selectable for direct DATA, and still occupies its slot. If all four entries
  remain occupied, a direct-required message for a fifth destination stays
  durably `Preparing` under the firmware's one-second backoff, which repeats
  while the registry remains full; registry pressure is not terminal failure.
  A short eligible message for that destination can still use opportunistic
  DATA. The alpha has no generic capacity-pressure or LRU eviction policy, so
  maintenance is not otherwise guaranteed to free a slot. Exact direct
  Link-DATA `DeliveryTimeout` is narrower: current source evicts that precise
  reusable handle and routes normal authenticated close after the attempt's
  exact terminal acknowledgement. The same accepted LXMF submission remains
  durably `Preparing` and may establish a fresh Link on a later board-owned
  attempt.
- Direct DATA is now single-flight per exact Link from Active attempt through
  durable acknowledgement of its Terminal owner. A later direct-required or
  routed-overflow submission for the busy destination remains durably
  `Preparing` under typed one-second backpressure, without creating a second
  same-destination Link. If the leader times out, its follower stays parked
  until durability-first retirement clears the stale handle and then requests
  a fresh Link, so session-wide close cannot invalidate a younger same-Link
  receipt. Eligible short LXMF may still use opportunistic delivery, and work
  on another usable Link remains schedulable.
- The four-entry reusable outbound registry and Rete's four-entry native
  owned-Link table are distinct bounds. Inbound responder Links share the
  native table with outbound initiators, so native pressure can defer new
  direct establishment even when the outbound registry has room. Runtime
  scheduling isolates that pressure to the affected submission instead of
  blocking later opportunistic work.
- Pinned Rete now closes and reclaims a responder-side `Handshake` that never
  authenticates LRRTT, closing the former indefinite four-slot occupancy
  defect. The Python-compatible budget is
  `360 + 6 * max(1, post-ingress hops)` seconds from confirmed LRPROOF
  completion, with LINKREQUEST admission as the bounded fallback. Two residuals
  remain: the accepted `u8` hop value can stretch occupancy from 366 seconds to
  1,890 seconds without a separate maximum-hop admission policy, and
  maintenance exposes only aggregate `closed_links`/`links_closed`, not a
  reason-specific establishment-timeout event. Timeout is intentionally not a
  malformed or cryptographic `links_failed` event. Native generic initiator
  expiry remains absent; the firmware's dispatch-relative deadline and exact
  abort continue to own that half. No upstream issue or PR has been opened.
- The registry cannot yet map a responder-side Link to the remote
  `lxmf.delivery` identity, so responder/backchannel reuse is deferred. Link
  transactions, registry contents, path and deadline clocks, retry history, and
  the Resource-wait marker are boot-volatile. The durable LXMF obligation is
  not: current boot recovery restores an LXMF submission already in
  `Preparing`, then arms a fresh carrier attempt after 15 seconds plus
  deterministic jitter of at most 20 percent. It retains the same signed wire
  and message ID but does not attempt to rehydrate the old receipt, ciphertext,
  or Link. Raw RNS DATA still conservatively finalizes ambiguous `Preparing` or
  `AwaitingDelivery` work as `InterruptedByReset`.
- Link establishment expiry or loss clears the volatile transaction and the
  E290 firmware retries the still-`Preparing` message after one second. This can
  repeat indefinitely in the same boot: there is no boot-local attempt ceiling
  and no persisted retry budget. Within that boot, Link-MDU overflow remains
  `Preparing` until Resource has bounded durable ownership and recovery; reboot
  restores that obligation but does not add Resource support.
- Basic composition currently uses allocation-backed Rete LXMF packing and
  signing before copying into caller storage. The E290 POC must measure heap
  high-water behavior. A bounded `encoded_len`/`pack_into` composer remains
  desirable for smaller targets and fallible-allocation handling.
- The client supplies one Unix timestamp in whole milliseconds in the exact
  product range `1..=8_796_093_022_207_999` and must retain it across retries.
  This is a deliberately narrower, JavaScript-friendly subset of Python LXMF's
  binary64 timestamp. The firmware has no trusted wall clock yet.
- Unknown destinations no longer terminalize after two path requests. The
  device retains the same durable `Preparing` submission, runs throttled
  two-request discovery cycles one minute apart, and bypasses the wait as soon
  as a usable path is learned. This keeps no-path retry autonomous while the
  board is powered, and boot restores the same `Preparing` obligation after its
  delayed retry arm.
- Each submission path-request offer is correlated with the exact ordinary
  packet slot and reuse generation. Its response clock starts only after a
  concrete interface reports a complete transmission: all LoRa frames reached
  TxDone or the complete TCP frame was written. CAD denial, expiry, rejection,
  partial transmission, and interface failure do not consume the request
  ordinal; if every eligible hop returns without confirmation, the same offer
  is retried after a bounded delay. Link-establishment and Nomad request timing
  still use their existing router-admission correlation and need the same
  physical-completion audit before adverse-link qualification.
- Opportunistic and direct LXMF receipt expiry now retire only the volatile RNS
  attempt. The logical submission stays in its one durable `Preparing` loop;
  the next attempt cannot start until the old receipt/packet terminal has been
  exactly acknowledged. Backoff bases are 5 seconds, 15 seconds, 60 seconds,
  5 minutes, and then a capped 15 minutes, with deterministic additive jitter
  no greater than 20 percent. One automatic retry may run globally, fresh sends
  are preferred, and an exact destination path transition from unusable to
  usable wakes only that destination. The signed LXMF wire and message ID stay
  fixed while every RNS attempt gets fresh ciphertext and a fresh token.
- The client no longer runs automatic rearm timers or wakes terminal rows on
  startup, reconnect, Sync, or Nearby observations. Commit-before-send
  reconciliation and status polling remain. Explicit same-row **Retry now** is
  retained as a transitional action for legacy or permanently terminal rows;
  it is not used to drive a current board-owned `Preparing` obligation. Source
  and host regressions cover the autonomous policy, but receipt timeout,
  disconnected-app retry, path wake, and reboot recovery still need powered
  E290 qualification.
- Persistent same-boot preparation pressure can currently return `NoAction`
  while the runtime still reports a completed step. A continuously saturated
  direct-Link or internal preparation resource may therefore make the firmware
  poll that obligation without a retry delay. This does not lose the durable
  LXMF message, but the pressure path still needs an explicit wake or bounded
  delay before soak qualification.
- Generic builds must provision at least as many projected-event slots as
  durable submission slots. Boot recovery can otherwise encounter more pending
  LXMF obligations than the projected-event FIFO can represent and make no
  further recovery progress. The E290 production profile uses 128 slots for
  both capacities; a compile-time relation or overflow recovery policy remains
  future hardening for other board profiles.

## Static Nomad node responder

- Current source registers one inbound Single `nomadnetwork.node` destination
  with raw UTF-8 `Metalbeard` announce application data, inbound Links enabled,
  and Reticulum `PROVE_NONE`. It accepts only the canonical anonymous
  MessagePack `nil` (`0xc0`) value for `/page/index.mu` and returns one static
  UTF-8 Micron page no larger than 400 bytes as a direct single-packet response.
- This is intentionally not a general Nomad host. It has no Resource, forms,
  files, dynamic content, or executable page support. Response-allocation
  pressure discards the request. An ambiguous terminal response fault may
  fail-stop the responder until reset rather than risk losing an exact action
  owner.
- The [bounded powered Nomad proof](e290-nomad-powered-proof.md) exercised one
  complete path: Board A announced its distinct `nomadnetwork.node`
  destination over LoRa, Board B exposed the associated destination through
  Nearby/Browse, MetalbeardMobile authenticated to B over production BLE, and
  the user confirmed that `/page/index.mu` was fetched from A and rendered on
  the phone. The permanent authenticated device API composes API 1.6 start/poll
  with the product Nomad client runtime. That client has one boot-scoped slot:
  it retains one principal-owned active or terminal fetch, replays an exact
  principal/idempotency-key retry, rejects a distinct start while active, and
  lets the next distinct start replace a terminal outcome. Poll returns only a
  complete UTF-8 page of at most 400 bytes or a closed failure; Resource
  responses are unsupported.
- The Expo client now drives API 1.6 through its existing authenticated
  appliance session and derives a nearby LXMF peer's associated
  `nomadnetwork.node` destination in Rust for one-tap browsing. Pasting the
  peer's primary or `lxmf.delivery` hash correctly fails because neither names
  the distinct Nomad destination. The powered proof covers only the bounded
  static page; an independent Nomad announce directory and a general Micron
  renderer remain absent, and pressure, reset, concurrent-client, flash
  readback, cache-disabled interaction, and soak remain unqualified.

## Discovery and multiple transports

- LoRa remains the only fully powered-qualified Reticulum transport, but
  submission, inbox, API, and routing ports do not name LoRa or SX1262. The
  `wifi-tcp-proof` composition adds an independent outbound TCP packet actor as
  interface 2. Bounded powered evidence now covers BLE/Wi-Fi coexistence,
  association and DHCP, a public-peer stream, native ingress, local announce
  writes, DNS through the DHCP resolver, and one 420-second stable diagnostic
  run. After the 2026-08-14 coherent upstream esp-rs jump, e13e88 restored its
  existing station/peer configuration and reached the public TCP interface on
  three consecutive boots; `rmap.world` resolved in 49, 72, and 33 ms, and the
  first run admitted an announce observed nine hops away. The third run stayed
  online through 25 bonded BLE connect/authenticate/drop cycles and ongoing
  LoRa transmissions without a logged Wi-Fi, DNS, TCP, or reset failure.
  Complete two-way LoRa/TCP forwarding, induced association-loss recovery, and
  long soak remain unqualified. BLE, USB, and the Wi-Fi SoftAP proof remain
  local management bearers rather than Reticulum packet interfaces.
- The board retains at most four Wi-Fi profiles and exactly one active outbound
  TCP peer. The peer may be literal IPv4 or a bounded hostname; DNS is resolved
  again at each reconnect, with no multi-address racing or persisted last-known
  address. The embedded resolver first gets one bounded attempt using DHCP DNS.
  On failure, the actor raw-queries each DHCP-provided resolver, then gives
  globally plausible dotted names bounded `1.1.1.1` and `9.9.9.9` attempts.
  Common local/private suffixes never reach the public resolvers. API 1.10 and
  the Expo Network card retain the gateway, DHCP list, built-in outcome,
  raw-socket setup, per-resolver stage/result, response code, and successful
  source/address so a system-resolver fault is distinguishable from UDP send,
  route, response, and parse failures. The fallback parser validates the source
  endpoint, transaction ID, response shape, and echoed question, but currently
  accepts the first answer-section A record without proving that its owner
  follows the requested-name/CNAME chain. That is a hardening defect, not a
  blocker for the observed direct-A `rmap.world` response. A global Wi-Fi
  switch suppresses station/TCP startup after reboot without deleting saved
  profiles or the peer. Every material change still requires reboot. The app's
  three source-linked public presets are convenience metadata only: they have
  no health service, automatic failover, or cryptographic transport-ID pin.
  Public peers are untrusted carriers under Reticulum's
  [trustless-network model][rns-trust]; they can observe source IP, timing,
  volume, and availability and can delay or drop traffic.
- Automatic primary/LXMF/Nomad service announces have a durable switch separate
  from Wi-Fi and RMAP. An authenticated manual request remains available when
  that switch is off, but success means only that one spacing-aware service
  cycle was queued. Repeated requests coalesce until the primary, optional
  `lxmf.delivery`, and Nomad items have been consumed; the operation is not a
  synchronous radio receipt. When RMAP is enabled, the same request also makes
  its cached stamped discovery payload due. Queue, reset-between-items, and
  RF-pressure behavior are host/source-qualified only.
- Opt-in RMAP support registers an announce-only
  `rnstransport.discovery.interface` destination and constructs the
  [RMAP v4][rmap-info] flags, MessagePack `RNodeInterface` map, and 32-byte
  cost-16 proof-of-work stamp without a resident expanded workblock. Stamp
  search is cooperative and the cached payload is scheduled every six hours.
  When a public TCP peer is configured, the initial due event remains pending
  until interface 2 is online; LoRa-only configurations retain immediate
  publication. This avoids consuming the six-hour cadence before the public
  uplink can carry the announce.
  Public ingestion, map presentation, proof-of-work timing under real load, and
  interaction with ordinary LoRa traffic remain unpowered. There is no
  withdrawal operation; after disabling discovery or location, the prior
  marker can remain visible for RMAP's documented seven-day retention window.
- Optional RMAP location comes from one explicit foreground phone capture, not
  continuous tracking or onboard GNSS. The app defaults to roughly 100-metre
  rounding before the board stores fixed E6 latitude/longitude. Firmware
  retains no accuracy, capture time, phone identity, or altitude. The last
  coordinate may remain stored while sharing is off, and changing or clearing
  it takes effect only after reboot; an already published position remains
  subject to the RMAP retention limit.
- The app's RMAP radio-profile importer is local paste handling, not a live
  RMAP lookup. It accepts exactly one copied Reticulum `RNodeInterface` block,
  normalizes supported numeric values, applies the E290-facing validation, and
  updates only an unsaved preview. Missing `txpower` retains the current draft
  power; an explicit **Save for next restart** remains necessary. A successful
  preview is neither regulatory authorization nor powered interoperability
  evidence for that tuple.
- Current source projects only authenticated `lxmf.delivery` announces into a
  volatile 32-peer table, with at most 256 application-data bytes per peer.
  API 1.5 pages one boot-scoped record at a time through the already
  authenticated appliance session, and the Expo **Nearby** picker can add or
  open the durable contact with one tap. A bounded iOS/two-E290 powered run
  opened an existing contact without endpoint entry and delivered one short
  opportunistic message in each direction with exact peer import. A
  fresh-contact **Add** remains unqualified; the direct-Link path was
  [qualified separately](e290-direct-link-powered-proof.md) with a forced
  oversize one-packet message.
  The table expires no peers by age. Current source carries the weakest
  complete-packet LoRa RSSI/SNR observation through sealed ingress and records
  the authoritative observing interface instead of assuming LoRa. TCP and
  future non-radio interfaces explicitly report unknown signal values. The app
  groups these authenticated observations by interface, including peers not
  saved as contacts; it does not claim that this is a connected-peer roster or
  an enumerable Reticulum route table. While the app is foregrounded and the
  connected **Nearby** surface is visible, it refreshes this bounded projection
  every ten seconds and advances observation ages locally from the successful
  fetch time. Reads do not overlap: the next delay starts only after the
  previous bounded page walk settles, and hiding the surface cancels its
  pending timer. The host retry scheduler retains the table's boot incarnation
  and generation: repeated reads of unchanged nonempty history do not wake
  terminal retries, while a strictly newer same-incarnation observation does.
  A board reboot establishes a new incomparable baseline rather than
  fabricating freshness.
- Nearby contact discovery is not appliance authorization. The current path
  does not scan phone-to-phone BLE, publish a public E290 share service, or
  implement the signed contact-card fallback reserved by ADR 0017. QR/deep
  links, E290-mediated BLE sharing, native proximity APIs, and NFC remain
  carrier work after the shared Rust-owned signed envelope exists.
- Local secondary-destination path responses currently use a temporary wrapper
  in `crates/rns-rete`. A cleaner uncommitted implementation exists only in the
  reference Rete checkout and is deliberately not combined with the pinned
  workaround. Moving ownership into Rete requires a separately reviewed
  upstream/local commit and a deliberate dependency-pin update.
- The wrapper suppresses rebroadcast of locally generated path responses and
  is qualified for the current one-interface powered product. It does not yet
  retain Python-style per-interface pending path-request forwarding state, so
  it must be revisited before qualifying simultaneous LoRa and TCP routing.
- Ingress-derived broadcasts now use the authoritative interface topology and
  exact packet identity: a received announce or unknown path request from a
  point-to-point TCP peer becomes `AllExcept` only for its matching forward,
  while unrelated due/local announces in the same envelope remain `All`.
  Shared-medium LoRa preserves same-interface forwarding and Rete's delayed
  announce retry. Current source additionally marks LoRa `Internal` and TCP
  `Boundary`, derives the exact allowed announce-egress ID set from the
  authoritative registry, and applies that set only to the matching
  ingress-derived announce. A public TCP announce therefore cannot enter LoRa;
  local and LoRa announces may still reach TCP, and DATA, proofs, Links, and
  path requests keep their ordinary targets. Any point-to-point announce whose
  target is changed or suppressed has only its exact nonlocal native retry
  removed, since Rete's pending queue does not retain the ingress role needed
  to route that delayed copy safely. Restoring redundant delayed forwarding
  requires Rete to carry per-entry ingress routing provenance.
- `AllExcept` also has a known single-interface waiting limit: when the excluded
  source is the only eligible interface, the ordinary router has no dispatch
  candidate. The current alpha border profile therefore qualifies this policy
  only with another eligible interface (normally LoRa); clean no-route
  completion remains follow-up work.
- The project-owned Full/Boundary/Internal announce subset does not implement
  the complete mode-specific rules Reticulum documents for Access Point,
  roaming, gateway, recursive path discovery, and per-egress announce caps
  [interface][rns-interface-modes]. True border routing therefore remains
  unqualified until those semantics and a powered two-direction gate pass.
- The outbound TCP stream currently implements Reticulum's standard HDLC
  framing but not Python Reticulum's TCP tunnel-synthesis/restoration behavior.
  A first connection can exchange packets and announces, but learned-path
  restoration across reconnects is not release-qualified until the tunnel
  mechanism is implemented and interoperable.
- The embedded TCP decoder currently accepts native Reticulum frames only up
  to the 500-byte Rete core MTU. Python's TCP interface and hosted Rete TCP
  transports allow larger stream frames. Oversized upstream frames are
  therefore a known interoperability gap even though they do not prevent the
  initial TCP connection or a small RMAP announce.
- A queued LINKREQUEST remains bound to the retained route selected when it was
  created. If that route's interface becomes ineligible before first dispatch
  while another transport remains usable, the ordinary router can wait
  indefinitely instead of rebuilding the request for the alternate route;
  definitive exhaustion currently closes local submission admission instead
  of returning that transaction to path discovery. The current one-interface
  powered proof is unaffected, but route re-resolution and a bounded
  pre-dispatch establishment deadline are required before simultaneous
  multi-transport Link establishment is release-ready.
- A retained route is considered usable when its selected local interface is
  online; that check does not prove that a remote peer or retained repeater is
  still reachable. Opportunistic LXMF DATA removes its exact retained path
  after a delivery timeout is durably projected, so several already-active
  attempts can still reuse the same stale route. There is no proactive
  next-hop reachability check before dispatch.
- Pinned Rete performs announce replay/dedup rejection before comparing the
  candidate's hop count or ingress interface with the retained path. If the
  same announce arrives over TCP and LoRa, whichever copy is accepted first
  can suppress a later copy that would have produced the more useful route.
  This did not affect the LoRa-only field configuration with no enabled TCP
  peer, but it must be corrected or explicitly qualified before simultaneous
  multi-interface route selection is release-ready.

## Client and product surface

- USB Serial/JTAG remains the provisioning/recovery bearer and was the first
  physically qualified authenticated bearer. The session and logical API are
  bearer-neutral, and the opt-in BLE profile now has bounded powered
  qualification carrying the same ordered RDA1 stream under suite 3. BLE now
  supports GPIO21/passkey-bound, fileless phone onboarding with Rust-owned
  app-private credential publication; both E290s completed that flow, remained
  available as device-keyed profiles, and passed explicit switching in both
  directions. The Wi-Fi profile remains implemented but not powered-qualified
  and still requires a pre-established credential. The native client also
  retains an alpha system-file import fallback: TypeScript copies an
  operator-selected artifact into app-owned staging without decoding its
  bytes, Rust validates and create-only publishes the canonical credential,
  and the E290 device ID supplies the exact BLE advertised-name selector. This
  implements and host-tests fresh-install sandbox seeding and removes
  first-match selection from the app source. The
  [bounded physical iOS proof](e290-expo-ios-ble-lora-proof.md) additionally
  covers one system-picker import into a signed Release, exact-E290 foreground
  BLE selection and suite-3 authentication, and one sequential basic LXMF
  message in each direction over LoRa. A follow-up cold foreground launch
  automatically reconnected and physically passed the keyboard-aware composer.
  Android hardware, background restoration, the full mobile lifecycle matrix,
  pressure, soak, and overlapping BLE owners remain unqualified, and the import
  clones transferable authentication authority.
  Import also publishes the first canonical artifact immediately: there is no
  Rust-owned secret-free identity preview/confirmation, so selecting the other
  board's valid file irreversibly binds that app data until the user clears it.
  The picker source remains for the user to delete, the app-private Documents
  credential may be backed up, and invalid/existing credentials have no in-app
  replacement flow.
  The transfer filename and 24-bit-suffix BLE local name are selection hints,
  not identity proofs; a peer can spoof either and only the credential-bound
  suite-3 handshake authenticates the device. That session provides record
  authentication and integrity but no application-layer confidentiality.
  Expo SDK 57 also retains Android's content-provider read grant without
  exposing a release operation; iOS's picker-created temporary copy is deleted
  after app-owned staging. Per-client revocation, an Android grant-release
  owner, identity-bound import preview/install, Keychain/Keystore storage,
  backup exclusion, stronger post-alpha wireless authentication policy,
  independently revocable multi-phone authority, factory-reset/recovery UX,
  and the full mobile lifecycle matrix remain deferred. Managed host
  profiles expose secret-free initialization, pairing, Pending resume/abort,
  and reset progress through the Expo client. Powered evidence covers one
  managed first run, real reset,
  retained-profile service restart, and Expo-to-LoRa-to-peer message pass on
  the E290 pair. Activation-ambiguous repair and the alternate Pending recovery
  paths remain to qualify.
- The signed iOS foreground build emits launch warnings that its delegate
  implements background fetch and remote-notification handlers without the
  corresponding `UIBackgroundModes`, and that its nib references a missing
  `SplashScreen` image. They did not prevent the bounded foreground BLE proofs,
  but the app configuration and launch artwork must be reconciled before
  claiming polished launch or background behavior.
- Native iOS and Android builds now reconcile the app's durable inbound-import
  activity journal into deduplicated local notifications while JavaScript is
  foregrounded or resumes. A per-profile atomic watermark prevents historical
  replay, exact-conversation foreground views are suppressed, and notification
  taps select the owning appliance and sender. This does not provide reliable
  locked-phone delivery: the BLE byte pump remains foreground-owned. Native
  Core Bluetooth restoration/AccessorySetupKit and Android companion-device
  lifecycle integration remain a separate phase.
- Native first-run BLE discovery lists bounded service advertisements and
  requires explicit selection without connecting. The production display now
  renders the same exact six-character board suffix as the app card, so an
  operator can match a selected appliance without guessing which GPIO21 button
  belongs to it. The suffix and advertisement remain unauthenticated selection
  hints; only the credential-bound suite-3 session authenticates the device.
- The permanent display actor now owns a coordinated Home snapshot after its
  physical boot-completion gate: board suffix, configured `LORA NA915`
  transport, BLE
  local-app bearer, configured LXMF/Nomad services, and application setup
  guidance. Application authority is composed first; an active authority with
  a usable BLE bearer but no durable bond is then refined to
  `BLE RECOVERY - OPEN APP`, while an empty authority continues to show initial
  pairing guidance. `READY - OPEN APP` therefore no longer conflates a
  preserved application credential with a missing Bluetooth bond. A durably
  successful application activation updates the cached setup fact before
  fallible client response delivery, and a successful replacement bond clears
  the recovery presentation without claiming application reactivation;
  failures and timeouts remain terminal. A durable uncollected-message count is
  reconstructed at boot and projected as `NEW n`/`NEW 99+`; the transition to
  nonzero and acknowledgement back to zero refresh promptly, while further
  nonzero bursts are coalesced for five seconds. Pairing, recovery, boot, and
  terminal pairing views retain priority over message telemetry. The remaining
  Home fields are deliberately composition, not task-spawn
  completion or live health:
  `LORA`, `BLE`, `LXMF`, and `NOMAD` do not assert that an actor remains online
  or connected. Live link state, outbound queue state, RF signal metadata,
  storage pressure, and post-boot faults remain future display inputs. Only the
  initial Home render has a
  physical completion gate. Pairing-success Home and timeout/failure terminal
  commands are accepted into the coalescing handoff without waiting for panel
  completion; a render fault can therefore leave stale e-paper pixels until a
  later successful update or reboot boot-clear. Adding a terminal render gate
  is deferred rather than expanding the pairing owner in this slice. A
  readback-verified upgrade of the paired `e13f88` board then passed the
  powered Home-screen visual check and retained-profile iOS reconnection.
  Fresh-pair success/failure transitions under the new Home presentation still
  need separate powered requalification.
- The allocation-free display model and coalescing handoff own a zeroizing
  six-digit passkey plus explicit timeout/failure/reboot clearing. The portable
  SSD1680 driver and isolated E290 display HIL pass powered full-refresh,
  deep-sleep, and visual demo checks; integrated secure BLE onboarding,
  GPIO21-bound Secure Connections, the durable bond store, and Rust-owned phone
  credential install are implemented and bounded by the alpha proof. See
  [ADR 0019](adr/0019-secure-ble-appliance-onboarding.md). Trouble `0.6.0`
  still has no public pre-SMP admission hook, so the first alpha design accepts
  transient non-bonding SMP work and immediate disconnect as a bounded
  connection/UX denial-of-service risk; only a presence-bound epoch with an
  authenticated, durably stored bond may reach device authorization.
- The current session authenticates records but does not encrypt the USB
  transcript. The diagnostic and chat-alpha CLIs accept title/content bytes in
  process arguments, where shell history or same-host process inspection can
  expose them. Do not use these POC paths for sensitive messages.
- The foreground [LXMF chat alpha](../crates/lxmf-chat-cli/README.md) provides a
  local SQLite conversation database, contacts, a durable outbox, one-shot
  inbox synchronization, reconciliation, and timelines over authenticated USB.
  The newer
  [host appliance service](../crates/lxmf-chat-service/README.md) adds exact USB-
  serial discovery, USB managed onboarding, authenticated macOS CoreBluetooth
  selection from the credential-derived E290 name, a sole session/database
  actor, automatic reconnect/backoff, continuous one-step inbox/status work,
  immutable state invalidations, and a bundled loopback Expo web export. The
  BLE service connector consumes an already-activated profile and deliberately
  leaves wireless onboarding and non-macOS host adapters for later. Its direct
  GATT stream, suite-3 authentication, and combined long-running
  BLE-service-to-LoRa-to-peer-import path are powered-qualified in both
  sequential directions. Starting one message on each half-duplex LoRa board
  at effectively the same time yielded one `Delivered` result and one durable
  `failed_delivery_timeout`; a later sequential send with new material
  delivered exactly. The likely RF collision was not instrumented closely
  enough to establish its cause, and simultaneous bidirectional scheduling
  remains unqualified. The shared Expo application compiles and runs as Android
  and iOS development builds, its first callable UniFFI round trip reaches
  Rust, and the bounded signed-iOS proof above composes its native Rust owner
  with authenticated BLE and the two-E290 LoRa path. It remains an external
  companion rather than an E290-served or onboard client: there is still no
  E290-served web UI, physically qualified Wi-Fi client bearer, display UI,
  general/Resource-backed NomadNet client, standards-complete Micron renderer,
  or physical Android qualification. The one bounded powered Nomad fetch/render
  is recorded separately in
  [the Nomad proof](e290-nomad-powered-proof.md).
- The host BLE connector currently reuses `BleTransport` through the
  E290-specific physical-qualifier package, which also owns its diagnostic CLI
  and browser bridge. This avoided duplicating the already-powered
  CoreBluetooth stream, but a dedicated reusable host BLE adapter crate should
  be extracted before adding other boards or native host backends.
- The loopback HTTP-v1 ready-state compatibility projection still serializes
  bearer-generic `endpoint` and `device_label` values under the historical
  field names `port` and `usb_serial`. The Expo adapter maps them back into the
  generic application model, and BLE now publishes the authenticated EUI-48 as
  the latter value, but a later wire-version break should make the field names
  transport-neutral.
- The native Rust bridge owns an app-private SQLite chat runtime and exposes
  contacts, the union of saved contacts and durable conversation peers,
  timelines, bounded message-activity queries, idempotent durable outbox
  writes, snapshots, sync, and close through generated shared Rust DTOs. The
  Expo client can rename a saved contact without retargeting its authenticated
  destination. An authenticated inbound message from an unsaved sender creates
  a message request that can be opened and replied to before the sender is
  saved; no reciprocal contact is required. Outbound-only unsaved history is
  kept distinct so it is not mislabeled as authenticated inbound contact.
  Saving either kind only adds phone-local display metadata and does not grant
  device authority or change remote state. These client behaviors are
  source/host-qualified, not yet covered by a dedicated powered two-phone
  qualification. USB Serial/JTAG and USB OTG remain deliberate native-app
  connector stubs. The foreground BLE central, native Rust command/ack pump,
  and suite-3 connector are implemented with bounded queues, one serialized
  write-with-response, indication subscription before readiness,
  generation-bound disconnect ownership, and a web unsupported stub. A
  separate Rust-owned browser qualifier uses Web Bluetooth only as an opaque
  bounded GATT-byte relay. Host protocol fakes and native platform builds pass.
  On 2026-07-23, the final disconnect-barrier production image,
  SHA-256 `74ce5f8a8ef5ddb1eec105a843c4fd633753585eaf81b592738f3f7b5c14b8ea`,
  was identity-safely flashed and read back on both 16 MiB `HT-RA62-HF` E290s.
  Board B (`AC:A7:04:E1:3F:88`) completed three consecutive direct macOS
  CoreBluetooth suite-3 sessions in 10,907 ms, 12,351 ms, and 11,595 ms; Board A
  (`AC:A7:04:E1:3E:88`) independently completed the same path in 12,193 ms. All
  four runs used 20-byte fragments, write-with-response, and indications and
  returned the board-correct device and destination identifiers. This qualifies
  the bounded production firmware's disconnect/drain/drop/re-advertise sequence
  plus the direct macOS qualifier path. The later signed-iOS run qualifies one
  Expo native-module foreground path and automatic cold-launch foreground
  reconnect, not a foreground/background/restoration lifecycle matrix. A sole
  BLE central must enable indications within four minutes and reach its first
  authenticated `Established` session within one absolute, non-refreshing
  five-minute deadline measured from authoritative Bluetooth authentication.
  These deadlines deliberately overlap on a restored bond: time spent waiting
  for CCCD subscription also consumes the application-authentication window;
  partial framing and stalled handshake flights do not extend ownership.
  Same-link authenticated idle replacement and attempt-rate policy remain
  deferred. Individual setup and teardown operations retain thirty-second
  bounds, while the CoreBluetooth connection attempt has a separate 90-second
  deadline because iOS supplies no native connection timeout and a board reboot
  may delay both success and failure callbacks past ten seconds. A late
  timed-out operation cannot tear down a replacement owned by the same
  `ForegroundBleCentral`. The React Native driver still wraps
  one process-global `BleManager`, however: after disposing one central, a
  stale native promise from that instance could later disconnect a new
  instance's connection to the same peripheral. The current app keeps one
  exclusive foreground owner, but Fast Refresh/restoration and any overlapping
  owner require the tracked module-wide/cross-instance ownership epoch (P2)
  before qualification. The app-private profile store can retain multiple
  device-keyed credentials and SQLite databases, but the alpha profile manager
  still opens only one board at a time. A nearby candidate whose normal or
  recovery advertised name matches a stored credential must be presented as
  already known and route to **Switch** or **Repair Bluetooth**, not a new
  pairing ceremony. Those names are only unauthenticated discovery hints;
  same-device re-pairing when both are absent or misleading is not qualified.
  The board can activate a fresh credential
  before the create-only mobile profile rejects replacing a different
  credential for the same device ID, leaving explicit reconciliation work
  rather than a safe implicit rotation. Canonical Active scratch artifacts from
  a late profile-publication failure are now exact-readback reconciled and
  finalized automatically; pending, ambiguous, malformed, and different
  same-device artifacts remain untouched and fail closed. Multi-phone use of
  one board is a separate limit: the device credential authority can retain
  independent application credentials, while the current E290 BLE profile
  retains one durable phone bond and a newly admitted phone replaces it. The
  profile manager now separates ordinary reconnect, replacement of a stale iOS
  Bluetooth bond while retaining the appliance credential/database, and
  confirmed deletion of an inactive local profile. Board-only recovery now
  clears only the durable BLE bond after a continuous GPIO21 reset-time hold.
  A bondless board advertises `reticulum-pair-<suffix>` while ordinary saved
  profiles target only `reticulum-e290-<suffix>`, preventing their reconnect
  loops from monopolizing the sole controller slot. Repair explicitly uses the
  recovery name for both the SMP leg and the replacement phone's first
  authenticated RDA1 session, then firmware returns to the normal name. GPIO21
  remains released during discovery and is held again after the recovery link
  opens. Powered qualification of this complete replacement-bond path remains
  pending. The recovery-advertising phase is currently RAM-only: a reboot after
  durable bond commit but before the first authenticated RDA1 session restores
  the bond and normal name. Persisting that one transitional fact is required
  for crash-complete recovery. iOS also requires the operator to use **Forget
  This Device** when the current phone itself caches the stale platform bond,
  because the BLE API cannot remove it.
  Local profile deletion removes that phone's credential, messages, contacts,
  and outbox, but does not revoke board authority or clear either side's
  Bluetooth bond. Credential revocation, credential replacement, and
  factory-reprovision recovery remain absent. A full board erase rotates the
  identity-derived BLE address but retains the MAC-derived advertised name and
  device-API ID; the saved-name guard correctly refuses to start a second
  credential-creation ceremony, while create-only storage cannot replace the
  old credential. Explicit revoke/reprovision must be implemented before a
  user-facing factory-reset operation ships. A bond command that times out after
  crossing to the flash owner correctly disables BLE until reboot, but the
  e-paper terminal still says `PAIR FAILED / PRESS 21 TO TRY AGAIN`; that rare
  ambiguity needs a neutral restart-required view before recovery UX is
  complete. The opt-in Wi-Fi constructor loads a fixed app-private activated
  credential, opens a
  finite-timeout raw TCP stream, and authenticates with the separately bound
  suite-2 profile. Its localhost partial-I/O handshake passes, but endpoint
  selection and SoftAP joining are still development-build/manual operations;
  the alpha credential import removes manual sandbox seeding but does not
  provide secure-storage migration. The exact profile
  image has been safely flashed to one credentialed E290 with an unchanged
  durable control-region readback, but no client has yet joined its SoftAP or
  completed the powered suite-2 path. The development Mac's only internet
  uplink is Wi-Fi, so that final exchange is intentionally left as a manual
  phone/alternate-uplink test.
  The host service's loopback/origin policy remains deliberately unsuitable as
  a phone transport. The mutable facade has run through Hermes and the
  generated TurboModule in an arm64 iOS simulator build: the native runtime
  opened, created a contact, durably queued an outbox message in schema-v2
  SQLite, and restored both after a forced app termination and relaunch. This
  qualifies mutable offline state across the generated boundary, not the new
  device transport. Cancellation, Rust panic translation, full Fast Refresh,
  background/foreground lifecycle, and mobile BLE disconnect/resume remain
  unqualified. Native generation and application builds are not yet in CI. The
  iOS XCFramework supports arm64 device and Apple-Silicon simulator only.
  Bundled C dependencies now receive the application's explicit 16.4 deployment
  target; exhaustive archive inspection and a clean Release link removed the
  earlier newer-SDK object warning. The Android proof used a large multi-ABI
  debug APK; release stripping, splits, startup, and memory remain unmeasured.
  The current target SDK 36 build correctly does not request Android's future
  local-network runtime permission. Before raising the target to SDK 37, add and
  exercise
  [`ACCESS_LOCAL_NETWORK`](https://developer.android.com/privacy-and-security/local-network-permission);
  Android 17 otherwise blocks the connector's outgoing raw-LAN TCP traffic by
  default.
- The application connection limit remains one, but the pinned esp-radio
  `Config::with_max_connections` writes Espressif's total `ble_max_act`
  controller-activity count. Espressif's official
  [`CONFIG_BT_CTRL_BLE_MAX_ACT` reference](https://docs.espressif.com/projects/esp-idf/en/v5.5.3/esp32s3/api-reference/kconfig-reference.html#config-bt-ctrl-ble-max-act)
  and
  [multi-connection guide](https://docs.espressif.com/projects/esp-idf/en/release-v5.1/esp32c3/api-guides/ble/ble-multiconnection-guide.html)
  count advertising and connections separately, so this one-link peripheral
  needs two activities. The activity-2 diagnostic proved controller,
  Trouble/GATT, runner, and advertising startup with the unchanged 72 KiB
  reclaimed heap and 41,040 internal-heap bytes free after advertising. That
  older diagnostic observed one immediate post-disconnect re-advertise
  transiently return HCI `0x07`. That event is historical to the older
  activity-2 artifact. Current source fails closed at that boundary: an exit
  which did not itself consume Trouble's `Disconnected` event requests
  disconnect and waits without a success timeout for the raw event, while an
  already-consumed event is carried explicitly in the connection outcome. Only
  then is the old `GattConnection` dropped, releasing the sole host-resource
  reference before another advertiser can start. Short rechecks only emit
  prolonged-drain diagnostics; they never authorize reuse. The final production
  ELF/image hashes are
  `39789a94cf060056f320765bbece079410e7352b953169e400e4bad48a712891` and
  `74ce5f8a8ef5ddb1eec105a843c4fd633753585eaf81b592738f3f7b5c14b8ea`.
  Exact flash/readback on both boards, three consecutive Board B sessions, and
  one independent Board A session now powered-qualify the corrected barrier
  with the unchanged two-activity/one-link budgets. Pressure, soak, and the
  powered mobile Expo lifecycle matrix beyond the bounded iOS foreground proof
  remain open.
- ESP32-S3 radio/display startup has a powered ordering invariant. Initializing
  SPI3/e-paper before the first esp-radio/PHY calibration stalled inside
  `esp_phy::enable_phy` registration/calibration and left the retained
  `STARTING` view. Current source constructs and retains the real
  `BleConnector` immediately after RTOS startup, before any display peripheral
  initialization, then moves that owner into the BLE task. The controlled A/B
  passed without increasing the 72 KiB internal heap, so this is not a memory
  ceiling. Keep this order covered when adding Wi-Fi, another radio owner, or
  new display startup work. Repeated boot, pressure, and soak qualification
  remain open.
- BLE controller initialization is still not fully isolated from the autonomous
  LoRa node. `BleConnector::new` returns a recoverable error only for
  configuration validation; pinned esp-radio controller and esp-rtos paths can
  still panic/assert on scheduler, strict-internal allocation, or controller
  initialization faults. BLE/Wi-Fi production profiles now initialize USB
  Serial/JTAG logging at `Info`, so alpha startup and lifecycle faults have a
  persistent observation path whenever USB remains functional. The legacy
  no-wireless profile still leaves logging uninitialized because its framed
  RDA1 bearer owns the same FIFO. The powered
  activity-budget diagnosis closed the historical controller-budget blocker,
  and the later USB-visible stack diagnostic separately found and closed the
  observed `NodeCore::new` construction overflow described under the hardware
  profile. Neither bounded diagnosis closes this general hardening residual.
- The Wi-Fi/BLE stack and runtime esp-rs crates are coherently pinned to
  exact upstream revision
  `b50efcb0dcd94b58ec337e511891057aa1f2e8fb`. It includes
  [esp-hal #5776](https://github.com/esp-rs/esp-hal/pull/5776), which pairs
  ESP32-S3 combo-PHY initialization with Wi-Fi RX enable/disable, and the
  upstream `esp-rtos 0.3.0` source contains the corrected main-task stack-slice
  element counts. No local esp-radio, esp-phy, or esp-rtos backport is selected.
  The product explicitly sets the Wi-Fi station maximum to 60 quarter-dBm
  (15 dBm) instead of relying on the controller default.
- ESP32-S3 Wi-Fi station plus connected BLE is an
  [officially supported coexistence mode](https://docs.espressif.com/projects/esp-idf/en/stable/esp32s3/api-guides/coexist.html),
  not a mutually exclusive hardware configuration. The observed station
  failure was instead bounded by strict internal memory: the former 72 KiB
  image left 4,092 bytes after controller construction and roughly 3--4 KiB
  after association/DHCP, then stopped producing Ethernet RX while association,
  IPv4 configuration, and transmit-token availability remained healthy.
  Wi-Fi driver and dynamic RX allocation cannot use the board's PSRAM through
  the current esp-radio adapter. Station builds now add 48 KiB ordinary DRAM
  for 120 KiB total internal heap. A 2026-08-14 A/B retained 53,244 bytes after
  controller construction, 52,440 after DHCP, and 51,368 at authenticated BLE
  application-session readiness; it then received public Reticulum announces
  over TCP interface 2 for more than ten active-BLE minutes with no DNS, TCP,
  RX, BLE, or station failure. Longer pressure, AP loss/recovery, and reconnect
  churn remain required gates.
- A 2026-08-14 powered run predating the boundary filter completed 282 LoRa
  transmissions in 618 seconds after public TCP came online and rejected 24
  additional ordinary owners whose deadlines were already insufficient. This
  established that public announce propagation, not heap or SX1262 failure,
  saturated LoRa. Current source blocks the high-volume Boundary-to-Internal
  direction before ordinary dispatch and has host regressions for the exact
  packet and interface IDs, but still needs powered qualification. It also
  retains accepted public announces in Rete's bounded 64-entry path/identity
  tables. Python's Boundary semantics likewise learns those routes, but its
  hosted storage does not impose this embedded eviction profile. Public churn
  can therefore still evict a useful LoRa-learned sender identity even though
  it no longer consumes LoRa airtime. Reserve/protect Internal-learned entries
  or otherwise bound Boundary occupancy before public-border operation is
  release-qualified; future allowed high-speed-to-Full egresses also require a
  Python-style per-egress announce cap.
- The local post-boot announce schedule is bounded but airtime-heavy at slow
  LoRa profiles. A 2026-08-14 two-board run showed the expected primary, LXMF,
  and NomadNet startup sequence settle completely before three minutes. Each
  destination is emitted in three identity-phased bootstrap cycles and Rete
  retransmits every emission once, for 18 baseline ordinary transmissions per
  boot before received-announce forwarding, path traffic, or proofs. At the
  qualified SF10/BW125/CR4/5 profile, those full announces consume roughly 31
  seconds of aggregate RF airtime and simultaneous boots can materially delay
  DATA even though no scheduler loop is unbounded. Revisit the bootstrap count,
  cadence, or profile-aware airtime budget before calling dense-network startup
  efficient. Production logs also collapse local announces, retransmissions,
  relays, path requests/responses, and proofs into `family=Ordinary`; add an
  ordinary-packet purpose field before relying on USB logs for exact airtime
  attribution.
- Current upstream's
  [`WifiTxToken::consume_token`](https://github.com/esp-rs/esp-hal/blob/b50efcb0dcd94b58ec337e511891057aa1f2e8fb/esp-radio/src/wifi/mod.rs#L1801-L1817)
  still increments the global Wi-Fi TX in-flight count before the send path
  validates that the station remains connected. If disconnect wins that race,
  no completion callback returns the credit. Both RX and TX admission require
  the count to remain below the configured queue size; this product uses three
  credits, so three stranded credits can wedge traffic while association and
  DHCP status still look healthy. The existing
  [`embassy_wifi_drop` test](https://github.com/esp-rs/esp-hal/blob/b50efcb0dcd94b58ec337e511891057aa1f2e8fb/qa-test/src/bin/embassy_wifi_drop.rs#L40-L102)
  exercises only one leak and therefore does not cover this terminal state.
  The combo-PHY fix in #5776 does not close this separate lifecycle defect.
  Reconnect churn, repeated DNS/TCP failure, and recovery remain required
  hardware gates; do not claim the border interface reliable until the credit
  lifecycle is corrected upstream or an explicitly reviewed alternative is
  selected.
- The host service has no operating-system single-instance lock, notification
  service, account migration, database encryption, activation-ambiguous repair,
  or cross-platform disconnect/host-suspend matrix. Managed profiles currently
  fail closed outside Unix until equivalent private-file semantics exist. Do
  not run it concurrently with another service or the foreground CLI against
  the same database/device.
  Current firmware accepts a canonical replacement handshake when the old
  session is idle, but a busy owner is never displaced and a terminal session
  fault still requires USB reset/re-enumeration.
- Current SQLite schema 9 retains schema 2's authenticated device ID, primary
  destination, and local LXMF destination binding, schema 3's legacy persisted
  automatic-rearm count, and schema 4's one-based app-submission field plus an
  immutable message-activity journal. Schema 5 adds nullable first-arrival
  interface/RSSI/SNR columns for inbound messages. Schema 6 gives every initial
  outbound commit and successful explicit manual replacement a closed
  phone-location stamp: either the validated phone fix and capture metadata
  supplied at queue time or an explicit unavailable reason. The journal records
  only durable application mutations: a novel inbound import; outbound commit,
  device acceptance, or advanced device state; and a successful explicit
  manual replacement. Historical automatic-rearm rows remain readable, but the
  current app runtime creates no automatic rearm events. Replays and
  unchanged/stale status polls do not fabricate events.
  Queries are newest first, globally or scoped to one timeline sequence, with
  pages bounded to 100 events and total retention bounded to 10,000 events.
  The host service performs and enforces the device binding; the foreground CLI
  does not yet. A schema-1 migration starts unbound because the old rows cannot
  prove their source, so migrate only a database already known to belong to
  that board. Any pre-schema-4 migration creates an empty journal alongside
  existing current message state and permanently marks activity history
  incomplete; pruning the oldest activity at the retention ceiling sets the
  same honest marker. Pre-schema-5 inbound rows keep unknown ingress; no
  historical signal is inferred. Migrating schema 5 marks legacy attempt
  locations `not_observed` and history incomplete rather than inventing a fix.
  Schema 7 adds idempotent boot/event radio-trace side tables without rebuilding
  schema-6 message/activity/location state. Schema 8 adds all-or-none message-
  location columns to inbound and outbox rows without inventing values for
  legacy messages. Schema 9 adds an optional first-import receiver-phone fix to
  inbound rows and preserves optional altitude plus vertical accuracy in phone
  observations. It does not invent a receiver fix for legacy rows. The
  in-memory restart image is schema 8 and accepts no older image schema.
  One database per paired board remains mandatory.
- Onboard GNSS integration is intentionally a stub. The current optional RMAP
  coordinate is supplied by a one-shot phone action through the board-owned
  configuration model; it does not couple location acquisition to core
  Reticulum routing. Continuous refresh, onboard fixes, validated
  mean-sea-level height, and powered public-location qualification remain open.
- The current API exposes normalized LXMF bytes plus authenticated metadata.
  The chat alpha adds host-local contacts and timelines, but device-synchronized
  contacts, threading, delivery receipts at the LXMF layer, message deletion,
  propagation-node selection, attachments, and human-friendly error recovery
  remain application-layer work.
- Timeline rows currently expose direction, message timestamp, message ID,
  local outbox ID, current submission ID, one-based attempt number, consumed
  legacy automatic-rearm count, lifecycle status, title, content, and current
  packet length/SHA-256 evidence when projected by the device. Board-owned RNS
  attempts do not become separate outbox rows; packet-correlated trace entries
  expose their attempt tokens. The global Activity
  surface and per-message details use the same durable journal to show queue,
  acceptance, status, and retry transitions. Each journal timestamp is the
  best-effort time at which the local app store observed and committed that
  transition, not an RF time, remote-delivery time, or peer clock.
  Inbound timeline rows now retain the exact first-arrival interface and
  optional paired RSSI/SNR reported by the receiving appliance. Schema 9 can
  additionally retain the receiver phone's latest available location when the
  foreground app first imports that message. Both observations are immutable
  receiver-local evidence; signal may describe a relay, while location is an
  import-time phone fix rather than the appliance or exact RF-arrival position.
  Outbound rows, legacy records, and transports without radio signal retain no
  such ingress values. Activity events still do not duplicate the observation,
  and no row contains end-to-end hop telemetry or remote receiver signal. The
  separate schema-7 radio trace can correlate a local outbound attempt with
  route, dispatcher, physical `TxDone`, proof-ingress, and terminal evidence.
  Nearby announce readings describe a different packet and are never
  substituted.
- Field location telemetry is a durable phone-local opt-in. Once enabled it
  remains enabled across app restarts and appliance switches until explicitly
  turned off; only the boolean preference is stored outside the per-appliance
  activity database. When enabled, the app durably stamps every initial send
  and explicit manual replacement with the latest phone-location state,
  including capture time,
  accuracy, authorization precision, source, and mocked-device indication when
  available. This is the phone position when the attempt was queued, not the
  exact RF emission position or time and not board GNSS. Autonomous board
  attempts reuse the existing submission observation without waking the app or
  sampling the phone again, so field analysis must inspect capture time and
  sample age. The Activity surfaces
  expose these stamps locally, and correlated radio-trace JSON/CSV exports
  include them. The Map plots the loaded history and, when an inbound message
  has both a sender-attached location and the schema-9 receiver fix, draws a
  solid endpoint line with horizontal distance, both reported phone elevations,
  elevation delta, optional three-dimensional separation, and final-hop signal.
  It remains endpoint evidence—not a measured RF path, board track, or Reticulum
  route—and legacy inbound rows have no inferred receiver endpoint.
- Sender-attached message location is a separate optional LXMF field. Each new
  composer inherits a durable phone-local default but can override it. When
  selected, the app requires a fresh foreground fix before committing the
  outbox row; the board encodes only Sideband `FIELD_TELEMETRY` (`0x02`) and
  signs it into the immutable message. Automatic and explicit retries retain
  that original location and message ID instead of sampling again. Incoming
  recognized locations are retained with the message and the app exposes full
  details plus an OpenStreetMap action. Malformed optional telemetry is ignored
  rather than making authenticated title/content unusable. The fields map can
  occupy 52 bytes instead of the one-byte empty map, reducing the available
  title/content budget by up to 51 bytes; arbitrary LXMF fields, attachments,
  Resources, and field editing remain deferred.
- Firmware now retains a 32-event boot-scoped trace ring and API 1.16 serves it
  through a boot-aware three-event cursor. The app incrementally imports route
  selection, DATA dispatch, physical `TxDone`, logical LoRa RX signal, and
  proof/timeout terminal evidence into additive SQLite schema 7. Import is
  idempotent, and a durable submission ID plus Reticulum attempt token correlates
  each local message attempt even when identical payloads are retried. The
  global Activity surface and per-message details read the same durable trace;
  complete paginated JSON and RFC 4180 CSV export are available.
- The board trace remains a bounded volatile handoff rather than a flight
  recorder. More than 32 events while the app is disconnected can overwrite
  evidence; a reboot before import loses the remaining ring. Both cases are
  surfaced as incomplete history instead of silently filling gaps. Board event
  times are monotonic since boot, app import time is a separate wall clock, and
  the joined phone location is sampled when the attempt was queued—not at exact
  RF emission. An outgoing trace cannot provide remote receiver RSSI; collect
  and export the receiving appliance's logical-RX trace for that evidence.
- The current trace is deliberately strongest for durable destination-DATA and
  complete logical LoRa receive. It does not claim complete event coverage for
  ordinary announces, all forwarding/control traffic, Link internals, or TCP,
  whose transports have different evidence. Missing trace rows must therefore
  not be interpreted as proof that no Reticulum traffic occurred.
- The API 1.14 proof probe is one volatile, principal-owned, boot-scoped slot
  and is not a monitoring service or durable test log. Success proves only
  Reticulum path-and-proof reachability to an enabled `rnstransport.probe`
  responder. It does not prove LXMF availability or throughput, and its RSSI/SNR
  is measured at the initiating appliance on the returning proof's final hop,
  which may be a relay—not at the remote receiver for the request. Public nodes
  may disable the responder. A timeout currently leaves the retained route in
  place, so another probe can repeat a stale-path failure; unlike a timed-out
  opportunistic LXMF DATA attempt, it is not route repair. The source/host path
  is qualified; powered cross-interface, multihop, pressure, reset, and
  third-party responder tests remain open.
- The app's radio-trace export is a structured local evidence snapshot, not an
  atomic two-appliance field-test bundle. A rigorous range run must still
  export both ends, record placement and antenna orientation, and preserve the
  exact profile and device association. The
  [completed powered proof](e290-api14-lxmf-poc.md) remains an example of
  manually retaining authenticated evidence across both sides.
- The identity-safe flash helper intentionally leaves native-USB E290s in the
  ROM loader after exact readback. Starting the application is still a separate
  operator step using the dependency set pinned in
  `interop/python/requirements-esptool-5.3.0.txt`: clear only
  `RTC_CNTL_OPTION1.FORCE_DOWNLOAD_BOOT`, then request an RTC-watchdog full-chip
  reset. Host USB re-enumeration is not a CPU reset, and `espflash 4.5.0 reset`
  cannot perform that exact transition. `probe-rs debug` halts the target again
  when its CLI disconnects, while `probe-rs reset` can lose its sole native-USB
  JTAG path during the ESP32-S3 reset-and-halt sequence. Neither probe-rs command
  is a supported run-and-detach mechanism. Integrate the qualified launch step
  into the identity-safe helper before treating flashing as a one-command
  product installer.

## Hardware profile

- The E290 owner admits one atomic frequency/BW/SF/CR/power tuple for the next
  boot. It requires the complete occupied channel to fit the HT-RA62-HF
  863--928 MHz path, SF7--12, CR 4/5--4/8, one of the supported canonical
  SX1262 bandwidths, and a bandwidth/SF combination whose low-data-rate
  optimization is qualified against RNode. Direct LoRa peers must match the
  four modulation fields; power may differ. These product checks do not decide
  regional frequency, bandwidth, duty-cycle, antenna, or EIRP legality.
- DATA and ordinary packet owners still receive a fixed 30-second lease rather
  than a lease derived from the selected radio profile. At the admitted
  SF12/BW125/CR4/8 extreme, a 500-byte logical packet has 28,852,224 us of
  exact RF airtime; setup, turnaround, reconciliation, maximum initial backoff,
  and the configured CAD allowance raise the bounded path to 29,899,368 us
  before earlier owner-handoff latency. That leaves only about 101 ms and
  cannot accommodate the configured post-busy holdoff and another CAD attempt.
  Profile-derived owner leases are required before the full advertised slow-
  profile range can be considered outbound-reliable.
- Power remains limited to the Semtech-optimal requested +14, +17, +20, and +22
  dBm SX1262 high-power PA rows, with exact command-log coverage for each.
  There is no separate +21 dBm row. +22 dBm requires adequate voltage and
  current at the module, and no conducted-power, EIRP, thermal, interference,
  configurable-modulation, or range qualification has yet been performed.
  Follow the
  [controlled range procedure](development/e290-range-testing.md)
  rather than treating one roughly 500-metre failure or one successful packet
  as a range result.
- Network-configuration semantic formats 1 and 2 mount with the historical
  915 MHz/BW125/SF7/CR4/5/+14 dBm profile; format 3 retains its power with that
  historical modulation. Every material current mutation writes semantic
  format 4 with the full tuple, which pre-format-4 firmware cannot mount. Erase
  the network-configuration store before such a downgrade; the normal
  merged-image flash preserves it. Legacy device-API key 9 and mutation kind 6
  remain as a power projection and power-only update, while key 10 and kind 7
  carry the atomic tuple. Saving any accepted profile changes only the
  after-restart state; the active radio remains immutable until reboot.
- The full E290 profile assumes external PSRAM. The resident submission runtime
  is initialized in place there. The current 128-entry target runtime is
  375,544 bytes; its 64-bit host fixture is 375,568 bytes. That includes an
  actor-owned replay scratch index which keeps boot, append validation, and
  compaction replay off the CPU stack while preserving the live index until a
  durable outcome. The complete permanent supervisor, including `NodeCore`, is
  now also validated, boxed, and leaked in PSRAM during boot composition.
  Channels, packet buffers, permit stores, Embassy task pools, and
  IRQ/DMA/cache-off state remain internal. The node's private identity now
  resides in PSRAM with that owner; this is not encryption or
  physical-extraction resistance. The Xtensa CPU stack itself cannot be placed
  in PSRAM, so constructor frames remain internal and are audited from each
  exact linked ELF.
- The current final linked-path gate records default mount/append/compact sums
  of 79,376/54,320/54,112 bytes and runtime-measurement-HIL sums of
  54,352/54,656/54,448 bytes. Each path must additionally fit a 4,096-byte
  ROM flash-read/interrupt reserve. The initially flashed 128-entry image
  failed this expanded gate and was not qualified. A corrected historical
  image passed the gate and a two-message powered run, but still needs
  allocator, stack-watermark, fill/pressure, and timing qualification before a
  release claim.
- The installed ESP 15.2.0 toolchain emits a 64,288-byte compiler frame for
  `NodeCore::new`. The Wi-Fi/TCP image originally retained that construction
  beneath an oversized async composition owner and overflowed CPU0 before the
  node or BLE tasks could start. Isolating the mutually exclusive constructor
  paths and returning only the PSRAM-backed supervisor reference reduces the
  permanent `product_main` task pool from 110,096 to 30,496 bytes. The exact
  corrected Wi-Fi/TCP ELF has 165,692 usable stack bytes, a 144,592-byte
  maximum constructor-path requirement including the reviewed 4,096-byte
  reserve, and 21,100 bytes of policy headroom. The corresponding BLE and
  headless profiles have 74,008 and 118,744 bytes of policy headroom. CI
  relinks and audits all three exact ELFs with compiler-emitted stack sizes.
  This closes the specific startup overflow, not simultaneous
  BLE/Wi-Fi/LoRa/cache-disabled pressure or soak qualification. The private
  node identity's PSRAM residence also needs explicit security review.
- The display's first Home/READY render deliberately precedes final
  supervisor/interface/task composition so its large boot frame is dead before
  the largest constructor paths. A rare later allocation, invariant, or task
  token failure enters the synchronous fail-stop and can therefore leave the
  e-paper showing stale READY rather than a terminal fault. Add a bounded
  post-composition display transition when doing so no longer regresses the
  linked startup-stack policy.
- The full E290 image rejects less than 8 MiB of detected PSRAM at boot. Smaller
  boards may retain separately selected reduced profiles, but non-PSRAM support
  is not a requirement for the full appliance and must not constrain its
  feature set.

[rmap-info]: https://rmap.world/info.html
[rns-interface-modes]: https://reticulum.network/manual/interfaces.html#interface-modes
[rns-trust]: https://reticulum.network/manual/networks.html#trustless-networking
