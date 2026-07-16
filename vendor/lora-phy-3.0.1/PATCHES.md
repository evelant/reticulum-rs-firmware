# Local `lora-phy` 3.0.1 patch inventory

Source: crates.io `lora-phy` 3.0.1, registry checksum
`61471c3b2909789e3332083577f6cf6c41a4fcf37674ef15156bcbb20504ac65`.

The unmodified driver derives every SX1262 high-power PA command from its
requested output power, enables a board's transmit RF switch only immediately
before `SetTx`, provides no board hook to finish external-front-end setup after
chip initialization, and leaves the software radio mode unchanged after the
public standby operation. This checkout adds three default-preserving hooks
and repairs that standby-state invariant:

1. an atomic, default-`None`
   `Sx126xVariant::high_power_pa_override()` hook, whose returned value carries
   PA duty cycle, `hpMax`, raw signed `SetTxParams` power byte and optional OCP
   trim together; and
2. a default-no-op `InterfaceVariant::complete_rf_switch_initialization()` hook
   reached via `RadioKind::complete_rf_switch_initialization()` after LoRa-chip,
   initial-power and IRQ setup but before cold-start completion;
3. a default-no-op `InterfaceVariant::prepare_rf_switch_tx()` hook reached via
   `RadioKind::prepare_rf_switch_tx()` after modem, power and standby
   normalization but before packet parameters, channel, FIFO and IRQ setup; and
4. `LoRa::enter_standby()` records `RadioMode::Standby` only after the hardware
   standby command succeeds, preventing a later TX preparation from issuing a
   redundant standby command after an early RF-path gate is armed.

- all existing variants return `None` and preserve the upstream command trace;
- override values are validated before any PA/OCP command is written;
- malformed PA duty, `hpMax`, encoded power or OCP fields fail with
  `RadioError::InvalidConfiguration`;
- the existing SX1262 TxClamp read-modify-write remains first;
- a valid override emits `SetPaConfig`, the optional OCP register write and
  `SetTxParams` in that order;
- all existing interface and radio-kind implementations retain no-op
  post-initialization and early packet-preparation behavior;
- LoRa invokes the post-initialization hook only after chip, initial-power and
  IRQ setup succeeds, and SX126x delegates both board hooks to its interface;
- the existing final `enable_rf_switch_tx()` gate remains immediately before
  `SetTx`;
- a failed public standby command still returns its original error without
  changing the software mode; and
- no delay or standby-clock behavior is changed.

The initial consumer is the Heltec Tracker V2 TX HIL interoperability image.
Its compile-time-fixed 0 dBm modem request selects the RNode-compatible
`SetPaConfig(0x04, 0x07, 0x00, 0x01)`, OCP `0x28`, and encoded power `0x00`.
Any other requested or clamped power returns `None`; callers cannot select a
different power through the HIL API. The same HIL interface completes cold
start by enabling and settling the external FEM with CTX low, then uses a
two-stage one-shot TX authorization: it asserts CTX after PA/OCP and standby
normalization, keeps CTX high through packet/FIFO preparation, and consumes the
prepared state again at the final pre-`SetTx` gate. No upstream issue or pull
request has been opened.
