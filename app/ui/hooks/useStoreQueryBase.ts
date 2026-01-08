/**
 * @internal Shared base utilities for store query hooks
 *
 * This module provides common functionality used by both useSuspenseStoreQuery
 * and useStoreQuery hooks for cross-window state synchronization.
 */
import { useEffect, useRef } from 'react';

import { useQueryClient, type QueryKey } from '@tanstack/react-query';
import type { StoreApi, UseBoundStore } from 'zustand';

import { createStore, getStore, destroyStore, getStoreIds, type State } from '@/utils/createStore';

/** State structure for query stores */
export interface QueryStoreState<TData> extends State {
  data: TData | undefined;
  lastUpdated: number;
  setData: (data: TData) => void;
}

/**
 * Creates a unique store ID from a query key.
 */
export function queryKeyToStoreId(queryKey: QueryKey): string {
  return `query-${JSON.stringify(queryKey)}`;
}

/**
 * Gets or creates a store for a query key using createStore.
 */
export function getOrCreateQueryStore<TData>(
  queryKey: QueryKey,
): UseBoundStore<StoreApi<QueryStoreState<TData>>> {
  const storeId = queryKeyToStoreId(queryKey);

  // Check if store already exists
  const existingStore = getStore<QueryStoreState<TData>>(storeId);
  if (existingStore) {
    return existingStore.useStore;
  }

  // Create new store using createStore
  return createStore<QueryStoreState<TData>>(
    storeId,
    (set) => ({
      data: undefined,
      lastUpdated: 0,
      setData: (data: TData) =>
        set((state) => {
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          state.data = data as any;
          state.lastUpdated = Date.now();
        }),
    }),
    {
      autoStart: true,
      syncStrategy: 'debounce',
      syncInterval: 50,
      save: false,
      filterKeys: ['setData'],
    },
  );
}

/** Options for the store sync hook */
export interface UseStoreSyncOptions<TData> {
  queryKey: QueryKey;
  syncAcrossWindows: boolean;
  data: TData | undefined;
}

/**
 * Hook that handles bidirectional synchronization between React Query cache
 * and the Tauri store for cross-window state sharing.
 *
 * @param options - Sync options including query key, sync flag, and current data
 */
export function useStoreSync<TData>({
  queryKey,
  syncAcrossWindows,
  data,
}: UseStoreSyncOptions<TData>): void {
  const queryClient = useQueryClient();

  // Ref to prevent circular updates between store and query
  const isSyncingFromQueryRef = useRef(false);
  const lastSyncedDataRef = useRef<TData | undefined>(undefined);

  // Get or create the store for this query
  const useQueryStore = getOrCreateQueryStore<TData>(queryKey);

  // Subscribe to store data changes for cross-window sync
  const storeData = useQueryStore((state) => state.data);
  const setStoreData = useQueryStore((state) => state.setData);

  // Sync query data to the store for other windows
  useEffect(() => {
    if (!syncAcrossWindows || data === undefined) return;

    // Only update if data has actually changed (by reference)
    if (lastSyncedDataRef.current !== data) {
      isSyncingFromQueryRef.current = true;
      lastSyncedDataRef.current = data;
      setStoreData(data);

      // Reset flag after the sync completes
      queueMicrotask(() => {
        isSyncingFromQueryRef.current = false;
      });
    }
  }, [data, syncAcrossWindows, setStoreData]);

  // Sync store data to query cache (for updates from other windows)
  useEffect(() => {
    if (!syncAcrossWindows || isSyncingFromQueryRef.current) return;

    // Only sync if store has data and it's different from what we last synced
    if (storeData !== undefined && storeData !== lastSyncedDataRef.current) {
      lastSyncedDataRef.current = storeData;
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      queryClient.setQueryData(queryKey, storeData as any);
    }
  }, [storeData, syncAcrossWindows, queryClient, queryKey]);
}

/**
 * Destroys a query store and removes it from the registry.
 *
 * @param queryKey - The query key used to create the store
 */
export async function destroyQueryStore(queryKey: QueryKey): Promise<void> {
  const storeId = queryKeyToStoreId(queryKey);
  await destroyStore(storeId);
}

/**
 * Gets all registered query store IDs.
 */
export function getQueryStoreIds(): string[] {
  return getStoreIds().filter((id) => id.startsWith('query-'));
}
