/** One verified appliance management application observed by the local PRNS node. */
export interface ReticulumApplianceCandidate {
  readonly managementDestination: string;
  readonly lxmfDestination: string;
  readonly interfaceId: string;
  readonly hops: number;
}

/** Cancellation for one bounded PRNS candidate verification pass. */
export interface ReticulumDiscoveryOptions {
  readonly signal?: AbortSignal;
}
