/** Render an unknown operation failure without discarding ordinary Error messages. */
export function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
