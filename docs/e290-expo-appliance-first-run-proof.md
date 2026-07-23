# E290 Expo appliance managed first-run proof

**Date:** 2026-07-22 EDT / 2026-07-23 UTC

**Status:** complete for a powered, credential-empty managed first run of one
E290 through the Expo web client, persistence across a host-service restart,
and one exact basic LXMF message sent from that newly paired E290 to a second
simultaneously connected E290 over LoRa. The sender reached Reticulum
`Delivered`, and the receiver imported the same message identifier, timestamp,
endpoints, title, and content.

This is an appliance integration proof, not a new firmware-artifact
qualification. It extends the earlier
[host-appliance alpha proof](e290-lxmf-appliance-alpha-proof.md) with the
managed onboarding path and browser client, while reusing the permanent E290
firmware and radio path qualified by the
[chat-alpha powered proof](e290-lxmf-chat-alpha-proof.md).

## Board and clean-start binding

| Role | USB serial / eFuse base MAC | Flash | LXMF delivery destination |
| --- | --- | ---: | --- |
| Newly paired sender | `AC:A7:04:E1:3F:88` / `ac:a7:04:e1:3f:88` | 16 MiB | `935caba93f7cd97c7c6658350ac02b45` |
| Existing paired receiver | `AC:A7:04:E1:3E:88` / `ac:a7:04:e1:3e:88` | 16 MiB | `03869ee76b74d1e2a4626f0c02ae3248` |

The destructive preparation was bound to the exact 3F USB serial, MAC, and
16 MiB flash capacity. The identity-safe helper wrote all `0xff` bytes to only
the credential partition at offset `0x614000`, length `0x2000`, then read back
that exact range. The write input and readback both had SHA-256
`7d2c7ac4888bfd75cd5f56e8d61f69595121183afc81556c876732fd3782c62f`.
The 3E peer was not erased.

The erase evidence is retained locally under
`/private/tmp/reticulum-expo-first-run-20260723`. This path is a development
evidence reference rather than a portable repository artifact. The run did not
rewrite or read back the complete firmware image.

## Managed Expo onboarding

A new owner-private host profile contained no credential for 3F. The appliance
service selected that board by exact USB descriptor serial, and the initial
Expo state was `NeedsPairing`. After browser session bootstrap, the Expo client
removed its fragment from the visible URL; no fragment value or other secret
material is retained in this record.

The powered onboarding sequence was:

1. The user selected **Start pairing** in the Expo client.
2. The user held the E290's middle physical button, labelled `21`, to satisfy
   the firmware's physical-presence gate.
3. Pairing completed, after which the user performed a real board reset.
4. The service observed the reset boundary and reconnected to the same exact
   USB serial.
5. The host credential became `Active`: a 96-byte file with mode `0600`
   beneath mode-`0700` managed directories.
6. The Expo UI reached `ready` and reported the 3F LXMF destination
   `935caba93f7cd97c7c6658350ac02b45`.

The service was then stopped and restarted against the same profile. It
authenticated to 3F and returned to `ready` without another pairing operation
or physical-presence prompt. This closes one ordinary host-process restart
against the retained owner-private credential.

The two boards were not visibly mapped to their USB serials during the
physical-presence step, so the user held button 21 on both boards. The host
protocol and destructive preparation remained bound to exact serial
`AC:A7:04:E1:3F:88`, but this run therefore does not prove that a user can
identify the intended physical board from the current UI alone.

## Simultaneous 3F-to-3E LXMF exchange

A second appliance service opened the already paired 3E board at the same time
and independently reached `ready`. Through the 3F Expo client, 3E's LXMF
destination `03869ee76b74d1e2a4626f0c02ae3248` was added as a contact and the
following exact message was queued:

| Field | Value |
| --- | --- |
| Source | `935caba93f7cd97c7c6658350ac02b45` |
| Destination | `03869ee76b74d1e2a4626f0c02ae3248` |
| Timestamp ms | `1784779075861` |
| Title | `expo-e2e-20260723` |
| Content | `Expo client to E290 3E over LoRa after managed first-run pairing` |
| LXMF message ID | `91e05bb6942785cf67490d3ef5441da9494586433009ff785e883a040f1bc1b1` |
| Sender terminal status | `Delivered` |

The 3E service imported the message from its device inbox with that exact
message ID, timestamp, source, local destination, title, and content. The 3F
sender subsequently displayed `Delivered`, closing the Expo client -> host
database -> authenticated USB -> 3F firmware -> LoRa -> 3E durable inbox ->
authenticated USB -> receiver database path.

An earlier onboarding attempt reported a transient broken-pipe error before
the successful pairing run. That interaction is not used as the powered
success result, and this record does not assign it a cause. The later Active
artifact, authenticated restart, and exact durable sender and receiver rows
above are the decisive evidence.

## What this run establishes

This bounded run establishes that:

- a credential-erased E290 and empty host profile enter `NeedsPairing`;
- the Expo web client can drive the physical-presence pairing and reset
  lifecycle to `ready`;
- the resulting credential has the expected bounded size and owner-private
  filesystem permissions;
- the retained credential authenticates after a host-service restart without
  re-pairing;
- two appliance services can concurrently own two exact-serial E290 sessions;
  and
- the newly paired board can send one exact LXMF message through the Expo
  client and reach `Delivered` after exact import by the peer.

## Limits and follow-up

This is one same-host, near-field, direct LoRa exchange. It does not qualify
electrical power loss, host suspend/resume, repeated cable churn, credential
corruption recovery, concurrent access to one board, pairing timeout and abort
paths, sustained traffic, storage pressure, multi-hop routing, propagation
nodes, NomadNet, BLE, Wi-Fi, or multiple simultaneous Reticulum transports. It
also does not qualify native iOS or Android builds, native Rust embedding, or a
SPA served directly by the E290.

The broken-pipe observation remains an unresolved transient that should be
covered by repeat and fault-injection testing. The physical-board
identification gap also needs explicit appliance UX, such as showing a
board-displayed identifier or offering an identity-bound locate action before
requesting physical presence. No credential bytes, session secrets, or URL
bootstrap values are included in this proof.
