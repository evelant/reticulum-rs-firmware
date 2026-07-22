# Local esp-rtos 0.3.0 patch

This directory is the published `esp-rtos` 0.3.0 crate with two local,
equivalent stack-unit corrections in `src/lib.rs`. The original crate is
dual-licensed MIT OR Apache-2.0. Workspace formatting also normalized imports
in three source files and removed one trailing space from a `src/lib.rs` doc
comment; those four formatting-only changes are recorded so the checked vendor
tree can still be reconstructed byte-for-byte to the published crate.

- crates.io version: `0.3.0`
- crates.io archive SHA-256: `551f90766e1527edaa0c91e8d559e9e2a60397b545e93357ac61fb31845e5712`
- upstream repository: <https://github.com/esp-rs/esp-hal>
- upstream source commit recorded by the crate: `347003de8a48320bb7724f53045be3afa9204411`
- upstream path: `esp-rtos`

`VENDOR-HASHES.json` records the complete published-crate inventory, the one
intentionally omitted package-local `Cargo.lock`, the project provenance files,
the pristine and patched hashes of all four changed source files, and the exact
six reviewed text replacements. `xtask graph-policy` verifies that inventory,
rejects symlinks or extra files, checks every retained file digest, reverses only
those six edits, and requires every reconstructed source file to match its
pristine registry-crate hash.

## `cpu0-main-stack-slice-uses-word-count`

`start_with_idle_hook()` constructs the main task stack as a
`*mut [MaybeUninit<u32>]`. Upstream 0.3.0 passes the stack's byte length as the
slice element count, creating a fat pointer whose represented range is four
times the linked stack reservation. The local patch changes the length
expression from:

```rust
stack_top as usize - stack_bottom as usize
```

to:

```rust
(stack_top as usize - stack_bottom as usize) / size_of::<MaybeUninit<u32>>()
```

The ESP linker script already aligns both stack symbols to four bytes, so this
is the exact unit correction needed before constructing the CPU0 slice.

## `cpu1-main-stack-slice-uses-word-count`

`start_second_core_with_stack_guard_offset()` receives an `esp-hal`
`Stack<STACK_SIZE>`. `STACK_SIZE` and `Stack::len()` are byte counts, while the
captured slice is again a `*mut [MaybeUninit<u32>]`. Upstream 0.3.0 passes
`STACK_SIZE` directly as the element count. The local patch changes it from:

```rust
STACK_SIZE
```

to:

```rust
STACK_SIZE / size_of::<MaybeUninit<u32>>()
```

`esp-hal` requires the stack size to be a multiple of 16, so division by the
four-byte element size is exact. This is the equivalent CPU1 unit correction.

Remove this vendor directory and restore the crates.io dependency only after a
released `esp-rtos` contains the equivalent fix and the project regression
guard has been updated to recognize that release.
