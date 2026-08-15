# Firmware guide

`firmware/e290` is the supported firmware composition. Its profiles are:

- `gateway` (default): the complete LoRa, BLE, display, Wi-Fi station, and
  Reticulum TCP appliance.
- `appliance`: the LoRa, BLE, and display appliance without the TCP uplink.

The full image targets the E290-HF with 16 MiB flash and at least 8 MiB mapped
PSRAM. USB Serial/JTAG is diagnostics-only; keep firmware logging
enabled unless a measured reliability or performance problem requires changing
it.

## Design rules

- Keep board initialization and pin ownership in the firmware/board layer;
  keep Reticulum, LXMF, routing, storage, and device-API behavior portable.
- Treat LoRa and TCP as independent Reticulum packet interfaces. Do not route
  based on assumptions that every packet uses LoRa.
- The board owns durable outbound retries and remains useful without an app.
- Preserve sole ownership of the radio, flash, display, BLE session, Wi-Fi
  station, and TCP actor. Never hide backpressure or owner loss behind retries.
- Use PSRAM for suitable resident state, but keep interrupt, DMA, cache-off,
  controller, and active stack requirements in internal memory.
- Keep RF profile, antenna, fitted HF radio, and regional-frequency assumptions
  explicit. Use an appropriate connected antenna whenever the image can
  transmit.
- Put reusable tests in the owning crate or firmware module. Do not create
  phase-specific firmware, HIL crates, proof binaries, or evidence machinery.

## Build and flash

Load the Espressif environment in each new shell:

```sh
source "$HOME/export-esp.sh"
```

Build and package the default gateway image from the repository root:

```sh
cargo run --locked -p xtask -- doctor
cargo run --locked -p xtask -- build
cargo run --locked -p xtask -- check-elf
cargo run --locked -p xtask -- package --output target/e290-gateway.bin
```

For the LoRa/BLE-only image, pass `--profile appliance` to `build` and
`package`, and write it to `target/e290-appliance.bin`. The maintained `xtask`
surface is `doctor`, `build`, `package`, and `check-elf`.

Select the intended serial port deliberately. Package with the explicit E290
flash mode, frequency, size, crystal, application partition, and checked-in
partition table from the guide; select the same partition and geometry for a
direct ELF flash. Do not use generic `cargo run`, implicit `espflash` defaults,
or `--all-features` for a firmware image.

Run the portable firmware tests explicitly with
`RUST_MIN_STACK=16777216 cargo test --locked -p reticulum-e290-firmware --lib`.
A successful host test is not a substitute for an Xtensa build or powered
verification when hardware behavior changes.
