import { describe, expect, test } from "bun:test";

import type { PhoneLocationObservationView } from "../generated/api.ts";
import { FieldTelemetryController } from "./field-telemetry.ts";
import {
  type FieldTelemetryPreferenceStore,
  MemoryFieldTelemetryPreferenceStore,
  serializeFieldTelemetryPreference,
} from "./field-telemetry-preference.ts";
import type { ForegroundPhoneLocationTelemetry } from "./phone-location.ts";

const sample = {
  state: "available",
  latitude_e6: 42_357_111,
  longitude_e6: -71_061_924,
  altitude_mm: 17_234,
  horizontal_accuracy_mm: 8_250,
  vertical_accuracy_mm: 3_125,
  captured_at_unix_ms: 1_785_084_000_123,
  authorization: "precise",
  source: "foreground_stream",
  mocked: false,
} as const satisfies PhoneLocationObservationView;

function preference(enabled: boolean): MemoryFieldTelemetryPreferenceStore {
  return new MemoryFieldTelemetryPreferenceStore(serializeFieldTelemetryPreference(enabled));
}

describe("field telemetry controller", () => {
  test("feeds foreground fixes to the local runtime and explicitly disables future stamps", async () => {
    const updates: PhoneLocationObservationView[] = [];
    const sinks: Array<(observation: PhoneLocationObservationView) => void | Promise<void>> = [];
    let removed = 0;
    const controller = new FieldTelemetryController(
      {
        phoneLocationObservation: async () => ({ state: "unavailable", reason: "not_observed" }),
        updatePhoneLocationObservation: async (observation) => {
          updates.push(observation);
          return observation;
        },
      },
      async (nextSink) => {
        sinks.push(nextSink);
        return { collecting: true, remove: () => (removed += 1) };
      },
    );

    await controller.activate("e290-a");
    await controller.setEnabled(true);
    expect(controller.state.runState).toBe("active");
    expect(sinks).toHaveLength(1);
    await sinks[0]?.(sample);
    expect(controller.state.observation).toEqual(sample);

    await controller.setEnabled(false);
    expect(removed).toBe(1);
    expect(updates.at(-1)).toEqual({ state: "unavailable", reason: "telemetry_disabled" });
    expect(controller.state.runState).toBe("disabled");
  });

  test("keeps the last timestamped fix when foreground collection is suspended", async () => {
    const controller = new FieldTelemetryController(
      {
        phoneLocationObservation: async () => sample,
        updatePhoneLocationObservation: async (observation) => observation,
      },
      async () => ({ collecting: true, remove() {} }),
      preference(true),
    );
    await controller.activate("e290-a");
    controller.suspend();

    expect(controller.state.observation).toEqual(sample);
    expect(controller.state.runState).toBe("inactive");
  });

  test("clears the previous profile location while loading a replacement profile", async () => {
    let resolveReplacement = (_value: PhoneLocationObservationView) => {};
    let reads = 0;
    const controller = new FieldTelemetryController(
      {
        phoneLocationObservation: () => {
          reads += 1;
          if (reads === 1) return Promise.resolve(sample);
          return new Promise((resolve) => {
            resolveReplacement = resolve;
          });
        },
        updatePhoneLocationObservation: async (observation) => observation,
      },
      async () => ({ collecting: true, remove() {} }),
      preference(true),
    );

    await controller.activate("e290-a");
    expect(controller.state.observation).toEqual(sample);
    const replacement = controller.activate("e290-b");
    expect(controller.state.deviceKey).toBe("e290-b");
    expect(controller.state.observation).toBeNull();

    const unavailable = {
      state: "unavailable",
      reason: "not_observed",
    } as const satisfies PhoneLocationObservationView;
    resolveReplacement(unavailable);
    await replacement;
    expect(controller.state.observation).toEqual(unavailable);
  });

  test("removes a watcher that finishes starting after telemetry was disabled", async () => {
    let finishStart = (_subscription: ForegroundPhoneLocationTelemetry) => {};
    let reportStarted = () => {};
    const started = new Promise<void>((resolve) => {
      reportStarted = resolve;
    });
    let removed = 0;
    const controller = new FieldTelemetryController(
      {
        phoneLocationObservation: async () => ({ state: "unavailable", reason: "not_observed" }),
        updatePhoneLocationObservation: async (observation) => observation,
      },
      () => {
        reportStarted();
        return new Promise((resolve) => {
          finishStart = resolve;
        });
      },
    );

    await controller.activate("e290-a");
    const enabling = controller.setEnabled(true);
    await started;
    await controller.setEnabled(false);
    finishStart({ collecting: true, remove: () => (removed += 1) });
    await enabling;

    expect(removed).toBe(1);
    expect(controller.state.enabled).toBeFalse();
    expect(controller.state.runState).toBe("disabled");
  });

  test("reports unavailable telemetry as inactive instead of claiming a live watcher", async () => {
    const controller = new FieldTelemetryController(
      {
        phoneLocationObservation: async () => ({ state: "unavailable", reason: "not_observed" }),
        updatePhoneLocationObservation: async (observation) => observation,
      },
      async (sink) => {
        await sink({ state: "unavailable", reason: "services_disabled" });
        return { collecting: false, remove() {} };
      },
    );

    await controller.activate("e290-a");
    await controller.setEnabled(true);
    expect(controller.state.observation).toEqual({
      state: "unavailable",
      reason: "services_disabled",
    });
    expect(controller.state.runState).toBe("inactive");
  });

  test("restores a durable opt-in after controller recreation without another toggle", async () => {
    const store = new MemoryFieldTelemetryPreferenceStore();
    const client = {
      phoneLocationObservation: async () =>
        ({
          state: "unavailable",
          reason: "not_observed",
        }) as const satisfies PhoneLocationObservationView,
      updatePhoneLocationObservation: async (observation: PhoneLocationObservationView) =>
        observation,
    };
    let starts = 0;
    const starter = async () => {
      starts += 1;
      return { collecting: true, remove() {} };
    };

    const first = new FieldTelemetryController(client, starter, store);
    await first.activate("e290-a");
    await first.setEnabled(true);
    expect(await store.load()).toBeTrue();
    first.dispose();

    const restarted = new FieldTelemetryController(client, starter, store);
    await restarted.activate("e290-a");
    expect(restarted.state.enabled).toBeTrue();
    expect(restarted.state.runState).toBe("active");
    expect(starts).toBe(2);
  });

  test("durable opt-out sanitizes every newly activated appliance", async () => {
    let activeDevice = "e290-a";
    const updates: Array<{ device: string; observation: PhoneLocationObservationView }> = [];
    let starts = 0;
    const controller = new FieldTelemetryController(
      {
        phoneLocationObservation: async () => sample,
        updatePhoneLocationObservation: async (observation) => {
          updates.push({ device: activeDevice, observation });
          return observation;
        },
      },
      async () => {
        starts += 1;
        return { collecting: true, remove() {} };
      },
      preference(false),
    );

    await controller.activate(activeDevice);
    activeDevice = "e290-b";
    await controller.activate(activeDevice);

    expect(starts).toBe(0);
    expect(updates).toEqual([
      {
        device: "e290-a",
        observation: { state: "unavailable", reason: "telemetry_disabled" },
      },
      {
        device: "e290-b",
        observation: { state: "unavailable", reason: "telemetry_disabled" },
      },
    ]);
  });

  test("does not start collection when a durable opt-in cannot be saved", async () => {
    let starts = 0;
    const store: FieldTelemetryPreferenceStore = {
      load: async () => false,
      save: async () => {
        throw new Error("disk unavailable");
      },
    };
    const controller = new FieldTelemetryController(
      {
        phoneLocationObservation: async () => ({ state: "unavailable", reason: "not_observed" }),
        updatePhoneLocationObservation: async (observation) => observation,
      },
      async () => {
        starts += 1;
        return { collecting: true, remove() {} };
      },
      store,
    );

    await controller.activate("e290-a");
    await controller.setEnabled(true);
    expect(starts).toBe(0);
    expect(controller.state.enabled).toBeFalse();
    expect(controller.state.runState).toBe("disabled");
    expect(controller.state.error).toContain("disk unavailable");
  });
});
