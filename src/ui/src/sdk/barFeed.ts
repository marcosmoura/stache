import type { QueryClient } from '@tanstack/react-query';

import {
  BAR_SNAPSHOT_QUERY_KEY,
  readBarSnapshot,
  subscribeToBar,
  type BarChanged,
  type BarSnapshotOutcome,
} from './client';

export type BarSnapshotFeedOptions = {
  client: QueryClient;
  /** Test seam: defaults to the generated `bar-changed` wrapper. */
  subscribe?: (onChanged: (changed: BarChanged) => void) => Promise<() => void>;
  /** Test seam: defaults to the generated `bar_snapshot` command. */
  read?: () => Promise<BarSnapshotOutcome>;
};

export type BarSnapshotFeed = {
  dispose(): void;
};

const DECIMAL = /^\d+$/;

/** Parses an exact decimal `u64` string; never goes through `Number`. */
function parseSequence(value: string): bigint | null {
  return DECIMAL.test(value) ? BigInt(value) : null;
}

/** The sequence an outcome carries; `unauthorized` has none. */
export function outcomeSequence(outcome: BarSnapshotOutcome): bigint | null {
  switch (outcome.kind) {
    case 'available':
      return parseSequence(outcome.view.sequence);
    case 'unavailable':
      return parseSequence(outcome.sequence);
    default:
      return null;
  }
}

function setBarError(client: QueryClient, error: unknown) {
  const normalized = error instanceof Error ? error : new Error(String(error));
  const query = client.getQueryCache().build(client, {
    queryKey: BAR_SNAPSHOT_QUERY_KEY,
  });

  query.setState({
    status: 'error',
    error: normalized,
    fetchStatus: 'idle',
    errorUpdatedAt: Date.now(),
  });
}

/**
 * Connects a query cache entry to the Bar bridge.
 *
 * The contract (see `bar_bridge.rs`): subscribe to `bar-changed` first, then
 * read `bar_snapshot`, and apply a snapshot or an event only if its `sequence`
 * is numerically greater than the one the cache holds. A snapshot read before a
 * newer event arrived is therefore discarded, and the comparison plus the cache
 * write happen in one synchronous step. An event with `resnapshot: true` carries
 * no view and triggers a fresh read. `unauthorized` has no sequence; it is kept
 * only while nothing with a sequence is held.
 */
export function connectBarSnapshot({
  client,
  subscribe = subscribeToBar,
  read = readBarSnapshot,
}: BarSnapshotFeedOptions): BarSnapshotFeed {
  let disposed = false;
  let unlisten: (() => void) | undefined;
  let applied = 0;

  const heldSequence = (): bigint | null => {
    const held = client.getQueryData<BarSnapshotOutcome>(BAR_SNAPSHOT_QUERY_KEY);

    return held === undefined ? null : outcomeSequence(held);
  };

  const apply = (outcome: BarSnapshotOutcome, sequence: bigint | null) => {
    const held = heldSequence();

    if (sequence !== null && held !== null && sequence <= held) {
      return;
    }

    if (sequence === null && held !== null) {
      return;
    }

    applied += 1;
    client.setQueryData(BAR_SNAPSHOT_QUERY_KEY, outcome);
  };

  const refresh = async () => {
    const version = applied;

    try {
      const outcome = await read();

      if (disposed) {
        return;
      }

      apply(outcome, outcomeSequence(outcome));
    } catch (error) {
      if (disposed || applied !== version) {
        return;
      }

      setBarError(client, error);
    }
  };

  const onChanged = (changed: BarChanged) => {
    if (disposed) {
      return;
    }

    if (changed.resnapshot) {
      void refresh();

      return;
    }

    const sequence = parseSequence(changed.sequence);
    const outcome = changed.outcome;

    if (sequence === null || !outcome) {
      return;
    }

    apply(outcome, sequence);
  };

  const start = async () => {
    let stop: () => void;

    try {
      stop = await subscribe(onChanged);
    } catch (error) {
      if (!disposed) {
        setBarError(client, error);
      }

      return;
    }

    if (disposed) {
      stop();

      return;
    }

    unlisten = stop;

    await refresh();
  };

  void start();

  return {
    dispose() {
      if (disposed) {
        return;
      }

      disposed = true;
      unlisten?.();
      unlisten = undefined;
    },
  };
}
