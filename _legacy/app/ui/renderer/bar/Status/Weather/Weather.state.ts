import { useWidgetToggle } from '@/hooks';
import {
  getWeatherIcon,
  useWeatherStore,
  type NormalizedCurrentConditions,
} from '@/stores/WeatherStore';

import type { WeatherState } from './Weather.types';

function getLabel(currentConditions: NormalizedCurrentConditions | undefined, isLoading: boolean) {
  if (isLoading || !currentConditions) {
    return 'Loading...';
  }

  return `${Math.ceil(currentConditions.feelslike || 0)}°C`;
}

export function useWeather(): WeatherState {
  const { ref, onClick } = useWidgetToggle('weather');
  const { weather, isLoading, isConfigured } = useWeatherStore();

  const currentConditions = weather?.currentConditions;

  return {
    label: getLabel(currentConditions, isLoading),
    icon: getWeatherIcon(currentConditions?.icon ?? 'clearDay'),
    ref,
    onClick,
    isConfigured,
  };
}
