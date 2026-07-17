# Vision Master E290 semantic LoRa HIL

**Status:** **powered PASS** for the isolated same-image E290 semantic LoRa
vertical slice. Both connected boards were physically confirmed with
`HT-RA62-HF` modules and attached 915 MHz antennas. The permanent node image
remains separately unflashed and unqualified.

This is a hazardous, one-shot development image, not product firmware. The
same ELF is installed on both qualified E290 boards. Exact eFuse base MACs
select complementary roles:

| Board | USB serial / eFuse base MAC | Role |
| --- | --- | --- |
| A | `AC:A7:04:E1:3E:88` / `ac:a7:04:e1:3e:88` | initiator |
| B | `AC:A7:04:E1:3F:88` / `ac:a7:04:e1:3f:88` | responder |

Every other MAC remains in reset-low/NSS-high RF containment and never
constructs SPI or the radio. Each active role performs exactly two
transmissions. Across the pair, the exchange is:

1. A sends a freshly signed ANNOUNCE; B validates it and retains its route.
2. B sends a freshly signed ANNOUNCE; A validates it and retains its route.
3. A sends encrypted destination-DATA only on the learned LoRa interface; B
   decrypts and validates the exact payload and generates one delivery proof.
4. B sends that proof; A correlates it with the outstanding DATA receipt and
   requires the terminal state `Delivered` with zero receipt slots left.

Before every transmission, the image runs deadline-bounded CAD through
`SoleRnodeRadio` and requires a clear result. RX, CAD, TX, and the complete
exchange each have finite deadlines. Dropping any timed-out hardware future
must trigger the radio owner's fail-closed cancellation path. The image uses
`IrqTimestampCapture::new_monotonic_us`, and the CAD, RX, and TX observations
share that clock domain.

The immutable physical profile is NA915 at 915 MHz, SF7/BW125/CR4/5,
24-symbol preamble, explicit header, CRC, normal IQ, private sync word
`0x1424`, and requested 14 dBm output. Semtech SX1261/2 Data Sheet Rev. 2.2
Table 13-21 specifies the corresponding SX1262 optimal row as raw
`SetPaConfig(0x02, 0x02, 0x00, 0x01)` plus `SetTxParams(+22)` (`0x16`); the
E290 command-log test requires that exact current mapping. This is a
configuration target, not a conducted or radiated calibration claim. The
image does not initialize PSRAM, storage, display, Wi-Fi, BLE, USB application
service, LXMF, or NomadNet.

## Powered result

The 2026-07-17 run is preserved under the ignored local evidence directory
`artifacts/e290-semantic-hil-powered-20260717T163607Z`. Its
[`RESULTS.md`](../artifacts/e290-semantic-hil-powered-20260717T163607Z/RESULTS.md)
records the identities, hashes, tool versions, failed-attempt provenance,
captures, and limitations.

| Evidence | Result |
| --- | --- |
| Same merged image | 421,296 bytes; SHA-256 `4584abdff80ab4b3151bf5168a364dc30016e29230f51f06195661b455a01085` |
| Flash integrity | Both immediate and both post-capture range readbacks matched the image exactly |
| Cross-log verifier | `status=PASS`; JSON SHA-256 `130e9212302215d495f363c2ab52318bea06871b72d67cb116c01cb9518d3271` |
| DATA receipt | `fc143c17784f784a8c68ff33e7d1bcf897f6bd2bfd4d1cc8a7ce68335baf0aa4`; terminal `Delivered`; zero receipt slots |
| Radio state | Two clear monotonic CAD observations and two TX completions per board; both ended `radio_active=false` |
| Received signal | Initiator: -6 dBm / 12 dB; responder: -4 dBm / 12--13 dB |

The raw initiator capture is 6,185 bytes with SHA-256
`918a027094a9a6fa52ee86956dcc7a92af2c24f979cbc3f40a5f3ddd2be07b2d`;
the responder capture is 6,102 bytes with SHA-256
`e377acbc1f245836b0c0243e2567fb93e3bf6218444236e05b5a8bc4774775cb`.
Their recorder metadata closes with `completed=true`, and the verifier used
only the segments after independently counted reset offsets 12 and 5.

