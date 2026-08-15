# Appliance store

`reticulum-appliance-store` defines the client-side durable domain model and
storage boundary. It covers device binding, contacts, conversations, inbound
messages, durable outbox rows, submission state, activity events, phone
location observations, and packet-correlated RF traces.

The default implementation is SQLite; an in-memory implementation supports
tests. Both preserve the same identity binding and idempotency rules. Device
mailbox collection state and human read state remain distinct. The board owns
carrier retries; an explicit app retry replaces only a retryable terminal
device submission while preserving the signed LXMF material and message
identity.

The current SQLite schema is 10 and the test-only portable memory image schema
is 9. Only the current SQLite schema and an empty, unversioned database are
accepted; another schema is rejected without mutation. Schema changes must be
tested across reopen and coordinated with the native profile contract when
mobile builds persist the affected data.

```sh
cargo test --locked -p reticulum-appliance-store
cargo clippy --locked -p reticulum-appliance-store --all-targets -- -D warnings
```
