# E290 rollback bootloader

The bootloader embedded by `espflash 4.5.0` was built with ESP-IDF defaults.
ESP-IDF disables application rollback by default, so that binary cannot satisfy
the OTA unhealthy-image gate.

This small recurring build project produces an ESP32-S3 second-stage bootloader
with `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y`. The container helper pins the
official ESP-IDF v5.5.5 image by architecture-specific digest and writes all
generated files beneath `target/e290-bootloader`:

```sh
firmware/e290/bootloader/build-container.sh
```

The packaging command requires
`target/e290-bootloader/bootloader/bootloader.bin` and passes it explicitly to
`espflash`. The dummy application in this directory exists only because the
ESP-IDF project generator configures a complete project before building its
`bootloader` target; it is never packaged or flashed.

The application confirms a `PendingVerify` image only after PRNS composition,
authorization seeding, service announcement, and a 30-second product-loop
health window. It commits `Valid` through ESP-IDF's existing `otadata` record
and reads it back. A powered test must still show that a reset or power loss
before confirmation selects the previous valid slot. Configuration and a
successful host build are not substitutes for that evidence.
