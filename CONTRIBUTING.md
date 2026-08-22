# Contributing

The project builds a standalone Reticulum appliance and its universal Expo
client. Contributions that fit the current architecture are welcome. Read
[`AGENTS.md`](AGENTS.md) first: it records the repository map, the durable
architecture invariants, and the working rules that the maintainers and coding
agents both follow.

## First setup

Clone the repository and verify the pinned toolchains and dependency graph:

```sh
git clone <repository-url>
cargo run --locked -p xtask -- doctor
```

`xtask doctor` checks the toolchain, exact PRNS pin, partition contract, and
required E290 build inputs.
Firmware builds additionally need the Espressif toolchain described in
[Build and flash E290 firmware](docs/getting-started/firmware-e290.md).

## Making a change

1. Open an issue or comment on an existing one describing the intended change
   before starting on anything large.
2. Keep one behavior in its owning package. Prefer a module or test over a new
   crate; a new crate must represent a durable ownership, portability, or
   dependency boundary, not a milestone.
3. Rust is the source of truth for shared API types. When a DTO or native
   callable changes, regenerate and commit the artifacts described in
   [`AGENTS.md`](AGENTS.md#generated-code).
4. Add or update doc comments where ownership, durability, timing, protocol, or
   hardware behavior is not obvious from the types.

## Verification

Run the smallest relevant gate while iterating, then the full set before
opening a pull request:

```sh
cargo fmt --all -- --check
RUST_MIN_STACK=16777216 cargo test --locked
RUST_MIN_STACK=16777216 cargo test --locked -p reticulum-e290-firmware --lib
cargo clippy --locked --all-targets -- -D warnings

cd clients/appliance
bun install --frozen-lockfile
bun run verify
```

Protocol or wire changes also require the isolated Python authority suites in
[`interop/README.md`](interop/README.md). Firmware changes require the E290
build and ELF checks in
[`docs/development/verification.md`](docs/development/verification.md). The
`host` and `firmware` CI jobs run these gates; make sure the pull request keeps
both green.

## Ownership of Reticulum behavior

The product consumes the exact unmodified PRNS revision pinned in the
workspace. Adapt product applications and board composition to its public APIs
first. Change PRNS only for a demonstrated generic gap useful to unrelated
Reticulum applications or boards, qualify that change against Python RNS, and
then update the exact workspace revision.

## Pull requests

- Keep commits focused and self-contained.
- Reference the issue the change addresses.
- Do not hand-edit generated TypeScript, UniFFI, C++, Kotlin, Objective-C++,
  CMake, Gradle, podspec, framework, JNI, or embedded web output.
- The maintainers review changes for fit with the documented architecture and
  the security and durability invariants, not just for correctness.
