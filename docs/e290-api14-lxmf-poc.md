# E290 API 1.4 bidirectional LXMF proof

**Date:** 2026-07-22

**Status:** the live bidirectional powered proof is complete. Both outbound
submissions reached Reticulum `Delivered`, both receivers committed the same
LXMF message identifiers, and authenticated chunked reads reproduced the
committed normalized wires exactly. The final audited follow-up image was then
written and read back exactly on both boards. After a physical CPU reset, each
sender still reported its original terminal delivery and each receiver listed
and served the same exact 126-byte normalized wire. The RF exchange is bound to
the first artifact below; the final image's powered claim is boot, durable
remount, authenticated status/list/read, and exact message readback rather than
a retroactive RF retransmission claim.

This is a bounded proof of the USB-controlled, source-free basic-send path over
the two-board NA915 LoRa link. It is not sustained traffic, multi-hop,
Link/Resource, propagation-node, compaction, power-cut, or BLE/Wi-Fi
qualification.

## Artifact and board binding

The RF-exchange ordinary ELF was 13,960,528 bytes with SHA-256
`16683a3627e70f68ffe3976e036e955ef934551f6e0833671c5b430f8ef9f283`.
The separately built runtime-measurement ELF was 14,119,464 bytes with SHA-256
`491c3b56041cbfba3ee9923242ac3b512f0742a51806d86d5dd1ae251bf8cd6a`.
Its explicit 16 MiB ordinary merged image was 883,856 bytes, used
818,320/6,291,456 application bytes, and had SHA-256
`4f96f17a9d15c79065658425ed34510de6bb66871fa5c26af1b512c7474a6a66`.
Identity-qualified writes and exact same-length readbacks matched this image on
both boards before boot.

The final audited ordinary ELF is 13,962,728 bytes with SHA-256
`ca7ff037671d13e39e59bc05c901ddbd1b2433e52be39c119661241157642971`.
Its separately built runtime-measurement ELF is 14,121,824 bytes with SHA-256
`ed2c082c828067f73bb826e4036f1e68962a04fe53a12e9324ac0fcc07a0e4af`.
The final explicit 16 MiB merged image is 882,512 bytes, uses
816,976/6,291,456 application bytes, and has SHA-256
`639b0e7b12c13d3b3236f1b546e8cf0d4fddf398cc8360d3841b45e3339f882e`.
Identity-qualified writes and exact 882,512-byte readbacks matched that digest
on both boards before the physical-reset persistence check.

| Board | USB serial / MAC | Primary destination | LXMF delivery destination |
| --- | --- | --- | --- |
| A | `AC:A7:04:E1:3E:88` / `ac:a7:04:e1:3e:88` | `c99e8ff1ec8629e4e1290e14462ae8af` | `03869ee76b74d1e2a4626f0c02ae3248` |
| B | `AC:A7:04:E1:3F:88` / `ac:a7:04:e1:3f:88` | `83a09ed807a0a7c631386deaa0448fb9` | `935caba93f7cd97c7c6658350ac02b45` |

After a normal reset of the RF artifact, both boards returned authenticated
`identity-summary` records with those exact destinations. After the final image
and a later physical reset, both again completed authenticated API sessions and
returned their board-bound durable state. These boots also prove that the
diagnosed pre-USB mount-stack overflow is fixed in both flashed product images.

## Powered exchange

The boards were allowed one complete 95-second announce window. Each send used
a caller-retained timestamp and idempotency key, returned durable acceptance,
and then waited for the ordinary Reticulum delivery proof.

| Direction | Timestamp ms | Idempotency key | Submission | Message ID | RNS packet bytes / SHA-256 | Result |
| --- | ---: | --- | ---: | --- | --- | --- |
| A to B | `1784732100000` | `e7f7bf601a2829e699b713cc2ce74428` | 2 | `7163fb6431f844ef5e01f54d66a052de60bf059531ca9bacf28010d57569d218` | 211 / `e3f13902a59a65e764359d67296a745863b3e1ccd611d326443780cccf7e4cb3` | `delivered` |
| B to A | `1784732100001` | `6a5ae193be6d794be92f7c05353c0187` | 2 | `f732a9f579cd47b9b20d5aea20ef074a43da1760fa587adf6476001d9ff4ae3b` | 211 / `e3e09a7d6c56e4c09771ac37f93a506d2caaa1e9c5f069266c0defed9d95f8c3` | `delivered` |

