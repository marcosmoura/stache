import { useTauri, useTauriSuspense } from '@/hooks/useTauri';

import { fetchLocationData } from './location';
import type { LocationData } from './location';
import type { WeatherConfig } from './providers';
import { createWeatherProvider, isProviderAvailable } from './providers';

const REFETCH_INTERVAL = 20 * 60 * 1000; // 20 minutes

/**
 * Hook-based Weather Store using React Query for data fetching.
 */
export const useWeatherStore = () => {
  const { data: config } = useTauriSuspense<WeatherConfig>({
    queryKey: ['weatherConfig'],
    command: 'get_weather_config',
    staleTime: Infinity,
  });

  const { data: location } = useTauri<LocationData>({
    queryKey: ['weatherLocation', config?.defaultLocation],
    queryFn: () => fetchLocationData(config!.defaultLocation),
    refetchInterval: REFETCH_INTERVAL,
    refetchOnReconnect: true,
    enabled: !!config,
  });

  const { data: weather, isLoading } = useTauri({
    queryKey: ['weather', location, config],
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
};
