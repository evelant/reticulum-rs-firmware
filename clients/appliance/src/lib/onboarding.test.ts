import { describe, expect, test } from "bun:test";

import type { OnboardingState, OnboardingView } from "../generated/api.ts";
import {
  bleCandidateDetails,
  bleCandidateName,
  bleDiscoveryPresentation,
  onboardingPresentation,
  selectedBleCandidate,
} from "./onboarding.ts";

function view(lifecycle: OnboardingState): OnboardingView {
  return {
    available: true,
    method: "managed_pairing",
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

describe("native credential import presentation", () => {
  test("labels the alpha import honestly and does not imply in-app BLE pairing", () => {
    const presentation = onboardingPresentation({
      available: true,
      method: "credential_import",
      snapshot: {
        revision: 0,
        usb_serial: "",
        lifecycle: { state: "needs_pairing" },
      },
    });

    expect(presentation.ready).toBeFalse();
    expect(presentation.canStart).toBeTrue();
    expect(presentation.startLabel).toBe("Choose credential");
    expect(presentation.identifierLabel).toBeNull();
    expect(presentation.instruction).toContain("qualified USB pairing workflow");
    expect(presentation.instruction).toContain("selecting another board");
    expect(presentation.instruction).toContain("clearing local app data");
    expect(presentation.instruction).toContain("Full in-app BLE pairing remains future work");
  });

  test("marks an imported credential ready and shows its exact target node", () => {
    const presentation = onboardingPresentation({
      available: true,
      method: "credential_import",
      snapshot: {
        revision: 0,
        usb_serial: "reticulum-e290-e13e88",
        lifecycle: { state: "credential_ready" },
      },
    });

    expect(presentation.ready).toBeTrue();
    expect(presentation.identifierLabel).toBe("Target node");
    expect(presentation.canStart).toBeFalse();
  });

  test("does not offer overwrite for an invalid create-only credential", () => {
    const presentation = onboardingPresentation({
      available: true,
      method: "credential_import",
      snapshot: {
        revision: 0,
        usb_serial: "",
        lifecycle: { state: "faulted", reason: "invalid_credential_artifact" },
      },
    });

    expect(presentation.ready).toBeFalse();
    expect(presentation.canStart).toBeFalse();
    expect(presentation.canRefresh).toBeTrue();
    expect(presentation.instruction).toContain("will not overwrite");
  });

  test("distinguishes a canonical credential with no exact BLE target from corruption", () => {
    const presentation = onboardingPresentation({
      available: true,
      method: "credential_import",
      snapshot: {
        revision: 0,
        usb_serial: "",
        lifecycle: { state: "faulted", reason: "unsupported_device" },
      },
    });

    expect(presentation.ready).toBeFalse();
    expect(presentation.title).toContain("BLE profile");
    expect(presentation.instruction).toContain("canonical");
    expect(presentation.instruction).toContain("No untargeted scan");
  });
});

describe("credential-free BLE discovery presentation", () => {
  const missingCredential: OnboardingView = {
    available: true,
    method: "credential_import",
    snapshot: {
      revision: 0,
      usb_serial: "",
      lifecycle: { state: "needs_pairing" },
    },
  };

  test("offers nearby appliance discovery only in the missing-credential native state", () => {
    const discovery = bleDiscoveryPresentation(missingCredential, true);
    expect(discovery).toMatchObject({
      available: true,
      title: "Nearby appliances",
    });
    expect(discovery.instruction).toContain("without connecting");
    expect(discovery.instruction).toContain("does not send credentials");

    expect(bleDiscoveryPresentation(missingCredential, false).available).toBeFalse();
    expect(
      bleDiscoveryPresentation(
        {
          ...missingCredential,
          snapshot: {
            revision: 0,
            usb_serial: "",
            lifecycle: { state: "credential_ready" },
          },
        },
        true,
      ).available,
    ).toBeFalse();
  });

  test("never selects a board merely because discovery returned one", () => {
    const candidates = [
      { peripheralId: "board-a", peripheralName: "Reticulum A", rssi: -61 },
    ] as const;

    expect(selectedBleCandidate(candidates, null)).toBeNull();
    expect(selectedBleCandidate(candidates, "board-b")).toBeNull();
    expect(selectedBleCandidate(candidates, "BOARD-A")).toEqual(candidates[0]);
  });

  test("presents a stable identity and optional signal without inventing a name", () => {
    expect(bleCandidateName({ peripheralId: "board-a", peripheralName: "  Ridge node  " })).toBe(
      "Ridge node",
    );
    expect(bleCandidateName({ peripheralId: "board-b" })).toBe("Unnamed appliance");
    expect(bleCandidateDetails({ peripheralId: "board-a", rssi: -72 })).toBe("-72 dBm · board-a");
    expect(bleCandidateDetails({ peripheralId: "board-b" })).toBe("board-b");
  });
});
