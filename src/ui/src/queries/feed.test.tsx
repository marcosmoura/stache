import { StrictMode } from 'react';

import { QueryClientProvider, type QueryClient } from '@tanstack/react-query';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import type { BarSnapshotOutcome, HostStatus } from '@/ipc';

import {
  barAvailable,
  barChanged,
  barUnauthorized,
  barUnavailable,
  hostStatusFixture,
} from '../../tests/fixtures';
import { flush, mockHost, type MockHost } from '../../tests/ipc';
import { createQueryClient } from '../../tests/queryClient';

import { barFeed, barOutcomeSequence, barSnapshotOptions } from './bar';
import { useFeed } from './feed';
import { hostStatusFeed, hostStatusOptions } from './host';

const BAR_EVENT = 'bar-changed';
const STATUS_EVENT = 'status-changed';

type Deferred<T> = { promise: Promise<T>; resolve: (value: T) => void };

function deferred<T>(): Deferred<T> {
  let resolve: (value: T) => void = () => {};
  const promise = new Promise<T>((done) => {
    resolve = done;
  });

  return { promise, resolve };
}

/** Scripts `bar_snapshot` to answer from a queue of deferreds, in call order. */
function queueSnapshots(host: MockHost) {
  const pending: Array<Deferred<BarSnapshotOutcome>> = [];

  host.answer('bar_snapshot', () => {
    const next = deferred<BarSnapshotOutcome>();

    pending.push(next);

    return next.promise;
  });

  return pending;
}

function barData(client: QueryClient) {
  return client.getQueryData(barSnapshotOptions().queryKey);
}

function barSequence(client: QueryClient) {
  const data = barData(client);

  return data ? barOutcomeSequence(data) : undefined;
}

async function startBar(host: MockHost, client: QueryClient) {
  const stop = barFeed.start(client);

  await vi.waitFor(() => expect(host.listenerCount(BAR_EVENT)).toBe(1));

  return stop;
}

describe('createFeed ordering (bar)', () => {
  it('subscribes before requesting the snapshot', async () => {
    const host = mockHost();
    const snapshots = queueSnapshots(host);
    const client = createQueryClient();
    const stop = barFeed.start(client);

    expect(host.callsOf('bar_snapshot')).toHaveLength(0);
    await vi.waitFor(() => expect(snapshots).toHaveLength(1));
    const commands = host.calls.map((call) => call.command);

    expect(commands.indexOf('plugin:event|listen')).toBeLessThan(commands.indexOf('bar_snapshot'));
    stop();
  });

  it('keeps an event that arrives before an older snapshot', async () => {
    const host = mockHost();
    const snapshots = queueSnapshots(host);
    const client = createQueryClient();
    const stop = await startBar(host, client);

    await vi.waitFor(() => expect(snapshots).toHaveLength(1));
    host.emit(BAR_EVENT, barChanged('5', barAvailable('5')));
    expect(barSequence(client)).toBe('5');
    snapshots[0]!.resolve(barAvailable('3'));
    await flush();
    expect(barSequence(client)).toBe('5');
    stop();
  });

  it('applies a newer snapshot over an earlier event', async () => {
    const host = mockHost();
    const snapshots = queueSnapshots(host);
    const client = createQueryClient();
    const stop = await startBar(host, client);

    await vi.waitFor(() => expect(snapshots).toHaveLength(1));
    host.emit(BAR_EVENT, barChanged('4', barAvailable('4')));
    snapshots[0]!.resolve(barAvailable('6'));
    await vi.waitFor(() => expect(barSequence(client)).toBe('6'));
    stop();
  });

  it('applies the snapshot then only strictly newer events', async () => {
    const host = mockHost();
    const snapshots = queueSnapshots(host);
    const client = createQueryClient();
    const stop = await startBar(host, client);

    await vi.waitFor(() => expect(snapshots).toHaveLength(1));
    snapshots[0]!.resolve(barAvailable('3'));
    await vi.waitFor(() => expect(barSequence(client)).toBe('3'));
    host.emit(BAR_EVENT, barChanged('4', barUnavailable('4')));
    expect(barData(client)?.kind).toBe('unavailable');
    host.emit(BAR_EVENT, barChanged('2', barAvailable('2')));
    expect(barSequence(client)).toBe('4');
    host.emit(BAR_EVENT, barChanged('4', barAvailable('4', { height: 99 })));
    expect(barData(client)?.kind).toBe('unavailable');
    host.emit(BAR_EVENT, barChanged('5', barAvailable('5')));
    expect(barSequence(client)).toBe('5');
    stop();
  });

  it('ignores a snapshot with the held sequence', async () => {
    const host = mockHost();
    const snapshots = queueSnapshots(host);
    const client = createQueryClient();
    const stop = await startBar(host, client);

    await vi.waitFor(() => expect(snapshots).toHaveLength(1));
    host.emit(BAR_EVENT, barChanged('7', barAvailable('7', { height: 1 })));
    snapshots[0]!.resolve(barAvailable('7', { height: 2 }));
    await flush();
    const data = barData(client);

    expect(data?.kind === 'available' && data.view.height).toBe(1);
    stop();
  });

  it('orders sequences above Number.MAX_SAFE_INTEGER exactly', async () => {
    const host = mockHost();
    const snapshots = queueSnapshots(host);
    const client = createQueryClient();
    const stop = await startBar(host, client);

    await vi.waitFor(() => expect(snapshots).toHaveLength(1));
    snapshots[0]!.resolve(barAvailable('9007199254740992'));
    await vi.waitFor(() => expect(barSequence(client)).toBe('9007199254740992'));
    // As numbers both are 2^53: the next one would be dropped by `Number`.
    host.emit(BAR_EVENT, barChanged('9007199254740993', barAvailable('9007199254740993')));
    expect(barSequence(client)).toBe('9007199254740993');
    host.emit(BAR_EVENT, barChanged('9007199254740992', barAvailable('9007199254740992')));
    expect(barSequence(client)).toBe('9007199254740993');
    host.emit(BAR_EVENT, barChanged('18446744073709551615', barAvailable('18446744073709551615')));
    expect(barSequence(client)).toBe('18446744073709551615');
    stop();
  });

  it('takes a new snapshot for a newer resnapshot request only', async () => {
    const host = mockHost();
    const snapshots = queueSnapshots(host);
    const client = createQueryClient();
    const stop = await startBar(host, client);

    await vi.waitFor(() => expect(snapshots).toHaveLength(1));
    snapshots[0]!.resolve(barAvailable('3'));
    await vi.waitFor(() => expect(barSequence(client)).toBe('3'));
    host.emit(BAR_EVENT, barChanged('2', null, true));
    await flush();
    expect(snapshots).toHaveLength(1);
    host.emit(BAR_EVENT, barChanged('9', null, true));
    expect(barSequence(client)).toBe('3');
    await vi.waitFor(() => expect(snapshots).toHaveLength(2));
    snapshots[1]!.resolve(barAvailable('9'));
    await vi.waitFor(() => expect(barSequence(client)).toBe('9'));
    stop();
  });

  it('keeps unauthorized only while nothing sequenced is held', async () => {
    const host = mockHost();
    const snapshots = queueSnapshots(host);
    const client = createQueryClient();
    const stop = await startBar(host, client);

    await vi.waitFor(() => expect(snapshots).toHaveLength(1));
    snapshots[0]!.resolve(barUnauthorized);
    await vi.waitFor(() => expect(barData(client)?.kind).toBe('unauthorized'));
    host.emit(BAR_EVENT, barChanged('1', barAvailable('1')));
    expect(barData(client)?.kind).toBe('available');
    host.emit(BAR_EVENT, barChanged('2', null, true));
    await vi.waitFor(() => expect(snapshots).toHaveLength(2));
    snapshots[1]!.resolve(barUnauthorized);
    await flush();
    expect(barData(client)?.kind).toBe('available');
    stop();
  });
});

