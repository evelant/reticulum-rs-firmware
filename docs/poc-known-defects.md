# Usable-firmware POC: known limits and deferred work

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
- The durable LXMF inbox has no delete, acknowledgement, compaction, or
  retention-policy operation. Its stable handles are designed to survive a
  future compactor, but the current store only appends.
- Received normalized LXMF and raw RNS inbox plaintext are stored unencrypted
  in their dedicated flash partitions. The selected outbound LXMF carrier and
  destination are likewise retained as plaintext intent in the durable
  submission journal. API authentication protects access over a bearer; it does
  not provide encryption at rest.
- LXMF enumeration and reads are currently global to every authenticated
  principal. They require no persisted permission bit and provide no
  per-principal mailbox ACL or ownership filtering.
- A host read verifies the final normalized-wire SHA-256 from committed
  metadata. Individual flash chunks revalidate their extent headers but do not
  independently hash the complete message on every read.

## Basic outbound LXMF subset

- The first semantic send operation composes only Python-compatible basic LXMF:
  binary title and content, an empty fields map, and no stamp. It durably
  retains one exact complete signed LXMF wire through 431 bytes without
  selecting a delivery method; generic RNS destination DATA remains a separate
  383-byte intent. The current automatic policy uses the destination-stripped
  bytes as its compatible opportunistic carrier when eligible and no ready
  matching cached Link is selected. Current source also establishes or reuses
  product-initiated outbound Links from a registry bounded to the native
  product table and prepares the exact complete wire as one Link DATA packet
  when required.
  Propagated, Resource, ticket, stamp, attachment, and nonempty-fields sends
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
  reusable handle and routes normal authenticated close, allowing a later
  submission to establish a fresh Link. The failed submission itself remains
  terminal and is not automatically retried.
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
  the Resource-wait marker are boot-volatile. Although the exact LXMF wire
  remains journaled, current boot recovery conservatively finalizes
  `Preparing` and `AwaitingDelivery` as `InterruptedByReset`; it does not resume
  even provably pre-frame path discovery or Link establishment. Safe pre-I/O
  resume needs a future durable state/schema distinction from work that may
  have exposed a frame.
- Link establishment expiry or loss clears the volatile transaction and the
  E290 firmware retries the still-`Preparing` message after one second. This can
  repeat indefinitely in the same boot: there is no boot-local attempt ceiling
  and no persisted retry budget. Within that boot, Link-MDU overflow remains
  `Preparing` until Resource has bounded durable ownership and recovery; reset
  applies the conservative `InterruptedByReset` rule above.
- Basic composition currently uses allocation-backed Rete LXMF packing and
  signing before copying into caller storage. The E290 POC must measure heap
  high-water behavior. A bounded `encoded_len`/`pack_into` composer remains
  desirable for smaller targets and fallible-allocation handling.
- The client supplies one Unix timestamp in whole milliseconds in the exact
  product range `1..=8_796_093_022_207_999` and must retain it across retries.
  This is a deliberately narrower, JavaScript-friendly subset of Python LXMF's
  binary64 timestamp. The firmware has no trusted wall clock yet.

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

- LoRa is the first active Reticulum transport, but submission, inbox, API, and
  routing ports do not name LoRa or SX1262. USB is connected only as the local
  client bearer; BLE, Wi-Fi, USB packet transport, and simultaneous
  multi-interface routing have not yet been connected as Reticulum interfaces.
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
  The table expires no peers by age, and its current LoRa provenance has no
  RSSI/SNR observation to report.
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
  is qualified for the current one-interface product. It does not yet retain
  Python-style per-interface pending path-request forwarding state, so it must
  be revisited before enabling simultaneous LoRa, Wi-Fi, and BLE routing.
- A queued LINKREQUEST remains bound to the retained route selected when it was
  created. If that route's interface becomes ineligible before first dispatch
  while another transport remains usable, the ordinary router can wait
  indefinitely instead of rebuilding the request for the alternate route;
  definitive exhaustion currently closes local submission admission instead
  of returning that transaction to path discovery. The current one-interface
  powered proof is unaffected, but route re-resolution and a bounded
  pre-dispatch establishment deadline are required before simultaneous
  multi-transport Link establishment is release-ready.

## Client and product surface