Receiver enumeration preserved the exact sender message ID and reversed the
expected source/destination pair. A stored B-to-A as handle 1. B already held
one earlier qualified message and appended A-to-B as handle 2; this exercised
append-to-nonempty behavior rather than silently replacing the earlier entry.

## Authenticated receiver readback

Each receiver served the complete normalized wire through authenticated,
bounded chunk reads. The host parser accepted both wires, the independently
computed file SHA-256 matched the device's committed metadata, and the parsed
binary title/content hashes matched the original request bytes.

| Receiver / handle | Message ID | Wire bytes / SHA-256 | Title / SHA-256 | Content / SHA-256 | Timestamp bits |
| --- | --- | --- | --- | --- | --- |
| B / 2 | `7163fb6431f844ef5e01f54d66a052de60bf059531ca9bacf28010d57569d218` | 126 / `80cef8e9e45c3ddf4ec226ae4a7f6ae29505cf2ebd1e665115bee1cca75ad63e` | `A2B` / `79cc85bec5cf3c5fa7796412b0da4b67452c6404e589da4127da0ef891b5ba24` | `api14-a-to-b` / `517d3d1139ad3f706776cbf12dbc9841d9c53d0b09db02d9bfbdb4af1c81f129` | `41da983671000000` |
| A / 1 | `f732a9f579cd47b9b20d5aea20ef074a43da1760fa587adf6476001d9ff4ae3b` | 126 / `c8e6f1de79f8c153f7538577b7e3b7c195ae8046a5a97726b6bd07370077d795` | `B2A` / `278211ed425dedc1b4d5b8a88213f9b9fd2b45b1bae2866e1a0a128428011b5d` | `api14-b-to-a` / `5209761ff78daff5d208613a178a1e72cad8a1925176971388d1dccd2a5dddec` | `41da983671001062` |

## Physical-reset persistence on the final image

After both final-image writes and exact readbacks, a physical reset established
a new CPU boot and USB epoch on each E290. Authenticated `submission-status`
reported submission 2 as `Delivered` on A with the original 211-byte packet
digest `e3f13902a59a65e764359d67296a745863b3e1ccd611d326443780cccf7e4cb3`
and on B with
`e3e09a7d6c56e4c09771ac37f93a506d2caaa1e9c5f069266c0defed9d95f8c3`.

Fresh authenticated enumeration then returned A's proof message as handle 1
and B's as handle 2 with the exact message IDs, endpoints, timestamp bits,
lengths, and wire digests above. Independent downloads were each 126 bytes;
their host SHA-256 values were again
`c8e6f1de79f8c153f7538577b7e3b7c195ae8046a5a97726b6bd07370077d795`
and `80cef8e9e45c3ddf4ec226ae4a7f6ae29505cf2ebd1e665115bee1cca75ad63e`.
The host parser again matched the original binary title/content hashes. This
closes the bounded CPU-reset journal/store remount and readback check; it is not
an electrical power-cut qualification.

The RF run directory is
`/private/tmp/e290-api14-stackfix.ex2GhV`. It retains the two send transcripts,
receiver list transcripts, read transcripts, and exact 126-byte read files.
The final-image flash/readback, physical-reset status/list/read transcripts,
USB-epoch traces, and exact downloaded files are under
`/private/tmp/e290-api14-final3-proof`. Those paths are local evidence
references, not portable repository artifacts.

## Static and host gates

The final audited pair passed the cumulative eight-frame pre-USB stack gate with
142,432/142,608-byte default/HIL chains. After the 4,096-byte lower-ROM and
interrupt reserve, policy headroom was 28,384/27,408 bytes. The complete
workspace test suite, all-target host Clippy, graph policy, default/HIL target
builds and target Clippy passed.

Two later read-only audits found that the public future
`PreparedBasicLxmf`/carrier wrapper did not bind the token to every carrier byte
and that a public summary constructor accepted physically impossible component
lengths. The final audited source stores and verifies the exact carrier SHA-256
before entropy, output, or receipt mutation, validates the exact overflow-safe
MessagePack lower bound, and includes substitution, post-prefix mutation, and
contradictory-length regressions. Both fixes are in the final flashed image.
The product API 1.4 RF path does not call the future wrapper, and the summary
hardening does not alter an already valid normalized LXMF wire. The original RF
claim nevertheless remains bound to its RF artifact; the final image was
separately powered through boot and the complete durable status/list/read path.
