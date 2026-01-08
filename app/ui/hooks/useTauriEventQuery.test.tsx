import { listen, type Event } from '@tauri-apps/api/event';
import { beforeEach, describe, expect, test, vi } from 'vitest';
import { renderHook } from 'vitest-browser-react';

import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';

import { useTauriEventQuery } from './useTauriEventQuery';

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(),
}));

const listenMock = vi.mocked(listen);

describe('useTauriEventQuery', () => {
  beforeEach(() => {
    listenMock.mockReset();
  });

  test('registers the Tauri listener and cleans it up on unmount', async () => {
    const eventName = 'custom-event';
    const unlisten = vi.fn();
    listenMock.mockResolvedValue(unlisten);

    const queryClient = createTestQueryClient();
    const wrapper = createQueryClientWrapper(queryClient);

    const { unmount } = await renderHook(() => useTauriEventQuery<{ foo: string }>({ eventName }), {
      wrapper,
    });

    await vi.waitFor(() => {
      expect(listenMock).toHaveBeenCalledWith(eventName, expect.any(Function), undefined);
    });

    await unmount();

    expect(unlisten).toHaveBeenCalledTimes(1);
    queryClient.clear();
  });

  test('stores the latest payload in the query cache', async () => {
    const eventName = 'payload-event';
    const unlisten = vi.fn();
    type Payload = { foo: string };
    let handler: ((event: Event<Payload>) => void) | undefined;

    listenMock.mockImplementation(async (_event, eventHandler) => {
      handler = eventHandler as typeof handler;
      return unlisten;
    });

    const queryClient = createTestQueryClient();
    const wrapper = createQueryClientWrapper(queryClient);

    const { result, unmount } = await renderHook(() => useTauriEventQuery<Payload>({ eventName }), {
      wrapper,
    });

    await vi.waitFor(() => {
      expect(handler).toBeDefined();
    });

    handler?.({ event: eventName, id: 0, payload: { foo: 'bar' } });

    await vi.waitFor(() => {
      expect(result.current.data).toEqual({ foo: 'bar' });
    });

    await unmount();
    queryClient.clear();
  });

  test('applies the transform function before caching', async () => {
    const eventName = 'transform-event';
    const unlisten = vi.fn();
    type Payload = { value: number };
    let handler: ((event: Event<Payload>) => void) | undefined;

    listenMock.mockImplementation(async (_event, eventHandler) => {
      handler = eventHandler as typeof handler;
      return unlisten;
    });

    const transformFn = vi.fn((payload: Payload) => payload.value * 2);

    const queryClient = createTestQueryClient();
    const wrapper = createQueryClientWrapper(queryClient);

    const { result, unmount } = await renderHook(
      () =>
        useTauriEventQuery<Payload, number>({
          eventName,
          transformFn,
        }),
      { wrapper },
    );

    await vi.waitFor(() => {
      expect(handler).toBeDefined();
    });

    handler?.({ event: eventName, id: 1, payload: { value: 21 } });

    await vi.waitFor(() => {
      expect(result.current.data).toBe(42);
    });
    expect(transformFn).toHaveBeenCalledWith({ value: 21 });

    await unmount();
    queryClient.clear();
  });
});
