export interface ExclusiveResource<T> {
  readonly value: T;
  release(): Promise<void>;
}

let ownershipTail: Promise<void> = Promise.resolve();

/**
 * Serialize ownership of a process-local resource across independent clients.
 *
 * The next opener does not run until the previous resource has finished
 * closing. A failed open or close still advances the queue.
 */
export async function acquireExclusiveResource<T>(
  open: () => T | Promise<T>,
  close: (value: T) => void | Promise<void>,
): Promise<ExclusiveResource<T>> {
  const predecessor = ownershipTail;
  let finishOwnership = () => {};
  const ownershipFinished = new Promise<void>((resolve) => {
    finishOwnership = resolve;
  });
  ownershipTail = predecessor.then(() => ownershipFinished);

  await predecessor;
  let value: T;
  try {
    value = await open();
  } catch (error) {
    finishOwnership();
    throw error;
  }

  let released = false;
  return {
    value,
    async release(): Promise<void> {
      if (released) return;
      released = true;
      try {
        await close(value);
      } finally {
        finishOwnership();
      }
    },
  };
}
