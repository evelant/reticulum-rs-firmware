# ADR 0002: Adopt Rete as the provisional RNS foundation

- **Status:** accepted
- **Date:** 2026-07-14
- **Decision owners:** project maintainers
- **Supersedes:** the equal Rete/Leviculum candidate-selection portions of
  [ADR 0001](0001-phase-0-scaffold.md)

## Context

The scaffold treated Rete and Leviculum as candidates that would undergo the
same full bake-off before either could drive product implementation. Further
source review found enough implemented Reticulum breadth and embedded-oriented
structure in Rete to begin the production integration without waiting for that
parallel exercise. Maintaining two equally deep adapters would delay the
vertical slice and would test abstraction work more than product risk.

Rete is not unconditionally accepted. Its checked-in Python interoperability
environment predates the project's Reticulum 1.3.8 baseline, several packet and
Resource paths can make large network-controlled heap allocations, and some
interface, queue and bounded-table failures are not propagated or measured.
Those are validation and hardening requirements, not reasons to start another
RNS implementation before trying focused fixes.

Rete's LXMF crates are outside this decision. In particular,
`rete-lxmf-core` is not the project's LXMF compatibility authority and remains
excluded from the product graph because its stamps, tickets and structured
fields do not match the required current LXMF behavior.

Leviculum remains valuable as an independently implemented protocol oracle and
as a credible fallback if Rete meets an abandonment criterion.

## Decision

### Integrate Rete first

Rete is the provisional RNS foundation. New packet, identity, announce,
transport, Link and Resource integration work targets Rete first. The project
will build the smallest useful end-to-end slice through Rete's native APIs
rather than first designing a broad backend-neutral RNS abstraction.

"Provisional" means implementation may proceed before every production gate
passes, but unvalidated paths are not exposed as production capabilities. In
particular, arbitrary inbound Resources, compressed Resources and large local
interface packets remain disabled or strictly capped until their allocation,
decompression and backpressure behavior satisfies the Phase-0 contract.

### Keep Leviculum as oracle and fallback

The isolated Leviculum package remains pinned, buildable and available for
targeted differential tests. It is not required to receive a feature-complete
product adapter or identical on-target benchmarking before Rete work proceeds.
Use it when an independently implemented result can distinguish a Rete defect
from a fixture or Python-peer defect, or when a Rete abandonment criterion is
under review.

This fallback is intentionally real: product-owned wire fixtures, radio
framing, storage models and device APIs must not depend on Rete-specific data
structures where that coupling provides no product value. It does not justify
a speculative universal RNS trait.

### Validate current protocol behavior continuously

Reticulum 1.3.8 is the released compatibility authority. Current Reticulum
`main` is a forward-compatibility warning lane. Rete's existing older Python
vectors and tests are useful evidence, but they do not satisfy this gate until
regenerated and run against the pinned peers in `interop/peers.toml`.

The Phase-0 conformance, hostile-input, allocator-failure, backpressure and
on-target measurement gates remain mandatory. A discovered discrepancy becomes
a focused test and repair; it is not ignored because Rete is now the default.

### Bound network-controlled work before enabling it

The integration must provide explicit, profile-configurable limits for packet
ingest, concurrent and total Resource state, Resource bytes and segments,
split-send and split-receive buffering, pending requests, event queues and
transport tables. Exhaustion must reject, evict or backpressure according to a
documented policy and expose a reason or metric. It must not rely on an
infallible allocator.

Whole-buffer Resource assembly and decompression may be used only where the
measured profile proves them safe. The full product still requires bounded,
preferably flash-streamed Resource handling; absence of PSRAM on the Tracker
does not remove that requirement or reduce protocol truth.

### Fork only when a source patch is required

Rete remains pinned to the reviewed upstream commit until the first required
source change. That change triggers a project fork retaining upstream history,
and every Rete crate in a product graph must use the same exact fork revision.
Do not mix upstream and forked Rete crates.

Generic fixes and tests should be offered upstream as small, independently
reviewable changes. Examples include current-Python conformance updates,
configurable resource limits, fallible queue/table operations, send-error
propagation, diagnostics and storage/streaming seams. Tracker pins and FEM
sequencing, regulatory and airtime policy, product quota values, flash schemas
and the local device API remain project-owned concerns.

## Promotion and abandonment criteria

Rete is promoted from `ProvisionalFoundation` to `ProductionFoundation` only
after every production hard gate in the
[Phase-0 validation contract](../phase-0-acceptance.md) has reproducible passing
evidence, the required memory and target measurements are published, and no
abandonment criterion below is met. Promotion requires an explicit follow-up
decision and metadata change; it is not inferred from a passing unit-test run.

Rete is abandoned as the provisional foundation when reproducible evidence
shows one or more of the following:

1. Required wire behavior cannot interoperate with released Python Reticulum
   1.3.8 after the fixture is independently validated, and correction would
   require replacing Rete's protocol model rather than a focused repair.
2. Network-controlled packet, Link or Resource work cannot be given explicit
   finite limits and recoverable exhaustion behavior without replacing the
   core lifecycle or data path.
3. Correctness-critical send, queue, event or routing-table failure cannot be
   surfaced or handled without replacing the core transport/runtime seam.
4. The required `no_std + alloc` product graph cannot be kept free of hosted
   runtime dependencies, or the minimum transport-node profile cannot execute
   on a suitable embedded target with a measured safe memory floor.
5. The cumulative Rete fork would own enough protocol machinery that continued
   maintenance is effectively a separate RNS implementation and a proven
   alternative has lower integration and correctness risk.

A failure in one of these areas first receives a bounded repair spike and a
test suitable for upstreaming. Abandonment is based on the resulting evidence
and patch scope, not merely on finding a bug. Lack of Tracker PSRAM by itself
is not an abandonment criterion; profiles may omit optional clients on that
board, and full-stack acceptance may move to suitable PSRAM hardware.

If an abandonment criterion is met, pause new Rete-specific integration,
preserve the conformance evidence, and run the failing contract plus the
minimum product vertical slice against Leviculum and any newly qualified
alternative. Do not weaken the contract to retain Rete.

## Consequences

- Product implementation can begin with the strongest current embedded Rust
  RNS base while protocol and safety evidence accumulates.
- Phase 0 becomes a Rete validation and hardening phase, not an equal bake-off.
- Leviculum maintenance cost stays bounded while preserving independent
  evidence and an actionable fallback.
- Some Rete changes are likely to live temporarily in a project fork while
  upstream reviews them.
- Passing Rete's own tests or compiling an example remains insufficient for
  production acceptance.

All other ADR 0001 decisions, including source pins, toolchain lanes, RF-safe
defaults, wire-size boundaries and license separation, remain in force.