- USB Serial/JTAG remains the provisioning/recovery bearer and was the first
  physically qualified authenticated bearer. The session and logical API are
  bearer-neutral, and the opt-in BLE profile now has bounded powered
  qualification carrying the same ordered RDA1 stream under suite 3. The Wi-Fi
  profile remains implemented but not powered-qualified. Both wireless
  profiles still require a credential established by the USB profile. The
  native client now has an alpha system-file import path: TypeScript copies an
  operator-selected artifact into app-owned staging without decoding its
  bytes, Rust validates and create-only publishes the canonical credential,
  and the E290 device ID supplies the exact BLE advertised-name selector.
  This implements and host-tests fresh-install sandbox seeding and removes
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
  after app-owned staging. Phone-native pairing, per-client revocation, an
  Android grant-release owner, identity-bound preview/install, Keychain/
  Keystore storage, backup exclusion, stronger post-alpha wireless
  authentication policy, and full recovery UX remain deferred. Managed host
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
- Native first-run BLE discovery lists bounded service advertisements and
  requires explicit selection without connecting. The production display now
  renders the same exact six-character board suffix as the app card, so an
  operator can match a selected appliance without guessing which GPIO21 button
  belongs to it. The suffix and advertisement remain unauthenticated selection
  hints; only the credential-bound suite-3 session authenticates the device.
- The permanent display actor now owns a static Home snapshot after its physical
  boot-completion gate: board suffix, configured `LORA NA915` transport, BLE
  local-app bearer, configured LXMF/Nomad services, and application setup
  guidance. `PAIRED` is derived only from the active credential count in
  publishable application authority, never from the BLE bond. An empty
  authority, or pre-authority media with a resident initialization/recovery
  pairing policy, shows pairing guidance; only the absence of both authority
  and a pairing path reports the local API unavailable. A durably successful
  application activation updates the cached setup fact before fallible client
  response delivery and queues Paired Home; failures and timeouts remain
  terminal. This snapshot is deliberately composition, not task-spawn
  completion or live health:
  `LORA`, `BLE`, `LXMF`, and `NOMAD` do not assert that an actor remains online
  or connected. Live link state, queue/message counts, RF signal metadata,
  storage pressure, and post-boot faults need a sole display-status coordinator
  before they can be shown truthfully. Only the initial Home render has a
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
  contacts, timelines, idempotent durable outbox writes, snapshots, sync, and
  close through generated shared Rust DTOs. USB Serial/JTAG and USB OTG remain
  deliberate native-app connector stubs. The foreground BLE central, native
  Rust command/ack pump, and suite-3 connector are implemented with bounded
  queues, one serialized write-with-response, indication subscription before
  readiness, generation-bound disconnect ownership, and a web unsupported
  stub. A separate Rust-owned browser qualifier uses Web Bluetooth only as an
  opaque bounded GATT-byte relay. Host protocol fakes and native platform builds
  pass. On 2026-07-23, the final disconnect-barrier production image,
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
  BLE central must
  enable indications within 15 seconds, then reach its first authenticated
  `Established` session within one absolute, non-refreshing 30-second deadline;
  partial framing and stalled handshake flights do not extend ownership.
  Same-link authenticated idle replacement and attempt-rate policy remain
  deferred. Setup and teardown operations are
  deadline-bound, and a late timed-out operation cannot tear down a replacement
  owned by the same `ForegroundBleCentral`. The React Native driver still wraps
  one process-global `BleManager`, however: after disposing one central, a
  stale native promise from that instance could later disconnect a new
  instance's connection to the same peripheral. The current app keeps one
  exclusive foreground owner, but Fast Refresh/restoration and any overlapping
  owner require the tracked module-wide/cross-instance ownership epoch (P2)
  before qualification. The opt-in
  Wi-Fi constructor loads a fixed app-private activated credential, opens a
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
- The application connection limit remains one, but esp-radio 0.18
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
  initialization faults. Every ordinary production profile keeps the logger as
  a no-op, including BLE/Wi-Fi profiles that retain native USB electrically and
  at runtime as a diagnostics-only sink, so such a panic is silent. The
  separately named diagnostic image enables USB Serial/JTAG output. Its powered
  activity-budget diagnosis closed the historical controller-budget blocker,
  and the later USB-visible stack diagnostic separately found and closed the
  observed `NodeCore::new` construction overflow described under the hardware
  profile. Neither bounded diagnosis closes this general hardening residual.
