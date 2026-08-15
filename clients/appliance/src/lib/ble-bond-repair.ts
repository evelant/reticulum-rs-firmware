export type BleBondRepairStage =
  | "searching_recovery_advertisement"
  | "waiting_for_physical_presence"
  | "reopening_authenticated_link";

export type BleBondRepairProgress = (stage: BleBondRepairStage) => void;

export function bleBondRepairProgressMessage(stage: BleBondRepairStage, label: string): string {
  switch (stage) {
    case "searching_recovery_advertisement":
      return `Finding ${label} in BLE Recovery…`;
    case "waiting_for_physical_presence":
      return `Connected to ${label}. Hold GPIO21 for about two seconds now, then enter the Bluetooth code shown on the board.`;
    case "reopening_authenticated_link":
      return `Bluetooth security completed for ${label}. Reopening the authenticated appliance link…`;
  }
}
