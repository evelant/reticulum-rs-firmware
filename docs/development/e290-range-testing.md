# E290 LoRa range-test runbook

Use this procedure to separate RF loss from route discovery, channel access,
framing, and proof-return failures. A message timeout by itself is not a range
measurement.

This is an engineering field run, not regulatory authorization. Confirm the
frequency, bandwidth, antenna, transmit power, and operating procedure are
legal and safe at the test location.

## Evidence rules

- Test two `HT-RA62-HF` E290s with known-good 915 MHz antennas attached before
  power-up. Identify each board by its full device identity, not USB port.
- Use the same firmware revision and compatible app build on both sides.
- Record the complete running profile on both boards: frequency, bandwidth,
  spreading factor, coding rate, and requested power. The four modulation
  fields must match. Requested power is not measured conducted power or EIRP.
- Run one logical message at a time. The board may make several serialized RNS
  carrier attempts for that one durable submission, so wait for delivery or
  deliberately end the observation window before starting another range
  sample. Keep each phone connected long enough for the Activity screen's
  packet-correlated RF trace to import from both 32-event board rings. A long
  disconnect or reset before import can lose trace rows; stop if either app
  reports incomplete history. Do not reset either board until both complete
  trace exports are saved.
- Keep each test message at or below a 254-byte encoded Reticulum packet so it
  uses one RNode physical frame. Confirm the actual packet length from packet
  evidence rather than estimating it from visible message text.
- Treat app activity timestamps as local database-observation times, not RF
  timestamps. Treat RSSI/SNR as receiver-local final-hop evidence; a relay can
  be the measured transmitter.

## Prepare a LoRa-only baseline

1. Disable or remove the Reticulum TCP peer on both appliances and reboot. In
   diagnostics, LoRa interface 1 must be online and TCP interface 2 must be
   absent or offline.
2. Confirm the two running radio profiles are identical. For an SF10 comparison
   use 125 kHz bandwidth and the same coding rate on both boards; change only
   one named variable during an A/B run.
3. Mount both antennas vertically with clear space around the radiating
   element. Record antenna-feed-point height above ground or water, orientation,
   antenna model, cable/adapter, enclosure, and power source.
4. Reboot both nodes at close range so old boot-volatile paths cannot influence
   the run. Request a fresh announce or use **Measure path**, then inspect the
   retained route to the exact peer destination on each board.
5. For a direct two-board RF baseline, require:

   | Route field | Required value |
   | --- | --- |
   | `retained_interface_id` | `1` |
   | `resolution` | `exact_ready` |
   | `hops` | `1` |
   | `next_hop_identity` | absent |
   | `learned_age_ms` | consistent with this boot/run |

   `exact_ready` proves only that the selected local interface is online. It
   does not prove that the remote node or a retained next hop is still
   reachable. Stop a direct-range run if the route names a repeater or another
   interface.
6. Send ten short messages in each direction at close range. Do not move
   outward unless every logical message reaches `Delivered`, its trace contains
   matching LoRa DATA-attempt evidence, and delivery is repeatable in both
   directions.

## Record location and placement

On each phone, open **Activity** and enable **Field location telemetry**. Grant
foreground location access and wait for the card to show a coordinate and
usable accuracy before sending. The phone remembers this setting across app
restarts and appliance switches until it is explicitly turned off; collection
still pauses whenever the app leaves the foreground, so confirm a fresh sample
at the beginning of each field run.

Once enabled, SQLite schema 6 durably stamps every initial app submission and
explicit manual replacement with the latest phone-location state. Board-owned
automatic carrier retries remain inside that same durable submission and reuse
its original location stamp; they do not wake the app or sample a new phone
position. The stamp is therefore the phone position when the app submission
was queued, not the exact RF emission position or time and not board GNSS. For
a moving endpoint, a later board retry can be far from that coordinate even
when its recorded capture time was fresh at initial queueing. SQLite schema 7
joins the retained stamp to correlated RF trace events. Use **Export JSON** for
the lossless run artifact and **Export CSV** for analysis; use the app Map for
inspection, not as a substitute for the complete exports.

Capture both endpoints, not only the mobile endpoint. For every app-created
submission, record:

- latitude and longitude at device precision;
- `captured_at_unix_ms`;
- horizontal accuracy in metres;
- sample age when the app submission was queued;
- which phone and appliance the sample represents; and
- whether the phone, board, and antenna remained together until RF completion.

Do not substitute the saved RMAP coordinate: that path deliberately rounds to
roughly 100 metres and firmware does not retain capture time or accuracy. For
every board-owned carrier attempt, retain its distinct attempt token and TxDone
evidence together with the original app-submission stamp. If either endpoint
moves after queueing, keep a timestamped phone GPS log and correlate it to the
board's boot-relative trace separately; otherwise mark later retries unusable
for distance evidence. Location belongs in the private test record unless the
operator separately chooses to share it.

For a stationary endpoint, take a fresh fix at the start and end of the run.
For a moving endpoint, queue a fresh logical message at each measurement point
or retain an independent continuous GPS track; do not interpret the initial
submission stamp as the position of a later autonomous retry. Reject a distance
datum if either endpoint fix is stale, has missing accuracy, or its accuracy is
too poor for the distance increment being evaluated. Preserve the raw
coordinates and accuracy; derive distance later with one documented geodesic
calculation.

## Paired attempt record

Use a stable run ID and fixed-length payload pattern such as
`RANGE-A-0001`. For each direction and sequence, inspect the message's RF trace
and retain complete exports from both appliances:

