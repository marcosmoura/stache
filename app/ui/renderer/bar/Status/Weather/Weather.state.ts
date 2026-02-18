import { useMemo } from 'react';

import { useMediaQuery, useWidgetToggle } from '@/hooks';
import { getWeatherIcon, useWeatherStore } from '@/stores/WeatherStore';
import { LAPTOP_MEDIA_QUERY } from '@/utils/media-query';

const formatLabel = (temp: number, label: string) => {
  return `${Math.ceil(temp)}°C (${label})`;
};

export const useWeather = () => {
  const { ref, onClick } = useWidgetToggle('weather');
  const isLaptopScreen = useMediaQuery(LAPTOP_MEDIA_QUERY);
  const { weather, isLoading, isConfigured } = useWeatherStore();

  const currentConditions = weather?.currentConditions;
  const address = weather?.address;

  const icon = useMemo(() => {
    if (!currentConditions) {
      return getWeatherIcon('clearDay');
    }

    return getWeatherIcon(currentConditions.icon);
  }, [currentConditions]);

  const label = useMemo((): string => {
    if (isLoading || !currentConditions) {
      return 'Loading weather...';
    }

    const feelsLike = Math.ceil(currentConditions.feelslike || 0);
    const condition = currentConditions.conditions || '';

    if (isLaptopScreen) {
      const city = address?.split(',')[0]?.trim() || '';
      return formatLabel(feelsLike, city);
    }

    return formatLabel(feelsLike, condition);
  }, [address, currentConditions, isLaptopScreen, isLoading]);

  return { label, icon, ref, onClick, isConfigured };
};
