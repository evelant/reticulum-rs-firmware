# Interoperability fixtures

`peers.toml` pins the upstream implementations used to generate and verify
wire behavior. Generated vectors belong under `vectors/` and must include:

- generator source revision;
- command and Python version;
- protocol/release lane;
- whether bytes came from creation, parsing or a captured exchange;
- expected result and any normalization applied.

Do not copy ad-hoc bytes from a moving `master` checkout without recording its
commit. Secrets and real user identities must never enter fixtures.
