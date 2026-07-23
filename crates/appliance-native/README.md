# Reticulum appliance native boundary

This host-only crate is the Rust source of truth for the Expo application's
native callable surface. It is deliberately separate from firmware dependency
graphs. Its contract query binds a compiled mobile application to the exact
device-API version and bounds it understands, while `NativeAppliance` owns the
transport-neutral durable chat runtime and its SQLite database.

The selected integration is UniFFI `0.31.0` through the pinned
`uniffi-bindgen-react-native` Expo TurboModule. Android and iOS development
builds compile this crate and have executed its immutable contract query.
Platform packaging belongs to the Expo client, while this crate remains
independent of React Native, iOS, Android, and application UI lifecycles.
Contacts, timelines, and idempotent outbox writes work offline immediately.
USB serial/JTAG, USB OTG, BLE, and Wi-Fi remain explicit unavailable connector
stubs: their stable variants and errors reserve the boundary without claiming
that a bearer works or silently selecting another one. Future adapters should
compose the existing portable device-client crates here rather than
reimplementing framing, authentication, or LXMF wire handling in TypeScript,
Swift, or Kotlin.
