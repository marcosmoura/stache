import { createStore, useStore, type StoreApi } from 'zustand';

/** The ticker's state: the wall-clock time of the last tick, in epoch milliseconds. */
export type TickerState = { now: number };

/** A vanilla zustand store that ticks on each whole second while it has subscribers. */
export type Ticker = StoreApi<TickerState> & {
  /** How many listeners are subscribed (for tests and diagnostics). */
  subscriberCount: () => number;
  /** Whether a timer is scheduled (for tests and diagnostics). */
  running: () => boolean;
};

const SECOND = 1000;

/**
 * A one-second ticker. Its timer runs only while it has at least one
 * subscriber: the first subscription refreshes `now` and schedules the next
 * tick at the next whole second; the last unsubscription clears the timer. Each
 * tick reads `Date.now()` and re-aligns to the next whole second, so a slow
 * tick never drifts. `now` is a moment, never a formatted value: consumers
 * format it from a `Date`.
 */
export function createTicker(): Ticker {
  const store = createStore<TickerState>()(() => ({ now: Date.now() }));
  const subscribe = store.subscribe;
  let subscribers = 0;
  let timer: ReturnType<typeof setTimeout> | null = null;

  const schedule = () => {
    timer = setTimeout(tick, SECOND - (Date.now() % SECOND));
  };

  function tick() {
    store.setState({ now: Date.now() });
    schedule();
  }

  return {
    ...store,
    subscribe: (listener) => {
      const unsubscribe = subscribe(listener);
      let subscribed = true;

      subscribers += 1;

      if (subscribers === 1) {
        store.setState({ now: Date.now() });
        schedule();
      }

      return () => {
        if (!subscribed) {
          return;
        }

        subscribed = false;
        unsubscribe();
        subscribers -= 1;

        if (subscribers === 0 && timer !== null) {
          clearTimeout(timer);
          timer = null;
        }
      };
    },
    subscriberCount: () => subscribers,
    running: () => timer !== null,
  };
}

/** The app-wide ticker every Clock shares. */
export const ticker = createTicker();

/**
 * Reads a slice of the shared ticker; re-renders only when the slice changes
 * (select the minute to re-render once a minute).
 */
export function useTicker<T>(selector: (state: TickerState) => T): T {
  return useStore(ticker, selector);
}
