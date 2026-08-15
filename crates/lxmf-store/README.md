# Durable LXMF store

`reticulum-lxmf-store` is the allocation-free, append-only physical owner for
validated normalized LXMF messages. It binds every record to the physical
device, partition range, stable handle, authenticated metadata, exact wire
digest, and commit-last footer. A caller supplies the bounded in-memory index.
The store does not own Reticulum routing, client collection state, deletion,
compaction, or retention policy.

## Physical format 2

Physical format 2 adds optional immutable first-arrival evidence in bytes that
format 1 reserved inside each extent header:

- one device-local ingress interface ID; and
- either both whole-unit RSSI/SNR values or neither value.

The header digest and record digest cover this evidence. It describes the
receiver-local final hop into the appliance and may therefore measure a relay.
A stream interface can identify itself without inventing radio signal values.

Current firmware mounts valid format-1 and format-2 records in the same store.
Legacy records return no ingress observation; new commits use format 2. Replay
keeps the first retained observation and does not rewrite an older record.

This compatibility is forward-only. After format-2 firmware appends any record,
format-1 firmware cannot safely mount the mixed store because it does not
understand the newer record version. Do not roll back to a format-1 image while
preserving such a store. Recovery requires a forward-compatible image or an
explicit backup-and-reprovision procedure; there is no in-place downgrade
migration.

The format change does not make signal an end-to-end measurement, add outbound
telemetry, or retrofit evidence onto existing records.
