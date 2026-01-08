import { describe, expect, it } from 'vitest';

import {
  queryKeyToStoreId,
  getOrCreateQueryStore,
  destroyQueryStore,
  getQueryStoreIds,
  type QueryStoreState,
  type UseStoreSyncOptions,
} from './useStoreQueryBase';

describe('useStoreQueryBase', () => {
  describe('queryKeyToStoreId', () => {
    it('should convert a simple query key to a store ID', () => {
      const storeId = queryKeyToStoreId(['test']);
      expect(storeId).toBe('query-["test"]');
    });

    it('should convert a complex query key to a store ID', () => {
      const storeId = queryKeyToStoreId(['users', 123, { active: true }]);
      expect(storeId).toBe('query-["users",123,{"active":true}]');
    });

    it('should handle empty query keys', () => {
      const storeId = queryKeyToStoreId([]);
      expect(storeId).toBe('query-[]');
    });

    it('should produce consistent IDs for the same query key', () => {
      const key = ['data', 'item', 42];
      const storeId1 = queryKeyToStoreId(key);
      const storeId2 = queryKeyToStoreId(key);
      expect(storeId1).toBe(storeId2);
    });
  });

  describe('getOrCreateQueryStore', () => {
    it('should create a store with the correct initial state', () => {
      const queryKey = ['test-store-creation'];
      const useStore = getOrCreateQueryStore<{ value: number }>(queryKey);

      const state = useStore.getState();
      expect(state.data).toBeUndefined();
      expect(state.lastUpdated).toBe(0);
      expect(typeof state.setData).toBe('function');
    });

    it('should return the same store for the same query key', () => {
      const queryKey = ['test-same-store'];
      const useStore1 = getOrCreateQueryStore<string>(queryKey);
      const useStore2 = getOrCreateQueryStore<string>(queryKey);

      expect(useStore1).toBe(useStore2);
    });

    it('should allow setting data through setData', () => {
      const queryKey = ['test-set-data'];
      const useStore = getOrCreateQueryStore<{ name: string }>(queryKey);

      const testData = { name: 'test' };
      useStore.getState().setData(testData);

      const state = useStore.getState();
      expect(state.data).toEqual(testData);
      expect(state.lastUpdated).toBeGreaterThan(0);
    });
  });

  describe('destroyQueryStore', () => {
    it('should be a function', () => {
      expect(typeof destroyQueryStore).toBe('function');
    });

    it('should return a promise', async () => {
      const result = destroyQueryStore(['non-existent']);
      expect(result).toBeInstanceOf(Promise);
      await result;
    });
  });

  describe('getQueryStoreIds', () => {
    it('should return an array', () => {
      const ids = getQueryStoreIds();
      expect(Array.isArray(ids)).toBe(true);
    });

    it('should only return IDs that start with "query-"', () => {
      const ids = getQueryStoreIds();
      ids.forEach((id) => {
        expect(id.startsWith('query-')).toBe(true);
      });
    });
  });

  describe('type exports', () => {
    it('should export QueryStoreState type', () => {
      // Type-only check - validates TypeScript compilation
      const state: QueryStoreState<string> = {
        data: 'test',
        lastUpdated: Date.now(),
        setData: () => {},
      };
      expect(state.data).toBe('test');
    });

    it('should export UseStoreSyncOptions type', () => {
      // Type-only check - validates TypeScript compilation
      const options: UseStoreSyncOptions<number> = {
        queryKey: ['test'],
        syncAcrossWindows: true,
        data: 42,
      };
      expect(options.data).toBe(42);
    });
  });
});
