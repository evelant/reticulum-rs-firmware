# ADR 0016: Bound Link DATA ownership for direct LXMF

- **Status:** accepted for responder-side source composition; powered direct
  LoRa qualification pending
- **Date:** 2026-07-23
- **Decision owners:** project maintainers
- **Extends:** [ADR 0012](0012-application-event-and-resource-ownership.md),
  [ADR 0013](0013-bounded-lxmf-wire-boundary.md), and
  [ADR 0014](0014-durable-lxmf-message-ownership.md)

## Context

Direct LXMF messages through the packet carrier arrive as context-`NONE` DATA
on an authenticated Reticulum Link. The portable LXMF wire parser already
supports this carrier, including the complete on-wire destination, but the
application event previously retained only the Link identifier, context, and
plaintext. Carrier shape alone cannot prove which local service owns the Link,
so the LXMF ingress correctly deferred every such event.

Direct LXMF delivery also has an RNS packet receipt. Python LXMF calls
`packet.prove()` before accepting a direct packet, and the resulting proof is
addressed to the Link rather than to the truncated packet hash used by an
ordinary Single-destination proof. Omitting that proof lets the durable receiver
commit the message while the sender continues waiting, retries, or tears down
the Link.

Pinned Rete already retains the destination associated with each owned Link.
For a responder this is the exact local Single destination that accepted the
LINKREQUEST. For an initiator it is the remote destination used to create the
Link. Looking the association up after the event leaves Rete would introduce a
race with Link closure and would make queued application events ambiguous.

RNS Resources are a separate problem. The current native path assembles an
allocation-backed body and accepts advertisements before this product has
bounded concurrent transfers, parts, bytes, storage placement, decompression,
or retry ownership. Enabling Links does not authorize that Resource path.

## Decision

`rns-rete` projects every native Link DATA event with a required opaque
`ApplicationLinkBinding`. Only the adapter can construct this value, directly
from the retained native Link during synchronous ingress. The binding keeps the
Link identifier and destination inseparable and exposes read-only accessors.
If Rete emits Link DATA without retaining the associated Link, ingress fails
closed before an application event is released.

For a responder-side context-`NONE` packet on a Link bound to the selected local
destination, `rns-rete` also retains the exact packet proof with that event. The
proof covers the full 32-byte hash of the received encrypted RNS packet and has
the canonical Python-compatible shape: HEADER_1/BROADCAST, packet type PROOF,
destination type LINK, destination hash equal to the Link ID, context `NONE`,
and explicit payload `packet_hash[32] || signature[64]`. The responder signs
with the local destination identity associated with the Link. A Single-
destination proof, a truncated covered hash, an implicit 64-byte proof, a
different Link ID, or any other packet shape does not satisfy this contract.

`lxmf-ingress` admits a Link DATA wire candidate only when all of these are
true:

1. the Link context is RNS `NONE`;
2. the bound Link destination equals the caller-selected local
   `lxmf.delivery` destination;
3. the complete LXMF wire destination independently equals the bound
   destination;
4. the existing structural, source-binding, signature, and stamp policies
   succeed.

The durable product pipeline additionally requires the application-event owner
to carry the exact Link packet proof described above. `lxmf-ingress` borrows
only the semantic event and therefore does not claim proof ownership by itself;
`lxmf-durable-ingress` enforces required-proof admission before any store I/O.

The payload remains in its original application-event allocation throughout
validation and durable commit. Durable rebinding checks the carrier kind,
binding destination, context, and physical length before constructing a
contiguous candidate. Direct Link DATA always selects required-proof durable
admission. The proof remains withheld from ordinary transmission until the
store returns `Committed` or a freshly received retransmission returns
`AlreadyDurable`; only then does the existing delayed-proof owner make it ready
for the transport-neutral supervisor.

The permanent E290 image enables inbound Links only on the mount-gated
`lxmf.delivery` destination; its primary destination continues to reject local
Link termination. Both opportunistic destination DATA and admitted direct Link
DATA require their exact retained RNS delivery proof before durable
acknowledgement.

This tranche accepts only responder-side direct receive on a Link whose retained
destination is the mounted local `lxmf.delivery` service. Initiator-side or
backchannel context-`NONE` receive remains unsupported. Python Link initiators
prove packets with the per-Link ephemeral Ed25519 key advertised in their
LINKREQUEST, not with the node identity, and the current wrapper does not yet
retain that private signing authority.

Native Resource ingress remains rejected before Rete begins Resource
allocation or assembly. `ResourceComplete` remains an explicitly deferred LXMF
carrier.

## Consequences

- Direct LXMF packet carriers through the RNS Link MDU are now representable
  through the source ownership and delayed-proof boundaries without weakening
  service ownership.
- Initiator-side traffic cannot be mistaken for a local LXMF delivery merely
  because it uses context `NONE`; its binding names the remote Link
  destination and this tranche rejects it.
- The permanent image's fixed four-Link table and sixteen application-event
  slots bound concurrent volatile ownership. These are product-profile limits,
  not protocol limits.
- LXMF messages larger than the direct packet boundary still require the
  separately qualified Resource tranche.
- This source milestone does not claim powered interoperability until two
  permanent E290 images complete responder-side Link establishment, direct LXMF
  commit/read, delayed proof release, and sender-visible delivery behavior over
  LoRa.

## Next protocol slice

The narrowest useful NomadNet client proof is an anonymous request for
`/page/index.mu` at an explicitly supplied `nomadnetwork.node` destination:
path discovery, outbound Link establishment, one bounded direct REQUEST, and
one correlated small direct RESPONSE returned as raw UTF-8 Micron bytes through
the device API. The node wrapper must first expose bounded request and pending-
response APIs: fixed concurrent-request capacity, explicit request/link
correlation, bounded request and response bytes, and a take-or-poll operation
that never exposes mutable Rete state. A response that does not fit the selected
direct Link response bound fails with a typed too-large/deferred result; it does
not silently enable Resource reception. Large Resource-backed responses, Link
Identify, discovery UI, forms, and Micron rendering remain later slices. Before
exposing that operation, the Rete request codec must be checked against Python's
arbitrary MessagePack request value and request-ID semantics rather than
cementing its current binary-only convenience shape.
