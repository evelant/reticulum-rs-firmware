import type { OnboardingStage, OnboardingView } from "../generated/api.ts";
import type { BleCandidate } from "./ble-central-types.ts";

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

export interface BleDiscoveryPresentation {
  readonly available: boolean;
  readonly instruction: string;
  readonly title: string;
}

export const BLE_SECURITY_CONTINUE_LABEL = "Continue after holding GPIO21";

export function bleDiscoveryPresentation(
  view: OnboardingView,
  scannerAvailable: boolean,
): BleDiscoveryPresentation {
  const lifecycle = view.snapshot?.lifecycle;
  const available =
    scannerAvailable &&
    ((view.method === "credential_import" && lifecycle?.state === "needs_pairing") ||
      (view.method === "managed_pairing" &&
        lifecycle !== undefined &&
        lifecycle.state !== "credential_ready" &&
        lifecycle.state !== "stopped"));
  return {
    available,
    title: "Nearby appliances",
    instruction:
      "Find nearby boards without connecting. Discovery does not send credentials. Select the physical board you intend to pair; only the following explicit secure-pairing action opens that exact BLE link.",
  };
}

export function bleCandidateName(candidate: BleCandidate): string {
  return candidate.peripheralName?.trim() || "Unnamed appliance";
}

export function bleCandidateDetails(candidate: BleCandidate): string {
  const signal = candidate.rssi === undefined ? null : `${candidate.rssi} dBm`;
  return [signal, candidate.peripheralId]
    .filter((value): value is string => value !== null)
    .join(" · ");
}

export function selectedBleCandidate(
  candidates: readonly BleCandidate[],
  selectedPeripheralId: string | null,
): BleCandidate | null {
  if (selectedPeripheralId === null) return null;
  const normalized = selectedPeripheralId.toLowerCase();
  return (
    candidates.find((candidate) => candidate.peripheralId.toLowerCase() === normalized) ?? null
  );
}

const PRESENCE_INSTRUCTION =
  "On the selected E290, release the middle button labelled 21, between RST and BOOT, " +
  "then hold it continuously for at least 2 seconds. " +
  "Keep holding until this screen advances. Once recognized, the five-minute setup window leaves " +
  "time to enter the six digits shown on that board in the phone's Bluetooth prompt.";

function workingInstruction(stage: OnboardingStage): string {
  switch (stage) {
    case "waiting_for_ble_security":
      return (
        "The selected BLE link is open and no appliance data has been sent. " +
        "First release GPIO21, then hold it continuously for at least 2 seconds; holding for " +
        "3 seconds is fine and there is no narrow start-time requirement. For a new Bluetooth " +
        "bond, enter the six digits shown on the board into the iOS prompt within 30 seconds, " +
        "then return here. If this phone has paired with this board before, iOS silently reuses " +
        "the saved bond and shows no code or prompt. After the hold (and code, when requested), " +
        `tap ${BLE_SECURITY_CONTINUE_LABEL}. The app will keep this exact link open and wait ` +
        "for the board instead of imposing a short confirmation timeout. Do not wait for the " +
        "board to say PAIRED before continuing: that screen appears only after the following " +
        "application credential exchange finishes."
      );
    case "waiting_for_initialization_presence":
    case "waiting_for_pairing_presence":
    case "waiting_for_abort_presence":
      return PRESENCE_INSTRUCTION;
    case "opening_device":
      return "Opening the selected device.";
    case "checking_initialization":
      return "Checking the device's public initialization state.";
    case "initializing":
      return "Initializing owner credentials. Keep the device connected.";
    case "resuming":
    case "proving":
      return "Completing the retained pairing proof. Keep the device connected.";
    case "activating":
      return "Publishing the active credential. Keep the device nearby until the app reconnects.";
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
          "Verify the transfer filename and physical board first. Each valid board is retained as an isolated profile; after setup, use Appliances to switch boards or add another one.",
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
          "Rust owns credential generation and pairing state while this alpha relays only opaque, bounded BLE fragments through the app.",
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
