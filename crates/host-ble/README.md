# Host BLE transport

`reticulum-host-ble` adapts the appliance GATT profile to one bounded,
synchronous `Read + Write` byte stream on macOS. It owns service-filtered
discovery, exact peripheral selection, connect/discover/subscribe sequencing,
write-with-response fragmentation, indication ordering, finite deadlines, and
clean disconnect.

The crate is a transport adapter only. Authentication, device operations,
credentials, persistence, and reconnect policy remain with the device client
and appliance runtime. Unsupported host platforms do not receive a pretend BLE
implementation.

```sh
cargo test --locked -p reticulum-host-ble
cargo clippy --locked -p reticulum-host-ble --all-targets -- -D warnings
```
