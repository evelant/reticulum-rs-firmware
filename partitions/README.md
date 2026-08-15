# E290 flash layout

[`e290.csv`](e290.csv) is the 16 MiB partition table for the supported E290
firmware. The application image occupies 6 MiB; product state begins at
`0x610000` and is owned by one firmware storage coordinator.

| Partition | Range | Size | Owner |
| --- | --- | ---: | --- |
| `nvs` | `0x009000..0x00f000` | 24 KiB | ESP NVS reserve |
| `phy_init` | `0x00f000..0x010000` | 4 KiB | ESP PHY reserve |
| `factory` | `0x010000..0x610000` | 6 MiB | firmware application |
| `node_identity` | `0x610000..0x612000` | 8 KiB | Reticulum identity mirrors |
| `announce_clock` | `0x612000..0x614000` | 8 KiB | monotonic announce epoch |
| `api_credentials` | `0x614000..0x616000` | 8 KiB | local API credential authority |
| `ble_bond` | `0x616000..0x618000` | 8 KiB | authenticated BLE bond |
| `device_config` | `0x618000..0x630000` | 96 KiB | network config, mailbox watermark, reserved config |
| `node_journal` | `0x630000..0x730000` | 1 MiB | durable outbound submissions |
| `lxmf_store` | `0x730000..0xb30000` | 4 MiB | durable inbound LXMF messages |
| unallocated | `0xb30000..0x1000000` | 4.8125 MiB | future OTA or layout growth |

The `device_config` partition assigns its first 8 KiB
(`0x618000..0x61a000`) to network configuration and its next 8 KiB
(`0x61a000..0x61c000`) to the durable LXMF collection watermark. The remainder
is reserved for future configuration formats.

Current on-device format versions are:

| State | Physical format | Semantic schema |
| --- | ---: | ---: |
| node identity | 1 | — |
| announce clock | 1 | — |
| API credentials | 1 | 2 |
| BLE bond | 1 | 2 |
| network configuration | 1 | 5 |
| LXMF collection watermark | 2 | — |
| submission journal | 2 | 4 |
| LXMF store | 3 | — |

The LXMF store retains optional immutable receiver-local ingress interface and
RSSI/SNR evidence. Constants and codecs in the owning crates remain
authoritative for the byte formats; this table primarily defines ranges and
ownership.

## Fresh provisioning

The firmware accepts only the format versions listed above and does not migrate
another partition or product-state layout. Fully erase a board before its first
installation of this image, or whenever its installed layout or formats are
unknown:

```sh
espflash erase-flash --chip esp32s3 --port PORT \
  --after no-reset --non-interactive --skip-update-check
```

Then flash the current merged image as described in the
[firmware guide](../docs/getting-started/firmware-e290.md). Erasing this range
removes the node identity, API credentials, BLE bond, network configuration,
outbound journal, and inbound messages. The app must pair again and will see a
new Reticulum identity.

Routine merged-image flashing starts at address zero and ends before product
state because packaging uses `--skip-padding`. Do not erase product partitions
during an ordinary firmware update.
When changing this CSV, update the firmware's partition contract and this
document in the same change.
