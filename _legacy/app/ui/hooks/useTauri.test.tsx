import { Suspense } from 'react';

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { beforeEach, describe, expect, test, vi } from 'vitest';
import { render, renderHook } from 'vitest-browser-react';

import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';

import { useTauri, useTauriSuspense } from './useTauri';

// Mock Tauri APIs
vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(),
}));

// Mock cross-window sync to avoid Zustand store dependencies in tests
vi.mock('./useCrossWindowSync', () => ({
  useCrossWindowSync: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);
const mockListen = vi.mocked(listen);

const waitForListenCallToResolve = async (callIndex: number) => {
  await vi.waitFor(() => {
    expect(mockListen.mock.results[callIndex]).toBeDefined();
  });

  const result = mockListen.mock.results[callIndex]?.value as Promise<unknown> | undefined;
  await result;
};

describe('useTauri', () => {
  let mockUnlisten: UnlistenFn;

  beforeEach(() => {
    mockUnlisten = vi.fn() as unknown as UnlistenFn;
    mockInvoke.mockReset();
    mockListen.mockReset();
    mockListen.mockResolvedValue(mockUnlisten);
  });

  describe('basic functionality', () => {
    test('fetches data using command', async () => {
      const queryClient = createTestQueryClient();
      const mockData = { value: 42 };
      mockInvoke.mockResolvedValue(mockData);

      const { result } = await renderHook(
        () =>
          useTauri<typeof mockData>({
            queryKey: ['test'],
            command: 'test_command',
          }),
        { wrapper: createQueryClientWrapper(queryClient) },
      );

      await vi.waitFor(() => {
        expect(result.current.isSuccess).toBe(true);
      });

      expect(mockInvoke).toHaveBeenCalledWith('test_command', undefined);
      expect(result.current.data).toEqual(mockData);
    });

    test('passes command arguments', async () => {
      const queryClient = createTestQueryClient();
      const mockData = { value: 'result' };
      mockInvoke.mockResolvedValue(mockData);

      const { result } = await renderHook(
        () =>
          useTauri<typeof mockData>({
            queryKey: ['test', 'arg1'],
            command: 'test_command',
            commandArgs: { id: 123, name: 'test' },
          }),
        { wrapper: createQueryClientWrapper(queryClient) },
      );

      await vi.waitFor(() => {
        expect(result.current.isSuccess).toBe(true);
      });

      expect(mockInvoke).toHaveBeenCalledWith('test_command', { id: 123, name: 'test' });
    });

    test('uses custom queryFn when provided', async () => {
      const queryClient = createTestQueryClient();
      const mockData = { custom: true };
      const customFn = vi.fn().mockResolvedValue(mockData);

      const { result } = await renderHook(
        () =>
          useTauri<typeof mockData>({
            queryKey: ['custom'],
            queryFn: customFn,
          }),
        { wrapper: createQueryClientWrapper(queryClient) },
      );

      await vi.waitFor(() => {
        expect(result.current.isSuccess).toBe(true);
      });

      expect(customFn).toHaveBeenCalled();
      expect(mockInvoke).not.toHaveBeenCalled();
      expect(result.current.data).toEqual(mockData);
    });

    test('rejects query when neither command nor queryFn provided', async () => {
      const queryClient = createTestQueryClient();
      const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});

      // Configure query client to capture errors
      queryClient.setDefaultOptions({
        queries: {
          ...queryClient.getDefaultOptions().queries,
          throwOnError: false,
        },
      });

      await renderHook(
        () =>
          useTauri({
            queryKey: ['invalid-no-command'],
          }),
        { wrapper: createQueryClientWrapper(queryClient) },
      );

      // Give React Query time to attempt the fetch and fail
      await vi.waitFor(
        () => {
          const queryState = queryClient.getQueryState(['invalid-no-command']);
          expect(queryState?.status).toBe('error');
        },
        { timeout: 2000 },
      );

      const queryState = queryClient.getQueryState(['invalid-no-command']);
      expect(queryState?.error).toBeDefined();
      expect((queryState?.error as Error)?.message).toContain(
        'Either `command` or `queryFn` must be provided',
      );

      consoleError.mockRestore();
    });

    test('handles query errors from command', async () => {
      const queryClient = createTestQueryClient();
      const error = new Error('Command failed');
      mockInvoke.mockRejectedValue(error);
      const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});

      await renderHook(
        () =>
          useTauri({
            queryKey: ['error-test-command'],
            command: 'failing_command',
          }),
        { wrapper: createQueryClientWrapper(queryClient) },
      );

      // Give React Query time to attempt the fetch and fail
      await vi.waitFor(
        () => {
          const queryState = queryClient.getQueryState(['error-test-command']);
          expect(queryState?.status).toBe('error');
        },
        { timeout: 2000 },
      );

      const queryState = queryClient.getQueryState(['error-test-command']);
      expect(queryState?.error).toBe(error);

      consoleError.mockRestore();
    });

    test('respects enabled option', async () => {
      const queryClient = createTestQueryClient();
      mockInvoke.mockResolvedValue({ data: 'test' });

      const { result, rerender } = await renderHook(
        (props?: { enabled: boolean }) =>
          useTauri({
            queryKey: ['conditional'],
            command: 'test_command',
            enabled: props?.enabled ?? false,
          }),
        {
          wrapper: createQueryClientWrapper(queryClient),
          initialProps: { enabled: false },
        },
      );

      // Should not fetch when disabled
      expect(result.current.fetchStatus).toBe('idle');
      expect(mockInvoke).not.toHaveBeenCalled();

      // Enable and verify fetch
      await rerender({ enabled: true });

      await vi.waitFor(() => {
        expect(result.current.isSuccess).toBe(true);
      });

      expect(mockInvoke).toHaveBeenCalled();
    });
  });

  describe('event listening', () => {
    test('sets up event listener when eventName provided', async () => {
      const queryClient = createTestQueryClient();
      mockInvoke.mockResolvedValue({ initial: true });

      await renderHook(
        () =>
          useTauri({
            queryKey: ['events'],
            command: 'test_command',
            eventName: 'test://event',
          }),
        { wrapper: createQueryClientWrapper(queryClient) },
      );

      await vi.waitFor(() => {
        expect(mockListen).toHaveBeenCalledWith('test://event', expect.any(Function));
      });
    });

    test('updates cache when event received', async () => {
      const queryClient = createTestQueryClient();
      const setQueryDataSpy = vi.spyOn(queryClient, 'setQueryData');
      mockInvoke.mockResolvedValue({ value: 'initial' });

      const { result, act } = await renderHook(
        () =>
          useTauri<{ value: string }>({
            queryKey: ['event-update'],
            command: 'test_command',
            eventName: 'test://update',
          }),
        { wrapper: createQueryClientWrapper(queryClient) },
      );

      await vi.waitFor(() => {
        expect(result.current.isSuccess).toBe(true);
      });

      // Wait for listener to be set up
      await vi.waitFor(() => {
        expect(mockListen).toHaveBeenCalledWith('test://update', expect.any(Function));
      });
      await waitForListenCallToResolve(0);

      // Get the callback from the mock call and simulate an event
      const stableCallback = mockListen.mock.calls[0]?.[1] as
        | ((event: { payload: unknown }) => void)
        | undefined;
      expect(stableCallback).toBeDefined();

      await act(() => {
        stableCallback?.({ payload: { value: 'from-event' } });
      });

      // Verify setQueryData was called with correct arguments
      expect(setQueryDataSpy).toHaveBeenCalledWith(['event-update'], { value: 'from-event' });

      // Verify the cache was actually updated
      const cachedData = queryClient.getQueryData(['event-update']);
      expect(cachedData).toEqual({ value: 'from-event' });
    });

    test('transforms event payload when eventTransform provided', async () => {
      const queryClient = createTestQueryClient();
      const setQueryDataSpy = vi.spyOn(queryClient, 'setQueryData');
      mockInvoke.mockResolvedValue({ count: 0 });

      const { result, act } = await renderHook(
        () =>
          useTauri<{ count: number }>({
            queryKey: ['transform'],
            command: 'test_command',
            eventName: 'test://transform',
            eventTransform: (payload) => ({
              count: (payload as { raw: number }).raw * 2,
            }),
          }),
        { wrapper: createQueryClientWrapper(queryClient) },
      );

      await vi.waitFor(() => {
        expect(result.current.isSuccess).toBe(true);
      });

      // Wait for listener to be set up
      await vi.waitFor(() => {
        expect(mockListen).toHaveBeenCalledWith('test://transform', expect.any(Function));
      });
      await waitForListenCallToResolve(0);

      // Get the callback and simulate an event
      const stableCallback = mockListen.mock.calls[0]?.[1] as
        | ((event: { payload: unknown }) => void)
        | undefined;
      expect(stableCallback).toBeDefined();

      await act(() => {
        stableCallback?.({ payload: { raw: 21 } });
      });

      // Verify setQueryData was called with transformed data
      expect(setQueryDataSpy).toHaveBeenCalledWith(['transform'], { count: 42 });

      // Verify the cache was actually updated with transformed value
      const cachedData = queryClient.getQueryData(['transform']);
      expect(cachedData).toEqual({ count: 42 });
    });

    test('cleans up event listener on unmount', async () => {
      const queryClient = createTestQueryClient();
      mockInvoke.mockResolvedValue({});

      const { unmount } = await renderHook(
        () =>
          useTauri({
            queryKey: ['cleanup'],
            command: 'test_command',
            eventName: 'test://cleanup',
          }),
        { wrapper: createQueryClientWrapper(queryClient) },
      );

      await vi.waitFor(() => {
        expect(mockListen).toHaveBeenCalled();
      });

      await waitForListenCallToResolve(0);

      await unmount();

      expect(mockUnlisten).toHaveBeenCalled();
    });

    test('does not set up listener when eventName not provided', async () => {
      const queryClient = createTestQueryClient();
      mockInvoke.mockResolvedValue({});

      await renderHook(
        () =>
          useTauri({
            queryKey: ['no-events'],
            command: 'test_command',
          }),
        { wrapper: createQueryClientWrapper(queryClient) },
      );

      await vi.waitFor(() => {
        expect(mockInvoke).toHaveBeenCalled();
      });

      expect(mockListen).not.toHaveBeenCalled();
    });
  });

  describe('React Query integration', () => {
    test('passes through additional React Query options', async () => {
      const queryClient = createTestQueryClient();
      const mockData = { timestamp: Date.now() };
      mockInvoke.mockResolvedValue(mockData);

      const { result } = await renderHook(
        () =>
          useTauri<typeof mockData>({
            queryKey: ['stale-time-test'],
            command: 'test_command',
            staleTime: 60_000, // React Query option
            gcTime: 120_000, // React Query option
          }),
        { wrapper: createQueryClientWrapper(queryClient) },
      );

      await vi.waitFor(() => {
        expect(result.current.isSuccess).toBe(true);
      });

      expect(result.current.data).toEqual(mockData);
      expect(result.current.isStale).toBe(false); // Not stale due to staleTime
    });
  });
});

