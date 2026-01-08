import { listen } from '@tauri-apps/api/event';
import { beforeEach, describe, expect, test, vi } from 'vitest';
import { renderHook } from 'vitest-browser-react';

import { useTauriEvent } from './useTauriEvent';

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(),
}));

const listenMock = vi.mocked(listen);

const waitForListenCallToResolve = async (callIndex: number) => {
  await vi.waitFor(() => {
    expect(listenMock.mock.results[callIndex]).toBeDefined();
  });

  const result = listenMock.mock.results[callIndex]?.value as Promise<unknown> | undefined;
  await result;
};

describe('useTauriEvent', () => {
  beforeEach(() => {
    listenMock.mockReset();
  });

  test('registers the Tauri listener and cleans it up on unmount', async () => {
    const eventName = 'test-event';
    const handler = vi.fn();
    const unlisten = vi.fn();

    listenMock.mockResolvedValue(unlisten);

    const { unmount } = await renderHook(() => useTauriEvent(eventName, handler));

    await vi.waitFor(() => {
      expect(listenMock).toHaveBeenCalledTimes(1);
    });

    const [calledEventName, stableCallback, target] = listenMock.mock.calls[0] ?? [];
    expect(calledEventName).toBe(eventName);
    expect(typeof stableCallback).toBe('function');
    expect(target).toBeUndefined();

    const payload = { payload: { foo: 'bar' } } as never;
    (stableCallback as (event: unknown) => void)(payload);

    expect(handler).toHaveBeenCalledWith(payload);

    await waitForListenCallToResolve(0);

    await unmount();

    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  test('re-registers the listener when the event name changes', async () => {
    const firstUnlisten = vi.fn();
    const secondUnlisten = vi.fn();
    const handler = vi.fn();

    listenMock.mockResolvedValueOnce(firstUnlisten).mockResolvedValueOnce(secondUnlisten);

    const { rerender, unmount } = await renderHook(
      (props?: { eventName: string }) => useTauriEvent(props!.eventName, handler),
      {
        initialProps: {
          eventName: 'first-event',
        },
      },
    );

    await vi.waitFor(() => {
      expect(listenMock).toHaveBeenCalledTimes(1);
    });

    await waitForListenCallToResolve(0);

    await rerender({ eventName: 'second-event' });

    expect(firstUnlisten).toHaveBeenCalledTimes(1);

    await vi.waitFor(() => {
      expect(listenMock).toHaveBeenCalledTimes(2);
    });

    const secondCall = listenMock.mock.calls[1];
    expect(secondCall?.[0]).toBe('second-event');
    expect(typeof secondCall?.[1]).toBe('function');
    expect(secondCall?.[2]).toBeUndefined();

    await waitForListenCallToResolve(1);

    await unmount();

    expect(secondUnlisten).toHaveBeenCalledTimes(1);
  });

  test('updates the callback reference without re-subscribing', async () => {
    const handlerOne = vi.fn();
    const handlerTwo = vi.fn();
    const unlisten = vi.fn();

    listenMock.mockResolvedValue(unlisten);

    const { rerender, unmount } = await renderHook(
      (props?: { onEvent: (payload: unknown) => void }) =>
        useTauriEvent('shared-event', props!.onEvent),
      {
        initialProps: {
          onEvent: handlerOne,
        },
      },
    );

    await vi.waitFor(() => {
      expect(listenMock).toHaveBeenCalledTimes(1);
    });

    const stableCallback = listenMock.mock.calls[0]?.[1] as
      | ((payload: unknown) => void)
      | undefined;
    expect(typeof stableCallback).toBe('function');

    const firstPayload = { payload: 1 } as never;
    stableCallback?.(firstPayload);
    expect(handlerOne).toHaveBeenCalledWith(firstPayload);

    await rerender({ onEvent: handlerTwo });

    expect(listenMock).toHaveBeenCalledTimes(1);

    const secondPayload = { payload: 2 } as never;
    stableCallback?.(secondPayload);
    expect(handlerTwo).toHaveBeenCalledWith(secondPayload);
    expect(handlerOne).toHaveBeenCalledTimes(1);

    await waitForListenCallToResolve(0);
    await unmount();

    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  test('passes targetWindowLabel as target option when provided', async () => {
    const eventName = 'window-event';
    const handler = vi.fn();
    const unlisten = vi.fn();
    const targetWindowLabel = 'main-window';

    listenMock.mockResolvedValue(unlisten);

    const { unmount } = await renderHook(() =>
      useTauriEvent(eventName, handler, targetWindowLabel),
    );

    await vi.waitFor(() => {
      expect(listenMock).toHaveBeenCalledTimes(1);
    });

    const [calledEventName, stableCallback, target] = listenMock.mock.calls[0] ?? [];
    expect(calledEventName).toBe(eventName);
    expect(typeof stableCallback).toBe('function');
    expect(target).toEqual({ target: targetWindowLabel });

    await waitForListenCallToResolve(0);

    await unmount();

    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  test('re-registers the listener when targetWindowLabel changes', async () => {
    const firstUnlisten = vi.fn();
    const secondUnlisten = vi.fn();
    const handler = vi.fn();

    listenMock.mockResolvedValueOnce(firstUnlisten).mockResolvedValueOnce(secondUnlisten);

    const { rerender, unmount } = await renderHook(
      (props?: { targetWindowLabel: string }) =>
        useTauriEvent('target-event', handler, props!.targetWindowLabel),
      {
        initialProps: {
          targetWindowLabel: 'window-1',
        },
      },
    );

    await vi.waitFor(() => {
      expect(listenMock).toHaveBeenCalledTimes(1);
    });

    const firstCall = listenMock.mock.calls[0];
    expect(firstCall?.[0]).toBe('target-event');
    expect(typeof firstCall?.[1]).toBe('function');
    expect(firstCall?.[2]).toEqual({ target: 'window-1' });

    await waitForListenCallToResolve(0);

    await rerender({ targetWindowLabel: 'window-2' });

    expect(firstUnlisten).toHaveBeenCalledTimes(1);

    await vi.waitFor(() => {
      expect(listenMock).toHaveBeenCalledTimes(2);
    });

    const secondCall = listenMock.mock.calls[1];
    expect(secondCall?.[0]).toBe('target-event');
    expect(typeof secondCall?.[1]).toBe('function');
    expect(secondCall?.[2]).toEqual({ target: 'window-2' });

    await waitForListenCallToResolve(1);

    await unmount();

    expect(secondUnlisten).toHaveBeenCalledTimes(1);
  });
});
