import { useEffect } from 'react';

import { useQuery, useQueryClient, type UseQueryResult } from '@tanstack/react-query';

import { connectBarSnapshot } from './barFeed';
import {
  APP_INFO_QUERY_KEY,
  BAR_SNAPSHOT_QUERY_KEY,
  readBarSnapshot,
  type BarSnapshotOutcome,
  HOST_STATUS_QUERY_KEY,
  readAppInfo,
  readHostStatus,
  type AppInfo,
  type HostStatus,
} from './client';
import { connectHostStatus } from './statusFeed';

export function useAppInfo(): UseQueryResult<AppInfo> {
  return useQuery({
    queryKey: APP_INFO_QUERY_KEY,
    queryFn: readAppInfo,
    staleTime: Infinity,
    retry: false,
  });
}

/**
 * Reads host status from the query cache and keeps it updated from the status
 * stream. The cache is written only by the feed, so there is no second copy of
 * host data.
 */
export function useHostStatus(): UseQueryResult<HostStatus> {
  const queryClient = useQueryClient();
  const status = useQuery({
    queryKey: HOST_STATUS_QUERY_KEY,
    queryFn: readHostStatus,
    enabled: false,
    staleTime: Infinity,
    retry: false,
  });

  useEffect(() => {
    const feed = connectHostStatus({ client: queryClient });

    return () => feed.dispose();
  }, [queryClient]);

  return status;
}

/**
 * Reads the Bar view from the query cache and keeps it updated from
 * `bar-changed`. The cache is written only by the feed, which subscribes before
 * it asks for the first snapshot and applies only a greater `sequence`.
 */
export function useBarSnapshot(): UseQueryResult<BarSnapshotOutcome> {
  const queryClient = useQueryClient();
  const snapshot = useQuery({
    queryKey: BAR_SNAPSHOT_QUERY_KEY,
    queryFn: readBarSnapshot,
    enabled: false,
    staleTime: Infinity,
    retry: false,
  });

  useEffect(() => {
    const feed = connectBarSnapshot({ client: queryClient });

    return () => feed.dispose();
  }, [queryClient]);

  return snapshot;
}
