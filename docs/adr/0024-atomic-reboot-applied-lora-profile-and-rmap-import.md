# ADR 0024: Atomic reboot-applied LoRa profile and RMAP import

- Status: accepted
- Date: 2026-07-31
- Extends: ADR 0003, ADR 0015, ADR 0020, ADR 0021

## Context

LoRa peers must agree on frequency, bandwidth, spreading factor, and coding
rate. Persisting or applying those fields independently could expose a partial
profile, while changing the active radio in place would invalidate the
boot-owned configuration fingerprint, airtime bounds, and diagnostics. Power is
part of that same durable radio choice even though peers may use different
power.

RMAP.world publishes copyable Reticulum `RNodeInterface` configuration. That is
useful setup input, but pasted text is neither trusted configuration nor
regulatory authorization and must not silently reconfigure an appliance.

## Decision

Network configuration owns one atomic LoRa tuple: center frequency, bandwidth,
spreading factor, coding-rate denominator, and requested transmit power. Device
API mutation kind 7 replaces the complete tuple. Snapshot key 10 carries the
same five-field map. Legacy key 9 remains the power projection, and legacy
mutation kind 6 changes only power while preserving the saved modulation. A
decoder requires key 9 and key 10 power to agree; a snapshot without key 10
uses the historical 915 MHz, 125 kHz, SF7, CR 4/5 modulation with key 9 power.

The store reads semantic formats 1 through 3 and writes format 4. Formats 1 and
2 receive the historical default profile; format 3 retains its saved power with
the historical modulation. Firmware older than format 4 cannot mount a format-4
snapshot, so the network-configuration partition must be deliberately erased
before such a downgrade.

Every material accepted change is saved for the next boot. The active radio
remains immutable, the mutation reports that restart is required, and clients
present the saved/after-restart tuple separately from the running tuple reported
by radio diagnostics.

The E290 product validates the whole tuple before committing it. The complete
occupied channel must fit the HT-RA62-HF path's 863--928 MHz range; SF is 7
through 12, coding rate is 4/5 through 4/8, power is one of +14, +17, +20, or
+22 dBm, and bandwidth/SF combinations with unqualified RNode low-data-rate
optimization behavior are rejected. These checks establish board and driver
compatibility only. The operator remains responsible for regional frequency,
bandwidth, duty-cycle, antenna, and EIRP rules. LoRa peers that should exchange
frames directly must use matching frequency, bandwidth, SF, and coding rate.

The app's RMAP importer accepts exactly one copied `RNodeInterface` block,
parses and normalizes its supported numeric fields locally, validates the
result with the same E290-facing checks, and places it in an unsaved preview.
An omitted `txpower` retains the current draft power. Only the separate
**Save for next restart** action sends the ordinary compare-and-swap mutation.

## Consequences

There is no transient partial radio profile and no live radio reconfiguration.
Previous clients retain power-only read/write compatibility, while current
clients can compare saved and running profiles explicitly. Imported RMAP text
is a convenience source, not a network fetch, trust decision, compatibility
guarantee, or permission to transmit. Wider profile qualification and live
reconfiguration remain separate work.
