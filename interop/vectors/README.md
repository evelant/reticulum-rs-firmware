# Golden vectors

`rns-1.3.8.json` is the first deterministic released-Python foundation corpus.
It covers a stable identity, signed announce, plain packet, packet hashes and
their exact wire bytes. Its peer revision and generator are embedded in the
file and checked by the host conformance runner.

This is intentionally only the first slice. The Phase-0 validation contract
still requires HEADER_2, encrypted packets, proofs, IFAC, ratchets, Links,
Resources and multi-node behavior, plus the independent LXMF corpus.
