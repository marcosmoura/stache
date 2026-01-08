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
import type { IconSvgElement } from '@hugeicons/react';

export const weatherIconMap: Record<string, IconSvgElement> = {
  snow: SnowIcon,
  'snow-showers-day': SunCloudSnowIcon,
  'snow-showers-night': MoonCloudSnowIcon,
  'thunder-rain': CloudAngledRainZapIcon,
  'thunder-showers-day': CloudAngledRainZapIcon,
  'thunder-showers-night': MoonAngledRainZapIcon,
  rain: CloudAngledRainIcon,
  'showers-day': SunCloudAngledRainIcon,
  'showers-night': MoonCloudAngledRainIcon,
  fog: CloudSlowWindIcon,
  wind: FastWindIcon,
  cloudy: CloudIcon,
  'partly-cloudy-day': SunCloud02Icon,
  'partly-cloudy-night': MoonCloudIcon,
  'clear-day': SunIcon,
  'clear-night': MoonIcon,
};

export const getWeatherIcon = (iconKey: string): IconSvgElement => {
  return weatherIconMap[iconKey] ?? weatherIconMap['clear-day'];
};
