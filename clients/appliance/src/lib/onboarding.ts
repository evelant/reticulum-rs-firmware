export type OnboardingLifecycle =
  | { readonly state: "unavailable" }
  | { readonly state: "needs_candidate" }
  | { readonly state: "ready"; readonly managementDestination: string }
  | { readonly state: "authorizing"; readonly managementDestination: string }
  | { readonly state: "faulted"; readonly reason: string };

/** App-local state for Reticulum management-identity enrollment. */
export interface OnboardingView {
  readonly available: boolean;
  readonly lifecycle: OnboardingLifecycle;
}

export interface OnboardingPresentation {
  readonly instruction: string;
  readonly ready: boolean;
  readonly title: string;
}

/**
 * Additional enrollment completes only when the selected management
 * destination becomes active. The pre-existing active profile is also
 * reported as ready while discovery is open, so readiness alone cannot close
 * the add-appliance flow.
 */
export function completesAdditionalApplianceEnrollment(
  view: OnboardingView,
  expectedManagementDestination: string | null,
): boolean {
  return (
    expectedManagementDestination !== null &&
    view.lifecycle.state === "ready" &&
    view.lifecycle.managementDestination.trim().toLowerCase() ===
      expectedManagementDestination.trim().toLowerCase()
  );
}

/** Present app-owned Reticulum enrollment without a bearer credential model. */
export function onboardingPresentation(view: OnboardingView): OnboardingPresentation {
  const { lifecycle } = view;
  if (lifecycle.state === "ready") {
    return {
      instruction: "The saved Reticulum management application is ready.",
      ready: true,
      title: "Appliance authorized",
    };
  }
  if (lifecycle.state === "authorizing") {
    return {
      instruction:
        "Keep the physical enrollment window open while the app verifies its identified request.",
      ready: false,
      title: "Authorizing Reticulum identity",
    };
  }
  if (lifecycle.state === "faulted") {
    return {
      instruction: lifecycle.reason.replaceAll("_", " "),
      ready: false,
      title: "Enrollment needs attention",
    };
  }
  if (lifecycle.state === "unavailable") {
    return {
      instruction: "Enrollment is managed by the host service operator.",
      ready: true,
      title: "Appliance selected",
    };
  }
  return {
    instruction: "Choose a verified appliance management announce.",
    ready: false,
    title: "Choose a Reticulum appliance",
  };
}
