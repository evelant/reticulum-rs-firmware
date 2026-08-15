export type NearbyPollScheduler = (callback: () => void, delayMs: number) => () => void;

const schedulePoll: NearbyPollScheduler = (callback, delayMs) => {
  const timer = setTimeout(callback, delayMs);
  return () => clearTimeout(timer);
};

/**
 * Runs one foreground Nearby read at a time and delays the next read until the
 * previous one has settled.
 *
 * The owner stops this gate when the Nearby surface is hidden or disconnected.
 * An already-started transport operation is allowed to settle, but cannot
 * re-arm the poll after stop().
 */
export class ForegroundNearbyPoll {
  readonly #delayMs: number;
  readonly #read: () => Promise<void>;
  readonly #schedule: NearbyPollScheduler;

  #cancelScheduled: (() => void) | null = null;
  #enabled = false;
  #inFlight = false;

  constructor(
    read: () => Promise<void>,
    delayMs: number,
    schedule: NearbyPollScheduler = schedulePoll,
  ) {
    if (!Number.isFinite(delayMs) || delayMs <= 0) {
      throw new Error("foreground Nearby poll delay must be positive");
    }
    this.#read = read;
    this.#delayMs = delayMs;
    this.#schedule = schedule;
  }

  start(): void {
    if (this.#enabled) return;
    this.#enabled = true;
    void this.#poll();
  }

  stop(): void {
    this.#enabled = false;
    this.#cancelScheduled?.();
    this.#cancelScheduled = null;
  }

  async #poll(): Promise<void> {
    if (!this.#enabled || this.#inFlight) return;
    this.#inFlight = true;
    try {
      await this.#read();
    } catch {
      // The read owner projects its own user-visible error. Poll cadence and
      // cancellation remain independent of one failed authenticated request.
    } finally {
      this.#inFlight = false;
      if (this.#enabled && this.#cancelScheduled === null) {
        this.#cancelScheduled = this.#schedule(() => {
          this.#cancelScheduled = null;
          void this.#poll();
        }, this.#delayMs);
      }
    }
  }
}
