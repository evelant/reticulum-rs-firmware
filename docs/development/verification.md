# Verification

The checked [CI workflow](../../.github/workflows/ci.yml) is the complete
machine-readable verification matrix. This guide lists the useful local entry
points without duplicating every package-specific CI command.

## Host and policy checks

From the repository root:

```sh
cargo fmt -- --check
RUST_MIN_STACK=16777216 cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo run --locked -p xtask -- graph-policy
```

`graph-policy` checks feature isolation, dependency provenance, target
composition, generated policy surfaces, and other boundaries that ordinary
Rust compilation cannot express.

Run the E290 flash-helper and documentation policy suite after changing
partition tables, image commands, or flash documentation:

```sh
PYTHONPATH=interop/python \
  python3.13 -m unittest -v interop/python/test_e290_qualification_host.py
```

## Expo client

```sh
cd clients/appliance
bun install --frozen-lockfile
bun run verify
```

`verify` checks dependencies, formatting, TypeScript, tests, generated API
types, and deterministic embedded assets. It does not require native mobile
toolchains.

When both Xcode and Android SDK/NDK toolchains are installed:

```sh
bun run native:verify
```

Use the [app build guide](../getting-started/app.md) for real simulator/device
compilation and installation.

## E290 target

The current turnkey profile has its own host, graph, strict Xtensa Clippy,
release build, and explicit image-packaging gates. Run the exact sequence in
the [E290 firmware guide](../getting-started/firmware-e290.md#2-build-the-appliance-image).

The default, display-only, Wi-Fi proof, runtime measurement, and commit-fault
profiles are separate graphs. Do not use `--all-features`: exceptional HIL
features are intentionally mutually exclusive.

## Interoperability

Released Reticulum and LXMF Python fixtures, deterministic vector generation,
and RNode HIL corpora are documented in
[interop/README.md](../../interop/README.md). Their pinned Python dependency
sets are protocol authorities, not firmware runtime dependencies.

## Powered HIL

Powered tests are not part of ordinary CI. Each runbook records:

- exact source revision and clean-tree requirement;
- firmware artifact and partition-table hashes;
- physical board identity and radio variant;
- transmit profile and antenna assumptions;
- write/readback evidence;
- acceptance criteria; and
- explicit limits of the result.

Use the [qualification index](../README.md#qualification-history) to select the
appropriate runbook. Do not treat an older artifact's powered result as
qualification of the current source.
