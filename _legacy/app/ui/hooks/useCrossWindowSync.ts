/**
 * Cross-window state synchronization utilities.
 *
 * This module provides functionality for synchronizing React Query cache
 * state across multiple Tauri windows using Zustand stores with @tauri-store/zustand.
 */
import { useEffect, useMemo, useRef } from 'react';

import { hashKey, useQueryClient, type QueryKey } from '@tanstack/react-query';
import type { StoreApi, UseBoundStore } from 'zustand';

import { createStore, getStore, destroyStore, getStoreIds, type State } from '@/utils/createStore';

// ============================================================================
// Types
// ============================================================================

/** State structure for query stores */
export interface QueryStoreState<TData> extends State {
  data: TData | undefined;
  lastUpdated: number;
  setData: (data: TData) => void;
}

/** Options for the cross-window sync hook */
export interface UseCrossWindowSyncOptions<TData> {
  /** Query key used for cache identification */
  queryKey: QueryKey;
  /** Whether to enable cross-window synchronization */
  syncAcrossWindows: boolean;
  /** Current data to sync */
  data: TData | undefined;
}

// ============================================================================
// Internal Utilities
// ============================================================================

/**
 * Creates a unique store ID from a query key.
 */
function queryKeyToStoreId(queryKey: QueryKey): string {
  return `query-${hashKey(queryKey)}`;
}

/**
 * Gets or creates a Zustand store for a query key.
 * The store is configured for cross-window sync via @tauri-store/zustand.
 */
function getOrCreateQueryStore<TData>(
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

// ============================================================================
// Public API
// ============================================================================

/**
 * Hook that handles bidirectional synchronization between React Query cache
 * and the Tauri store for cross-window state sharing.
 *
 * This hook:
 * 1. Syncs local query data changes to the Zustand store (for other windows)
 * 2. Syncs store changes from other windows to the local query cache
 *
 * @example
 * ```tsx
 * // Inside a custom hook that uses React Query
 * const result = useQuery({ queryKey: ['data'], queryFn: fetchData });
 *
 * useCrossWindowSync({
 *   queryKey: ['data'],
 *   syncAcrossWindows: true,
 *   data: result.data,
 * });
 * ```
 */
export function useCrossWindowSync<TData>({
  queryKey,
  syncAcrossWindows,
  data,
}: UseCrossWindowSyncOptions<TData>): void {
  const queryClient = useQueryClient();

  // Ref to prevent circular updates between store and query
  const isSyncingFromQueryRef = useRef(false);
  const lastSyncedDataRef = useRef<TData | undefined>(undefined);

  // Serialize the query key so effects depend on its content rather than its
  // (often recreated) array reference. Callers frequently inline query keys,
  // which would otherwise re-run the sync effect on every render.
  const queryKeyHash = useMemo(() => hashKey(queryKey), [queryKey]);
  const queryKeyRef = useRef(queryKey);

  useEffect(() => {
    queryKeyRef.current = queryKey;
  }, [queryKey]);

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
      queryClient.setQueryData(queryKeyRef.current, storeData as any);
    }
  }, [storeData, syncAcrossWindows, queryClient, queryKeyHash]);
}

/**
 * Destroys a query store and removes it from the registry.
 * Use this for cleanup when a query is no longer needed.
 *
 * @param queryKey - The query key used to create the store
 */
export async function destroyQueryStore(queryKey: QueryKey): Promise<void> {
  const storeId = queryKeyToStoreId(queryKey);
  await destroyStore(storeId);
}

/**
 * Gets all registered query store IDs.
 * Useful for debugging or bulk cleanup operations.
 */
export function getQueryStoreIds(): string[] {
  return getStoreIds().filter((id) => id.startsWith('query-'));
}
