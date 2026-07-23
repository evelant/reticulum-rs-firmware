import { describe, expect, test } from "bun:test";

import type { OnboardingState, OnboardingView } from "../generated/api.ts";
import { onboardingPresentation } from "./onboarding.ts";

function view(lifecycle: OnboardingState): OnboardingView {
  return {
    available: true,
    snapshot: { revision: 1, usb_serial: "ACA704E13E88", lifecycle },
  };
}

describe("onboarding recovery presentation", () => {
  test("identifies the E290 user key without confusing it with reset or boot", () => {
    const before = onboardingPresentation(view({ state: "needs_pairing" }));
    const waiting = onboardingPresentation(
      view({ state: "working", stage: "waiting_for_pairing_presence" }),
    );
    expect(before.instruction).toContain("middle button labelled 21");
    expect(waiting.instruction).toContain("between RST and BOOT");
  });

  test("never offers an unsafe action after activation ambiguity", () => {
    const presentation = onboardingPresentation(view({ state: "activation_ambiguous" }));
    expect(presentation.ready).toBeFalse();
    expect(presentation.canStart).toBeFalse();
    expect(presentation.canResume).toBeFalse();
    expect(presentation.canAbort).toBeFalse();
    expect(presentation.canRefresh).not.toBeTrue();
  });

  test("requires a real disconnect before describing reconnection", () => {
    const before = onboardingPresentation(
      view({ state: "awaiting_usb_reset", observed_disconnect: false }),
    );
    const after = onboardingPresentation(
      view({ state: "awaiting_usb_reset", observed_disconnect: true }),
    );
    expect(before.instruction).toContain("disappears");
    expect(after.instruction).toContain("disappearance was observed");
  });

  test("offers an explicit local recheck only from faulted state", () => {
    const presentation = onboardingPresentation(
      view({ state: "faulted", reason: "device_unavailable" }),
    );
    expect(presentation.canRefresh).toBeTrue();
    expect(presentation.canStart).toBeFalse();
    expect(presentation.canResume).toBeFalse();
    expect(presentation.canAbort).toBeFalse();
  });
});
