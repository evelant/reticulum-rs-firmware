# ADR 0022: Durable local message notifications and appliance indicator

- **Status:** accepted; foreground/resume phone notifications and the durable
  appliance indicator are the first implementation slice, while native
  background BLE wake remains deferred
- **Date:** 2026-07-30
- **Extends:** [ADR 0015](0015-universal-expo-client-and-generated-bindings.md),
  [ADR 0019](0019-secure-ble-appliance-onboarding.md), and
  [ADR 0020](0020-wifi-station-reticulum-tcp-border-interface.md)

## Context

An appliance can receive and durably retain LXMF while its controlling phone is
locked, disconnected, or absent. A useful messenger must make that arrival
visible without weakening the existing exactly-once storage boundary or
pretending that a suspended React Native runtime can continuously service BLE.

Two states are related but not interchangeable:

- **uncollected** means a durable appliance message has not yet been durably
  imported by the controlling app;
- **unread** means an app-local imported message has not yet been viewed by the
  user.

Clearing one state must not silently claim the other. In particular,
background import may clear an appliance's uncollected count without claiming
that a human read the conversation.

The e-paper panel is also a shared output. Boot, pairing, recovery, and ordinary
home telemetry must not race independent physical publishers or allow a normal
message update to overwrite a pairing passkey.

## Decision

### Derive appliance state only from durable, novel ingress

Only a successful `DurableIngressCommitKind::New` advances appliance
notification state. Replays and ambiguous retries never increment the count.

The appliance persists an acknowledged-through LXMF handle and validates it
against the mounted message-store incarnation. Its public status contains the
latest committed handle, the acknowledged-through handle, and the bounded
uncollected count. A monotonic acknowledge request is idempotent, rejects
regression or unknown handles, and is exposed through the bearer-neutral
authenticated device API.

When notification state is introduced over an existing nonempty LXMF store, the
first valid state baselines at the current latest handle. A firmware upgrade
therefore does not present already-imported historical messages as newly
arrived.

The client acknowledges only after the corresponding LXMF message is durably
present in its local store. It batches the highest safely imported cursor and
retries the same monotonic acknowledgement after an ambiguous transport
failure.

### Give the display one semantic coordinator

The Home snapshot contains a saturating uncollected-message count. The E290
renders a compact `NEW n` or `NEW 99+` badge without sender or message content.
The count is reconstructed from durable state at boot.

One display coordinator owns the complete desired Home projection. Pairing and
recovery views have priority over normal count changes. Ordinary updates are
deduplicated and burst-coalesced to avoid unnecessary full e-paper refreshes;
the transition from zero to nonzero and the return to zero remain prompt.
Restoring Home after a temporary pairing view uses the newest count rather than
the boot-time copy.

### Reconcile phone notifications from durable app activity

The app treats its durable inbound-import activity journal as the notification
source. A per-profile watermark and the LXMF message ID prevent duplicate
notifications across polling, reconnect, profile switching, and process
restart. First enablement baselines existing history instead of flooding the
phone with notifications for old conversations.

Expo owns permission UX, Android notification-channel configuration, foreground
presentation, and tap handling. A notification carries the appliance profile,
remote peer, and message ID needed to activate the correct profile and open the
conversation. Content previews remain optional; generic text is safe when the
full message is not yet available.

This first slice is explicitly foreground/resume reconciliation. It must not be
described as reliable locked-phone delivery.

### Add a native background wake path separately

Reliable local notification while JavaScript is suspended requires a native
platform owner below the current foreground BLE pump. A later slice will add a
bonded, metadata-only mailbox-change signal, iOS Core Bluetooth restoration and
AccessorySetupKit onboarding, and Android companion-device lifecycle support.
The event carries only a version, appliance/store incarnation, latest cursor,
and count; message content remains in the authenticated LXMF synchronization
path.

If the phone is outside BLE range, the appliance retains its indicator and the
app reconciles on reconnect. Immediate remote notification over the Internet
would require a separately optional APNs/FCM-compatible relay carrying an
opaque signed wake event, not provider credentials embedded in firmware.

## Consequences

- A message arriving over LoRa, TCP, or a future Reticulum interface follows
  the same notification boundary.
- Replayed LXMF cannot create a new appliance badge or duplicate phone
  notification.
- The display remains useful without a phone and does not expose private
  message text.
- App synchronization and human read state remain independently truthful.
- Multiple known appliances eventually require lightweight native watchers
  independent of the one active full chat session.
- User force-quit, disabled Bluetooth, and out-of-range operation cannot promise
  an immediate local phone alert; durable reconciliation remains the fallback.

## Deferred decisions

- per-contact mute, notification preview, sound, and quiet-hour policy;
- per-principal appliance acknowledgement if multiple simultaneous controlling
  phones become a supported product mode;
- app-icon and per-conversation unread badges;
- native background BLE lifecycle and multi-appliance watchers;
- optional Internet push relay; and
- e-paper partial-refresh qualification and long-term ghosting policy.
