# Management application protocol

The product management API is an allocation-bounded application protocol above
PRNS. The current unreleased logical version is **6.0**. Rust definitions in
`crates/device-api` and their wire tests are authoritative.

## Layering

The logical request and response bodies are strict CBOR. They do not own an
executor, storage device, board, packet interface, or PRNS runtime. PRNS owns
Link encryption, requester identity, request paths, response framing,
settlement, and transport selection.

The E290 registers these routes on its shared management/OTA destination:

| Path | PRNS policy | Product behavior |
| --- | --- | --- |
| `/reticulum/embedded-node/public` | `AllowAll` | Read-only capabilities and destination identity |
| `/reticulum/embedded-node/api` | initially empty `AllowList` | Authorized CBOR management operations |
| `/reticulum/embedded-node/enroll` | `AllowAll` | Identified-Link request checked against the physical-presence window |
| `/e290/ota/*` | initially empty `AllowList` | Manifest, chunk arming, status, and reboot for one Link-bound OTA session |

Enrollment commits the identified requester's Reticulum identity hash before
asking PRNS to admit it on every privileged path. There is no record framing,
device credential, possession-proof protocol, or custom BLE bearer below the
logical API.

## Version and encoding

Every CBOR message carries major and minor version numbers. A decoder accepts a
minor revision within its major generation and skips unknown numeric map
fields, but rejects another major version, missing or duplicate known fields,
unknown closed-enum values, indefinite containers, excessive nesting, and
trailing bytes.

Encoder output uses definite containers, ascending unsigned numeric keys, and
preferred integer representations. One logical message is exactly one CBOR
item carried as one PRNS request or response value.

| Limit | Value |
| --- | ---: |
| Logical message | 512 encoded bytes |
| Operation body | 480 encoded bytes |
| Ordinary PRNS Single plaintext | 383 bytes |
| Recognized fields in one API map | 32 |
| Container/tag nesting | 8 levels |
| Basic LXMF title | 295 bytes |
| Basic LXMF content | 295 bytes |
| LXMF read chunk | 416 bytes |

Title and content limits are structural per-field limits. The complete request
and opportunistic one-packet LXMF composer impose a lower combined bound.

## Envelope

Requests and responses use this map:

| Key | Type | Meaning |
| ---: | --- | --- |
| 0 | map | `{0: major u16, 1: minor u16}` |
| 1 | u64 | client-selected request ID echoed by the response |
| 2 | u16 | operation number or response kind |
| 3 | map or operation-specific fixed array | operation body |

The request ID correlates one exchange. It is not an idempotency key or an
authorization credential.

## Operations active in the PRNS composition

| Number | Operation | Purpose |
| ---: | --- | --- |
| `0x0001` | `system.capabilities` | Report active operation availability and bounds |
| `0x0002` | `submission.status` | Read one principal-owned durable outbound state |
| `0x0003` | `identity.summary` | Read management and optional LXMF destination hashes |
| `0x0004` | `appliance_label.get` | Read the product-owned appliance label and durable revision |
| `0x0005` | `appliance_label.mutate` | Compare-and-swap the optional appliance label |
| `0xf004` | `lxmf.next` | Page committed LXMF summaries |
| `0xf005` | `lxmf.read` | Read normalized exact LXMF wire bytes |
| `0xf006` | `lxmf.basic_send` | Compose, sign, and durably accept one opportunistic LXMF message |
| `0xf007` | `lxmf.peer_next` | Page the boot-scoped projection of authenticated `lxmf.delivery` announces observed by this appliance |
| `0xf00e` | `diagnostics.node` | Read live PRNS interface, route-count, and link-count facts |
| `0xf00f` | `diagnostics.routes` | Page best-effort live PRNS route snapshots |
| `0xf010` | `lxmf.mailbox_status` | Read the durable client-collection watermark |
| `0xf011` | `lxmf.mailbox_acknowledge` | Advance that watermark monotonically |

