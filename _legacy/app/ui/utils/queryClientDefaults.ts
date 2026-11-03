export const queryClientDefaults = {
  queries: {
    // Data is updated via Tauri events, minimize unnecessary refetching
    staleTime: Infinity,
    gcTime: 5 * 60 * 1000, // 5 minutes
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
    refetchOnMount: false,
    retry: false,
    // Keep background refetch for interval-based queries
    refetchIntervalInBackground: true,
  },
};