describe('createFeed failures', () => {
  it('records a listen failure and requests no snapshot', async () => {
    const host = mockHost();

    host.answer('bar_snapshot', barAvailable('1'));
    host.failListen('listen refused');
    const client = createQueryClient();
    const stop = barFeed.start(client);

    await vi.waitFor(() =>
      expect(client.getQueryState(barSnapshotOptions().queryKey)?.error?.message).toContain(
        'listen refused',
      ),
    );
    expect(host.callsOf('bar_snapshot')).toHaveLength(0);
    stop();

    // A later start subscribes again and recovers.
    host.failListen(null);
    const again = barFeed.start(client);

    await vi.waitFor(() => expect(barSequence(client)).toBe('1'));
    const state = client.getQueryState(barSnapshotOptions().queryKey);

    expect(state?.status).toBe('success');
    expect(state?.error).toBeNull();
    again();
  });

  it('records a snapshot failure and clears it on the next applied event', async () => {
    const host = mockHost();

    host.answer('bar_snapshot', () => Promise.reject(new Error('snapshot exploded')));
    const client = createQueryClient();
    const stop = await startBar(host, client);
    const key = barSnapshotOptions().queryKey;

    await vi.waitFor(() => expect(client.getQueryState(key)?.status).toBe('error'));
    expect(client.getQueryState(key)?.error?.message).toBe('snapshot exploded');
    host.emit(BAR_EVENT, barChanged('1', barAvailable('1')));
    expect(client.getQueryState(key)?.status).toBe('success');
    expect(client.getQueryState(key)?.error).toBeNull();
    stop();
  });

  it('records a malformed sequence as an error and keeps the held data', async () => {
    const host = mockHost();
    const snapshots = queueSnapshots(host);
    const client = createQueryClient();
    const stop = await startBar(host, client);
    const key = barSnapshotOptions().queryKey;

    await vi.waitFor(() => expect(snapshots).toHaveLength(1));
    snapshots[0]!.resolve(barAvailable('2'));
    await vi.waitFor(() => expect(barSequence(client)).toBe('2'));
    host.emit(BAR_EVENT, barChanged('-1', barAvailable('-1')));
    expect(client.getQueryState(key)?.status).toBe('error');
    expect(client.getQueryState(key)?.error?.message).toContain('invalid sequence');
    expect(barSequence(client)).toBe('2');
    host.emit(BAR_EVENT, barChanged('3', barAvailable('3')));
    expect(client.getQueryState(key)?.error).toBeNull();
    stop();
  });

  it('wraps a non-Error rejection', async () => {
    const host = mockHost();

    host.answer('bar_snapshot', () => Promise.reject('plain'));
    const client = createQueryClient();
    const stop = await startBar(host, client);

    await vi.waitFor(() =>
      expect(client.getQueryState(barSnapshotOptions().queryKey)?.error?.message).toBe('plain'),
    );
    stop();
  });
});

