# Storage HIL dump verifier

This host-only tool verifies an exact 1 MiB raw `retlog` partition dump after
the Heltec Tracker V2 storage HIL completes:

```sh
cargo run -p reticulum-storage-hil-verify -- path/to/retlog-after.bin
```

It fails closed unless the production journal mount and semantic replay APIs
prove bank B generation 2, five committed records in five slots, one accepted
fixture submission at revision 4 in the exact `Delivered` state, and no pending
compaction. It independently checks the fixture's immutable acceptance, packet,
attempt and audit fields, requires the retired bank-A manifest sector to be
fully erased, and requires every byte after the five compacted bank-B records
to remain erased.
