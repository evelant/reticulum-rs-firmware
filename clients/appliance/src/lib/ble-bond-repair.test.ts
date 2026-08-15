import { describe, expect, test } from "bun:test";

import { bleBondRepairProgressMessage } from "./ble-bond-repair.ts";

describe("BLE bond repair progress", () => {
  test("turns the live security barrier into an explicit GPIO21 prompt", () => {
    expect(bleBondRepairProgressMessage("waiting_for_physical_presence", "e13e88")).toBe(
      "Connected to e13e88. Hold GPIO21 for about two seconds now, then enter the Bluetooth code shown on the board.",
    );
  });

  test("distinguishes recovery discovery from authenticated-link reopening", () => {
    expect(bleBondRepairProgressMessage("searching_recovery_advertisement", "e13e88")).toBe(
      "Finding e13e88 in BLE Recovery…",
    );
    expect(bleBondRepairProgressMessage("reopening_authenticated_link", "e13e88")).toBe(
      "Bluetooth security completed for e13e88. Reopening the authenticated appliance link…",
    );
  });
});
