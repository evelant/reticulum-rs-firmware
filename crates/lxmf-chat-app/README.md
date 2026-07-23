# Reticulum LXMF chat application services

`reticulum-lxmf-chat-app` is the reusable application layer used by the
long-running host appliance service and intended to replace the equivalent
ordering currently duplicated in the foreground CLI. It connects the
transport-neutral chat store to one authenticated sequential device session
without owning serial discovery, reconnect policy, HTTP, or UI state.

The engine commits outbound material before device I/O, replays exact retained
material after reconnect, projects device status monotonically, and scans the
device inbox one stable summary at a time. Inbox cursors are intentionally
session-local: the current device API has no durable inbox generation marker,
so persisting a handle alone could skip mail after a device-store reset.

Each engine call performs at most one device operation plus its associated
durable local mutation. That stepwise contract lets a foreground client, host
service actor, or later native application supply its own scheduling and
reconnect policy without moving serial, HTTP, or executor dependencies into the
application core. Known inbox message IDs are detected before downloading the
complete normalized wire, so restarting a session can safely rescan summaries.
