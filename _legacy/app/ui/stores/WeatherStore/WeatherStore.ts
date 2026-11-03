import { useTauri, useTauriSuspense } from '@/hooks';

import { fetchLocationData } from './location';
import type { LocationData } from './location';
import type { WeatherConfig } from './providers';
import { createWeatherProvider, isProviderAvailable } from './providers';

const REFETCH_INTERVAL = 20 * 60 * 1000; // 20 minutes

/**
 * Hook-based Weather Store using React Query for data fetching.
 */
export function useWeatherStore() {
  // The config contains the Visual Crossing API key, so it must not be synced
  // across windows (which persists it in the shared store) nor included in
  // derived query keys (which leaks it into cache/store IDs).
  const { data: config } = useTauriSuspense<WeatherConfig>({
    queryKey: ['weatherConfig'],
    command: 'get_weather_config',
    staleTime: Infinity,
    syncAcrossWindows: false,
  });

  const { data: location } = useTauri<LocationData>({
    queryKey: ['weatherLocation', config.defaultLocation],
    queryFn: () => fetchLocationData(config.defaultLocation),
    refetchInterval: REFETCH_INTERVAL,
    refetchOnReconnect: true,
    enabled: !!config,
  });

  // Intentionally exclude the full config from the key: it contains the
  // Visual Crossing API key, which must not leak into query/store IDs.
  // eslint-disable-next-line @tanstack/query/exhaustive-deps -- see above
  const { data: weather, isLoading } = useTauri({
    queryKey: ['weather', location, config?.provider, config?.defaultLocation],
    queryFn: async () => {
      if (!config || !location) {
        throw new Error('Config or location not available');
      }

      const provider = createWeatherProvider(config);
      return provider.fetch(location, config.defaultLocation);
    },
    refetchInterval: REFETCH_INTERVAL,
    refetchOnReconnect: true,
    enabled: !!config && !!location && isProviderAvailable(config),
  });

  const isConfigured = isProviderAvailable(config);

  return {
    config,
    location,
    weather,
    isLoading,
    isConfigured,
  };
}
