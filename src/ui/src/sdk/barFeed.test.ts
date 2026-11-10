import { QueryClient } from '@tanstack/react-query';
import { beforeEach, expect, test, vi } from 'vitest';

import { availableOutcome, unavailableOutcome } from '../../tests/barFixtures';

import { connectBarSnapshot, outcomeSequence } from './barFeed';
import { BAR_SNAPSHOT_QUERY_KEY, type BarChanged, type BarSnapshotOutcome } from './client';

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });

  return { promise, resolve, reject };
}

let client: QueryClient;

beforeEach(() => {
  client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
});

function held() {
  return client.getQueryData<BarSnapshotOutcome>(BAR_SNAPSHOT_QUERY_KEY);
}

function changed(sequence: string, outcome: BarSnapshotOutcome): BarChanged {
  return { generation: '1', sequence, resnapshot: false, outcome };
}

function connect(read: () => Promise<BarSnapshotOutcome>) {
  let publish!: (changed: BarChanged) => void;
  const unlisten = vi.fn();
  const subscribe = vi.fn(async (onChanged: (changed: BarChanged) => void) => {
    publish = onChanged;

    return unlisten;
  });
  const feed = connectBarSnapshot({ client, subscribe, read });

  return { feed, unlisten, subscribe, publish: (value: BarChanged) => publish(value) };
}

test('subscribes before it reads the initial snapshot', async () => {
  const order: string[] = [];
  const listener = deferred<() => void>();
  const read = vi.fn(async () => {
    order.push('read');

    return availableOutcome({ sequence: '5' });
  });

  const feed = connectBarSnapshot({
    client,
    subscribe: () => {
      order.push('subscribe');

      return listener.promise;
    },
    read,
  });

  expect(order).toEqual(['subscribe']);
  expect(read).not.toHaveBeenCalled();

  listener.resolve(vi.fn());
  await vi.waitFor(() => expect(held()).toEqual(availableOutcome({ sequence: '5' })));
  expect(order).toEqual(['subscribe', 'read']);
  feed.dispose();
});

test('a newer event wins over a stale initial snapshot', async () => {
  const snapshot = deferred<BarSnapshotOutcome>();
  const { publish, feed } = connect(() => snapshot.promise);

  await vi.waitFor(() => expect(typeof publish).toBe('function'));
  await new Promise((resolve) => setTimeout(resolve, 0));

  const newer = availableOutcome({ sequence: '7', menu: { state: 'visible', reason: null } });

  publish(changed('7', newer));
  snapshot.resolve(availableOutcome({ sequence: '6' }));
  await new Promise((resolve) => setTimeout(resolve, 0));

  expect(held()).toEqual(newer);
  feed.dispose();
});

test('compares sequences numerically, not as text', async () => {
  const { publish, feed } = connect(async () => availableOutcome({ sequence: '9' }));

  await vi.waitFor(() => expect(held()).toBeDefined());

  publish(changed('10', unavailableOutcome('10')));
  expect(held()?.kind).toBe('unavailable');

  // "9" sorts after "10" as text but is numerically older.
  publish(changed('9', availableOutcome({ sequence: '9' })));
  expect(held()?.kind).toBe('unavailable');

  publish(changed('18446744073709551615', availableOutcome({ sequence: '18446744073709551615' })));
  expect(held()?.kind).toBe('available');
  expect(outcomeSequence(held() as BarSnapshotOutcome)).toBe(18446744073709551615n);
  feed.dispose();
});

test('an equal or older event is discarded and a malformed one ignored', async () => {
  const { publish, feed } = connect(async () => availableOutcome({ sequence: '4' }));

  await vi.waitFor(() => expect(held()).toBeDefined());

  publish(changed('4', unavailableOutcome('4')));
  publish(changed('3', unavailableOutcome('3')));
  publish(changed('x1', unavailableOutcome('5')));
  publish({ generation: '1', sequence: '5', resnapshot: false, outcome: null });
  expect(held()?.kind).toBe('available');
  feed.dispose();
});