- The host service has no operating-system single-instance lock, notification
  service, account migration, database encryption, activation-ambiguous repair,
  or cross-platform disconnect/host-suspend matrix. Managed profiles currently
  fail closed outside Unix until equivalent private-file semantics exist. Do
  not run it concurrently with another service or the foreground CLI against
  the same database/device.
  Current firmware accepts a canonical replacement handshake when the old
  session is idle, but a busy owner is never displaced and a terminal session
  fault still requires USB reset/re-enumeration.
- SQLite schema 2 can bind a database to the authenticated device ID, primary
  destination, and local LXMF destination. The host service performs and
  enforces that binding; the foreground CLI does not yet. A schema-1 migration
  starts unbound because the old rows cannot prove their source, so migrate only
  a database already known to belong to that board. One database per paired
  board remains mandatory.
- GNSS/location integration is intentionally a stub. Location must later be an
  optional service and must not couple core Reticulum routing to this board.
- The current API exposes normalized LXMF bytes plus authenticated metadata.
  The chat alpha adds host-local contacts and timelines, but device-synchronized
  contacts, threading, delivery receipts at the LXMF layer, message deletion,
  propagation-node selection, attachments, and human-friendly error recovery
  remain application-layer work.
- Unlike `submit-and-wait` and the raw-inbox qualification commands, current
  LXMF list/read/send, chat-alpha, and appliance-service operations do not write
  a structured evidence sidecar. The
  [completed powered proof](e290-api14-lxmf-poc.md) manually retained the
  authenticated stdout records, private read files, exact retry inputs, and
  independently recorded hashes; product tooling should generate that bundle
  atomically.
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

- The full E290 profile assumes external PSRAM. The resident submission runtime
  is initialized in place there. The current 128-entry target runtime is
  375,544 bytes; its 64-bit host fixture is 375,568 bytes. That includes an
  actor-owned replay scratch index which keeps boot, append validation, and
  compaction replay off the CPU stack while preserving the live index until a
  durable outcome. The complete permanent supervisor, including `NodeCore`, is
  now also validated, boxed, and leaked in PSRAM before radio initialization.
  Channels, packet buffers, permit stores, Embassy task pools, and
  IRQ/DMA/cache-off state remain internal. The node's private identity now
  resides in PSRAM with that owner; this is not encryption or
  physical-extraction resistance.
- The current final linked-path gate records default mount/append/compact sums
  of 79,376/54,320/54,112 bytes and runtime-measurement-HIL sums of
  54,352/54,656/54,448 bytes. Each path must additionally fit a 4,096-byte
  ROM flash-read/interrupt reserve. The initially flashed 128-entry image
  failed this expanded gate and was not qualified. A corrected historical
  image passed the gate and a two-message powered run, but still needs
  allocator, stack-watermark, fill/pressure, and timing qualification before a
  release claim.
- The installed ESP 15.2.0 toolchain emits a 64,288-byte compiler frame for
  `NodeCore::new`. In the old production BLE ELF it nested beneath the
  62,016-byte `product_main` poll frame against only 122,808 usable CPU0 stack
  bytes: 126,304 bytes crossed the guard by 3,496 bytes before the reviewed
  4,096-byte reserve. A powered diagnostic captured that exact stack-guard
  panic at `main.rs:954`. Removing the duplicate internal supervisor
  `StaticCell` and moving the complete supervisor to `ExternalMemory` before
  the radio await leaves the frames unchanged but raises the fixed production
  BLE ELF to 149,320 raw/149,256 usable stack bytes. The 130,400-byte
  frames-plus-reserve requirement therefore has 18,856 bytes of linked-policy
  headroom. The fixed diagnostic reached advertising with 40,996 internal-heap
  bytes free and no panic; the fixed production image independently
  advertised, authenticated over macOS CoreBluetooth, and returned identities.
  This closes the specific startup defect, not powered flash/readback plus
  simultaneous BLE/LoRa/cache-disabled interaction, pressure, or soak
  qualification. The private node identity's new PSRAM residence also needs
  explicit security review.
- Non-PSRAM ESP32 boards may compile reduced profiles with services disabled.
  They do not define the maximum product feature set, and fitting the complete
  stack on the Tracker V2 is not a requirement.
