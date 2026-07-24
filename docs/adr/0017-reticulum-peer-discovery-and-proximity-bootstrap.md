# ADR 0017: Reticulum peer discovery and proximity bootstrap

- **Status:** accepted; the peer projection and picker are implemented, and
  the bounded existing-contact powered proof completed 2026-07-24, while
  fresh contact creation and direct-Link acceptance remain pending
- **Date:** 2026-07-23
- **Extends:** [ADR 0010](0010-device-api-live-pairing-protocol.md),
  [ADR 0015](0015-universal-expo-client-and-generated-bindings.md), and
  [ADR 0016](0016-bound-link-data-lxmf-ingress.md)

## Context

The first Expo chat surface requires a user to type a 16-byte
`lxmf.delivery` destination as 32 hexadecimal characters. That is useful as a
diagnostic escape hatch, but it is not credible appliance UX and conceals one
of Reticulum's strengths: a node already authenticates destination announces,
learns the announcing public identity, and retains the path needed to reach
that destination.

There are two superficially similar but security-distinct exchanges:

1. **Peer discovery** adds another person's public LXMF identity to the local
   contact database. It must never transfer a private identity key or device-API
   authentication authority.
2. **Appliance onboarding** authorizes this app installation to control one
   physical node. It creates independently revocable client authority and
   requires explicit user intent plus physical presence.

The alpha credential-file import addresses the second exchange by cloning an
already activated credential. It does not solve peer discovery and must not
become the contact-sharing format.

The E290 firmware already emits periodic `lxmf.delivery` announces. Pinned Rete
authenticates received announces, records their identity and path, and projects
an owned `AnnounceReceived` application event. The permanent firmware now
projects supported events into its bounded peer service, and the native Expo
app reads that service through its foreground authenticated BLE connection to
its own board. Together these provide the shortest path to a no-typing
demonstration without inventing a parallel phone-only network.

## Decision

### Reticulum discovery is the primary nearby mechanism

Add a bounded discovered-peer service to the portable node and device API. The
first E290 profile retains 32 peers and at most 256 authenticated announce
application-data bytes per peer.
Only valid announces for supported application destinations, initially
`lxmf.delivery`, may enter it. Each record contains enough immutable public
evidence for selection and display:

- the complete destination hash;
- a digest or fingerprint derived from the authenticated public identity;
- bounded announce application data;
- hop count and last-observed age;
- the ingress interface identity; and
- optional physical-link observations such as RSSI and SNR when the owning
  interface can supply them without guessing or cross-packet lookup.

The Rete path and identity tables remain the protocol authority. The discovered
peer table is a bounded application projection, not a second routing table.
Expiry or eviction from the UI table must not mutate Rete path state, and path
expiry must not silently delete a durable contact.

The public device API exposes discovered peers through a cursor-based,
one-record-at-a-time read surface rather than an unbounded array. Reads require
an authenticated appliance session even though the underlying identities were
announced publicly; a local client should not gain a bulk proximity-observation
oracle before it owns the appliance. Rust wire types remain authoritative and
generate the Expo TypeScript declarations.

The universal app adds a **Nearby** action beside manual contact entry. It
shows validated records from the connected board, their route/signal hints,
and a stable short fingerprint. One deliberate tap on **Add** uses the existing
durable contact mutation; tapping an existing peer opens its conversation. The
hexadecimal form remains available under an advanced/manual path.

Selecting a discovered peer does not claim current reachability. The completed
product path will use this transport-neutral automatic-delivery lifecycle:

1. use an unexpired learned path when one exists;
2. otherwise request a path for the selected destination;
3. reuse an already-active compatible Link when one exists;
4. otherwise send an eligible short message as opportunistic destination DATA;
5. establish a Link when packet size, repeated interaction, bounded
   escalation, or an explicit future preference requires it; and
6. project proof, timeout, cancellation, and retry through the existing durable
   submission status.

No step chooses LoRa directly. The learned route can originate on any enabled
Reticulum interface. LoRa remains the intended first powered qualification
path for both automatic opportunistic delivery and the separately reusable
direct-Link capability. Propagated delivery remains an explicit
store-and-forward policy rather than a fallback silently selected by peer
discovery.

### One public contact-card envelope for fallback mechanisms

Some peers will be physically nearby without sharing a currently usable
Reticulum path. Define one versioned, bounded, Rust-owned public contact-card
envelope that alternate proximity carriers can exchange. It contains no
secret material. At minimum it binds:

- the application namespace and `lxmf.delivery` destination;
- the corresponding public identity;
- bounded user-visible metadata;
- a freshness nonce or explicit non-expiring designation; and
- an identity signature over the complete canonical envelope.

Import verifies the signature and recomputes the destination from the public
identity and application name before presenting it. A transport label,
advertised name, QR contents, or nearby-session endpoint is only a discovery
hint and never authenticates the card. The same Rust validator serves every
carrier; TypeScript and platform modules transport opaque bounded bytes and do
not duplicate cryptographic parsing.

The fallback implementation order is:

1. **E290-mediated BLE share mode.** An authenticated owner explicitly opens a
   short public sharing window. Another app scans a separate service and reads
   one bounded card from the peer board. This reuses the board's reliable
   peripheral role and the app's existing central role.
2. **QR/deep-link import.** This is the universal visual fallback and exercises
   exactly the same envelope and confirmation flow.
3. **Cross-platform native nearby transport.** Google Nearby Connections is a
   candidate because its current API supports Android and iOS while selecting
   Bluetooth, Wi-Fi, and other local mechanisms internally. It requires a
   deliberate Expo native-module and dependency review before adoption.
4. **Platform-native peer networking.** Apple's Network and Wi-Fi Aware
   frameworks are a candidate for phone-to-phone exchange on supported Apple
   OS/hardware, subject to entitlement, deployment-target, and interoperability
   review. This does not imply that the current E290 Wi-Fi hardware can
   participate in Wi-Fi Aware.
5. **NFC or platform-specific sharing.** These are optional carrier adapters,
   not alternate identity formats.

Phone-to-phone raw BLE peripheral mode is not the first implementation. The
current React Native BLE dependency owns the central role used for the
appliance connection, while reliable iOS peripheral/background advertising
would add a second native lifetime and restoration surface. Apple's
MultipeerConnectivity types are currently deprecated and are not selected as
a new foundation. These decisions can be revisited without changing the
contact-card or device-API model.

### Appliance onboarding remains a separate early milestone

Replace transferable credential files with phone-native live pairing after the
core bidirectional messaging path is stable. A limited BLE onboarding service may expose
only secret-free device identity and the existing initialization/pairing state
machine. The app must show the exact board identity, request physical presence,
and install fresh per-client authority atomically into platform secure storage.
The public contact-card service cannot authorize device-API access, and an
appliance credential cannot be accepted as a peer contact.

## First powered demonstration acceptance

The early no-typing demonstration is complete when:

1. two E290s boot the same permanent image and advertise their
   `lxmf.delivery` destinations over LoRa;
2. a phone authenticated to board A over BLE lists board B under **Nearby**
   without a typed destination;
3. the app confirms and durably adds B as a contact;
4. one method-neutral basic LXMF message drives path discovery if needed,
   selects the appliance's automatic delivery policy, reaches B's durable
   inbox, and returns a valid packet proof;
5. A's timeline reaches `Delivered`; and
6. the contact and conversation survive app and board restart.

The first powered proof may use one phone and two boards. A later two-phone
proof qualifies symmetric discovery and sharing. Background discovery,
continuous location inference, and automatic contact insertion are explicitly
out of scope.

### 2026-07-24 bounded powered result

The [first Reticulum-native Nearby powered
record](../e290-reticulum-nearby-powered-proof.md) qualified both E290s running
the same permanent image, board B learning board A's authenticated
`lxmf.delivery` announce, the signed iOS Release opening A's existing durable
contact through **Nearby** without endpoint entry, and one short message in
each direction reaching durable `Delivered` over LoRa with exact peer import.
An app-process relaunch retained a byte-identical SQLite snapshot containing
the contact, conversation, first message, packet evidence, and terminal state.

That run deliberately does not close the complete acceptance list above. The
contact already existed, so first-time **Add** was not exercised, and the
boards were not restarted after those exact messages. The 259- and 291-byte
Reticulum packets correctly selected the compatible Header-1 opportunistic
path under the automatic policy. They therefore qualify the intended
short-message delivery method, but do not qualify the separately pending
product-owned authenticated-Link capability.

## Consequences

- The impressive demo exercises the real mesh, routing, identity, BLE control,
  and intended automatic short-message LXMF path instead of a disposable
  proximity shortcut; reusable direct-Link support remains the next tranche.
- Normal Reticulum announces become useful application events rather than log
  noise, while routing remains owned by Rete.
- Peer discovery is naturally transport-neutral and can later include Wi-Fi,
  Ethernet, BLE Reticulum interfaces, or relayed paths.
- Manual endpoint entry remains available for testing and for identities that
  cannot announce.
- Public identity exchange and privileged appliance pairing have separate
  schemas, permissions, storage, and confirmation language.
- The bounded peer table, cursor API, Rust projection, and Expo picker are
  implemented, and the existing-contact no-typing path has a bounded
  two-board powered record. Fresh contact creation, direct-Link capability,
  peer-age expiry, and multi-peripheral BLE ownership remain open.

## References

- [Google Nearby Connections overview](https://developers.google.com/nearby/overview)
- [Apple Wi-Fi Aware overview](https://developer.apple.com/documentation/wifiaware)
- [Apple Core Bluetooth background processing](https://developer.apple.com/library/archive/documentation/NetworkingInternetWeb/Conceptual/CoreBluetooth_concepts/CoreBluetoothBackgroundProcessingForIOSApps/PerformingTasksWhileYourAppIsInTheBackground.html)
- [Android Bluetooth permissions](https://developer.android.com/develop/connectivity/bluetooth/bt-permissions)
- [React Native BLE Manager Expo integration](https://github.com/innoveit/react-native-ble-manager/blob/master/docs/expo.markdown)