| Evidence | Sender | Receiver |
| --- | --- | --- |
| Identity and location | board ID, coordinate, accuracy, capture time | board ID, coordinate, accuracy, capture time |
| Route | destination, next hop, hops, selected interface, resolution | reverse-route fields when present |
| Packet correlation | durable submission, app-submission number, RNS attempt token, encoded length, SHA-256 | inbound message ID plus matching logical-RX digest/hash when available |
| Radio evidence | dispatch outcome, planned/completed frames, every `TxDone` time | logical-RX time, interface, RSSI, and SNR |
| Result | board-owned pending/retrying, delivered, or permanent failure | no frame, frame/drop, packet accepted, or LXMF imported |

Sender trace and counter evidence:

- `tx_terminal_jobs`, `tx_successes`, and `tx_completed_frames`;
- `tx_access_rejects`, `tx_failures`, `cad_busy`, and `cad_clear`; and
- route-selected submission/token and exact interface;
- DATA outcome, interface, encoded length, packet SHA-256, and authorization;
- planned versus completed frames and their monotonic `TxDone` timestamps; and
- attempt-terminal outcome plus returning-proof interface/RSSI/SNR when
  delivered.

Receiver trace and counter evidence:

- `rx_physical_frames`, `rx_packets`, `rx_errors`, and `rx_drops`; and
- matching logical-RX packet digest/hash, monotonic time, interface, RSSI, and
  SNR, plus the inbound message's first-arrival interface/signal when imported.

Interpret the pair, not one status in isolation:

| Observation | Classification |
| --- | --- |
| No route-selected event for the submission | DATA never reached routing; inspect path discovery, queueing, or submission projection |
| Route exists but no matching terminal DATA dispatch | inspect router/actor handoff and trace-history completeness |
| Sender access rejection or rising `cad_busy` without `TxDone` | local channel-access result, not remote RF range |
| Sender records every planned `TxDone`; receiver has no matching logical RX | RF/PHY path is the leading boundary |
| Receiver physical/error counters rise but `rx_packets` does not | PHY, CRC, or RNode framing boundary |
| Receiver accepts a packet but does not import LXMF | RNS/LXMF filtering, decoding, or persistence boundary |
| Receiver imports LXMF; sender records delivery timeout | return proof/path or proof-correlation boundary |
| Sender records delivered plus proof ingress | end-to-end success for that exact attempt only; proof RSSI is sender-local final-hop evidence |

Aggregate counter deltas identify a boundary window, not a packet. Record any
unrelated traffic heard during that window and require packet/message
correlation before treating a receiver delta as the matching attempt.

`Preparing` now means the board still owns the LXMF obligation and may be
waiting, discovering a path, or backing off before a fresh attempt. Inspect
whether path requests, announces, and DATA attempts were actually transmitted
before describing it as message-range failure. If a DATA attempt times out,
capture the route and exact attempt terminal before the next automatic attempt;
current opportunistic handling removes the timed-out path while leaving the
logical submission `Preparing`. A probe timeout remains a separate volatile
operation and does not currently evict its retained path.

## Distance and A/B matrix

Use ten sequential logical messages per direction at each point. Start at 50 metres,
then use approximately 250 m, 500 m, and 1 km. Extend to 2 km and 5 km only
after the shorter points remain valid. Record actual GPS-derived distance;
labels are navigation aids, not evidence.

At the first marginal point, hold distance, payload, frequency, bandwidth,
spreading factor, coding rate, boards, and weather constant. Run these A/B
comparisons separately:

| Question | A | B | Keep fixed |
| --- | --- | --- | --- |
| Requested power | +14 dBm | +22 dBm | antenna placement and power source |
| Antenna height | low measured feed height | raised measured feed height | orientation and power |
| Polarization | both vertical | one antenna rotated 90 degrees | feed height and position |
| Unit/antenna fault | original board/antenna assignment | swap antennas between boards | endpoint positions and profile |
| Supply integrity | field supply | known-good adequately rated supply | board, antenna, and placement |

Use at least two measured antenna heights appropriate to the site; do not label
them merely "handheld" and "raised". Over water, record height above the water
surface and boat motion. In all environments record foliage, buildings,
vehicles, coated glass, nearby conductors, weather, and whether true optical
line of sight existed.

## Stop and invalidate conditions

Stop the run and fix the setup when any of these occurs:

- an antenna is missing, loose, damaged, wrong-band, or connected through an
  unrecorded adapter;
- profiles differ, the running profile does not match the saved profile, TCP
  interface 2 is online, or a direct baseline route is not interface 1/hop 1;
- close-range bidirectional control fails;
- either board resets, faults, overheats, loses power, or its diagnostics
  become unavailable;
- the matching DATA terminal or paired receiver counters cannot be captured;
- location accuracy/sample age is unsuitable for the distance increment;
- CAD/access rejection dominates the batch; or
- weather, water, traffic, terrain, access rules, or radio regulations make
  continuation unsafe or unauthorized.

At the first 0/10 receive batch, do not immediately travel farther. Repeat the
same point once, then return to the close-range control without changing the
hardware. If close-range control now fails, invalidate the outward result and
diagnose the equipment/state. If close-range control passes and a second
well-instrumented batch shows completed LoRa DATA with no receiver RX delta,
record that point as the current RF no-reception bound and stop extending the
distance for that configuration.

## Close the run

Before resetting either board, save complete JSON radio-trace exports from both
Activity screens and verify neither marks history incomplete. Also preserve
the radio/route diagnostics, message details, and CSV exports used for analysis.
Record the source revision, firmware artifact hash, app version, board IDs,
antenna/power setup, and any deviations from this procedure. A range statement
must report sample counts and both directions; a farthest single success is not
a reliable range.

See the [E290 firmware guide](../getting-started/firmware-e290.md),
[radio owner contract](../heltec-vision-master-e290.md#radio-owner-contract),
[device API](../api/device-api-v1.md), and
[known POC defects](../poc-known-defects.md) for the underlying product limits.
