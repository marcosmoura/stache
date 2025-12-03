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
      expect(listenMock).toHaveBeenCalledWith(eventName, handler);
    });

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

    await waitForListenCallToResolve(1);

    await unmount();

    expect(secondUnlisten).toHaveBeenCalledTimes(1);
  });

  test('re-registers the listener when the callback reference changes', async () => {
    const firstUnlisten = vi.fn();
    const secondUnlisten = vi.fn();

    const firstHandler = vi.fn();
    const secondHandler = vi.fn();

    listenMock.mockResolvedValueOnce(firstUnlisten).mockResolvedValueOnce(secondUnlisten);

    const { rerender, unmount } = await renderHook(
      (props?: { onEvent: (payload: unknown) => void }) =>
        useTauriEvent('shared-event', props!.onEvent),
      {
        initialProps: {
          onEvent: firstHandler,
        },
      },
    );

    await vi.waitFor(() => {
      expect(listenMock).toHaveBeenCalledWith('shared-event', firstHandler);
    });

    await waitForListenCallToResolve(0);

    await rerender({ onEvent: secondHandler });

    expect(firstUnlisten).toHaveBeenCalledTimes(1);

    await vi.waitFor(() => {
      expect(listenMock).toHaveBeenLastCalledWith('shared-event', secondHandler);
    });

    await waitForListenCallToResolve(1);

    await unmount();

    expect(secondUnlisten).toHaveBeenCalledTimes(1);
  });
});
