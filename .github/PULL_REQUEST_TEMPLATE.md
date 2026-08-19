## Description

What this change does and why.

## Scope

- [ ] Portable protocol, routing, storage, or client behavior
- [ ] E290 firmware or board composition
- [ ] Expo client or native bridge
- [ ] Documentation only

## Compatibility

Note any persisted-format, device API, or generated-binding changes, and
whether they require fresh provisioning or an explicit migration.

## Verification

Commands run:

```text
cargo fmt --all -- --check
RUST_MIN_STACK=16777216 cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
# and, for the app:
# bun install --frozen-lockfile && bun run verify
```

Reference any interoperability or firmware gates that apply. Keep commits
focused and do not hand-edit generated output.
