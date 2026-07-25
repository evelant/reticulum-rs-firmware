# E290 bounded Nomad page powered proof

## Result

The corrected production BLE firmware completed one bounded, physical
Nomad-page path across two E290 boards and MetalbeardMobile:

1. Board A emitted its distinct signed `nomadnetwork.node` destination announce
   over the LoRa Reticulum interface.
2. Board B learned the peer and exposed its associated Nomad destination through
   the authenticated Nearby/Browse path.
3. MetalbeardMobile connected to Board B and authenticated over the production
   BLE GATT device-API bearer.
4. **Browse** selected Board A's associated `nomadnetwork.node` destination and
   requested `/page/index.mu`.
5. Board A returned the bounded static Micron page, and the user confirmed that
   the phone fetched and rendered the page.

This proves one complete application path from a physical phone, through BLE
authentication and Board B's device API, over LoRa Reticulum request/response
to Board A's embedded Nomad responder, and back to the phone. It does not move
Reticulum or Nomad protocol ownership onto the phone.

## Destination-selection observation

The contact's primary, `lxmf.delivery`, and `nomadnetwork.node` destinations
are distinct hashes derived from the same node identity with different
application/aspect names. Pasting an LXMF or primary contact hash into
**Browse** correctly failed; it did not name the Nomad responder. Successful
browsing used the associated `nomadnetwork.node` destination exposed by the
Nearby/Browse flow. Client UX should continue to prefer that association over
manual endpoint entry.

## Deliberate proof limits

This is a bounded functional proof, not a general NomadNet implementation or a
release qualification:

- the responder serves only `/page/index.mu`, accepts only the canonical
  anonymous MessagePack `nil` value, and returns one static UTF-8 Micron page
  no larger than 400 bytes in a direct single-packet response;
- Resource-backed pages, files, forms, dynamic content, executable programs,
  and an independent Nomad announce directory remain absent;
- the device API retains one boot-scoped Nomad fetch owner, so pressure,
  cancellation, retry, reset, and concurrent-client behavior remain bounded by
  the documented alpha contract;
- the user-confirmed page fetch/render is not a standards-complete Micron
  rendering, accessibility, or hostile-content qualification;
- no soak, range, low-memory, allocation-failure, or multi-interface pressure
  campaign was run; and
- the corrected image still needs a complete exact-readback flash campaign and
  concurrent BLE/LoRa/cache-disabled interaction qualification.

The linked-stack correction and its separate powered startup evidence are
recorded in [the E290 node runbook](e290-node.md). No artifact hash, timestamp,
or additional screen-state claim is inferred beyond the observed path above.
