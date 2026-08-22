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

The current SQLite schema is 13 and the test-only portable memory image schema
is 9. An empty, unversioned database is initialized at the current schema.
Older and future schemas are rejected without mutation. Schema changes must be
tested across reopen and coordinated with the native profile contract when
mobile builds persist the affected data. The alpha schema-13 boundary adds the
terminal application-delivered lifecycle value; existing app data must be
reset instead of being mounted under the wider constraint.

```sh
cargo test --locked -p reticulum-appliance-store
cargo clippy --locked -p reticulum-appliance-store --all-targets -- -D warnings
```
