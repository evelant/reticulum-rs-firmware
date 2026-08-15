import type { MessageLocationView } from "../generated/api.ts";
import type { DraftIdentity } from "./draft.ts";

export interface DraftSubmission {
  readonly attachLocation: boolean;
  readonly identity: DraftIdentity;
  readonly location: MessageLocationView | null;
}

/**
 * Prepare the exact idempotent request state for one composer draft.
 *
 * A retained submission reuses both identity and its captured fix after an
 * ambiguous send failure. An explicitly requested capture error rejects the
 * operation; it never degrades into an unlocated message.
 */
export async function prepareDraftSubmission(
  retained: DraftSubmission | null,
  attachLocation: boolean,
  createIdentity: () => DraftIdentity,
  captureLocation: () => Promise<MessageLocationView>,
): Promise<DraftSubmission> {
  if (retained !== null && retained.attachLocation === attachLocation) return retained;
  const location = attachLocation ? await captureLocation() : null;
  return {
    attachLocation,
    identity: createIdentity(),
    location,
  };
}
