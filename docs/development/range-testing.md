# Range testing

Use the app's ordinary message, activity, radio-trace, and map data for field
tests. A separate recorder is unnecessary: the app keeps the durable events
and packet correlations produced during normal use and can export them for
analysis.

## Prepare the nodes

1. Flash the same current firmware on every participating board.
2. Attach the correct HF antenna before powering or transmitting.
3. Apply the same LoRa frequency, bandwidth, spreading factor, coding rate,
   preamble, header, CRC, and IQ settings on every LoRa interface.
4. Select a legal transmit power and account for antenna gain and local rules.
5. Reboot after a radio-profile change and confirm the applied profile in
   **Network > Radio & Routes**.
6. Exchange announces and verify a short-range message in each direction.

Record the firmware revision, applied radio profile, antenna type and
orientation, node placement, battery or USB power, terrain, vegetation,
weather, and any nearby RF sources. A useful comparison changes one variable
at a time.

## Collect a run

- Enable **Field location telemetry** on each phone. It is a persistent,
  foreground-only diagnostic preference.
- Enable **Attach phone location** on test messages when the recipient should
  receive the sender's position as signed LXMF application data.
- Keep the receiver stationary when comparing distances, then move the sender
  through measured waypoints.
- Send several uniquely labelled messages at each waypoint. Allow time for
  path discovery, channel access, board-owned retry, and delivery proof before
  moving on.
- If testing relays, place the relay where it can hear both ends, leave it
  powered without an app, and allow announces and paths to propagate.
- Use **Measure path** as an additional reachability sample. It measures a
  Reticulum request and returning proof, not LXMF service or throughput.
- Export the activity and radio trace as JSON after the run. CSV is convenient
  for plotting, but JSON preserves the complete typed data.

The board owns accepted-message retry. Closing or disconnecting the app should
not stop eventual delivery.

## Interpret the evidence

For an inbound message, retained RSSI and SNR describe the receiver-local final
LoRa hop into that appliance. On a relayed route they measure the relay-to-
receiver hop, not the original sender. An outbound phone cannot report the
remote receiver's signal unless that receiver later shares its own observation.

Map lines show endpoint phone locations. They are not RF paths: the location
may have been sampled at queue or import time, the phone may not coincide with
the board, and terrain between endpoints is not represented. Horizontal
distance, endpoint elevation, and location accuracy should be considered
together.

Retained routes are routing-table entries, not a list of currently audible or
reachable peers. Likewise, the latest LoRa RX is the most recent accepted
logical packet rather than a spectrum scan.

Packet-correlated trace stages help isolate failures:

| Last observed stage | Likely boundary to inspect |
| --- | --- |
| No route selection | announce propagation, retained path, destination identity |
| Route selected, no terminal DATA authorization | queue ownership, channel access, radio task |
| Authorized, no `TxDone` | SX1262/PA control, SPI/IRQ handling, power or reset |
| `TxDone`, no receiver RX | profile mismatch, antenna/RF path, interference, link budget |
| Receiver RX, no accepted message | Reticulum/LXMF validation, routing, duplicate handling |
| Accepted message, no terminal delivery | proof return path, retry timing, client synchronization |

Do not infer calibrated conducted output from the configured dBm value. If
range is unexpectedly poor after software evidence reaches `TxDone`, verify
the antenna, connector, feedline, board variant, PA path, supply stability, and
actual output with RF test equipment before tuning protocol retries.
