# E290 lake range result analysis — 2026-08-01

The approximately one-mile cutoff is not yet a demonstrated radio sensitivity
limit. The recovered evidence shows a large unexplained link loss, but it does
not persist the sender's physical-TX completion or the receiver's raw counters
for the failed attempts. The next run must collect both sides of that boundary.

## Recovered evidence

- The attached E290's persisted profile after the run was 915 MHz, 125 kHz,
  SF10, CR 4/5, and requested +22 dBm. It had no Wi-Fi profiles and no TCP peer,
  so the saved configuration did not provide another usable Reticulum path.
- The sender's app database retained three delivered field messages followed by
  seven failed messages across two destinations. Every failed message was
  automatically attempted four times and ended in `delivery_timeout` after
  progressing through accepted, preparing, and awaiting-delivery states.
- Each failed attempt projected a 211-byte Reticulum packet. It fits in one
  212-byte RNode physical frame rather than fragmenting. At this profile that
  frame occupies the channel for approximately 2.06 seconds.
- One other phone's retained inbound evidence for the run contained only three
  messages, received over LoRa at approximately -92 dBm/+6 dB, -86 dBm/+8 dB,
  and -98 dBm/+4 dB RSSI/SNR. No later imported message was present there.
- `awaiting_delivery` proves that the client and Reticulum delivery state
  advanced. It does **not** prove that the matching SX1262 transmission reached
  TxDone. The relevant board diagnostics were boot-volatile and were not
  captured before reset.

## What the numbers imply

Free-space path loss at 915 MHz over one statute mile is approximately 95.8 dB.
Using the [E290 datasheet](../../reference/HT-VME290-Datasheet.pdf)'s nominal
+21 dBm maximum and zero-dBi antennas gives an ideal received level near
-74.8 dBm. The observed -98 dBm packet therefore contains roughly 23 dB of
excess loss before considering antenna gain or measurement tolerance.

That final received packet was not near the stated SF10/BW125 demodulation
limit: the local E290 datasheet lists approximately -130 dBm sensitivity and
the app retained +4 dB SNR. An abrupt transition from that much apparent margin
to no imported messages needs more explanation than ordinary inverse-square
loss alone.

Low antennas over water are a strong candidate. At one mile, the midpoint first
Fresnel-zone radius is about 11.5 m, so 60% clearance is about 6.9 m. Handheld or
boat-height antennas do not clear it even when the endpoints look optically
line-of-sight. A simple equal-height two-ray illustration predicts destructive
loss relative to free space of roughly 33 dB at 1 m, 25 dB at 1.5 m, and 20 dB
at 2 m antenna height. The exact null moves sharply with height, distance, boat
motion, and surface conditions; these figures classify the risk rather than
predicting the exact lake result.

Twenty miles is plausible from link budget alone, but not from two low lake-level
antennas. With a standard effective-earth-radius approximation, two antennas at
1.5 m have only about a 6.3-mile combined radio horizon. Two endpoints near 15 m
reach about 20 miles before allowing for Fresnel clearance and local clutter.

## Ranked working hypotheses

1. **Low-over-water geometry and antenna placement.** Fresnel obstruction and
   two-ray cancellation can account for loss of the observed magnitude and an
   abrupt distance/height-dependent null.
2. **Antenna or feed fault.** A wrong-band or damaged antenna, loose IPEX
   connector, poor adapter, nearby conductor, enclosure, body, or boat loading
   can consume tens of decibels. Requested +22 dBm does not measure conducted
   output, EIRP, or VSWR.
3. **The failed attempts did not complete physical DATA transmission or used a
   stale path.** Retained routes only prove that the selected local interface is
   online; the current failed-attempt journal cannot reconstruct interface
   dispatch or TxDone after reset.
4. **PA supply droop.** The high-power path has no powered conducted-output
   qualification. A weak cable, regulator path, or simultaneous Wi-Fi/BLE load
   could reduce output or reset radio state without changing the requested
   configuration.

No source-level defect was found in the current +22 dBm PA parameters, DIO2
RF-switch control, DIO3 TCXO control, OCP setting, image calibration, or
per-frame transmit sequence. Receiver boost is currently not enabled, but its
roughly 3 dB benefit cannot explain the recovered loss by itself.

## Decisive next test

Follow the [E290 range-test runbook](e290-range-testing.md). In particular:

1. Reboot both endpoints LoRa-only, request fresh announces, and require an
   interface-1, hop-1 route in both directions.
2. Establish a ten-of-ten close-range control in both directions.
3. Compare every board/antenna combination at one fixed short distance, then
   swap only the antennas. This separates a board/feed problem from an antenna.
4. At the first marginal lake point, hold distance fixed and raise both feed
   points through measured heights. A strong oscillation with height identifies
   a two-ray null more directly than travelling farther.
5. For one attempt at a time, retain the sender DATA terminal/TxDone counters,
   receiver raw-frame/error/accepted counters, exact route, packet hash, and
   both phone locations before any reset or retry overwrites latest-value data.
6. Repeat +14 versus +22 dBm from a known-good supply. A healthy link should
   move by roughly the requested power delta; little or no change justifies
   conducted-power and supply-rail measurement.

The private app location option records a device-precision phone observation
with each initial, manual-retry, and automatic-retry attempt. It is the phone
position when the attempt was queued, not an RF timestamp or board GNSS fix.
Distance/map export remains follow-up work.
