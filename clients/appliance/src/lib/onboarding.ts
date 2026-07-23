import type { OnboardingStage, OnboardingView } from "../generated/api.ts";

export interface OnboardingPresentation {
  readonly ready: boolean;
  readonly title: string;
  readonly instruction: string;
  readonly startLabel: string;
  readonly identifierLabel: string | null;
  readonly canStart: boolean;
  readonly canResume: boolean;
  readonly canAbort: boolean;
  readonly canRefresh: boolean;
}

const PRESENCE_INSTRUCTION =
  "On the selected E290, release the middle button labelled 21, between RST and BOOT, " +
  "then hold it continuously for at least 2 seconds. " +
  "Keep holding until this screen advances; the 60-second window begins when the hold is recognized.";

function workingInstruction(stage: OnboardingStage): string {
  switch (stage) {
    case "waiting_for_initialization_presence":
    case "waiting_for_pairing_presence":
    case "waiting_for_abort_presence":
      return PRESENCE_INSTRUCTION;
    case "opening_device":
      return "Opening the selected device by its stable USB serial.";
    case "checking_initialization":
      return "Checking the device's public initialization state.";
    case "initializing":
      return "Initializing owner credentials. Keep the device connected.";
    case "resuming":
    case "proving":
      return "Completing the retained pairing proof. Keep the device connected.";
    case "activating":
      return "Activating the credential. Do not disconnect until reset is requested.";
  }
}

function baseOnboardingPresentation(view: OnboardingView): Omit<
  OnboardingPresentation,
  "canRefresh" | "identifierLabel" | "startLabel"
> & {
  readonly canRefresh?: boolean;
  readonly identifierLabel?: string | null;
  readonly startLabel?: string;
} {
  if (view.method === "credential_import") {
    const lifecycle = view.snapshot?.lifecycle;
    if (lifecycle?.state === "credential_ready") {
      return {
        ready: true,
        title: "Credential ready",
        instruction:
          "The activated credential is installed in app-private storage and ready for authenticated use.",
        canStart: false,
        canResume: false,
        canAbort: false,
        identifierLabel: "Target node",
        startLabel: "Choose credential",
      };
    }
    if (lifecycle?.state === "needs_pairing") {
      return {
        ready: false,
        title: "Import an activated credential",
        instruction:
          "Choose the .rdpkey produced by the qualified USB pairing workflow. " +
          "The app copies it into app-private storage without reading its secret bytes in TypeScript. " +
          "Verify the transfer filename and physical board first: selecting another board's valid credential binds this create-only app install to that board, and changing it requires clearing local app data. " +
          "Full in-app BLE pairing remains future work.",
        canStart: true,
        canResume: false,
        canAbort: false,
        identifierLabel: null,
        startLabel: "Choose credential",
      };
    }
    if (lifecycle?.state === "faulted") {
      const unsupportedDevice = lifecycle.reason === "unsupported_device";
      return {
        ready: false,
        title: unsupportedDevice
          ? "Credential does not identify this BLE profile"
          : "Credential needs attention",
        instruction: unsupportedDevice
          ? "The credential is canonical, but it does not provide an exact target for this E290 BLE connector. No untargeted scan will run."
          : "The app-private credential is invalid. This create-only alpha flow will not overwrite it; clear this app's local data before importing again.",
        canStart: false,
        canResume: false,
        canAbort: false,
        canRefresh: true,
        identifierLabel: null,
        startLabel: "Choose credential",
      };
    }
    return {
      ready: false,
      title: "Credential import unavailable",
      instruction: "Recheck local credential state. Full in-app BLE pairing remains future work.",
      canStart: false,
      canResume: false,
      canAbort: false,
      canRefresh: true,
      identifierLabel: null,
      startLabel: "Choose credential",
    };
  }

  if (!view.available || view.snapshot === null) {
    return {
      ready: true,
      title: "External credential",
      instruction: "Managed onboarding is not enabled for this process.",
      canStart: false,
      canResume: false,
      canAbort: false,
    };
  }

  const lifecycle = view.snapshot.lifecycle;
  switch (lifecycle.state) {
    case "credential_ready":
      return {
        ready: true,
        title: "Pairing complete",
        instruction: "The device credential is ready for authenticated use.",
        canStart: false,
        canResume: false,
        canAbort: false,
      };
    case "needs_pairing":
      return {
        ready: false,
        title: "Set up this node",
        instruction:
          "Release the selected E290's middle button labelled 21 before starting. " +
          "After you select Start, follow the hold prompt; " +
          "the appliance keeps credential secrets out of this app.",
        canStart: true,
        canResume: false,
        canAbort: false,
      };
    case "abort_required":
      return {
        ready: false,
        title: "Recover an interrupted start",
        instruction:
          "A lost Begin may have created device Pending state. Only a physically confirmed abort is safe.",
        canStart: false,
        canResume: false,
        canAbort: true,
      };
    case "resume_available":
      return {
        ready: false,
        title: "Resume pairing",
        instruction:
          "A complete Pending credential is safely retained. Resume it, or explicitly abort it with physical presence.",
        canStart: false,
        canResume: true,
        canAbort: true,
      };
    case "activation_ambiguous":
      return {
        ready: false,
        title: "Activation needs reconciliation",
        instruction:
          "Activate may already have committed. Do not resume, abort, or guess Active; retain the device and profile for authenticated recovery.",
        canStart: false,
        canResume: false,
        canAbort: false,
      };
    case "working":
      return {
        ready: false,
        title: lifecycle.stage.replaceAll("_", " "),
        instruction: workingInstruction(lifecycle.stage),
        canStart: false,
        canResume: false,
        canAbort: false,
      };
    case "awaiting_usb_reset":
      return {
        ready: false,
        title: "Reset the USB connection",
        instruction: lifecycle.observed_disconnect
          ? "USB disappearance was observed. Reconnect the node and wait for authentication."
          : "Activation is saved. Reset or unplug the node until it disappears, then reconnect it; an app acknowledgement cannot replace a real USB reset.",
        canStart: false,
        canResume: false,
        canAbort: false,
      };
    case "faulted":
      return {
        ready: false,
        title: "Onboarding needs attention",
        instruction: lifecycle.reason.replaceAll("_", " "),
        canStart: false,
        canResume: false,
        canAbort: false,
        canRefresh: true,
      };
    case "stopped":
      return {
        ready: false,
        title: "Onboarding stopped",
        instruction: "Restart the local appliance service to continue.",
        canStart: false,
        canResume: false,
        canAbort: false,
      };
  }
}

export function onboardingPresentation(view: OnboardingView): OnboardingPresentation {
  const presentation = baseOnboardingPresentation(view);
  return {
    ...presentation,
    canRefresh: presentation.canRefresh ?? false,
    identifierLabel:
      presentation.identifierLabel ?? (view.method === "managed_pairing" ? "USB serial" : null),
    startLabel: presentation.startLabel ?? "Start pairing",
  };
}
