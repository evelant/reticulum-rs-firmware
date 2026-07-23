# Reticulum appliance native boundary

This host-only crate is the Rust source of truth for the Expo application's
native callable surface. It is deliberately separate from firmware dependency
graphs and starts with a read-only contract query that binds a compiled mobile
application to the exact device-API version and bounds it understands.

The selected integration is UniFFI `0.31.0` through the pinned
`uniffi-bindgen-react-native` Expo TurboModule. Android and iOS development
builds compile this crate and have executed its immutable contract query.
Platform packaging belongs to the Expo client, while this crate remains
independent of React Native, iOS, Android, BLE, and application UI lifecycles.
Later native client/session operations should compose the existing portable
device-client crates here rather than reimplementing framing, authentication,
or LXMF wire handling in TypeScript, Swift, or Kotlin.