describe('createFeed without sequences (host status)', () => {
  it('lets an event received during the snapshot win', async () => {
    const host = mockHost();
    const pending = deferred<HostStatus>();

    host.answer('host_status', () => pending.promise);
    const client = createQueryClient();
    const stop = hostStatusFeed.start(client);
    const key = hostStatusOptions().queryKey;

    await vi.waitFor(() => expect(host.callsOf('host_status')).toHaveLength(1));
    host.emit(STATUS_EVENT, { status: hostStatusFixture({ error: 'from event' }) });
    pending.resolve(hostStatusFixture({ error: 'from snapshot' }));
    await flush();
    expect(client.getQueryData(key)?.error).toBe('from event');
    host.emit(STATUS_EVENT, { status: hostStatusFixture({ error: null }) });
    expect(client.getQueryData(key)?.error).toBeNull();
    stop();
  });

  it('applies the snapshot when no event arrived', async () => {
    const host = mockHost();

    host.answer('host_status', hostStatusFixture({ ready: false }));
    const client = createQueryClient();
    const stop = hostStatusFeed.start(client);

    await vi.waitFor(() =>
      expect(client.getQueryData(hostStatusOptions().queryKey)?.ready).toBe(false),
    );
    stop();
  });
});

describe('createFeed lifetime', () => {
  it('unlistens a listen that resolves after disposal and requests no snapshot', async () => {
    const host = mockHost();

    host.answer('bar_snapshot', barAvailable('1'));
    host.holdListen();
    const client = createQueryClient();
    const stop = barFeed.start(client);

    await vi.waitFor(() => expect(host.callsOf('plugin:event|listen')).toHaveLength(1));
    stop();
    stop();
    host.releaseListen();
    await vi.waitFor(() => expect(host.callsOf('plugin:event|unlisten')).toHaveLength(1));
    expect(host.listenerCount(BAR_EVENT)).toBe(0);
    expect(host.callsOf('bar_snapshot')).toHaveLength(0);
    expect(barData(client)).toBeUndefined();
  });

  it('drops a snapshot that resolves after the last stop', async () => {
    const host = mockHost();
    const snapshots = queueSnapshots(host);
    const client = createQueryClient();
    const stop = await startBar(host, client);

    await vi.waitFor(() => expect(snapshots).toHaveLength(1));
    stop();
    snapshots[0]!.resolve(barAvailable('1'));
    await flush();
    expect(barData(client)).toBeUndefined();
    expect(host.listenerCount(BAR_EVENT)).toBe(0);
  });

  it('is reference counted per query client', async () => {
    const host = mockHost();

    host.answer('bar_snapshot', barAvailable('1'));
    const first = createQueryClient();
    const second = createQueryClient();
    const a = barFeed.start(first);
    const b = barFeed.start(first);

    await vi.waitFor(() => expect(host.listenerCount(BAR_EVENT)).toBe(1));
    const c = barFeed.start(second);

    await vi.waitFor(() => expect(host.listenerCount(BAR_EVENT)).toBe(2));
    a();
    await flush();
    expect(host.listenerCount(BAR_EVENT)).toBe(2);
    b();
    await vi.waitFor(() => expect(host.listenerCount(BAR_EVENT)).toBe(1));
    c();
    await vi.waitFor(() => expect(host.listenerCount(BAR_EVENT)).toBe(0));
    expect(host.callsOf('bar_snapshot')).toHaveLength(2);
  });

  it('leaves one listener under StrictMode and none after the last unmount', async () => {
    const host = mockHost();

    host.answer('bar_snapshot', barAvailable('1'));
    host.holdListen();
    const client = createQueryClient();

    function Consumer() {
      useFeed(barFeed);

      return <span>consumer</span>;
    }

    const screen = await render(
      <StrictMode>
        <QueryClientProvider client={client}>
          <Consumer />
          <Consumer />
        </QueryClientProvider>
      </StrictMode>,
    );

    await expect.element(screen.getByText('consumer').first()).toBeVisible();
    host.releaseListen();
    await vi.waitFor(() => expect(barSequence(client)).toBe('1'));
    expect(host.listenerCount(BAR_EVENT)).toBe(1);
    expect(host.callsOf('bar_snapshot')).toHaveLength(1);
    await screen.unmount();
    await vi.waitFor(() => expect(host.listenerCount(BAR_EVENT)).toBe(0));
  });
});
