# ADR 0021: Owner-controlled announces, public TCP bootstrap, and RMAP discovery

- **Status:** accepted; source and host/target build gates are implemented,
  while powered public-TCP, RMAP presentation, and border-routing behavior
  remain unqualified
- **Date:** 2026-07-26
- **Extends:** [ADR 0003](0003-lora-first-interface-fabric.md),
  [ADR 0017](0017-reticulum-peer-discovery-and-proximity-bootstrap.md), and
  [ADR 0020](0020-wifi-station-reticulum-tcp-border-interface.md)

## Context

The Wi-Fi station/TCP slice gives an E290 a second Reticulum packet-interface
shape, but an appliance owner still needs explicit control over participation:
whether Wi-Fi starts, which upstream it uses, whether routine service announces
run, and whether the node publishes richer public interface metadata.

These are different choices. Connecting to an untrusted public TCP carrier
must not implicitly publish a location. Disabling routine service announces
must not remove the owner's ability to announce on demand. Disabling Wi-Fi
should preserve saved access-point and endpoint configuration for later use.

Reticulum itself is designed for [trustless networking][rns-trust]: application
traffic remains end-to-end protected across untrusted carriers. That does not
make a public TCP operator private. The operator can still observe the
appliance's IP address, connection times, traffic volume, and availability, and
can delay or discard traffic.

## Decision

### Keep three independent owner controls

The durable network configuration owns three separate policies:

1. **Wi-Fi transport enabled** is a master runtime gate. Turning it off
   suppresses station association and the outbound TCP interface after reboot
   without deleting saved Wi-Fi profiles or the selected peer.
2. **Automatic ordinary announces enabled** gates the existing boot/retry and
   steady-state primary, LXMF, and NomadNet service-announce schedule.
3. **RMAP interface discovery enabled** independently opts into the public
   interface-discovery destination and its six-hour publication cadence.

The controls are independent by design. In particular, turning off automatic
ordinary announces does not disable authenticated manual announces, and it
does not silently change the separately selected RMAP policy.

Existing version-1 configuration media migrates with Wi-Fi and automatic
ordinary announces enabled to preserve its prior behavior. RMAP discovery and
location sharing default off and require explicit opt-in.

Like the other material network settings in ADR 0020, these policies are
durably committed and take effect on the next reboot. The API reports that
reboot requirement rather than implying a live transition.

### Queue one coalesced manual service-announce cycle

The authenticated app exposes **Announce now**. It queues work into the
existing ordinary announce lane; it does not synchronously transmit from the
API handler. One request schedules the primary destination, optional
`lxmf.delivery`, and `nomadnetwork.node` with the existing inter-destination
quiet interval.

If a manual cycle is already waiting or in progress, another request returns
`AlreadyPending` and does not create a second cycle. Admission pressure defers
the retained item instead of dropping it. When RMAP discovery is enabled, the
same accepted request makes the cached stamped discovery payload immediately
due; the initial public-uplink gate still applies.

### Retain hostnames and curate a small public bootstrap catalog

The one active outbound TCP peer may be a literal IPv4 address or a bounded DNS
hostname. A hostname is retained as configured and resolved again on each
reconnect, so the appliance does not persist a stale address. The one-peer
bound remains unchanged.

The app includes a deliberately small bootstrap catalog:

| Preset | Endpoint | Advertised transport ID when verified | Source |
| --- | --- | --- | --- |
| RMAP World | `rmap.world:4242` | `682e34edf6dd0daa867831ebc9b4e204` | [RMAP information][rmap-info] |
| ReticulumNet.nl | `node.reticulumnet.nl:4242` | `8a2c0d3c3fee8bea4a8172dc6f4d7ea6` | [Operator instructions][reticulumnet] |
| McSwain Reticulum | `reticulum.mcswain.dev:4242` | `72d389bca0703e185155f2d2c3eace57` | [RMAP live catalog][rmap-json] |

The entries were verified on 2026-07-26. Their transport IDs are diagnostic
expectations, not cryptographic pins; an operator can legitimately rotate its
Reticulum identity. Presets are convenience metadata, not trusted
infrastructure, an availability promise, or a replacement for manual endpoint
entry.

Hostname resolution remains DHCP-first so local network policy and split DNS
continue to work. The built-in resolver gets one bounded attempt. If it fails,
the firmware raw-queries each DHCP-provided resolver before trying `1.1.1.1`
and then `9.9.9.9`, with a separate bounded deadline for each raw attempt.
This distinguishes a built-in resolver failure from a common UDP or routing
failure. Public fallback is limited to globally plausible dotted names:
single-label names and common local/private suffixes still use only DHCP
resolvers. The fallback is allocation-free, does not persist a resolved
address, and drops its temporary UDP socket before the TCP actor acquires the
same bounded network-stack slot.

