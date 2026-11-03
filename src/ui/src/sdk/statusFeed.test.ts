import { QueryClient } from '@tanstack/react-query';
import { beforeEach, expect, test, vi } from 'vitest';

import { hostStatus, moduleStatus } from '../../tests/hostFixtures';

import { HOST_STATUS_QUERY_KEY } from './client';
import { connectHostStatus } from './statusFeed';

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function flushTasks() {
  return new Promise<void>((resolve) => setTimeout(resolve, 0));
}

let client: QueryClient;

beforeEach(() => {
  client = new QueryClient({
    defaultOptions: {
      queries: {
        retry: false,
        staleTime: Infinity,
      },
    },
  });
});

function queryState() {
  return client.getQueryCache().find({ queryKey: HOST_STATUS_QUERY_KEY })?.state;
}

test('subscribes before requesting the initial snapshot', async () => {
  const order: string[] = [];
  const listener = deferred<() => void>();
  const unlisten = vi.fn();
  const status = hostStatus();
  const read = vi.fn(async () => {
    order.push('read');
    return status;
  });

  const feed = connectHostStatus({
    client,
    subscribe: () => {
      order.push('subscribe');
      return listener.promise;
    },
    read,
  });

  expect(order).toEqual(['subscribe']);
  expect(read).not.toHaveBeenCalled();

  listener.resolve(unlisten);

  await vi.waitFor(() => expect(client.getQueryData(HOST_STATUS_QUERY_KEY)).toEqual(status));
  expect(order).toEqual(['subscribe', 'read']);

  feed.dispose();
  expect(unlisten).toHaveBeenCalledTimes(1);

  feed.dispose();
  expect(unlisten).toHaveBeenCalledTimes(1);
});

test('a live event wins over an overlapping initial snapshot', async () => {
  const unlisten = vi.fn();
  let publish: ((status: ReturnType<typeof hostStatus>) => void) | undefined;
  const snapshot = deferred<ReturnType<typeof hostStatus>>();

  const feed = connectHostStatus({
    client,
    subscribe: async (onStatus) => {
      publish = onStatus;
      return unlisten;
    },
    read: () => snapshot.promise,
  });

  await vi.waitFor(() => expect(publish).toBeTypeOf('function'));

  const live = hostStatus({ modules: [moduleStatus({ id: 'from-event' })] });
  publish?.(live);
  expect(client.getQueryData(HOST_STATUS_QUERY_KEY)).toEqual(live);

  snapshot.resolve(hostStatus({ modules: [moduleStatus({ id: 'from-snapshot' })] }));
  await flushTasks();

  expect(client.getQueryData(HOST_STATUS_QUERY_KEY)).toEqual(live);
  expect(queryState()?.status).toBe('success');

  feed.dispose();
  expect(unlisten).toHaveBeenCalledTimes(1);
});

test('later events update the cache and a disposed feed ignores them', async () => {
  const unlisten = vi.fn();
  let publish: ((status: ReturnType<typeof hostStatus>) => void) | undefined;
  const initial = hostStatus({ ready: false });

  const feed = connectHostStatus({
    client,
    subscribe: async (onStatus) => {
      publish = onStatus;
      return unlisten;
    },
    read: async () => initial,
  });

  await vi.waitFor(() => expect(client.getQueryData(HOST_STATUS_QUERY_KEY)).toEqual(initial));

  const live = hostStatus({ ready: true, modules: [moduleStatus()] });
  publish?.(live);
  expect(client.getQueryData(HOST_STATUS_QUERY_KEY)).toEqual(live);

  feed.dispose();
  publish?.(hostStatus({ modules: [moduleStatus({ id: 'after-dispose' })] }));

  expect(client.getQueryData(HOST_STATUS_QUERY_KEY)).toEqual(live);
  expect(unlisten).toHaveBeenCalledTimes(1);
});

test('a listener that resolves after dispose is unlistened without fetching', async () => {
  const listener = deferred<() => void>();
  const unlisten = vi.fn();
  const read = vi.fn(async () => hostStatus());

  const feed = connectHostStatus({
    client,
    subscribe: () => listener.promise,
    read,
  });

  feed.dispose();
  listener.resolve(unlisten);

  await vi.waitFor(() => expect(unlisten).toHaveBeenCalledTimes(1));
  expect(read).not.toHaveBeenCalled();
  expect(client.getQueryData(HOST_STATUS_QUERY_KEY)).toBeUndefined();
});

test('reports a subscription failure in the query cache', async () => {
  const read = vi.fn(async () => hostStatus());

  connectHostStatus({
    client,
    subscribe: async () => {
      throw new Error('event transport unavailable');
    },
    read,
  });

  await vi.waitFor(() => expect(queryState()?.status).toBe('error'));
  expect(queryState()?.error).toEqual(new Error('event transport unavailable'));
  expect(read).not.toHaveBeenCalled();
});

test('reports an initial snapshot failure in the query cache', async () => {
  connectHostStatus({
    client,
    subscribe: async (): Promise<() => void> => vi.fn<() => void>(),
    read: async () => {
      throw new Error('socket closed');
    },
  });

  await vi.waitFor(() => expect(queryState()?.status).toBe('error'));
  expect(queryState()?.error).toEqual(new Error('socket closed'));
});

test('does not report a stale snapshot failure after a live event', async () => {
  let publish: ((status: ReturnType<typeof hostStatus>) => void) | undefined;
  const snapshot = deferred<ReturnType<typeof hostStatus>>();

  connectHostStatus({
    client,
    subscribe: async (onStatus): Promise<() => void> => {
      publish = onStatus;
      return vi.fn<() => void>();
    },
    read: () => snapshot.promise,
  });

  await vi.waitFor(() => expect(publish).toBeTypeOf('function'));
  publish?.(hostStatus({ ready: true }));
  snapshot.reject(new Error('stale failure'));
  await flushTasks();

  expect(queryState()?.status).toBe('success');
  expect(queryState()?.error).toBeNull();
});
