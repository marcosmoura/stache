import type { QueryClient } from '@tanstack/react-query';

import {
  HOST_STATUS_QUERY_KEY,
  readHostStatus,
  subscribeToStatus,
  type HostStatus,
} from './client';

export type HostStatusFeedOptions = {
  client: QueryClient;
  /** Test seam: defaults to the generated event wrapper. */
  subscribe?: (onStatus: (status: HostStatus) => void) => Promise<() => void>;
  /** Test seam: defaults to the generated status command. */
  read?: () => Promise<HostStatus>;
};

export type HostStatusFeed = {
  dispose(): void;
};

function setStatusError(client: QueryClient, error: unknown) {
  const normalized = error instanceof Error ? error : new Error(String(error));
  const query = client.getQueryCache().build(client, {
    queryKey: HOST_STATUS_QUERY_KEY,
  });

  query.setState({
    status: 'error',
    error: normalized,
    fetchStatus: 'idle',
    errorUpdatedAt: Date.now(),
  });
}

/**
 * Connects a query cache entry to the host's status stream.
 *
 * The listener is registered before the first snapshot is requested so an
 * event that arrives while the snapshot is in flight is never overwritten by
 * the older snapshot. Every publish is synchronous, which makes the version
 * check and cache write atomic.
 */
export function connectHostStatus({
  client,
  subscribe = subscribeToStatus,
  read = readHostStatus,
}: HostStatusFeedOptions): HostStatusFeed {
  let disposed = false;
  let unlisten: (() => void) | undefined;
  let liveVersion = 0;

  const publish = (status: HostStatus) => {
    if (disposed) {
      return;
    }

    liveVersion += 1;
    client.setQueryData(HOST_STATUS_QUERY_KEY, status);
  };

  const fail = (error: unknown) => {
    if (disposed) {
      return;
    }

    setStatusError(client, error);
  };

  const start = async () => {
    let stop: () => void;

    try {
      stop = await subscribe(publish);
    } catch (error) {
      fail(error);

      return;
    }

    if (disposed) {
      stop();

      return;
    }

    unlisten = stop;

    const version = liveVersion;

    try {
      const snapshot = await read();

      if (disposed || liveVersion !== version) {
        return;
      }

      client.setQueryData(HOST_STATUS_QUERY_KEY, snapshot);
    } catch (error) {
      if (disposed || liveVersion !== version) {
        return;
      }

      setStatusError(client, error);
    }
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
