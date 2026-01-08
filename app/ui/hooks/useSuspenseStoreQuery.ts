/**
 * @experimental Suspense-enabled query hook with cross-window state synchronization
 *
 * This hook combines React Query's data fetching with Tauri store synchronization,
 * allowing query results to be shared across multiple windows.
 *
 * @example
 * ```tsx
 * const fetchBattery = () => invoke<BatteryInfo>('get_battery_info');
 *
 * const { data: battery, isLoading } = useSuspenseStoreQuery({
 *   queryKey: ['battery-info'],
 *   queryFn: fetchBattery,
 * });
 * ```
 */
import {
  useSuspenseQuery,
  type UseSuspenseQueryOptions,
  type UseSuspenseQueryResult,
  type QueryKey,
} from '@tanstack/react-query';

import { useStoreSync, destroyQueryStore, getQueryStoreIds } from './useStoreQueryBase';

/** Options for useSuspenseStoreQuery */
export interface UseSuspenseStoreQueryOptions<
  TQueryFnData,
  TError = Error,
  TData = TQueryFnData,
  TQueryKey extends QueryKey = QueryKey,
> extends Omit<UseSuspenseQueryOptions<TQueryFnData, TError, TData, TQueryKey>, 'queryKey'> {
  /** Query key used for both React Query and store identification */
  queryKey: TQueryKey;
  /** Whether to sync query results across windows (default: true) */
  syncAcrossWindows?: boolean;
}

/**
 * A suspense-enabled query hook that syncs results across windows.
 *
 * This combines React Query's `useSuspenseQuery` with Tauri store synchronization,
 * ensuring query data is shared across all windows of the application.
 *
 * @param options - Query options including queryKey, queryFn, and sync settings
 * @returns Standard React Query result object
 *
 * @example
 * ```tsx
 * // Basic usage
 * const { data } = useSuspenseStoreQuery({
 *   queryKey: ['user', userId],
 *   queryFn: () => fetchUser(userId),
 * });
 *
 * // With transform
 * const { data: userName } = useSuspenseStoreQuery({
 *   queryKey: ['user', userId],
 *   queryFn: () => fetchUser(userId),
 *   select: (user) => user.name,
 * });
 *
 * // Disable cross-window sync
 * const { data } = useSuspenseStoreQuery({
 *   queryKey: ['local-data'],
 *   queryFn: fetchLocalData,
 *   syncAcrossWindows: false,
 * });
 * ```
 */
export function useSuspenseStoreQuery<
  TQueryFnData,
  TError = Error,
  TData = TQueryFnData,
  TQueryKey extends QueryKey = QueryKey,
>(
  options: UseSuspenseStoreQueryOptions<TQueryFnData, TError, TData, TQueryKey>,
): UseSuspenseQueryResult<TData, TError> {
  const { queryKey, syncAcrossWindows = true, ...queryOptions } = options;

  // Execute the query
  const result = useSuspenseQuery<TQueryFnData, TError, TData, TQueryKey>({
    queryKey,
    ...queryOptions,
  } as UseSuspenseQueryOptions<TQueryFnData, TError, TData, TQueryKey>);

  // Handle cross-window synchronization
  useStoreSync({
    queryKey,
    syncAcrossWindows,
    data: result.data,
  });

  return result;
}

// Re-export utility functions from base
export { destroyQueryStore, getQueryStoreIds };
