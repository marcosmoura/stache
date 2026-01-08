/**
 * @experimental Query hook with cross-window state synchronization
 *
 * This hook combines React Query's data fetching with Tauri store synchronization,
 * allowing query results to be shared across multiple windows.
 *
 * Unlike useSuspenseStoreQuery, this hook does not suspend and returns
 * loading/error states like the standard useQuery hook.
 *
 * @example
 * ```tsx
 * const fetchWeather = () => fetch('/api/weather').then(r => r.json());
 *
 * const { data: weather, isLoading, error } = useStoreQuery({
 *   queryKey: ['weather'],
 *   queryFn: fetchWeather,
 * });
 * ```
 */
import {
  useQuery,
  type UseQueryOptions,
  type UseQueryResult,
  type QueryKey,
} from '@tanstack/react-query';

import { useStoreSync } from './useStoreQueryBase';

/** Options for useStoreQuery */
export interface UseStoreQueryOptions<
  TQueryFnData,
  TError = Error,
  TData = TQueryFnData,
  TQueryKey extends QueryKey = QueryKey,
> extends Omit<UseQueryOptions<TQueryFnData, TError, TData, TQueryKey>, 'queryKey'> {
  /** Query key used for both React Query and store identification */
  queryKey: TQueryKey;
  /** Whether to sync query results across windows (default: true) */
  syncAcrossWindows?: boolean;
}

/**
 * A query hook that syncs results across windows.
 *
 * This combines React Query's `useQuery` with Tauri store synchronization,
 * ensuring query data is shared across all windows of the application.
 *
 * @param options - Query options including queryKey, queryFn, and sync settings
 * @returns Standard React Query result object with loading and error states
 *
 * @example
 * ```tsx
 * // Basic usage
 * const { data, isLoading, error } = useStoreQuery({
 *   queryKey: ['user', userId],
 *   queryFn: () => fetchUser(userId),
 * });
 *
 * // With transform
 * const { data: userName } = useStoreQuery({
 *   queryKey: ['user', userId],
 *   queryFn: () => fetchUser(userId),
 *   select: (user) => user.name,
 * });
 *
 * // Conditional fetching
 * const { data } = useStoreQuery({
 *   queryKey: ['weather', location],
 *   queryFn: () => fetchWeather(location),
 *   enabled: !!location,
 * });
 *
 * // Disable cross-window sync
 * const { data } = useStoreQuery({
 *   queryKey: ['local-data'],
 *   queryFn: fetchLocalData,
 *   syncAcrossWindows: false,
 * });
 * ```
 */
export function useStoreQuery<
  TQueryFnData,
  TError = Error,
  TData = TQueryFnData,
  TQueryKey extends QueryKey = QueryKey,
>(
  options: UseStoreQueryOptions<TQueryFnData, TError, TData, TQueryKey>,
): UseQueryResult<TData, TError> {
  const { queryKey, syncAcrossWindows = true, ...queryOptions } = options;

  // Execute the query
  const result = useQuery<TQueryFnData, TError, TData, TQueryKey>({
    queryKey,
    ...queryOptions,
  } as UseQueryOptions<TQueryFnData, TError, TData, TQueryKey>);

  // Handle cross-window synchronization
  useStoreSync({
    queryKey,
    syncAcrossWindows,
    data: result.data,
  });

  return result;
}
