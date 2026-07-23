export type RetryScheduler = (callback: () => void, delayMs: number) => () => void;

const scheduleRetry: RetryScheduler = (callback, delayMs) => {
  const timer = setTimeout(callback, delayMs);
  return () => clearTimeout(timer);
};

/**
 * Prevents overlapping foreground reconnects while re-arming a settled
 * attempt. The owner decides whether reconnecting is currently appropriate by
 * calling begin() or suspend().
 */
export class ForegroundReconnect {
  readonly #delayMs: number;
  readonly #requestRetry: () => void;
  readonly #schedule: RetryScheduler;

  #cancelScheduled: (() => void) | null = null;
  #enabled = false;
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

  #clearScheduled(): void {
    this.#cancelScheduled?.();
    this.#cancelScheduled = null;
  }
}