## Reuse boundary

The E290 executable owns its MAC gate and depends on the E290 radio wrapper.
Its packet loop uses only the board-neutral `SoleRnodeRadio` interface. The
cryptographic identities, payload fixture, announce builder, and four-step
state machine come from the allocation-free, board-independent
`reticulum-semantic-roundtrip-hil` crate. Its stable six-byte selectors choose
only public test identities and never authorize physical radio construction.
The Tracker and E290 wrappers independently map their exact eFuse MACs onto
those selectors.

`xtask graph-policy` proves that the E290 HIL selects only the portable
`semantic-roundtrip-hil` feature and cannot reach Tracker firmware, board,
radio, FEM, or runtime packages. The permanent E290 node is separately barred
from the HIL fixture crate.

## Verified build commands

From the repository root:

```sh
cargo test --locked \
  -p reticulum-heltec-vision-master-e290-semantic-hil --lib
cargo run --locked -p xtask -- graph-policy

source ~/export-esp.sh
cargo +esp build --locked --release \
  -p reticulum-heltec-vision-master-e290-semantic-hil \
  --target xtensa-esp32s3-none-elf
cargo +esp clippy --locked --release \
  -p reticulum-heltec-vision-master-e290-semantic-hil \
  --target xtensa-esp32s3-none-elf -- -D warnings

python3.13 -m unittest discover -s interop/python \
  -p 'test_verify_e290_semantic_hil_logs.py' -v
python3.13 -m unittest discover -s interop/python \
  -p 'test_e290_qualification_host.py' -v
python3.13 -m unittest discover -s interop/python \
  -p 'test_esp32s3_usb_serial_capture.py' -v
```

The HIL intentionally rejects debug builds. Its explicit partition table is
[`partitions/heltec-vision-master-e290-semantic-hil.csv`](../partitions/heltec-vision-master-e290-semantic-hil.csv).
It allocates a 4 MiB low-address factory slot and no writable application-data
partition. It is not the product partition layout.

[`verify_e290_semantic_hil_logs.py`](../interop/python/verify_e290_semantic_hil_logs.py)
is the fail-closed verifier for this image. The older
`verify_semantic_roundtrip_hil_logs.py` is intentionally Tracker-specific and
must not be used to claim an E290 result. The E290 verifier requires the exact
physical MAC/role pair, fixed radio profile, runtime patch, two clear and
monotonically timestamped CAD observations, two transmissions per board, all
four cross-bound packet hashes and receipts, explicit semantic ingress events,
one successful terminal, and subsequent RF shutdown. Nineteen verifier tests
cover the successful trace plus offset, replay, mismatch, omission,
extra-event, signal, CAD, receipt, state, fatal-output, and firmware-schema
failures.
The identity/flash host gate has 24 tests, including nested USB-hub isolation,
hash-bound flash/readback and post-capture range verification; the counted
capture recorder has another 19 tests.

## Reproduction runbook

The physical HF-module and antenna precondition is satisfied. The software
gate also requires the current Semtech Rev. 2.2 optimal +14 dBm trace:
`SetPaConfig 02/02/00/01` and raw `SetTxParams 0x16`.

1. Record the physical module confirmation as `HT-RA62-HF` for each board and
   keep both 915 MHz antennas attached.
2. Map each current serial port by USB serial descriptor. Immediately before
   every destructive command, run `espflash board-info --chip esp32s3` and
   require the exact MAC above, 16 MB flash, disabled flash encryption, and
   disabled secure boot. A cached `/dev/cu.*` path is not identity.
3. Preserve a fresh 16 MiB full-flash backup of each board. Record source
   status, `Cargo.lock`, tool versions, the partition CSV, ELF, merged image,
   and SHA-256 hashes in a new ignored evidence directory.
4. Build one release ELF with the commands above, then create one merged image:

   ```sh
   ELF=target/xtensa-esp32s3-none-elf/release/reticulum-heltec-vision-master-e290-semantic-hil
   espflash save-image --chip esp32s3 --merge --skip-padding \
     --flash-mode dio --flash-freq 80mhz --flash-size 16mb \
     --xtal-freq 40mhz \
     --partition-table partitions/heltec-vision-master-e290-semantic-hil.csv \
     --target-app-partition factory "$ELF" e290-semantic-hil.bin
   ```

