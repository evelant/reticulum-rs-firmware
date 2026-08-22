# E290 flash layout

[`e290.csv`](e290.csv) is the 16 MiB partition table for the supported E290
firmware. Two equal OTA application slots are followed by one generic product
arena and one PRNS-owned protocol arena. Enabling another Reticulum application
does not change this physical layout.

| Partition | Range | Size | Owner |
| --- | --- | ---: | --- |
| `nvs` | `0x009000..0x00f000` | 24 KiB | ESP NVS reserve |
| `phy_init` | `0x00f000..0x010000` | 4 KiB | ESP PHY reserve |
| `ota_0` | `0x010000..0x510000` | 5 MiB | firmware slot A |
| `ota_1` | `0x510000..0xa10000` | 5 MiB | firmware slot B |
| `otadata` | `0xa10000..0xa12000` | 8 KiB | ESP OTA boot selection |
| `product_state` | `0xa12000..0xe80000` | 4.43 MiB | one product-owned application arena |
| `prns_state` | `0xe80000..0x1000000` | 1.5 MiB | PRNS routes, ratchets, timebase, and journal |

The physical table deliberately does not allocate a partition per app. The
product store currently assigns these typed quotas inside `product_state`:

| Product quota | Range | Size | Purpose |
| --- | --- | ---: | --- |
| identity | `0xa12000..0xa14000` | 8 KiB | mirrored Reticulum identity |
| network configuration | `0xa14000..0xa16000` | 8 KiB | radio, Wi-Fi, TCP, and feature configuration |
| management authorization | `0xa16000..0xa18000` | 8 KiB | mirrored Reticulum identity allow-list |
| LXMF mailbox state | `0xa18000..0xa1a000` | 8 KiB | mirrored collection watermark |
| LXMF outbound intent | `0xa1a000..0xa5a000` | 256 KiB | 64-record durable retry queue |
| appliance settings | `0xa5a000..0xa5c000` | 8 KiB | mirrored product label and revision |
| unassigned registry | `0xa5c000..0xa80000` | 144 KiB | future typed product quotas |
| LXMF payload log | `0xa80000..0xe80000` | 4 MiB | initial durable messaging allocation |

These ranges are a resettable product-store format, not bootloader partitions
or a promise that LXMF always owns a fixed board partition. A future generic
application allocator can revise the quotas without creating physical layouts
for every application combination.

Current on-device format versions are:

| State | Physical format | Semantic schema |
| --- | ---: | ---: |
| node identity | 1 | — |
| network configuration | 1 | 7 |
| management authorization | 1 | — |
| appliance settings | 1 | — |
| LXMF store | 5 | — |
| PRNS state | PRNS 0.3.6 flash journal | — |

The management allow-list holds at most eight complete Reticulum identity
hashes. It uses two commit-last mirrored sectors and is replayed into PRNS's
ordinary request-handler allow-list at boot; it is not a bearer credential or
a second live authorization gate. The LXMF store retains optional immutable
receiver-local ingress interface and
RSSI/SNR evidence, PRNS's complete eight-byte interface identity, and Python
LXMF-compatible signature state (`validated`, `source unknown`, or `invalid`).
Earlier formats are rejected after the migration reset. Constants and codecs
in the owning crates remain authoritative for byte formats; this table defines
ranges and ownership.

## Fresh provisioning

The firmware does not migrate an earlier partition or product-state layout.
Fully erase a board before its first installation of this image, or whenever
its installed layout or formats are unknown:

```sh
espflash erase-flash --chip esp32s3 --port PORT \
  --after no-reset --non-interactive --skip-update-check
```

Then flash the current merged image as described in the
[firmware guide](../docs/getting-started/firmware-e290.md). Erasing flash
removes the node identity, all product/application data, network
configuration, and PRNS route and ratchet state. The app will see a new
Reticulum identity.

Routine merged-image flashing starts at address zero and ends before product
state because packaging uses `--skip-padding`. Do not erase state during an
ordinary firmware update. When changing the CSV, update the firmware partition
contract and this document in the same change.
