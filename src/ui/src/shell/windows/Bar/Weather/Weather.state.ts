import { useBarData } from '../Bar.data';

import { getWeatherIcon } from './Weather.icons';
import type { WeatherReading, WeatherState } from './Weather.types';

function getLabel(currentConditions: WeatherReading['currentConditions'], isLoading: boolean) {
  if (isLoading || !currentConditions) {
    return 'Loading...';
  }

  return `${Math.ceil(currentConditions.feelslike || 0)}°C`;
}

export function useWeather(): WeatherState {
  const { weather } = useBarData();
  const { isLoading, isConfigured, currentConditions } = weather;

  return {
    label: getLabel(currentConditions, isLoading),
    icon: getWeatherIcon(currentConditions?.icon ?? 'clearDay'),
    isConfigured,
  };
}
