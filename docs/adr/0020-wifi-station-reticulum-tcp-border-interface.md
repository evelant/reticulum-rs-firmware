# ADR 0020: Wi-Fi station and Reticulum TCP border interface

- **Status:** accepted; the station/TCP implementation passes host and
  ESP32-S3 source/build gates, with an explicit 16 MiB package target; powered
  station, coexistence, TCP interoperability, and border routing remain
  unqualified
- **Date:** 2026-07-26
- **Extends:** [ADR 0003](0003-lora-first-interface-fabric.md),
  [ADR 0004](0004-sole-flash-coordinator.md), and
  [ADR 0019](0019-secure-ble-appliance-onboarding.md)

## Context

The E290 appliance has completed end-to-end LXMF messaging between two phones
and two LoRa nodes. Its existing `wifi-api-proof` image is a deliberately
separate local-management proof: the board creates a WPA2 SoftAP and carries
the authenticated RDA1 device API over one TCP connection. It neither joins an
existing LAN nor registers Wi-Fi as a Reticulum packet interface.

A useful always-on appliance should also forward Reticulum traffic between
LoRa and IP networks. Merely associating with an access point does not create
that path. The node needs a second concrete Reticulum interface with standard
wire interoperability, independent lifecycle, and the same bounded packet
ownership guarantees as the LoRa actor.

The app must be able to configure that uplink without first joining the board's
SoftAP. BLE is already the qualified local-management bearer and remains
available when station association or the upstream peer is unavailable.

## Decision

### Keep management bearers and Reticulum interfaces separate

The normal appliance profile will continue to expose the local device API over
bonded BLE. A new Wi-Fi station task joins a configured access point and owns
the IPv4/DHCP lifecycle. A distinct Reticulum TCP actor owns the upstream
packet stream.

The current `wifi-api-proof` SoftAP remains an optional development and future
fallback-provisioning profile. It is not renamed or silently treated as the
Reticulum Wi-Fi implementation.

### Start with one outbound TCP peer

The first IP packet interface is an outbound TCP client connecting to one
configured Reticulum `TCPServerInterface` peer. The default port is 4242. The
stream carries complete native Reticulum packets using standard HDLC framing.

Outbound client mode is the first slice because it works with DHCP and NAT,
does not require discovering a changing board address, and maps one configured
peer to one stable interface slot. A project-owned Embassy actor uses Rete's
HDLC codec and interoperability behavior without instantiating a second Rete
node or bypassing the existing interface fabric.

LoRa keeps packet-interface ID 1. The first TCP client uses packet-interface ID
2. TCP disconnect marks only interface 2 offline; LoRa, BLE management, local
LXMF, and durable services continue operating.

The node already runs in Reticulum transport mode, and this composition is the
intended foundation for forwarding eligible Reticulum packets between the two
interfaces. It is a Reticulum border-node shape, not an IP router, bridge,
proxy, or NAT.

That shape is not yet a complete border-routing claim. Current source assigns
TCP the **Boundary** announce role and LoRa the **Internal** role, and blocks
only the exact TCP-learned announce from LoRa while preserving ordinary packet
routing and Internal-to-Boundary announces. The pinned Rete foundation and
product still do not implement every Access Point/roaming/gateway rule,
recursive-discovery policy, cache partition, or per-egress announce cap from
Reticulum's [interface modes][rns-interface-modes]. Source/build qualification
therefore precedes, but does not substitute for, powered two-way forwarding
and protocol-semantics qualification.

### Persist a bounded board-owned configuration

The versioned model permits up to four known Wi-Fi networks and one active
outbound Reticulum TCP peer in its first physical format. Each network has an
opaque ID, bounded SSID, WPA2-Personal passphrase, enabled state, and priority.
The TCP peer has an enabled state, either a literal IPv4 address or bounded DNS
hostname, and a nonzero port. Hostnames are retained and resolved afresh on
each reconnect rather than being replaced with a persisted address.

The app receives only redacted network projections. Passphrases are accepted
on mutation, never returned by reads, and never included in logs or diagnostic
formatting. Configuration is stored on each board rather than copied into the
app's appliance-profile database.

The initial UI uses manual SSID and passphrase entry. It also offers a small
curated public-endpoint catalog without changing the one-active-peer bound.
Board-side scanning, hidden-network refinements, and automatic selection policy
can follow without changing ownership. Endpoint and public-disclosure policy
are refined by [ADR 0021](0021-owner-controlled-announces-public-tcp-bootstrap-and-rmap-discovery.md).

