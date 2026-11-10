/**
 * A controllable clock for the `motion` library's JS-driven Surface fade.
 *
 * `motion` animates `opacity` with its own frame loop (inline style per frame), so the fade has no
 * Web Animation to pause. This module must be the FIRST import of the parity test so it patches the
 * host window's `requestAnimationFrame` and `performance.now` before `motion` captures them. While
 * `clock.manual` is false both delegate to the browser; while it is true time only moves when the
 * test calls `stepClock`, so the fade can be sampled at exact timeline positions.
 */
const nativeRaf = window.requestAnimationFrame.bind(window);
const nativeCancel = window.cancelAnimationFrame.bind(window);
const nativeNow = performance.now.bind(performance);

export const clock = {
  manual: false,
  time: 0,
  nextId: 0,
  queue: new Map<number, FrameRequestCallback>(),
};

window.requestAnimationFrame = (callback: FrameRequestCallback): number => {
  if (!clock.manual) {
    return nativeRaf(callback);
  }

  clock.nextId += 1;
  clock.queue.set(clock.nextId, callback);

  return clock.nextId;
};

window.cancelAnimationFrame = (handle: number): void => {
  clock.queue.delete(handle);
  nativeCancel(handle);
};

performance.now = (): number => (clock.manual ? clock.time : nativeNow());

/**
 * Origin of the frozen timeline. It is not 0 because `motion` treats a falsy start time as unset and
 * would restart the fade on every frame.
 */
const ORIGIN = 1000;

/** Freezes time at the origin and routes frames to the manual queue. */
export function freezeClock() {
  clock.manual = true;
  clock.time = ORIGIN;
  clock.queue.clear();
}

/** Moves the frozen time to `ms` after the origin and runs the waiting frames (motion chains them). */
export async function stepClock(ms: number) {
  clock.time = ORIGIN + ms;

  for (let pass = 0; pass < 3; pass++) {
    const frames = [...clock.queue.values()];

    clock.queue.clear();

    for (const frame of frames) {
      frame(clock.time);
    }

    // Motion renders in a later task/microtask, not inside the frame callback.
    await new Promise<void>((resolve) => setTimeout(resolve, 15));
  }
}

/** Returns to real time; frames motion was waiting for are handed to the real scheduler. */
export function releaseClock() {
  const pending = [...clock.queue.values()];

  clock.manual = false;
  clock.queue.clear();

  for (const frame of pending) {
    nativeRaf(frame);
  }
}
