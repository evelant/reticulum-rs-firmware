# Local `lora-phy` 3.0.1 patch inventory

Source: crates.io `lora-phy` 3.0.1, registry checksum
`61471c3b2909789e3332083577f6cf6c41a4fcf37674ef15156bcbb20504ac65`.

The published driver derives every SX1262 high-power PA command from requested
output power, offers no board hooks around RF-front-end initialization or early
TX preparation, and does not synchronize its public standby operation with the
software radio mode. Its continuous-receive surface also calls `do_rx()` from
every `LoRa::rx()` invocation and exposes no public split between the
cancellation-safe IRQ wait and the non-cancel-safe SPI work that drains an IRQ.
That forces an otherwise continuous caller to issue another `SetRx` between
packets and creates a receive blind interval.

This checkout carries the following reviewed changes:

1. An atomic, default-`None` `Sx126xVariant::high_power_pa_override()` hook. Its
   value binds PA duty cycle, `hpMax`, raw signed `SetTxParams` power byte, and
   optional OCP trim; every field is validated before any PA/OCP command.
2. Default-no-op `InterfaceVariant::complete_rf_switch_initialization()` and
   `prepare_rf_switch_tx()` hooks, delegated through `RadioKind`. The first runs
   after LoRa-chip, initial-power, and IRQ setup; the second runs after modem,
   power, and standby normalization but before packet/channel/FIFO/IRQ setup.
3. `LoRa::enter_standby()` synchronizes `RadioMode::Standby` immediately after
   the hardware standby command succeeds. It then disables IRQ/DIO routing and
   clears pending IRQ status. The same explicit quiescent transition precedes
   modem reconfiguration for CAD or TX.
4. A public `ReceiveIrq` result plus `LoRa::start_rx()` and
   `LoRa::process_rx_irq()`. Continuous users arm once, await DIO, and drain
   successive packet IRQs without another `SetRx`; only the IRQ wait is safe to
   cancel, while start and IRQ processing must run to completion once polled.
5. SX126x receive classification treats preamble, sync-word, and valid-header
   IRQs as progress; header/CRC errors and receive timeout are terminal.
   Invalid-frame error wins over `RxDone`, valid `RxDone` wins over timeout, and
   timeout wins over progress.
6. LoRaWAN continuous setup now starts RX once, and its receive method drains
   IRQs through the arm-once API rather than calling `LoRa::rx()` per packet.
7. `RadioKind::clear_irq_status()` implementations clear all pending SX126x or
   SX127x flags during an explicit quiesce. Ordinary SX127x IRQ processing
   clears only the captured flag snapshot so it cannot erase a distinct IRQ
   that latches between status read and clear.

The PA override preserves the existing SX1262 TxClamp read-modify-write first,
then emits `SetPaConfig`, optional OCP, and `SetTxParams` in that order. Existing
variants return `None`; existing interface implementations inherit no-op board
hooks. The final `enable_rf_switch_tx()` gate remains immediately before
`SetTx`. A failed standby command retains the original error and prior software
mode; if later IRQ cleanup fails, software mode remains truthfully Standby and
the project owner fails closed.

Single-shot receive retains its upstream command shape except for the repaired
terminal classification and explicit cleanup. Continuous receive intentionally
changes the command trace: a session has one `SetRx(0xffffff)`, no standby or
rearm between successfully drained packets, and a standby/IRQ-disable/IRQ-clear
sequence before CAD or TX. The project adds no artificial inter-frame TX delay;
interoperable RNode senders may transmit split frames back-to-back. The project
radio owner separately applies a recoverable progress deadline and reissues
continuous `SetRx` when a false preamble leaves the modem latched.

The historical initial consumer was the Heltec Tracker V2 TX HIL. Its fixed
0 dBm modem request selects `SetPaConfig(0x04, 0x07, 0x00, 0x01)`, OCP `0x28`,
and encoded power `0x00`, while its private hooks settle and one-shot-arm the
external FEM. Current consumers also include the shared board-neutral RNode
radio core, the permanent E290 LoRa actor, and the optional generic LoRaWAN
continuous path. No upstream issue or pull request has been opened.
