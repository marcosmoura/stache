/**
 * Unified Tauri data fetching hooks.
 *
 * This module provides a single, consolidated API for fetching data in Tauri apps:
 * - `useTauri` - Standard query with optional event listening and cross-window sync
 * - `useTauriSuspense` - Suspense-enabled variant
 *
 * Both hooks support:
 * - Initial data fetching via Tauri commands
 * - Real-time updates via Tauri events
 * - Cross-window state synchronization via Zustand stores
 * - Automatic cleanup of event listeners
 */
import { useEffect, useRef, useCallback } from 'react';

import {
  useQuery,
  useSuspenseQuery,
  useQueryClient,
  type UseQueryOptions,
  type UseQueryResult,
  type UseSuspenseQueryOptions,
  type UseSuspenseQueryResult,
  type QueryKey,
} from '@tanstack/react-query';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import { useCrossWindowSync } from './useCrossWindowSync';

// ============================================================================
// Types
// ============================================================================

/** Base options shared by both query variants */
interface BaseTauriQueryOptions<TData, TQueryKey extends QueryKey> {
  /**
   * Unique key for the query cache.
   * Should be a stable array that identifies this query.
   */
  queryKey: TQueryKey;

  /**
   * Tauri command name to invoke for initial data fetch.
   * If not provided, `queryFn` must be supplied.
   */
  command?: string;

  /**
   * Arguments to pass to the Tauri command.
   */
  commandArgs?: Record<string, unknown>;

  /**
   * Optional Tauri event name to listen for real-time updates.
   * When an event is received, the query cache is automatically updated.
   */
  eventName?: string;

  /**
   * Transform function for event payloads before updating cache.
   * If not provided, the event payload is used as-is.
   */
  eventTransform?: (payload: unknown) => TData;

  /**
   * Whether to sync query results across windows.
   * When enabled, data is synchronized via Zustand stores using @tauri-store/zustand.
   * @default true
   */
  syncAcrossWindows?: boolean;
}

/** Options for useTauri hook */
export interface UseTauriOptions<
  TData = unknown,
  TError = Error,
  TQueryKey extends QueryKey = QueryKey,
>
  extends
    BaseTauriQueryOptions<TData, TQueryKey>,
    Omit<UseQueryOptions<TData, TError, TData, TQueryKey>, 'queryKey' | 'queryFn'> {
  /**
   * Custom query function. If provided, takes precedence over `command`.
   */
  queryFn?: () => Promise<TData>;
}

/** Options for useTauriSuspense hook */
export interface UseTauriSuspenseOptions<
  TData = unknown,
  TError = Error,
  TQueryKey extends QueryKey = QueryKey,
>
  extends
    BaseTauriQueryOptions<TData, TQueryKey>,
    Omit<UseSuspenseQueryOptions<TData, TError, TData, TQueryKey>, 'queryKey' | 'queryFn'> {
  /**
   * Custom query function. If provided, takes precedence over `command`.
   */
  queryFn?: () => Promise<TData>;
}

// ============================================================================
// Internal Utilities
// ============================================================================

/**
 * Creates a query function from command options.
 * Returns a function that throws if neither command nor queryFn is provided.
 * The error is thrown inside the returned function so React Query can catch it.
 */
function createCommandQueryFn<TData>(
  command: string | undefined,
  commandArgs: Record<string, unknown> | undefined,
  customQueryFn: (() => Promise<TData>) | undefined,
): () => Promise<TData> {
  if (customQueryFn) {
    return customQueryFn;
  }

  if (!command) {
    // Return a function that throws, so React Query can catch the error
    return () =>
      Promise.reject(new Error('useTauri: Either `command` or `queryFn` must be provided'));
  }

  return () => invoke<TData>(command, commandArgs);
}

/**
 * Hook to set up Tauri event listener that updates query cache.
 * Extracted to share between suspense and non-suspense variants.
 */
function useTauriEventSync<TData, TQueryKey extends QueryKey>(
  queryKey: TQueryKey,
  eventName: string | undefined,
  eventTransform: ((payload: unknown) => TData) | undefined,
): void {
  const queryClient = useQueryClient();

  // Use refs to avoid re-subscribing on every render
  const queryKeyRef = useRef(queryKey);
  const transformRef = useRef(eventTransform);

  useEffect(() => {
    queryKeyRef.current = queryKey;
    transformRef.current = eventTransform;
  });

  useEffect(() => {
    if (!eventName) return;

    let unlisten: UnlistenFn | undefined;
    let mounted = true;

    const setupListener = async () => {
      try {
        unlisten = await listen<unknown>(eventName, ({ payload }) => {
          const data = transformRef.current ? transformRef.current(payload) : (payload as TData);
          queryClient.setQueryData<TData>(queryKeyRef.current, data);
        });

        // If component unmounted while setting up, clean up immediately
        if (!mounted && unlisten) {
          unlisten();
        }
      } catch (error) {
        console.error(`[useTauri] Failed to set up event listener for "${eventName}":`, error);
      }
    };

    setupListener();

    return () => {
      mounted = false;
      unlisten?.();
    };
  }, [eventName, queryClient]);
}