API 1.10 exposes the secret-free trace as optional runtime status: gateway,
DHCP resolver list, built-in resolver outcome, raw-socket setup, each typed
DHCP/public attempt, response code when present, and the successful resolution
source and address. The Expo client renders those stages directly instead of
collapsing every fallback failure into the original built-in resolver error.

This fallback is a connectivity policy, not a privacy or authenticity upgrade.
It discloses the configured public hostname to the selected resolver and does
not provide DNSSEC. Literal IPv4 remains available for owners who need to avoid
DNS, and a future configuration format may make public resolver policy
explicit per peer.

### Publish the standard RMAP interface-discovery shape

When explicitly enabled, the firmware registers the Reticulum
`rnstransport.discovery.interface` destination and publishes the E290's
LoRa-facing `RNodeInterface` metadata. The application data follows the
[RMAP v4 format][rmap-info]: a flags byte, a bounded MessagePack map, and a
32-byte LXMF-compatible proof-of-work stamp at cost 16.

The node computes the stamp cooperatively. With no public TCP peer configured,
the first completed payload is immediately eligible for the current
transports. With a public TCP peer configured, the first payload remains due
until interface 2 is online, so an earlier LoRa-only fanout cannot consume the
six-hour public cadence. The cached stamped application data is then
republished in a fresh signed announce every six hours. The projection includes
the appliance transport identity, public interface name, transport role, and
configured LoRa frequency, bandwidth, spreading factor, and coding rate. It
does not advertise the outbound TCP client as though that client were a public
server.

An RMAP marker describes configured interface metadata. It is not evidence that
this firmware has passed full gateway qualification. Current source implements
the required public-TCP **Boundary** to LoRa **Internal** announce block, but
the pinned Rete foundation and product do not yet implement the complete
interface-mode matrix, bounded role-aware path/identity retention, or
per-egress announce caps described by Reticulum's
[interface modes][rns-interface-modes]. True public-network border routing
therefore remains unqualified even when both packet interfaces are online.

### Make phone location a separate, explicit disclosure

Location publication requires both RMAP discovery and **Share location** to be
enabled. The app requests foreground location permission only after an explicit
capture action and obtains one current phone fix; it does not subscribe to
updates or request background location. This follows the foreground, one-shot
shape in [Expo Location][expo-location].

The app defaults to roughly 100-metre coordinate rounding and converts the
result to signed millionths of a degree before mutation. Firmware retains only
that fixed E6 latitude/longitude pair. It does not retain sensor accuracy,
capture time, phone identity, or altitude. Phone altitude is deliberately
omitted because it is not a validated mean-sea-level height for the RMAP field.

Turning off **Share location** retains the last coordinate for later reuse but
omits it from future discovery payloads. Clearing the coordinate removes it
from the next boot's payload. Both changes use the same reboot-to-apply policy
as the rest of the configuration. Neither action retracts an already propagated
announcement: RMAP states that entries can remain visible for up to seven days
after their last announce. The app must present that persistence before the
owner enables location sharing.

## Consequences

- An appliance can remain a private LoRa node with Wi-Fi, automatic ordinary
  announces, and RMAP independently disabled.
- Owners can make a node discoverable without a synchronous radio action in
  the API or a repeated-button announce burst.
- Public endpoint host changes are picked up at reconnect, but DNS availability
  becomes part of the TCP interface's failure surface. Public fallback improves
  availability for global names while exposing those names to Cloudflare or
  Quad9 after both the built-in and raw DHCP DNS paths fail.
- RMAP proof-of-work and payload ownership stay bounded and cooperative; they
  cannot make ordinary LoRa/LXMF service conditional on discovery success.
- The app must distinguish desired configuration from live Wi-Fi, DNS, TCP,
  forwarding, and RMAP publication status.
- Public TCP and public location are explicit privacy choices. End-to-end
  Reticulum protection does not hide connection metadata from the carrier, and
  disabling publication cannot immediately erase propagated RMAP state.

## Deferred decisions

- a remotely updateable endpoint catalog, health scoring, failover, and
  multiple simultaneous upstreams;
- per-peer DNS fallback policy, DNSSEC, and answer-owner/CNAME-chain
  validation hardening;
- live policy apply with learned-path invalidation;
- automatic or background phone location refresh;
- onboard GNSS integration and validated mean-sea-level height;
- explicit RMAP withdrawal if the ecosystem defines one;
- complete Reticulum interface-mode, cache-protection, and announce-cap
  semantics plus powered two-way LoRa-to-public-TCP qualification; and
- production IFAC, credential wrapping, and flash encryption policy.

[expo-location]: https://docs.expo.dev/versions/latest/sdk/location/
[reticulumnet]: https://www.reticulumnet.nl/en/get-started/
[rmap-info]: https://rmap.world/info.html
[rmap-json]: https://rmap.world/?json=1
[rns-interface-modes]: https://reticulum.network/manual/interfaces.html#interface-modes
[rns-trust]: https://reticulum.network/manual/networks.html#trustless-networking