The sole `ProductStorageCoordinator` remains the only physical flash owner.
The first 8 KiB of the currently unused `device_config` reservation becomes an
alternating two-sector raw-NOR network-configuration store with commit-last,
exact-readback, and fail-closed mount semantics. No Wi-Fi or TCP task receives
raw flash ownership.

Development boards currently run without flash encryption. Plaintext
passphrases at rest are an explicit alpha limitation, not a reason to delay the
border-node proof. The protocol and UI nevertheless avoid disclosing them.

### Apply material changes at reboot first

An authenticated BLE client can inspect redacted configuration and replace the
bounded desired configuration. The initial implementation durably commits and
verifies the successor, reports that a reboot is required, and leaves the
running interfaces unchanged.

This is intentional: Rete currently associates learned paths with an
interface's byte-sized ID, not this project's registry generation. Rebooting
the node owner after changing an AP or TCP endpoint prevents old learned paths
from being attributed to a materially different interface configuration.
Live apply can be added only with explicit path invalidation or stronger path
provenance.

Configuration mutation receives a dedicated device-API permission. The alpha
firmware grants it to newly paired developer credentials and wires
secret-bearing mutation only through the authenticated BLE appliance profile.
Bearer-general confidential-mutation policy and production at-rest key
protection remain later hardening work.

Developer boards may already contain credentials paired by the immediately
preceding fixed policy, which granted exactly `READ_SUBMISSION_STATUS |
EXPERIMENTAL_SUBMIT_RNS_DATA`. Re-pairing every board is unnecessary for this
alpha transition. The E290 appliance dispatcher applies a temporary runtime
compatibility rule only to an active, exact-generation credential whose durable
record has that exact two-bit mask, `UsbPhysicalPresence` origin, and
authorization-policy version 1. It adds `MANAGE_NETWORK_CONFIG` only to the
ephemeral context of a network-configuration mutation dispatch. Other
operations continue to see the exact persisted two-bit mask. The stored record,
generation, authority revision, and durable authorization provenance are not
rewritten. A credential with a subset, superset, different origin, or different
policy version receives no extra permission.

This overlay is deliberately narrower than a general permission migration and
exists only because the current credential store has no independently
authorized administration successor. A future durable permission-update flow
must advance the credential generation and replace this rule; broadening the
match is not an acceptable substitute.

### Keep the powered coexistence and forwarding gate

The first powered gate uses one configured WPA2 access point and one outbound
TCP peer. It must demonstrate:

1. Wi-Fi station association and DHCP while BLE remains reconnectable;
2. ordinary two-board LoRa LXMF still works while Wi-Fi is online;
3. upstream loss takes only the TCP interface offline and reconnects with
   bounded backoff;
4. one Reticulum packet or announce travels TCP to E290 to LoRa;
5. one Reticulum packet travels LoRa to E290 to TCP; and
6. the complete return path works without duplicate node ownership.

Heap, task-stack, and coexistence diagnostics are part of this gate. The
ESP32-S3 build enables the pinned radio stack's BLE/Wi-Fi coexistence support;
the E290's PSRAM is available for non-DMA protocol state, while controller and
DMA-visible allocations remain in internal memory.

## Consequences

- Wi-Fi failure cannot make the LoRa appliance unusable or unconfigurable.
- The transport-neutral interface registry grows from one concrete actor to
  two without adding TCP concepts to node-core or LoRa concepts to the stream
  actor.
- The TCP actor uses its own bounded stream-credit permit policy rather than
  emulating LoRa airtime.
- Configuration and runtime status are different objects. Desired
  configuration is durable; association, DHCP, RSSI, peer connection, backoff,
  and last-error state are volatile projections.
- App connection transport continues to describe how the app reaches the
  device API. It must not be overloaded to describe the node's Reticulum
  interfaces.
- A global Wi-Fi transport policy can suppress station/TCP startup without
  deleting either the saved Wi-Fi profiles or selected upstream. Announce and
  public-discovery policy remain separate owner choices under ADR 0021.

## Deferred decisions

- board-hosted TCP server mode;
- mDNS endpoint discovery;
- Reticulum AutoInterface over IPv6 link-local multicast;
- multiple simultaneous TCP peers and server clients;
- IFAC configuration and key handling;
- SoftAP fallback and AP-plus-station mode;
- live configuration apply and learned-path invalidation;
- production flash encryption or application-level credential wrapping;
- background mobile provisioning and platform-specific Wi-Fi credential APIs;
- a remotely updateable endpoint catalog, health diagnostics, and failover; and
- complete interface-mode, cache-protection, and announce-cap semantics plus
  powered border-node qualification.

[rns-interface-modes]: https://reticulum.network/manual/interfaces.html#interface-modes
