export type ApplianceWorkspace = "activity" | "connectivity" | "lxmf" | "map" | "nomad";

export type MessagePane = "chats" | "contacts";

export type NetworkSubtopic = "overview" | "radio" | "routes" | "peers" | "discovery";

export interface WorkspaceDestination {
  readonly label: string;
  readonly labelWide: string;
  readonly path: string;
  readonly workspace: ApplianceWorkspace;
}

export const WORKSPACE_DESTINATIONS: readonly WorkspaceDestination[] = [
  { label: "Chat", labelWide: "Messages", path: "/", workspace: "lxmf" },
  { label: "Browse", labelWide: "Browse", path: "/nomad", workspace: "nomad" },
  { label: "Activity", labelWide: "Activity", path: "/activity", workspace: "activity" },
  { label: "Map", labelWide: "Map", path: "/map", workspace: "map" },
  { label: "Net", labelWide: "Network", path: "/net", workspace: "connectivity" },
];

export function workspaceFromPathname(pathname: string): ApplianceWorkspace | null {
  if (pathname === "/") return "lxmf";
  if (pathname.startsWith("/nomad")) return "nomad";
  if (pathname.startsWith("/activity")) return "activity";
  if (pathname.startsWith("/map")) return "map";
  if (pathname.startsWith("/net")) return "connectivity";
  return null;
}

export function workspaceTitle(workspace: ApplianceWorkspace): string {
  switch (workspace) {
    case "lxmf":
      return "Messages";
    case "nomad":
      return "NomadNet";
    case "activity":
      return "Activity";
    case "map":
      return "Map";
    case "connectivity":
      return "Network";
  }
}

export function pathForWorkspace(workspace: ApplianceWorkspace): string {
  switch (workspace) {
    case "lxmf":
      return "/";
    case "nomad":
      return "/nomad";
    case "activity":
      return "/activity";
    case "map":
      return "/map";
    case "connectivity":
      return "/net";
  }
}
