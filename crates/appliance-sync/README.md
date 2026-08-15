# Appliance synchronization

`reticulum-appliance-sync` contains application services shared by the host
and native clients. `ChatEngine` reconciles a durable client store with one
authenticated device session: it submits pending outbox material, imports and
acknowledges contiguous inbox messages, and maps device submission state into
the local model.

The engine performs bounded steps and owns no executor, connection discovery,
backoff loop, UI, or platform API. Those policies belong to
`reticulum-appliance-runtime` and its connector. The appliance remains the
authority for retry after it accepts a message.

```sh
cargo test --locked -p reticulum-appliance-sync
cargo clippy --locked -p reticulum-appliance-sync --all-targets -- -D warnings
```
