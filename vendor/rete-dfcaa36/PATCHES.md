# Rete dfcaa36 local patch inventory

This directory is a minimal source snapshot of the four Rete crates used by
the firmware, taken exactly from
`dfcaa36b2d45c22d9cba8f0a7eaeb4cf78cabf08` in
<https://github.com/evelant/rete>. It deliberately excludes examples, build
artifacts, unrelated workspace crates, and the ignored `reference/` checkout.

The local delta adds construction-only APIs:

- `rete_transport::Transport::new_in` initializes each transport field at its
  final caller-owned `MaybeUninit` address.
- `rete_stack::NodeCore::new_in` validates the expanded destination name before
  writing, initializes its nested transport with `Transport::new_in`, and then
  completes every remaining field at its final address.

Existing constructors and protocol behavior are unchanged. The E290 product
uses these APIs recursively so its 64-route/128-dedup node can live in PSRAM
without first creating a capacity-sized CPU-stack temporary. Narrow unsafe
blocks are limited to audited raw field projection and are covered by focused
constructor-equivalence tests. The final linked firmware remains gated by the
compiler-emitted startup stack audit before packaging or flashing.

The upstream tree at this exact revision does not contain a top-level license
file. Project provenance and the retained upstream license declaration remain
recorded in the repository `NOTICE` and `docs/provenance.md` files.