test('resnapshot refetches and applies only a greater sequence', async () => {
  const reads = [availableOutcome({ sequence: '2' }), availableOutcome({ sequence: '9' })];
  const read = vi.fn(async () => reads.shift() as BarSnapshotOutcome);
  const { publish, feed } = connect(read);

  await vi.waitFor(() => expect(held()).toBeDefined());
  expect(read).toHaveBeenCalledTimes(1);

  publish({ generation: '1', sequence: '9', resnapshot: true, outcome: null });
  await vi.waitFor(() => expect(outcomeSequence(held() as BarSnapshotOutcome)).toBe(9n));
  expect(read).toHaveBeenCalledTimes(2);

  reads.push(availableOutcome({ sequence: '8' }));
  publish({ generation: '1', sequence: '10', resnapshot: true, outcome: null });
  await vi.waitFor(() => expect(read).toHaveBeenCalledTimes(3));
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(outcomeSequence(held() as BarSnapshotOutcome)).toBe(9n);
  feed.dispose();
});

test('unavailable answers are kept and a later available answer replaces them', async () => {
  const { publish, feed } = connect(async () => unavailableOutcome('1', 'starting'));

  await vi.waitFor(() => expect(held()?.kind).toBe('unavailable'));
  publish(changed('2', availableOutcome({ sequence: '2' })));
  expect(held()?.kind).toBe('available');
  feed.dispose();
});

test('unauthorized is kept only while nothing with a sequence is held', async () => {
  const first = connect(async () => ({ kind: 'unauthorized' }));

  await vi.waitFor(() => expect(held()?.kind).toBe('unauthorized'));
  first.publish(changed('1', availableOutcome({ sequence: '1' })));
  expect(held()?.kind).toBe('available');
  first.feed.dispose();

  client.clear();

  const second = connect(async () => availableOutcome({ sequence: '3' }));

  await vi.waitFor(() => expect(held()?.kind).toBe('available'));
  second.publish({ generation: '1', sequence: '4', resnapshot: true, outcome: null });
  second.feed.dispose();
  expect(held()?.kind).toBe('available');
});

test('disposing before listen resolves unlistens at once and fetches nothing', async () => {
  const listener = deferred<() => void>();
  const unlisten = vi.fn();
  const read = vi.fn(async () => availableOutcome());
  const feed = connectBarSnapshot({ client, subscribe: () => listener.promise, read });

  feed.dispose();
  listener.resolve(unlisten);
  await new Promise((resolve) => setTimeout(resolve, 0));

  expect(unlisten).toHaveBeenCalledTimes(1);
  expect(read).not.toHaveBeenCalled();
  expect(held()).toBeUndefined();
});

test('disposal unlistens once and ignores later events and snapshots', async () => {
  const snapshot = deferred<BarSnapshotOutcome>();
  const { publish, feed, unlisten } = connect(() => snapshot.promise);

  await new Promise((resolve) => setTimeout(resolve, 0));
  feed.dispose();
  feed.dispose();
  expect(unlisten).toHaveBeenCalledTimes(1);

  publish(changed('3', availableOutcome({ sequence: '3' })));
  snapshot.resolve(availableOutcome({ sequence: '4' }));
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(held()).toBeUndefined();
});

test('subscription and snapshot failures land in the query error state', async () => {
  const failing = connectBarSnapshot({
    client,
    subscribe: async () => {
      throw new Error('listen failed');
    },
    read: async () => availableOutcome(),
  });

  await vi.waitFor(() =>
    expect(client.getQueryCache().find({ queryKey: BAR_SNAPSHOT_QUERY_KEY })?.state.status).toBe(
      'error',
    ),
  );
  failing.dispose();
  client.clear();

  const reading = connect(async () => {
    throw new Error('ipc failed');
  });

  await vi.waitFor(() =>
    expect(client.getQueryCache().find({ queryKey: BAR_SNAPSHOT_QUERY_KEY })?.state.error).toEqual(
      new Error('ipc failed'),
    ),
  );

  reading.publish(changed('1', availableOutcome({ sequence: '1' })));
  expect(client.getQueryCache().find({ queryKey: BAR_SNAPSHOT_QUERY_KEY })?.state.status).toBe(
    'success',
  );
  reading.feed.dispose();
});
