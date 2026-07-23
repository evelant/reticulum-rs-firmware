export const EXPECTED_BUN_VERSION = "1.3.13";
export const EXPECTED_BUN_REVISION = "bf2e2cecf";

export function assertExpectedBun(): void {
  if (Bun.version !== EXPECTED_BUN_VERSION) {
    throw new Error(`expected Bun ${EXPECTED_BUN_VERSION}, observed ${Bun.version}`);
  }
  if (!Bun.revision.startsWith(EXPECTED_BUN_REVISION)) {
    throw new Error(`expected Bun revision ${EXPECTED_BUN_REVISION}, observed ${Bun.revision}`);
  }
}
