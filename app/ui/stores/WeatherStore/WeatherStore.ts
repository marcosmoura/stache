import { invoke } from '@tauri-apps/api/core';

import { useStoreQuery, useSuspenseStoreQuery } from '@/hooks';

import type {
  IpApiResponse,
  IpInfoResponse,
  WeatherConfig,
  WeatherData,
} from './WeatherStore.types';

/* API Constants */
const API_URL = 'https://weather.visualcrossing.com/VisualCrossingWebServices/rest/services';
const API_ELEMENTS =
  'name,address,resolvedAddress,temp,tempmax,tempmin,feelslike,humidity,windspeed,winddir,pressure,visibility,cloudcover,conditions,icon,moonphase,precip,precipprob,preciptype,snow,snowdepth,datetime';
const API_INCLUDE = 'current,hours,days';

const REFETCH_INTERVAL = 20 * 60 * 1000; // 20 minutes

/* Helper functions */
const getWeatherConfig = (): Promise<WeatherConfig> => {
  return invoke<WeatherConfig>('get_weather_config');
};

const buildLocationString = (parts: Array<string | undefined>): string =>
  parts.filter(Boolean).join(', ');

const getBrowserLocation = async (): Promise<string | undefined> => {
  if (!navigator.geolocation) {
    return undefined;
  }

  return new Promise((resolve) => {
    navigator.geolocation.getCurrentPosition(
      (position) => {
        const { latitude, longitude } = position.coords;
        resolve(`${latitude},${longitude}`);
      },
      () => resolve(undefined),
      { timeout: 5000 },
    );
  });
};

const fetchIpApiLocation = async (): Promise<string | undefined> => {
  try {
    const response = await fetch('https://ipapi.co/json/');

    if (!response.ok) {
      throw new Error('Failed to fetch from ipapi.co');
    }

    const data = (await response.json()) as IpApiResponse;
    const location = buildLocationString([data.city, data.country_name]);

    return location || undefined;
  } catch (error) {
    console.error(error);
    return undefined;
  }
};

const fetchIpInfoLocation = async (): Promise<string | undefined> => {
  try {
    const response = await fetch('https://ipinfo.io/json');

    if (!response.ok) {
      throw new Error('Failed to fetch from ipinfo.io');
    }

    const data = (await response.json()) as IpInfoResponse;
    const location = buildLocationString([data.city, data.country]);

    return location || undefined;
  } catch (error) {
    console.error(error);
    return undefined;
  }
};

const fetchLocationData = async (defaultLocation: string): Promise<string> => {
  const browserLocation = await getBrowserLocation();

  if (browserLocation) {
    return browserLocation;
  }

  const location = (await fetchIpApiLocation()) ?? (await fetchIpInfoLocation());

  return location || defaultLocation;
};

const fetchWeatherData = async (
  apiKey: string,
  location: string,
  defaultLocation: string,
): Promise<WeatherData> => {
  const encodedLoc = encodeURIComponent(location || defaultLocation);
  const params = new URLSearchParams({
    key: apiKey,
    unitGroup: 'metric',
    elements: API_ELEMENTS,
    include: API_INCLUDE,
    iconSet: 'icons2',
    contentType: 'json',
  });

  const url = `${API_URL}/timeline/${encodedLoc}/next5days?${params.toString()}`;
  const response = await fetch(url);

  if (!response.ok) {
    throw new Error('Network response was not ok');
  }

  return await response.json();
};

/**
 * @experimental Hook-based Weather Store using React Query for data fetching
 * and Zustand + @tauri-store/zustand for cross-window state synchronization.
 */
export const useWeatherStore = () => {
  // Fetch config (suspends until loaded)
  const { data: config } = useSuspenseStoreQuery<WeatherConfig>({
    queryKey: ['weatherConfig'],
    queryFn: getWeatherConfig,
    staleTime: Infinity, // Config doesn't change during runtime
  });

  // Fetch location (depends on config)
  const { data: location } = useStoreQuery<string>({
    queryKey: ['weatherLocation', config?.defaultLocation],
    queryFn: () => fetchLocationData(config!.defaultLocation),
    refetchInterval: REFETCH_INTERVAL,
    enabled: !!config,
  });

  // Fetch weather data (depends on config and location)
  const { data: weather, isLoading } = useStoreQuery<WeatherData>({
    queryKey: ['weather', location, config?.visualCrossingApiKey],
    queryFn: () =>
      fetchWeatherData(config!.visualCrossingApiKey, location!, config!.defaultLocation),
    refetchInterval: REFETCH_INTERVAL,
    enabled: !!config?.visualCrossingApiKey && !!location,
  });

  return {
    config,
    location,
    weather,
    isLoading,
  };
};
