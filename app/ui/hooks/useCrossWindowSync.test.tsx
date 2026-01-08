import { describe, expect, test, vi, beforeEach, afterEach } from 'vitest';
import { renderHook } from 'vitest-browser-react';

import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';

import { useCrossWindowSync, destroyQueryStore, getQueryStoreIds } from './useCrossWindowSync';

// Mock the createStore utilities
const mockSetData = vi.fn();
const mockStoreState = {
  data: undefined as unknown,
  lastUpdated: 0,
  setData: mockSetData,
};

// Create a mock store with selector support
const createMockStore = () => {
  let state = { ...mockStoreState };

  const useStore = <T,>(selector: (s: typeof mockStoreState) => T): T => {
    return selector(state);
  };

  // Allow tests to update state
  (
    useStore as unknown as { setState: (newState: Partial<typeof mockStoreState>) => void }
  ).setState = (newState: Partial<typeof mockStoreState>) => {
    state = { ...state, ...newState };
  };

  return useStore;
};

let mockUseStore = createMockStore();

vi.mock('@/utils/createStore', () => ({
  createStore: vi.fn(() => mockUseStore),
  getStore: vi.fn(() => ({ useStore: mockUseStore })),
  destroyStore: vi.fn(),
  getStoreIds: vi.fn(() => ['query-["test"]-store']),
}));

const {
  createStore,
  getStore,
  destroyStore: mockDestroyStore,
  getStoreIds: mockGetStoreIds,
} = await vi.importMock<typeof import('@/utils/createStore')>('@/utils/createStore');

describe('useCrossWindowSync', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockSetData.mockReset();
    mockUseStore = createMockStore();
    vi.mocked(createStore).mockReturnValue(mockUseStore as ReturnType<typeof createStore>);
    vi.mocked(getStore).mockReturnValue({ useStore: mockUseStore } as ReturnType<typeof getStore>);
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  describe('basic functionality', () => {
    test('does nothing when syncAcrossWindows is false', async () => {
      const queryClient = createTestQueryClient();

      await renderHook(
        () =>
          useCrossWindowSync({
            queryKey: ['test'],
            syncAcrossWindows: false,
            data: { value: 42 },
          }),
        { wrapper: createQueryClientWrapper(queryClient) },
      );

      expect(mockSetData).not.toHaveBeenCalled();
    });

    test('does nothing when data is undefined', async () => {
      const queryClient = createTestQueryClient();

      await renderHook(
        () =>
          useCrossWindowSync({
            queryKey: ['test'],
            syncAcrossWindows: true,
            data: undefined,
          }),
        { wrapper: createQueryClientWrapper(queryClient) },
      );

      expect(mockSetData).not.toHaveBeenCalled();
    });

    test('syncs data to store when data changes', async () => {
      const queryClient = createTestQueryClient();

      await renderHook(
        () =>
          useCrossWindowSync({
            queryKey: ['test'],
            syncAcrossWindows: true,
            data: { value: 42 },
          }),
        { wrapper: createQueryClientWrapper(queryClient) },
      );

      await vi.waitFor(() => {
        expect(mockSetData).toHaveBeenCalledWith({ value: 42 });
      });
    });

    test('uses existing store if available', async () => {
      const queryClient = createTestQueryClient();

      // First render
      const { rerender } = await renderHook(
        () =>
          useCrossWindowSync({
            queryKey: ['test'],
            syncAcrossWindows: true,
            data: { value: 1 },
          }),
        { wrapper: createQueryClientWrapper(queryClient) },
      );

      // Clear mock to check subsequent calls
      mockSetData.mockClear();

      // Second render with new data
      await rerender();

      // Should still use the same store
      expect(getStore).toHaveBeenCalled();
    });

    test('creates unique store IDs for different query keys', async () => {
      const queryClient = createTestQueryClient();

      await renderHook(
        () => {
          useCrossWindowSync({
            queryKey: ['key1'],
            syncAcrossWindows: true,
            data: { a: 1 },
          });
          useCrossWindowSync({
            queryKey: ['key2', 'nested'],
            syncAcrossWindows: true,
            data: { b: 2 },
          });
        },
        { wrapper: createQueryClientWrapper(queryClient) },
      );

      // Both hooks should interact with the store
      expect(getStore).toHaveBeenCalled();
    });
  });

  describe('data synchronization', () => {
    test('only syncs when data reference changes', async () => {
      const queryClient = createTestQueryClient();
      const data = { value: 42 };

      const { rerender } = await renderHook(
        (props?: { data: typeof data }) =>
          useCrossWindowSync({
            queryKey: ['test'],
            syncAcrossWindows: true,
            data: props?.data ?? data,
          }),
        {
          wrapper: createQueryClientWrapper(queryClient),
          initialProps: { data },
        },
      );

      await vi.waitFor(() => {
        expect(mockSetData).toHaveBeenCalledTimes(1);
      });

      // Rerender with same reference - should not sync again
      await rerender({ data });

      // Still only 1 call
      expect(mockSetData).toHaveBeenCalledTimes(1);
    });

    test('syncs when data reference changes', async () => {
      const queryClient = createTestQueryClient();

      const { rerender } = await renderHook(
        (props?: { data: { value: number } }) =>
          useCrossWindowSync({
            queryKey: ['test'],
            syncAcrossWindows: true,
            data: props?.data,
          }),
        {
          wrapper: createQueryClientWrapper(queryClient),
          initialProps: { data: { value: 1 } },
        },
      );

      await vi.waitFor(() => {
        expect(mockSetData).toHaveBeenCalledWith({ value: 1 });
      });

      // Rerender with different reference
      await rerender({ data: { value: 2 } });

      await vi.waitFor(() => {
        expect(mockSetData).toHaveBeenCalledWith({ value: 2 });
      });
    });
  });
});

describe('destroyQueryStore', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  test('calls destroyStore with correct store ID', async () => {
    await destroyQueryStore(['test', 'key']);

    expect(mockDestroyStore).toHaveBeenCalledWith('query-["test","key"]');
  });

  test('handles complex query keys', async () => {
    await destroyQueryStore(['users', { id: 123 }]);

    expect(mockDestroyStore).toHaveBeenCalledWith('query-["users",{"id":123}]');
  });
});

describe('getQueryStoreIds', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  test('filters store IDs to only include query stores', () => {
    vi.mocked(mockGetStoreIds).mockReturnValue([
      'query-["test"]-store',
      'query-["other"]-store',
      'battery-store',
      'weather-store',
    ]);

    const ids = getQueryStoreIds();

    expect(ids).toEqual(['query-["test"]-store', 'query-["other"]-store']);
  });

  test('returns empty array when no query stores exist', () => {
    vi.mocked(mockGetStoreIds).mockReturnValue(['battery-store', 'weather-store']);

    const ids = getQueryStoreIds();

    expect(ids).toEqual([]);
  });
});
