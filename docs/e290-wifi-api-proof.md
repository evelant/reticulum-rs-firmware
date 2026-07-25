# E290 Wi-Fi local-API proof profile

**Status:** opt-in, build-qualified, and safely flashed to one credentialed
E290. The exact post-format image was written without erasing product data and
the complete durable control range compared byte-for-byte before and after.
The historical 2026-07-23 image made USB disappear after the deliberate reset;
that observation alone did not prove that Wi-Fi initialized. Current source
instead retains native USB Serial/JTAG electrically and at runtime as a
diagnostics-only sink, while the ordinary production `esp-println` backend
remains no-op and emits nothing. SoftAP association, DHCP, authenticated TCP
exchange, reconnect behavior, and powered memory headroom remain open for a
manually operated client.

## Purpose and boundary

`wifi-api-proof` adds the first concrete wireless carrier for the local RDA1
device API. It does not add a Wi-Fi Reticulum packet interface. LoRa remains
the sole Reticulum interface, and the node, durable LXMF, inbox, routing, and
radio tasks are unchanged.

The proof image deliberately chooses exactly one local API bearer:

- the ordinary image runs the existing USB Serial/JTAG bearer;
- the Wi-Fi profile runs the local API over Wi-Fi while retaining native USB
  as an electrically/runtime-available, silent diagnostics-only sink; and
- the separately enabled BLE proof profile follows the same one-bearer
  replacement rule, while other local API carriers remain future work.

This replacement rule avoids two independent session-epoch allocators sharing
the current reply-routing namespace. It also keeps a Wi-Fi initialization
failure local to the detached API owner: LoRa and the autonomous node have
already been spawned and remain resident. Every ordinary production profile
keeps `esp-println` on its no-op backend. Only a separately named diagnostic
image may emit USB Serial/JTAG diagnostics.

## Fixed proof profile

| Property | Value |
|---|---|
| mode | WPA2-Personal ESP32-S3 SoftAP |
| SSID | `reticulum-e290-` plus the final three eFuse MAC bytes in lowercase hex |
| development passphrase | `reticulum-e290-dev` |
| channel | 6 |
| admitted stations | 1 |
| gateway | `192.168.4.1/24` |
| address service | DHCPv4 via `edge-dhcp` |
| API carrier | raw TCP |
| API endpoint | `192.168.4.1:29716` |
| application framing | unchanged RDA1 framed byte stream |
| session binding | Wi-Fi qualification suite 2 |
| concurrent API clients | 1 |
| TCP RX/TX buffers | 1,536 bytes each |

Port 29716 is intentionally not the conventional RNode TCP port. This endpoint
is a device-management and application API; it is not a raw Reticulum or RNode
interface.

The implementation follows the APIs in the official `esp-radio` 0.18.0
Embassy access-point example and pins its compatible network family:
`esp-radio` 0.18.0, `embassy-net` 0.8.0, `edge-dhcp` 0.7.0, `edge-nal` 0.6.0,
and `edge-nal-embassy` 0.8.1. These dependencies are target-only and optional,
so the default LoRa/USB graph is unchanged.

## Credential prerequisite

The Wi-Fi proof has no initialization or live-pairing ceremony. Before flashing
it, provision an Active client credential with the ordinary USB image. The
credential journal remains in its existing flash partition across the profile
change. The client must then use the same credential through the Wi-Fi-bound
session suite; a USB-suite transcript is rejected by construction.

If media is erased or no Active credential exists, the Wi-Fi profile cannot
repair that state. Reflash the ordinary USB image, initialize/pair there, and
then return to the Wi-Fi profile.

## Security scope

RDA1 suite 2 authenticates the client and provides transcript/record integrity,
but it does not encrypt content at the application layer. The proof SoftAP uses
WPA2-Personal so the firmware cannot accidentally expose an open radio link.
Its fixed development passphrase is intentionally published above, however,
so it is an operator convenience and baseline link guard rather than a durable
secret. Anyone holding that passphrase can join the local network, while a peer
without the separate RDA1 client credential still cannot construct an accepted
API request. This is a development qualification profile, not a production
wireless security posture.

Production work needs per-device Wi-Fi credential provisioning, a reviewed
application-confidentiality design, retry/rate controls, and a wireless-safe
pairing or transfer ceremony. A SoftAP passphrase alone does not replace
end-to-end application confidentiality.

## Build

With the installed esp-rs environment loaded:

```sh
source "$HOME/export-esp.sh"
CARGO_TARGET_DIR=target/e290-wifi-api-proof \
  cargo +esp build --locked --release \
  -p reticulum-heltec-vision-master-e290-node \
  --bin reticulum-heltec-vision-master-e290-node \
  --no-default-features \
  --features wifi-api-proof \
  --target xtensa-esp32s3-none-elf
```

The strict target review uses the same arguments with `clippy` and
`-- -D warnings`.

## 2026-07-23 safe-flash checkpoint

The post-format release ELF had SHA-256
`bc43fdb037ed0b71ba9f278564155d2ee52540f92d6c888f2d561a8c559b0c8b`.
Its application image occupied 1,155,744 bytes in the 6 MiB factory slot. The
corresponding unpadded 16 MiB package ended at 1,221,280 bytes, below the
`0x610000` product-data boundary.

The target was the credentialed E290 with USB serial
`AC:A7:04:E1:3F:88` and eFuse MAC `ac:a7:04:e1:3f:88`. The flash operation
wrote only the bootloader, partition table, and factory application. A
separate 131,072-byte readback covering `0x610000..0x630000` compared exactly
before and after the write; that range includes the product identity,
announce-clock, API-credential, and configuration partitions. Secure boot and
flash encryption were disabled on this development board.

After a deliberate hard reset, the target's USB Serial/JTAG port disappeared
while the peer E290 remained enumerated. That was consistent with this
historical image's USB quarantine, but it was not evidence that the SoftAP,
DHCP server, TCP listener, or LoRa/Wi-Fi coexistence reached a healthy steady
state. It does not describe current source, which retains the USB peripheral
but keeps the ordinary production logger silent.

The development Mac's sole internet uplink is its Wi-Fi interface. Joining the
E290 would sever the active development session, so the powered network
exchange was intentionally deferred rather than treating a connectivity loss
as a useful test result.

## Manual powered qualification still required

A useful first hardware pass should prove, in order:

1. the Wi-Fi image boots LoRa and exposes `reticulum-e290-e13f88`;
2. a phone or computer obtains a lease and reaches `192.168.4.1:29716`;
3. fragmented RDA1 handshake and logical API requests authenticate under suite
   2;
4. a request crosses the existing node handoff and its response survives
   partial TCP writes;
5. disconnect/reconnect allocates a fresh session epoch without disturbing
   autonomous LoRa receive/forwarding; and
6. stack, internal heap, PSRAM use, and Wi-Fi/LoRa coexistence remain bounded.

At the time of this powered profile record, the Expo native bridge required a
manually configured endpoint and manually seeded RDA1 credential. Current
source now adds a host-tested, create-only system-file import into the native
app sandbox, but that later work does not retroactively qualify this proof.
SoftAP joining, the Android/iOS import lifecycle, and a powered Wi-Fi E290
exchange remain unqualified.
