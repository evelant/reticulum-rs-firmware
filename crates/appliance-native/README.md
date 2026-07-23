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
USB serial/JTAG, USB OTG, and BLE remain explicit unavailable connector stubs:
their stable variants and errors reserve the boundary without claiming that a
bearer works or silently selecting another one. `NativeAppliance::open_wifi`
is the first real connector. It loads an app-private activated credential,
opens a finite-timeout raw TCP stream, and delegates framing, the
Wi-Fi-transcript-bound suite-2 handshake, logical device operations, and LXMF
validation to the portable Rust client crates. The proof suite provides
authentication and integrity, not confidentiality. Future adapters should
preserve this composition rather than reimplementing protocol or LXMF handling
in TypeScript, Swift, or Kotlin.
