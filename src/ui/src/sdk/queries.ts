import { useEffect } from 'react';

import { useQuery, useQueryClient, type UseQueryResult } from '@tanstack/react-query';

import {
  APP_INFO_QUERY_KEY,
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
