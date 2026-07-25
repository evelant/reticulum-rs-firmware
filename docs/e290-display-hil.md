# E290 display HIL powered qualification

## Result

The isolated E290 display HIL completed its controller lifecycle on Board A and
the retained demo passed visual inspection on 2026-07-25.

| Gate | Result | Evidence |
| --- | --- | --- |
| Board identity | PASS | USB serial `AC:A7:04:E1:3E:88`, eFuse MAC `ac:a7:04:e1:3e:88`, 16 MiB flash |
| Locked release target build | PASS | text 85,961 B; data 8,772 B; BSS 464,056 B; total 558,789 B; application image 118,464 B |
| Controller initialization | PASS | serial completion in 12 ms |
| Boot clear | PASS for control flow | serial completion in 1,557 ms; the transient optical clear was not separately observed |
| Fixed demo refresh | PASS | serial completion in 1,557 ms, followed by operator-confirmed visible output |
| Visual output | PASS | operator reported correct text, layout, black/white polarity, and landscape orientation |
| Shutdown | PASS | serial completion reported SSD1680 deep sleep followed by GPIO18 low |
| Radio interlock | PASS for firmware control path | SX1262 reset remained low and NSS remained high; no radio owner was constructed |

The retained screen is a fixed, explicitly non-secret demonstration containing
`PAIR 123456`; it is not a live pairing credential. The visual PASS applies to
that final retained demo. It does not claim that the short boot-clear interval
was optically witnessed, nor does it qualify partial refresh, repeated sleep/
wake cycles, production display content, or the permanent node's optional
display actor. A later integrated startup proof is recorded separately below.

## Powered trace

The release image enabled the switched display rail on GPIO18, initialized the
SSD1680, wrote a white full frame, wrote the fixed demo full frame, entered
deep-sleep mode 1, and drove GPIO18 low. Its bounded BUSY owner returned
successfully for each controller wait. The recorded durations were:

```text
controller-initialize  12 ms
boot-clear           1557 ms
demo-refresh         1557 ms
```

After completion, the e-paper panel retained the demo without controller
power. The operator then confirmed that the complete demo appeared correctly.
The firmware's `rf_inert=true` trace and retained GPIO owners establish the
intended SX1262 reset-low/NSS-high software state throughout this HIL; this was
not a separate electrical probe measurement.

## Integrated BLE plus display follow-up

On 2026-07-25, a powered integrated startup diagnostic exercised the permanent
display actor with the real BLE controller on Board A. The first ownership
order initialized SPI3/e-paper before the first esp-radio/PHY calibration. It
stalled in `esp_phy::enable_phy` registration/calibration, leaving the retained
`STARTING` view because the blocked startup could not let the display actor
consume `Ready`.

The controlled A/B constructed and retained the real `BleConnector` immediately
after RTOS startup and before SPI3/e-paper initialization. That boot passed
display boot clear, BLE advertising, the exact `Ready` rendered-completion
gate, the visually confirmed retained `READY` view, and composition readiness.
This establishes a peripheral-startup ownership/order invariant, not a
framebuffer, PSRAM, or internal-heap ceiling. The connector must remain owned
and later move into the BLE task; a disposable PHY warm-up is not the product
boundary.

This follow-up qualifies one integrated startup, not repeated resets,
sleep/wake cycles, live pairing content, concurrent-load behavior, pressure,
or soak. The isolated HIL table and runbook above remain the qualification
record for the display-only controller lifecycle.

## Reproduction runbook

Use the board's USB serial descriptor and eFuse MAC as identity. Do not infer
Board A from a changing `/dev/cu.*` enumeration order.

1. Put Board A in the ROM loader, resolve its current port as `PORT`, and
   recheck its identity before writing:

   ```sh
   espflash board-info --chip esp32s3 --port "$PORT" \
     --after no-reset --non-interactive --skip-update-check
   ```

   Require eFuse MAC `ac:a7:04:e1:3e:88`, 16 MiB detected flash, disabled
   secure boot, and disabled flash encryption.

2. Build the exact release image with the locked dependency graph:

   ```sh
   source "$HOME/export-esp.sh"
   cargo +esp build --locked --release \
     -p reticulum-heltec-vision-master-e290-display-hil \
     --target xtensa-esp32s3-none-elf

   ELF=target/xtensa-esp32s3-none-elf/release/reticulum-heltec-vision-master-e290-display-hil
   ```

3. Flash the identified 16 MiB board:

   ```sh
   espflash flash --chip esp32s3 --port "$PORT" \
     --flash-size 16mb \
     --partition-table partitions/heltec-vision-master-e290-node.csv \
     --skip-update-check "$ELF"
   ```

   Although this image never mounts product storage, it is flashed onto a
   product board and therefore uses the canonical 16 MiB product partition map.
   Do not rely on the workspace runner's 8 MiB default or an implicit espflash
   table.

4. Attach the serial monitor to the re-enumerated port and reset once for a
   complete trace:

   ```sh
   espflash monitor --chip esp32s3 --port "$PORT" \
     --elf "$ELF" --skip-update-check
   ```

   Require PASS records for boot, display power, controller initialization,
   boot clear, demo refresh, and completion. Completion must report controller
   deep sleep, display power low, the fixed non-secret retained view, and
   `rf_inert=true`. Reject any FAIL, panic, BUSY timeout, unexpected reset, or
   identity mismatch.

5. Visually inspect the final retained demo. Confirm the border and text are
   complete, landscape orientation is correct, and foreground/background
   polarity is correct. Record the boot clear separately only if it is
   deliberately observed; the PASS above does not infer that transient visual
   state from the final demo.

This record does not include a source commit, ELF hash, or captured raw serial
artifact identifier. Preserve those bindings in any release-grade repeat.