5. Use the single-process helper action below for each board. It rediscovers
   the callout by USB serial, revalidates the exact MAC and security state,
   preserves the hash-bound image, writes it at address zero, and immediately
   reads back exactly the image byte length. It accepts only the explicitly
   confirmed `HT-RA62-HF` module acknowledgement and leaves the board in the
   loader for the capture's counted reset. Never infer a role from enumeration
   order.

   ```sh
   IMAGE=e290-semantic-hil.bin
   IMAGE_SHA256="$(shasum -a 256 "$IMAGE" | cut -d ' ' -f 1)"

   python3.13 interop/python/e290_qualification_host.py flash-merged \
     --usb-serial AC:A7:04:E1:3E:88 \
     --expected-mac ac:a7:04:e1:3e:88 \
     --expected-flash-bytes 16777216 \
     --evidence-prefix "$RUN/board-a/flash" \
     --image "$IMAGE" --expected-image-sha256 "$IMAGE_SHA256" \
     --confirmed-radio-module HT-RA62-HF

   python3.13 interop/python/e290_qualification_host.py flash-merged \
     --usb-serial AC:A7:04:E1:3F:88 \
     --expected-mac ac:a7:04:e1:3f:88 \
     --expected-flash-bytes 16777216 \
     --evidence-prefix "$RUN/board-b/flash" \
     --image "$IMAGE" --expected-image-sha256 "$IMAGE_SHA256" \
     --confirmed-radio-module HT-RA62-HF
   ```
6. Start continuous, separate serial captures for both USB serial descriptors
   with the receive-only recorder below. Start B's command first, then A's in a
   separate process; each command performs one counted normal reset after a
   bounded pre-reset drain. Preserve both raw streams and both metadata files.
   The `counted_reset_offset` printed in each metadata file is the sole offset
   passed to the verifier; do not combine terminal scrollback by hand.

   ```sh
   python3.13 interop/python/esp32s3_usb_serial_capture.py \
     --port "$RESPONDER_PORT" --hard-reset-after-open \
     --duration-seconds 190 > responder.raw 2> responder.capture.txt

   python3.13 interop/python/esp32s3_usb_serial_capture.py \
     --port "$INITIATOR_PORT" --hard-reset-after-open \
     --duration-seconds 190 > initiator.raw 2> initiator.capture.txt
   ```

7. Verify the independently counted segments and preserve the JSON result:

   ```sh
   python3.13 interop/python/verify_e290_semantic_hil_logs.py \
     --initiator-byte-offset "$INITIATOR_OFFSET" \
     --responder-byte-offset "$RESPONDER_OFFSET" \
     initiator.raw responder.raw > semantic-hil.verified.json
   ```

   Accept a powered run only if both captures have the expected exact MAC and
   role, profile PASS, radio-init PASS, two clear CAD observations, two TX
   completions, all role-specific semantic validation events, and one terminal
   PASS followed by `radio_active=false`. Reject any panic, FAIL, deadline,
   unknown MAC, busy CAD, extra transmission, reconnect ambiguity, or missing
   receipt delivery.
8. After capture, use the read-only `verify-merged` action for each board with
   the same identity, image, expected hash, and module arguments shown in step
   5. Give each call a new evidence prefix such as
   `$RUN/board-a/post-capture` and `$RUN/board-b/post-capture`. Retain the
   evidence directory as the powered-test record only after both
   `.verify-image.verified.json` files exist.

This run establishes the first E290 end-to-end CAD/RX/TX/RNode/Rete vertical
slice. It is one near-field exchange with fixed public HIL identities and a
USB-triggered counted reset. It does not qualify cold power-on, busy-CAD retry,
range or sensitivity, conducted power or calibration, sustained routing,
multi-hop propagation, concurrent interfaces, storage durability, PSRAM use,
regional duty policy, maximum power, application clients, LXMF, NomadNet, or
the permanent node graph.
