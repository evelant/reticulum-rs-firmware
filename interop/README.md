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

## Released Reticulum lane

Create a CPython 3.13.7 environment, install the exact released source revision
and regenerate or check the deterministic foundation corpus:

```sh
python3.13 -m pip install \
  --target artifacts/phase0/rns-1.3.8-python \
  -r interop/python/requirements-rns-1.3.8.txt
PYTHONPATH=artifacts/phase0/rns-1.3.8-python \
  PYTHON=python3.13 \
  cargo run --locked -p xtask -- check-rns-vectors
```

The committed corpus deliberately excludes generated ciphertext: Reticulum
uses a fresh ephemeral key and IV, so byte equality would not be reproducible.
Separate semantic tests will decrypt Python ciphertext and encrypt Rust data
for Python as the Link/Resource lanes are added.
