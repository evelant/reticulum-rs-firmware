# LXMF chat core

`reticulum-lxmf-chat-core` is the persistence-facing domain core for the host
appliance and future desktop or mobile clients. It owns contacts, inbound LXMF
deduplication, commit-before-send outbox records, device acceptance identifiers, durable
status projection, conversation timelines, and restart reconciliation.

The crate deliberately has no device transport, UI, async runtime, or
Reticulum implementation dependency. `ChatStore` is the database-neutral
adapter boundary. `MemoryChatStore` is the executable reference implementation
and can export and reopen an opaque `MemoryImage` for restart tests.

The default `sqlite` feature provides `SqliteChatStore` using exactly
`rusqlite 0.40.1` and bundled SQLite. It creates and versions its own schema,
and every mutation is a database transaction. Disable default features to use
only the domain and in-memory layers. The root workspace manifest and lockfile
pin the adapter and bundled SQLite dependency for reproducible client builds.

SQLite schema 2 adds a singleton authenticated device binding: device ID,
primary Reticulum destination, and local `lxmf.delivery` destination. Binding
an unbound database is transactional; a different observed identity is rejected
without mutation. Schema-1 databases migrate to schema 2 as **unbound** because
their existing rows contain no authoritative device identity. The service
adapter performs binding after authentication; callers that use the store
directly must enforce that step themselves.
