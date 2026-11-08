import { setTimeout as sleep } from 'node:timers/promises';

/** How often an interruptible wait re-checks the interruption flag. */
const INTERRUPT_POLL_MS = 200;

/** Thrown when a signal asks the tool to stop before the run completes. */
export class InterruptedError extends Error {}

export function throwIfInterrupted(isInterrupted: () => boolean, message: string): void {
  if (isInterrupted()) {
    throw new InterruptedError(message);
  }
}

/**
 * Sleeps in short slices so an interrupt ends the wait instead of delaying it.
 *
 * Cleanup never uses this helper: stopping an owned tree must finish even after
 * the first signal.
 */
export async function sleepUnlessInterrupted(
  milliseconds: number,
  isInterrupted: () => boolean,
): Promise<void> {
  const deadline = Date.now() + milliseconds;

  for (;;) {
    throwIfInterrupted(isInterrupted, 'interrupted during a wait');
    const remaining = deadline - Date.now();

    if (remaining <= 0) {
      return;
    }

    await sleep(Math.min(INTERRUPT_POLL_MS, remaining));
  }
}
