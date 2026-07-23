export interface DraftIdentity {
  readonly idempotencyKey: string;
  readonly timestampMs: number;
}

export function ensureDraftIdentity(
  current: DraftIdentity | null,
  createKey: () => string,
  now: () => number,
): DraftIdentity {
  return current ?? { idempotencyKey: createKey(), timestampMs: now() };
}
