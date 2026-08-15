# Documentation

This index separates normal setup and current design from revision-bound
qualification records. Start with the getting-started guides; use the longer
engineering dossiers only when investigating a specific implementation or
powered result.

## Getting started

- [Build, package, and flash E290 firmware](getting-started/firmware-e290.md)
- [Build and install the Expo app](getting-started/app.md)
- [Pair, add, and switch appliances](getting-started/pairing.md)
- [Run repository verification](development/verification.md)
- [Run a controlled E290 LoRa range test](development/e290-range-testing.md)
- [Review the 2026-08-01 E290 lake range result](development/e290-lake-range-analysis-2026-08-01.md)

## Current capabilities and limits

- [Status and known limitations](status.md) is the concise current product
  view.
- [Detailed POC limits and deferred work](poc-known-defects.md) retains the
  engineering-level capacity, protocol, persistence, security, and test gaps.
- [Rete upstream hardening backlog](rete-upstream-backlog.md) records fork
  work and possible upstream contributions. Opening any issue or pull request
  still requires explicit user approval.

## Architecture

- [Current architecture overview](architecture/overview.md) is the canonical
  system-level explanation.
- [Device API](api/device-api-v1.md) defines the logical local-client protocol.
- [Interface router](interface-router.md)
- [Node-core outbox](node-core-outbox.md)
- [Async transmit handoff](async-tx-handoff.md)
- [Transmit supervisor](tx-supervisor.md)
- [Durable submissions](durable-submissions.md)
- [Submission runtime](submission-runtime.md)
- [Storage actor](storage-actor.md)
- [Storage journal](storage-journal.md)
- [Durable LXMF store](../crates/lxmf-store/README.md)

The [architecture and feasibility record](firmware-architecture.md) and
[permanent E290 implementation dossier](e290-node.md) are detailed,
chronological engineering records. They contain historical artifact hashes,
superseded implementation stages, and deep runbooks; they are not concise
setup guides.

## Hardware and reference

- [Vision Master E290 hardware target](heltec-vision-master-e290.md)
- [E290 permanent-node partition map](../partitions/README.md)
- [Tracker V2 radio target](tracker-v2-radio.md)
- [Dependency and source provenance](provenance.md)
- [Interoperability fixtures](../interop/README.md)

The E290 is the primary full-stack target. The Tracker V2 remains a constrained
radio regression fixture rather than the product baseline.

## Decisions

The [ADR index](adr/README.md) links every accepted design decision, from the
initial scaffold and Rete adoption through durable LXMF, the universal Expo
client, Reticulum-native discovery, secure BLE onboarding, receiver-local
message evidence, one-shot Reticulum proof probes, durable radio tracing, and
Sideband-compatible LXMF message location.

## Qualification history

These records freeze the source revision, artifact identity, setup, evidence,
and limits of bounded tests. A passing proof does not automatically qualify a
newer image or a broader product behavior.

### Current E290 and application path

- [Secure BLE appliance onboarding decision and powered acceptance](adr/0019-secure-ble-appliance-onboarding.md)
- [Expo appliance first-run proof](e290-expo-appliance-first-run-proof.md)
- [Physical iOS BLE-to-LoRa proof](e290-expo-ios-ble-lora-proof.md)
- [Reticulum Nearby proof](e290-reticulum-nearby-powered-proof.md)
- [Nomad/Micron page proof](e290-nomad-powered-proof.md)
- [Direct-Link proof](e290-direct-link-powered-proof.md)
- [Stale-Link recovery proof](e290-stale-link-recovery-powered-proof.md)
- [Same-Link reuse and replay proof](e290-same-link-reuse-replay-powered-proof.md)

### Firmware and protocol bring-up

- [E290 semantic LoRa HIL](e290-semantic-hil.md)
- [E290 display HIL](e290-display-hil.md)
- [E290 Wi-Fi API proof](e290-wifi-api-proof.md)
- [API 1.4 LXMF POC](e290-api14-lxmf-poc.md)
- [Persistent LXMF chat proof](e290-lxmf-chat-alpha-proof.md)
- [Host appliance proof](e290-lxmf-appliance-alpha-proof.md)
- [Expo native Rust bridge proof](expo-native-rust-bridge-proof.md)

### Earlier phases and Tracker fixtures

- [Phase 0 acceptance](phase-0-acceptance.md)
- [Phase 1 receive slice](phase-1-rx-slice.md)
- [Phase 1 receive HIL](phase-1-rx-hil.md)
- [Phase 1 transmit HIL](phase-1-tx-hil.md)

Proof paths remain stable so existing ADRs, artifact manifests, and historical
records continue to resolve. Their placement is therefore intentionally less
important than this index.
