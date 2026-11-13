import { useEffect } from 'react';

import { useQueryClient, type QueryClient, type QueryKey } from '@tanstack/react-query';

import { subscribe, type HostEvent } from '@/ipc';

/** What one event payload means for the feed. */
export type FeedUpdate<TData> = {
  /** The new value; absent or `null` when the payload carries none. */
  value?: TData | null;
  /** The payload's decimal sequence; absent for feeds without sequences. */
  sequence?: string | null;
  /** Ask for a fresh snapshot instead of (or besides) applying `value`. */
  resnapshot?: boolean;
};

export type FeedConfig<TData, TPayload> = {
  /** The query the feed keeps current (its `queryKey` is all that is read). */
  options: { queryKey: QueryKey };
  /** The snapshot command. */
  snapshot: () => Promise<TData>;
  /** The generated event. */
  event: HostEvent<TPayload>;
  /** Reads an event payload. */
  read: (payload: TPayload) => FeedUpdate<TData>;
  /** Reads the sequence of a snapshot value; omit for feeds without sequences. */
  sequenceOf?: (value: TData) => string | null | undefined;
};

export type Feed = {
  readonly queryKey: QueryKey;
  /**
   * Starts the feed for `client` (reference counted) and returns the matching
   * stop. The first start subscribes and then snapshots; the last stop unlistens.
   */
  start: (client: QueryClient) => () => void;
};

/** One live subscription of a feed in one client. */
type Run = {
  alive: boolean;
  /** The highest applied sequence, or `null` while nothing sequenced is held. */
  held: bigint | null;
  /** Bumped by every applied event; a snapshot requested earlier loses to it. */
  generation: number;
  dispose: () => void;
};

type Slot = { count: number; run: Run | null };

function parseSequence(text: string): bigint {
  if (!/^\d+$/.test(text)) {
    throw new Error(`feed: invalid sequence "${text}"`);
  }

  return BigInt(text);
}

/**
 * Keeps one query current from a snapshot command and a host event. Ordering:
 * subscribe first, then snapshot; apply a value only when its sequence is
 * strictly greater than the held one (`BigInt`); a value without a sequence is
 * applied only while nothing sequenced is held and no event arrived since its
 * request. Failures become the query's error; the next write clears it.
 */
export function createFeed<TData, TPayload>(config: FeedConfig<TData, TPayload>): Feed {
  const { queryKey } = config.options;
  const slots = new WeakMap<QueryClient, Slot>();

  const fail = (client: QueryClient, error: unknown) => {
    const query = client.getQueryCache().build(client, { queryKey });

    query.setState({
      status: 'error',
      fetchStatus: 'idle',
      error: error instanceof Error ? error : new Error(String(error)),
      errorUpdateCount: query.state.errorUpdateCount + 1,
      errorUpdatedAt: Date.now(),
    });
  };

  const write = (client: QueryClient, value: TData) => {
    client.setQueryData<TData>(queryKey, value);
  };

  const requestSnapshot = (client: QueryClient, run: Run) => {
    const generation = run.generation;

    config.snapshot().then(
      (value) => {
        if (!run.alive) {
          return;
        }

        try {
          const text = config.sequenceOf?.(value);

          if (text === undefined || text === null) {
            if (run.held === null && run.generation === generation) {
              write(client, value);
            }

            return;
          }

          const sequence = parseSequence(text);

          if (run.held === null || sequence > run.held) {
            run.held = sequence;
            write(client, value);
          }
        } catch (error) {
          fail(client, error);
        }
      },
      (error: unknown) => {
        if (run.alive) {
          fail(client, error);
        }
      },
    );
  };

  const onEvent = (client: QueryClient, run: Run, payload: TPayload) => {
    try {
      const update = config.read(payload);
      const value = update.value ?? null;

      if (update.sequence !== undefined && update.sequence !== null) {
        const sequence = parseSequence(update.sequence);

        if (run.held !== null && sequence <= run.held) {
          return;
        }

        if (value !== null && !update.resnapshot) {
          run.held = sequence;
        }
      }

      if (update.resnapshot || value === null) {
        if (update.resnapshot) {
          requestSnapshot(client, run);
        }

        return;
      }

      run.generation += 1;
      write(client, value);
    } catch (error) {
      fail(client, error);
    }
  };

  const begin = (client: QueryClient): Run => {
    const run: Run = { alive: true, held: null, generation: 0, dispose: () => {} };

    run.dispose = subscribe(config.event, (payload) => onEvent(client, run, payload), {
      onReady: () => requestSnapshot(client, run),
      onError: (error) => fail(client, error),
    });

    return run;
  };

  return {
    queryKey,
    start(client) {
      let slot = slots.get(client);

      if (!slot) {
        slot = { count: 0, run: null };
        slots.set(client, slot);
      }

      slot.count += 1;

      if (slot.count === 1) {
        slot.run = begin(client);
      }

      const owned = slot;
      let stopped = false;

      return () => {
        if (stopped) {
          return;
        }

        stopped = true;
        owned.count -= 1;

        if (owned.count === 0 && owned.run) {
          owned.run.alive = false;
          owned.run.dispose();
          owned.run = null;
        }
      };
    },
  };
}

/** Keeps `feed` running in the current query client while the caller is mounted. */
export function useFeed(feed: Feed) {
  const client = useQueryClient();

  useEffect(() => feed.start(client), [feed, client]);
}
