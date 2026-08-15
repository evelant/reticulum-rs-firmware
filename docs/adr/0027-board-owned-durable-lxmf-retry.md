# ADR 0027: Board-owned durable LXMF delivery loop

- **Status:** accepted and implemented; source/host qualified, powered hardware
  qualification pending
- **Date:** 2026-08-02
- **Extends:** [ADR 0014](0014-durable-lxmf-message-ownership.md),
  [ADR 0018](0018-durable-lxmf-delivery-policy.md), and
  [ADR 0025](0025-durable-packet-correlated-radio-tracing.md)

## Context

The appliance must continue delivering accepted LXMF messages while every app
is disconnected. The predecessor firmware owned and durably accepted only the
first attempt, then turned one RNS receipt timeout into an immutable submission
failure. Its app created another board submission with a new API idempotency
key. That interim client retry mechanism was not a standalone messaging
appliance: reconnect could create a burst, an offline phone could not help, and
one logical LXMF message consumed several durable submissions.

An RNS receipt belongs to one encrypted packet attempt. An LXMF message has a
stable signed wire representation and message ID across any number of carrier
attempts. Delivery policy therefore cannot treat an individual packet timeout
as the terminal state of the accepted message. Generic raw RNS DATA does not
have the same application-level duplicate suppression and must retain its
conservative one-attempt semantics.

The retry design must not consume one durable record per carrier attempt.
Doing so would invalidate the journal's fixed lifetime reservation and make
eventual delivery depend on implementing semantic log reclamation first.

## Decision

Durable acceptance is the ownership boundary. Once the device returns
`LxmfBasicSendAccepted`, the board owns path discovery, packet attempts,
delivery proofs, backoff, wakeups, reboot recovery, and eventual terminal
status. The app stages material until acceptance and then only observes or
accelerates the same board submission. It must not create replacement
submissions after a proof timeout.

The accepted LXMF wire, signature, message ID, sender-attached location, and
device-API submission ID remain immutable. Each retry prepares fresh RNS
ciphertext and therefore receives fresh packet metadata and a fresh proof
token. Only one attempt for a submission may be outstanding. A timed-out
attempt is reconciled and exactly acknowledged before another is admitted, so
the first implementation needs no concurrent sibling receipts.

The internal durable lifecycle keeps an LXMF delivery obligation nonterminal:

- `Queued` means accepted work has not crossed its preparation barrier;
- `Preparing` is a durable board-owned delivery loop within which any number of
  volatile, serialized carrier attempts may be prepared.

An authorized frame does not append an `AwaitingDelivery` transition for LXMF.
The durable `Preparing` barrier already proves that the exact accepted wire
must remain owned across timeout or reset. `Final(Delivered)` stores the
winning attempt metadata, still requires a proof, and remains
persist-before-ack.
Policy rejection, cancellation, unrecoverable invariant failure, or explicit
future expiry may still be final. A receipt timeout by itself is not final for
LXMF. The existing public API state vocabulary remains unchanged; clients see
one continuous nonterminal submission rather than attempt-internal states.

Conditions that prove no packet was transmitted also recycle only the carrier
attempt. These include a queued-owner rollback, permit-deadline expiry,
recovery-required completion, and an otherwise unpermitted final hop. A
semantic policy denial remains final. Recovery-backed unsent outcomes retain
their exact attempt correlation until both the recovered packet owner and the
terminal tombstone have been acknowledged; their ordering cannot unlock a
younger attempt early. The already-durable LXMF `Preparing` obligation covers
that volatile recovered owner directly, so retries do not consume the schema's
single transport-audit record or pin a later winning attempt to an older proof
token. Losing every eligible interface before admission is likewise transient
and arms the same board scheduler instead of committing `NoPath`.

Raw experimental RNS DATA remains one-shot. Its timeout and ambiguous reboot
handling continue to fail conservatively.

### Retry scheduling

The board permits at most one automatic LXMF retry globally. Freshly accepted
sends take precedence over retry work so one unreachable peer cannot monopolize
LoRa airtime. After an attempt timeout, the board uses base delays of 5 seconds,
15 seconds, 60 seconds, 5 minutes, and then 15 minutes for every later attempt.
It adds deterministic submission/attempt-derived additive jitter no greater
than 20 percent of the selected base delay.

An exact destination path transition from unusable to usable wakes that
submission before its deadline. Unrelated announces do not wake other
destinations. The wake edge is consumed only when a new carrier attempt is
actually bound, so shared discovery, Link pressure, or same-boot preparation
pressure cannot swallow it. A timeout may schedule the next attempt, but the
old attempt must first complete its exact terminal acknowledgement and release
its receipt and packet owners; no replacement attempt overlaps it.

Path discovery keeps its bounded two-request burst. Exhaustion leaves LXMF
pending and starts a later discovery cycle after 60 seconds, while raw RNS DATA
still resolves to terminal `NoPath`. Discovery traffic may be shared by
destination, but exhaustion policy remains per durable submission; a raw and
an LXMF waiter cannot inherit each other's completion behavior.

The firmware has no trusted wall clock. Retry deadlines and diagnostic attempt
ordinals are boot-volatile. After reboot, every replayed `Preparing` LXMF
obligation is restored rather than finalized as `InterruptedByReset`; it
becomes retry-eligible after a 15-second boot-relative delay plus the same
bounded deterministic jitter. Replaying the same signed LXMF message is safe
because the receiver's durable message-ID deduplication is authoritative. This
relaxation does not apply to raw RNS DATA.

Because attempts are volatile children of one durable `Preparing` obligation,
attempt frames, timeouts, and coherent recovered owners append no state or
audit records and require no new journal schema. Exact volatile recovery and
terminal acknowledgements still serialize reuse of their packet and attempt
owners. Quarantine remains fail-closed on the existing durable audit/final
path, and a replayed quarantine audit is never treated as a resumable LXMF
delivery loop. The current bounded resident-submission count and fixed record
reservation remain valid.
Terminal-submission retirement and semantic compaction may still be useful for
reclaiming acceptance capacity, but they are a separate storage decision and
are not on the correctness path for autonomous retry.

### App migration

The app no longer has an automatic retry timer, startup/reconnect wake, Sync or
Nearby wake, or automatic rearm budget. Commit-before-send reconciliation and
status polling remain. The explicit **Retry now** action is retained only as a
transitional escape hatch for legacy or permanently terminal rows; it creates a
replacement board submission while preserving the outbox row and signed LXMF
wire, and is not the normal path for a current board-owned `Preparing`
obligation. A later additive `retry_now(submission_id)` capability should
instead accelerate the existing obligation without creating another durable
submission or LXMF identity.

## Qualification

Source and host regressions cover the durable `Preparing` loop, fresh
ciphertext and attempt tokens over one immutable signed wire/message ID,
timeout retirement and exact acknowledgement, capped backoff and jitter,
single-global-retry scheduling with fresh-send priority, exact-path wake, and
boot restoration. Powered E290 timeout, retry, reboot, and disconnected-app
qualification remains pending.

## Consequences

The appliance can make progress with no phone connected and survives app
termination, BLE loss, and board reboot. Retry timing no longer produces a
client-side burst, and all physical attempts remain correlatable through their
distinct radio-trace tokens while the logical message stays stable.

At-least-once packet transmission is intentional. A proof may be lost after
the receiver durably imported the message, so a later attempt can be a
duplicate at the transport boundary. LXMF message-ID deduplication and the
receiver's `AlreadyDurable` proof path make that safe.

The number of distinct accepted submissions remains bounded until terminal
retirement is designed, even though each resident LXMF submission can retry
indefinitely without growing its journal history.
