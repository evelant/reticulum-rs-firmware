# Appliance runtime

`reticulum-appliance-runtime` is the long-running, transport-neutral client
actor shared by native and host applications. It exclusively owns one store
and authenticated session, serializes mutations, reconnects through a supplied
`Connector`, reconciles the outbox and inbox, imports radio traces, and
publishes immutable UI projections.

Connectors open identified PRNS Links and return management-destination
metadata plus any guard that must live for the authenticated session.
Retryable, unavailable, and permanent connection failures remain distinct.
The runtime does not model the packet interface selected by PRNS as a separate
product connection.

Scheduling is bounded and fair: foreground commands cannot starve inbox,
submission, or trace work; a transient API `RetryLater` backs off only the
contended lane while preserving the authenticated session. Once the appliance
accepts a message, this runtime polls its existing submission rather than
creating another one. Autonomous delivery retry belongs to firmware.

```sh
cargo test --locked -p reticulum-appliance-runtime
cargo clippy --locked -p reticulum-appliance-runtime --all-targets -- -D warnings
```
