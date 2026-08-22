import { describe, expect, test } from "bun:test";

import { completesAdditionalApplianceEnrollment, onboardingPresentation } from "./onboarding.ts";

describe("Reticulum enrollment presentation", () => {
  test("a saved management application is ready", () => {
    expect(
      onboardingPresentation({
        available: true,
        lifecycle: { state: "ready", managementDestination: "11".repeat(16) },
      }).ready,
    ).toBeTrue();
  });

  test("first-run setup asks for a verified announce", () => {
    const presentation = onboardingPresentation({
      available: true,
      lifecycle: { state: "needs_candidate" },
    });
    expect(presentation.ready).toBeFalse();
    expect(presentation.instruction).toContain("verified appliance management announce");
  });

  test("authorization work is described as an identified Reticulum request", () => {
    const presentation = onboardingPresentation({
      available: true,
      lifecycle: { state: "authorizing", managementDestination: "11".repeat(16) },
    });
    expect(presentation.title).toBe("Authorizing Reticulum identity");
    expect(presentation.instruction).toContain("identified request");
  });

  test("an existing ready profile does not complete additional enrollment", () => {
    expect(
      completesAdditionalApplianceEnrollment(
        {
          available: true,
          lifecycle: { state: "ready", managementDestination: "11".repeat(16) },
        },
        null,
      ),
    ).toBeFalse();
    expect(
      completesAdditionalApplianceEnrollment(
        {
          available: true,
          lifecycle: { state: "ready", managementDestination: "11".repeat(16) },
        },
        "22".repeat(16),
      ),
    ).toBeFalse();
  });

  test("the selected management destination completes additional enrollment", () => {
    expect(
      completesAdditionalApplianceEnrollment(
        {
          available: true,
          lifecycle: { state: "ready", managementDestination: "AA".repeat(16) },
        },
        `  ${"aa".repeat(16)}  `,
      ),
    ).toBeTrue();
  });
});
