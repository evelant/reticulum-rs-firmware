export type RetryScheduler = (callback: () => void, delayMs: number) => () => void;

export type ForegroundReconnectProgress =
  | { readonly state: "attempting" }
  | { readonly reason: string; readonly state: "waiting_retry" };

export interface ForegroundConnectionClient {
  ensureConnected(): Promise<unknown>;
}

/** Wake the selected bearer without replacing an already usable link. */
export async function ensureForegroundConnection(
  client: ForegroundConnectionClient,
): Promise<void> {
  await client.ensureConnected();
}

export function foregroundReconnectMessage(progress: ForegroundReconnectProgress): string {
  if (progress.state === "attempting") {
    return "Pairing is saved. Connecting to the node.";
  }
  return (
    "Pairing is saved. The node is not advertising yet; retrying automatically. " +
    `Last attempt: ${progress.reason}`
  );
}

const scheduleRetry: RetryScheduler = (callback, delayMs) => {
  const timer = setTimeout(callback, delayMs);
  return () => clearTimeout(timer);
};

/**
 * Prevents overlapping foreground reconnects while re-arming a settled
 * attempt. The owner can temporarily suspend retries or persistently inhibit
 * them until a later explicit action.
 */
export class ForegroundReconnect {
  readonly #delayMs: number;
  readonly #requestRetry: () => void;
  readonly #schedule: RetryScheduler;

  #cancelScheduled: (() => void) | null = null;
  #enabled = false;
  #inhibited = false;
  #inFlight = false;
  #lastRequest: number | null = null;

  constructor(requestRetry: () => void, delayMs: number, schedule: RetryScheduler = scheduleRetry) {
    if (!Number.isFinite(delayMs) || delayMs <= 0) {
      throw new Error("foreground reconnect delay must be positive");
    }
    this.#delayMs = delayMs;
    this.#requestRetry = requestRetry;
    this.#schedule = schedule;
  }

  begin(request: number): boolean {
    if (this.#inhibited) return false;
    this.#enabled = true;
    if (this.#inFlight || this.#lastRequest === request) return false;
    this.#clearScheduled();
    this.#lastRequest = request;
    this.#inFlight = true;
    return true;
  }

  settle(): void {
    if (!this.#inFlight) return;
    this.#inFlight = false;
    if (!this.#enabled || this.#cancelScheduled !== null) return;

    this.#cancelScheduled = this.#schedule(() => {
      this.#cancelScheduled = null;
      if (this.#enabled) this.#requestRetry();
    }, this.#delayMs);
  }

  suspend(): void {
    this.#enabled = false;
    this.#lastRequest = null;
    this.#clearScheduled();
  }

  /**
   * Keep automatic attempts stopped across later begin() calls until a user
   * action explicitly clears the inhibition.
   *
   * A failed BLE bond-repair scan uses this stronger state so an ordinary
   * normal-name reconnect cannot immediately reclaim the bondless board.
   */
  inhibit(): void {
    this.#inhibited = true;
    this.suspend();
  }

  allow(): void {
    this.#inhibited = false;
  }

  #clearScheduled(): void {
    this.#cancelScheduled?.();
    this.#cancelScheduled = null;
  }
}
