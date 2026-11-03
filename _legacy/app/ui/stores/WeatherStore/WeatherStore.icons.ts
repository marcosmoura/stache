import {
  CloudAngledRainIcon,
  CloudAngledRainZapIcon,
  CloudIcon,
  CloudSlowWindIcon,
  FastWindIcon,
  MoonAngledRainZapIcon,
  MoonCloudAngledRainIcon,
  MoonCloudIcon,
  MoonCloudSnowIcon,
  MoonIcon,
  SnowIcon,
  SunCloud02Icon,
  SunCloudAngledRainIcon,
  SunCloudSnowIcon,
  SunIcon,
} from '@hugeicons/core-free-icons';

import type { AnyIcon } from '@/components/Icon';

export const weatherIconMap: Record<string, AnyIcon> = {
  snow: SnowIcon,
  snowShowersDay: SunCloudSnowIcon,
  snowShowersNight: MoonCloudSnowIcon,
  thunder: CloudAngledRainZapIcon,
  thunderShowersDay: CloudAngledRainZapIcon,
  thunderShowersNight: MoonAngledRainZapIcon,
  rain: CloudAngledRainIcon,
  rainDay: SunCloudAngledRainIcon,
  rainNight: MoonCloudAngledRainIcon,
  fog: CloudSlowWindIcon,
  windy: FastWindIcon,
  cloudy: CloudIcon,
  partlyCloudyDay: SunCloud02Icon,
  partlyCloudyNight: MoonCloudIcon,
  clearDay: SunIcon,
  clearNight: MoonIcon,
};

export const getWeatherIcon = (iconKey: string): AnyIcon => {
  return weatherIconMap[iconKey] ?? weatherIconMap.clearDay;
};
