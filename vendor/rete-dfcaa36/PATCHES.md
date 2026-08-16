# Rete dfcaa36 local patch inventory

This directory is a minimal source snapshot of the four Rete crates used by
the firmware, taken exactly from
`dfcaa36b2d45c22d9cba8f0a7eaeb4cf78cabf08` in
<https://github.com/evelant/rete>. It deliberately excludes examples, build
artifacts, unrelated workspace crates, and the ignored `reference/` checkout.

The local delta adds:

- `rete_transport::Transport::new_in` initializes each transport field at its
  final caller-owned `MaybeUninit` address.
- `rete_stack::NodeCore::new_in` validates the expanded destination name before
  writing, initializes its nested transport with `Transport::new_in`, and then
  completes every remaining field at its final address.
- path requests use the endpoint and transport wire shapes from Python
  Reticulum, require a tag, deduplicate exactly by destination and tag, and
  rebuild recursive requests with this relay's transport identity and the
  original tag. The public request builder likewise always emits a fresh tag;
- a known cached response is suppressed when the requester is the path's next
  hop. Otherwise delayed responses retain the requesting interface, coalesce by
  destination, report bounded queue exhaustion, are emitted once after the
  one-second embedded approximation of the path-response grace period, and
  leave only through that exact interface;
- cached responses are marked with `PATH_RESPONSE` while preserving their
  signed announce payload, context flag, hop count, and HEADER_2 transport
  identity. Received path responses are learned but never queued for immediate
  or timer-driven rebroadcast;
- duplicate SINGLE announces bypass the global packet-hash filter and reach
  signed-announce replay handling. A repeated valid announce is rejected while
  its path remains retained but can restore a removed path, including repeated
  `PATH_RESPONSE` recovery cycles;
- ANNOUNCE packets whose destination type is not SINGLE and PLAIN non-announce
  packets above post-ingress hop one (wire hop zero on physical ingress) are
  rejected before deduplication or path state mutation; and
- path-request dispatch requires the same DATA/PLAIN/path-control-destination
  envelope used by the product adapter. HEADER_2 ownership remains the ordinary
  transport admission check, and request context is intentionally unrestricted; and
- each path's cached raw announce (`Path::announce_raw`) is retained in an
  inline, MTU-sized `AnnounceCache` buffer instead of a heap `Vec`, so the
  announce cache lives in the caller's PSRAM-backed path table rather than the
  strict internal heap a Wi-Fi/BLE controller needs for receive buffers.

These discovery changes mirror Python Reticulum's tagged request parsing,
requester attachment, `PATH_RESPONSE`, announce replay, and packet-admission
behavior. The embedded adapter outside this vendor snapshot supplies the
separate bounded destination-to-requester table used for recursive discovery;
it does not add an ingress minimum-interval throttle. They fix the pinned
revision's globally broadcast cached response, ordinary-announce response
rebroadcast, malformed-request admission, and inability to relearn a removed
path from the same signed announce.

The E290 product uses the construction APIs recursively so its
256-route/512-dedup node can live in PSRAM without first creating a
capacity-sized CPU-stack temporary. Narrow unsafe blocks are limited to audited
raw field projection and are covered by focused constructor-equivalence tests.
The final linked firmware remains gated by the compiler-emitted startup stack
audit before packaging or flashing.

Remove the overlay after one coherent upstream Rete revision supplies the
in-place construction contract, the request parsing, recursive rebuilding,
source-bound/coalesced `PATH_RESPONSE`, non-rebroadcast learning, signed-
announce replay recovery, ingress admission behavior, and inline announce
cache listed above, and the workspace regression suites pass on that revision.

The upstream tree at this exact revision does not contain a top-level license
file. Project provenance and the retained upstream license declaration remain
recorded in the repository `NOTICE` and
`docs/reference/dependencies.md` files.