The public path answers only capabilities and identity. The authorized path
answers the complete table above. Other known version-6 operations still
present in shared DTOs return `CapabilityUnavailable` until their product owner
is ported to the PRNS application boundary; they are never forwarded into the
retired dispatcher.

The `0xf000..=0xffff` range is reserved for product extensions. Availability
can depend on mounted storage and runtime state. A known but unavailable
operation returns a typed error instead of falling back to volatile behavior.

The appliance label identifies the physical product in the app and on its
display. It is stored in the generic product-state arena and is deliberately
separate from LXMF, NomadNet, or other application announce data. Mutations
use the last observed revision; a conflict returns the current revision so the
client can refresh rather than overwrite another editor.

`lxmf.peer_next` is contact-selection and display evidence, not routing
authority. Each page retains the complete destination and identity hashes,
authenticated announce app data, hop count, complete observing-interface ID,
observation age, and a boot-scoped cursor. A changed incarnation or lost live
projection history is explicit. The phone combines these rows with its own
PRNS announce observations while preserving which Reticulum node observed each
row; it never inserts appliance observations into the phone's route table.

## Messaging semantics

`lxmf.basic_send` accepts destination, timestamp, title, content, an
idempotency key, and optional typed phone location. The appliance constructs
and signs the LXMF message. Callers cannot inject arbitrary signed wire.

A successful response means the exact signed intent is durable in product
flash. It does not mean PRNS accepted a command, an interface transmitted, or
the recipient delivered it. Board-owned retry keeps the LXMF wire and message
ID stable while each ordinary PRNS send uses fresh transport state. The later
PRNS delivery receipt advances the product's delivered marker.

Repeating an uncertain mutation with the same principal, idempotency key, and
identical content is safe. Reusing that key for different content is an error.

Mailbox acknowledgement is an appliance-wide collection watermark, not human
read/unread state. The client advances it only after committing the contiguous
inbox prefix to its own durable SQLite store.

Incoming messages retain Python LXMF signature state as `validated`, `source
unknown`, or `invalid`. PRNS immediate proof timing is outside this API and is
not delayed by application persistence.

## OTA wire protocol

OTA control uses compact MessagePack values on its dedicated paths instead of
the CBOR operation envelope. The start manifest names version, image size, and
SHA-256. Each ordinary PRNS Resource carries at most 7 KiB of image data and an
exact 32-byte MessagePack `bin8` metadata value containing session, index, and
offset. Status reports phase, slot, version, verified bytes, next chunk,
Resource-gate state, and stable failure.

The same protocol runs over Bluetooth Auto, LoRa, TCP, or a routed combination.
It does not create another destination or transport-specific OTA session.

## Errors

CBOR error responses use kind `0x0000` and contain an error code plus the
related operation when known.

| Code | Meaning |
| ---: | --- |
| 1 | unsupported operation |
| 2 | unsupported version |
| 3 | authentication required |
| 4 | permission denied |
| 5 | not found |
| 6 | invalid request |
| 7 | capability unavailable |
| 8 | internal failure |
| 9 | capacity exhausted |
| 10 | idempotency conflict |
| 11 | retry later |

`RetryLater` is product pressure, not a Reticulum protocol state. Link loss,
request timeout, path failure, and PRNS command settlement remain typed network
outcomes in the native or host requester.

## Authorization

The public route is readable through a normal PRNS Link without management
authorization. Privileged routes require the remote Link initiator to identify
itself and for that identity hash to be present in PRNS's request-handler
allow-list. The product's mirrored allow-list is the durable policy source and
is replayed into PRNS at boot.

The enrollment route is reachable before authorization only so product policy
can inspect the already identified requester and physical-presence window. It
accepts one canonical empty value, commits the identity, consumes the window,
waits for PRNS authorization settlement, and then returns success.

## Generated clients

The Expo application uses Rust-generated DTOs in
`clients/appliance/src/generated/api.ts`. Change Rust sources first, then run
from `clients/appliance`:

```sh
bun run api:generate
bun run api:check
bun run native:bindings
```

The logical API version, operation vocabulary, capabilities, native bridge
contract, and generated TypeScript must advance together.
