# ADR 0023: Receiver-local ingress evidence and Reticulum proof probe

- Status: accepted
- Date: 2026-07-30
- Extends: ADR 0014, ADR 0015, ADR 0017, ADR 0020

## Context

Field testing needs both passive evidence about delivered messages and an
active reachability measurement. Nearby announce observations cannot answer
either question reliably: they are not tied to a message's first arrival, can
change later, and do not prove that a destination returned a Reticulum proof.

The evidence must survive board and app restarts without changing LXMF message
identity or weakening the receive-before-proof durability barrier. The active
operation must use normal Reticulum routing without creating a durable chat
message or submission-journal record.

## Decision

The first newly committed LXMF arrival retains its interface and optional
RSSI/SNR pair. Signal fields are all-or-none. Replay does not replace this
immutable observation. It follows the message through the firmware ingress
event, append-only store, device API, app import, SQLite timeline, activity log,
and message details.

This is receiver-local final-hop evidence. A routed message may therefore
measure a relay rather than the original sender. Old records can have no
ingress evidence, and the app must not infer it from Nearby observations or
current interface state. At app persistence, an exact duplicate may fill a
currently missing observation once, but it never replaces an existing one.

LXMF physical format 2 stores the observation in previously reserved header
space. Current firmware reads both format-1 and format-2 records but writes only
format 2. Format-1 records decode with no ingress evidence. After a format-2
record is appended, format-1 firmware cannot safely roll back onto that store.
This supersedes only ADR 0014's physical-format-1 statement.

Unreleased device API 1.14 bundles both additions:

- optional `LxmfMessageSummary` map key 10 for ingress evidence;
- capability key 21 for the proof probe;
- operation `0xf012` to start a probe; and
- operation `0xf013` to poll its result.

The probe targets the canonical `rnstransport.probe` destination through the
ordinary router and proof machinery. Firmware retains one volatile
principal/idempotency result slot and bypasses both the durable submission
journal and LXMF store. Its operation remains bounded by consecutive
60-second identity and probe-path lookups, a 30-second packet-capacity wait,
and the 60-second DATA receipt lease. The path and hop snapshot is revalidated
immediately before packet preparation. Protocol clock maintenance retains a
fair lane even under ordinary action pressure so receipt expiry is not
starved.

The one volatile record cannot be replaced while active or before its owner
has polled a terminal result once. After that first terminal poll, a different
start may reuse the slot. The app retains and resumes the exact accepted ID
across ambiguous poll failures; clearing that local recovery state is an
explicit reboot-recovery action.

The local responder uses Reticulum `PROVE_ALL`. Rete emits its proof as an
immediate transport-neutral packet action on the ingress interface; the
accompanying proofless semantic event is acknowledged without entering the
delayed-proof owner or durable inbox.

A successful probe establishes Reticulum path-and-proof reachability only. It
does not establish LXMF availability, application throughput, or RSSI for the
request at the remote appliance. Reported return signal is measured by the
initiator on the proof's final hop and may describe a relay. Public nodes may
omit or disable the responder.

## Consequences

The app can distinguish message-specific passive evidence from a deliberate
one-shot path measurement without duplicating transport logic. Missing
historical evidence remains explicitly unknown.

API 1.14 remains unreleased and the ingress and probe changes ship as one
versioned contract. Portable tests qualify encoding, persistence, replay, and
probe state-machine behavior. Powered E290, multi-hop, and third-party
responder qualification remain field work. Outbound egress evidence, remote
request signal, general packet tracing, durable probe history, concurrent probe
slots, LXMF service checks, and throughput tests remain separate features.