// ============================================================================
// Public Hooks
// ============================================================================

/**
 * Unified Tauri query hook for data fetching with optional event updates and cross-window sync.
 *
 * @example
 * ```tsx
 * // Basic command invocation
 * const { data, isLoading } = useTauri<BatteryInfo>({
 *   queryKey: ['battery'],
 *   command: 'get_battery_info',
 * });
 *
 * // With real-time event updates
 * const { data } = useTauri<MediaInfo>({
 *   queryKey: ['media'],
 *   command: 'get_current_media_info',
 *   eventName: 'stache://media/playback-changed',
 * });
 *
 * // With custom query function
 * const { data } = useTauri<WeatherData>({
 *   queryKey: ['weather', location],
 *   queryFn: () => fetchWeatherFromAPI(location),
 *   enabled: !!location,
 * });
 *
 * // With event transformation
 * const { data } = useTauri<ProcessedData>({
 *   queryKey: ['processed'],
 *   command: 'get_raw_data',
 *   eventName: 'stache://data/updated',
 *   eventTransform: (raw) => processData(raw as RawData),
 * });
 *
 * // Disable cross-window sync for local-only data
 * const { data } = useTauri<LocalData>({
 *   queryKey: ['local'],
 *   command: 'get_local_data',
 *   syncAcrossWindows: false,
 * });
 * ```
 */
export function useTauri<TData = unknown, TError = Error, TQueryKey extends QueryKey = QueryKey>(
  options: UseTauriOptions<TData, TError, TQueryKey>,
): UseQueryResult<TData, TError> {
  const {
    queryKey,
    command,
    commandArgs,
    eventName,
    eventTransform,
    syncAcrossWindows = true,
    queryFn: customQueryFn,
    ...queryOptions
  } = options;

  const queryFn = useCallback(
    () => createCommandQueryFn<TData>(command, commandArgs, customQueryFn)(),
    [command, commandArgs, customQueryFn],
  );

  // Set up event listener for real-time updates
  useTauriEventSync<TData, TQueryKey>(queryKey, eventName, eventTransform);

  // Execute the query
  const result = useQuery<TData, TError, TData, TQueryKey>({
    queryKey,
    queryFn,
    ...queryOptions,
  });

  // Handle cross-window synchronization
  useCrossWindowSync({
    queryKey,
    syncAcrossWindows,
    data: result.data,
  });

  return result;
}

/**
 * Suspense-enabled Tauri query hook for data fetching with optional event updates and cross-window sync.
 *
 * Suspends the component until initial data is loaded.
 * Use inside a `<Suspense>` boundary.
 *
 * @example
 * ```tsx
 * // In parent component
 * <Suspense fallback={<Loading />}>
 *   <BatteryStatus />
 * </Suspense>
 *
 * // In BatteryStatus component
 * const { data } = useTauriSuspense<BatteryInfo>({
 *   queryKey: ['battery'],
 *   command: 'get_battery_info',
 *   eventName: 'stache://battery/state-changed',
 * });
 * // `data` is guaranteed to be defined here
 *
 * // Disable cross-window sync
 * const { data } = useTauriSuspense<LocalData>({
 *   queryKey: ['local'],
 *   command: 'get_local_data',
 *   syncAcrossWindows: false,
 * });
 * ```
 */
export function useTauriSuspense<
  TData = unknown,
  TError = Error,
  TQueryKey extends QueryKey = QueryKey,
>(
  options: UseTauriSuspenseOptions<TData, TError, TQueryKey>,
): UseSuspenseQueryResult<TData, TError> {
  const {
    queryKey,
    command,
    commandArgs,
    eventName,
    eventTransform,
    syncAcrossWindows = true,
    queryFn: customQueryFn,
    ...queryOptions
  } = options;

  const queryFn = useCallback(
    () => createCommandQueryFn<TData>(command, commandArgs, customQueryFn)(),
    [command, commandArgs, customQueryFn],
  );

  // Set up event listener for real-time updates
  useTauriEventSync<TData, TQueryKey>(queryKey, eventName, eventTransform);

  // Execute the query with suspense
  const result = useSuspenseQuery<TData, TError, TData, TQueryKey>({
    queryKey,
    queryFn,
    ...queryOptions,
  });

  // Handle cross-window synchronization
  useCrossWindowSync({
    queryKey,
    syncAcrossWindows,
    data: result.data,
  });

  return result;
}
