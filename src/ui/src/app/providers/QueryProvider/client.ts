import { QueryClient } from '@tanstack/react-query';

/**
 * The query client for a local IPC app: always online, no focus or reconnect
 * refetch, no retry, data fresh until a feed or an explicit fetch replaces it.
 * A query that reaches a remote service overrides these in its own options.
 */
export function createQueryClient() {
  return new QueryClient({
    defaultOptions: {
      queries: {
        networkMode: 'always',
        refetchOnWindowFocus: false,
        refetchOnReconnect: false,
        retry: false,
        staleTime: Infinity,
      },
      mutations: { networkMode: 'always', retry: false },
    },
  });
}
