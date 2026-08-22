# Durable LXMF store

`reticulum-lxmf-store` is the allocation-free, append-only NOR owner for parsed
normalized LXMF messages. Every record is bound to the physical device, flash
range, stable message handle, signature-input metadata, exact wire digest, and
a commit-last footer. The caller supplies the bounded in-memory index.

The physical format retains the complete eight-byte Reticulum interface
identity for the immutable first arrival and either both RSSI/SNR values or
neither. It also records Python LXMF's signature state: validated, source
unknown, or invalid. That evidence describes the receiver-local final hop and
may therefore measure a relay. Replay keeps the first durable observation.

The store owns mount, lookup, chunked wire reads, idempotent commit, and
fail-closed media validation. It does not own Reticulum routing, client
collection watermarks, deletion, compaction, or retention policy. Current E290
partition ownership is documented in
[`partitions/README.md`](../../partitions/README.md).

The reader accepts only the current physical format. Format changes require an
explicit appliance reprovision.

```sh
cargo test --locked -p reticulum-lxmf-store
cargo clippy --locked -p reticulum-lxmf-store --all-targets -- -D warnings
```
