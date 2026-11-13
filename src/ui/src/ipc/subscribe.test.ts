import { describe, expect, it, vi } from 'vitest';

import { barChanged, barUnauthorized } from '../../tests/fixtures';
import { flush, mockHost } from '../../tests/ipc';

import { events } from './index';
import { subscribe } from './subscribe';

describe('subscribe', () => {
  it('delivers payloads until disposed, then unlistens once', async () => {
    const host = mockHost();
    const handler = vi.fn();
    const onReady = vi.fn();
    const dispose = subscribe(events.barChanged, handler, { onReady });

    await vi.waitFor(() => expect(onReady).toHaveBeenCalledOnce());
    host.emit('bar-changed', barChanged('1', barUnauthorized));
    expect(handler).toHaveBeenCalledWith(barChanged('1', barUnauthorized));
    dispose();
    dispose();
    host.emit('bar-changed', barChanged('2', barUnauthorized));
    expect(handler).toHaveBeenCalledOnce();
    await vi.waitFor(() => expect(host.callsOf('plugin:event|unlisten')).toHaveLength(1));
  });

  it('reports a listen failure, but not after disposal', async () => {
    const host = mockHost();
    const onError = vi.fn();

    host.failListen('nope');
    subscribe(events.statusChanged, () => {}, { onError });
    await vi.waitFor(() => expect(onError).toHaveBeenCalledOnce());

    const late = vi.fn();

    subscribe(events.statusChanged, () => {}, { onError: late })();
    await flush();
    expect(late).not.toHaveBeenCalled();
  });

  it('works without options', async () => {
    const host = mockHost();
    const dispose = subscribe(events.statusChanged, () => {});

    await vi.waitFor(() => expect(host.listenerCount('status-changed')).toBe(1));
    dispose();
  });
});
