export type SettledPollScheduler = (callback: () => void, delayMs: number) => () => void;

const schedulePoll: SettledPollScheduler = (callback, delayMs) => {
  const timer = setTimeout(callback, delayMs);
  return () => clearTimeout(timer);
};

/**
 * Run a slow read on a settled cadence rather than a fixed interval.
 *
 * A transport-backed read can take longer than its nominal cadence. The next
 * turn is therefore scheduled only after the current one settles. A blocked
 * turn is skipped and retried later instead of queueing behind an in-flight
 * mutation.
 */
export class SettledPoll {
  readonly #blocked: () => boolean;
  readonly #delayMs: number;
  readonly #read: () => Promise<void>;
  readonly #schedule: SettledPollScheduler;

  #cancelScheduled: (() => void) | null = null;
  #enabled = false;
  #inFlight = false;

  constructor(
    read: () => Promise<void>,
    delayMs: number,
    blocked: () => boolean = () => false,
    schedule: SettledPollScheduler = schedulePoll,
  ) {
    if (!Number.isFinite(delayMs) || delayMs <= 0) {
      throw new Error("settled poll delay must be positive");
    }
    this.#blocked = blocked;
    this.#delayMs = delayMs;
    this.#read = read;
    this.#schedule = schedule;
  }

  start(): void {
    if (this.#enabled) return;
    this.#enabled = true;
    this.#scheduleNext();
  }

  stop(): void {
    this.#enabled = false;
    this.#cancelScheduled?.();
    this.#cancelScheduled = null;
  }

  async #poll(): Promise<void> {
    if (!this.#enabled || this.#inFlight) return;
    if (this.#blocked()) {
      this.#scheduleNext();
      return;
    }
    this.#inFlight = true;
    try {
      await this.#read();
    } catch {
      // The read owner projects its own error; one failed read must not stop
      // future refreshes.
    } finally {
      this.#inFlight = false;
      this.#scheduleNext();
    }
  }

  #scheduleNext(): void {
    if (!this.#enabled || this.#cancelScheduled !== null) return;
    this.#cancelScheduled = this.#schedule(() => {
      this.#cancelScheduled = null;
      void this.#poll();
    }, this.#delayMs);
  }
}
