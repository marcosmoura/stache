import { useCallback, useMemo, useRef } from 'react';

import { useMediaQuery } from '@/hooks';
import type { WidgetConfig } from '@/renderer/widgets/Widgets.types';
import { getWeatherIcon, useWeatherStore } from '@/stores/WeatherStore';
import { WidgetsEvents } from '@/types';
import { emitTauriEvent } from '@/utils/emitTauriEvent';
import { LAPTOP_MEDIA_QUERY } from '@/utils/media-query';

export const useWeather = () => {
  const ref = useRef<HTMLButtonElement>(null);
  const isLaptopScreen = useMediaQuery(LAPTOP_MEDIA_QUERY);
  const { weather, isLoading } = useWeatherStore();

  const currentConditions = weather?.currentConditions;

  const icon = useMemo(() => {
    if (!currentConditions) {
      return getWeatherIcon('clear-day');
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
      return `${feelsLike}°C`;
    }

    return `${feelsLike}°C (${condition})`;
  }, [currentConditions, isLaptopScreen, isLoading]);

  const onClick = useCallback(() => {
    if (!ref.current) {
      return;
    }

    const { x, y, width, height } = ref.current.getBoundingClientRect();

    emitTauriEvent<WidgetConfig>({
      eventName: WidgetsEvents.TOGGLE,
      target: 'widgets',
      payload: {
        name: 'weather',
        rect: {
          x,
          y,
          width,
          height,
        },
      },
    });
  }, []);

  return { label, icon, ref, onClick };
};
