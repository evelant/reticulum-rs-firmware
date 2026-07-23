export interface NativeCoreStatus {
  readonly label: string;
  readonly state: "ready" | "faulted";
}