describe('useTauriSuspense', () => {
  let mockUnlisten: UnlistenFn;

  beforeEach(() => {
    mockUnlisten = vi.fn() as unknown as UnlistenFn;
    mockInvoke.mockReset();
    mockListen.mockReset();
    mockListen.mockResolvedValue(mockUnlisten);
  });

  test('fetches data and suspends until ready', async () => {
    const queryClient = createTestQueryClient();
    mockInvoke.mockResolvedValue({ value: 'suspense-data' });

    const TestComponent = () => {
      const { data } = useTauriSuspense<{ value: string }>({
        queryKey: ['suspense-test'],
        command: 'test_command',
      });
      return <div data-testid="result">{data.value}</div>;
    };

    const SuspenseWrapper = createQueryClientWrapper(queryClient);
    const screen = await render(
      <SuspenseWrapper>
        <Suspense fallback={<div data-testid="loading">Loading...</div>}>
          <TestComponent />
        </Suspense>
      </SuspenseWrapper>,
    );

    await vi.waitFor(async () => {
      await expect.element(screen.getByTestId('result')).toBeVisible();
    });

    await expect.element(screen.getByTestId('result')).toHaveTextContent('suspense-data');
  });

  test('sets up event listener', async () => {
    const queryClient = createTestQueryClient();
    mockInvoke.mockResolvedValue({});

    const TestComponent = () => {
      useTauriSuspense({
        queryKey: ['suspense-events'],
        command: 'test_command',
        eventName: 'test://suspense-event',
      });
      return <div>Loaded</div>;
    };

    const SuspenseWrapper = createQueryClientWrapper(queryClient);
    await render(
      <SuspenseWrapper>
        <Suspense fallback={<div>Loading...</div>}>
          <TestComponent />
        </Suspense>
      </SuspenseWrapper>,
    );

    await vi.waitFor(() => {
      expect(mockListen).toHaveBeenCalledWith('test://suspense-event', expect.any(Function));
    });
  });

  test('supports custom queryFn', async () => {
    const queryClient = createTestQueryClient();
    const customFn = vi.fn().mockResolvedValue({ custom: true });

    const TestComponent = () => {
      const { data } = useTauriSuspense<{ custom: boolean }>({
        queryKey: ['suspense-custom'],
        queryFn: customFn,
      });
      return <div data-testid="result">{String(data.custom)}</div>;
    };

    const SuspenseWrapper = createQueryClientWrapper(queryClient);
    const screen = await render(
      <SuspenseWrapper>
        <Suspense fallback={<div>Loading...</div>}>
          <TestComponent />
        </Suspense>
      </SuspenseWrapper>,
    );

    await vi.waitFor(async () => {
      await expect.element(screen.getByTestId('result')).toBeVisible();
    });

    expect(customFn).toHaveBeenCalled();
    expect(mockInvoke).not.toHaveBeenCalled();
  });
});
