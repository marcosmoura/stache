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

/** The normalized condition both providers map onto. */
export type WeatherCondition =
  | 'snow'
  | 'snowShowersDay'
  | 'snowShowersNight'
  | 'thunder'
  | 'thunderShowersDay'
  | 'thunderShowersNight'
  | 'rain'
  | 'rainDay'
  | 'rainNight'
  | 'fog'
  | 'windy'
  | 'cloudy'
  | 'partlyCloudyDay'
  | 'partlyCloudyNight'
  | 'clearDay'
  | 'clearNight';

/** The condition used for an unknown provider value. */
export const DEFAULT_CONDITION: WeatherCondition = 'clearDay';

/** Condition to its Hugeicons free icon. */
export const conditionIcons: Record<WeatherCondition, IconSvgElement> = {
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

/** Visual Crossing `icons2` names. */
export const visualCrossingConditions: Readonly<Record<string, WeatherCondition>> = {
  snow: 'snow',
  'snow-showers-day': 'snowShowersDay',
  'snow-showers-night': 'snowShowersNight',
  'thunder-rain': 'thunder',
  'thunder-showers-day': 'thunderShowersDay',
  'thunder-showers-night': 'thunderShowersNight',
  rain: 'rain',
  'showers-day': 'rainDay',
  'showers-night': 'rainNight',
  fog: 'fog',
  wind: 'windy',
  cloudy: 'cloudy',
  'partly-cloudy-day': 'partlyCloudyDay',
  'partly-cloudy-night': 'partlyCloudyNight',
  'clear-day': 'clearDay',
  'clear-night': 'clearNight',
};

/** Open Meteo WMO weather codes (day variants). */
export const wmoConditions: Readonly<Record<number, WeatherCondition>> = {
  0: 'clearDay',
  1: 'clearDay',
  2: 'partlyCloudyDay',
  3: 'cloudy',
  45: 'fog',
  48: 'fog',
  51: 'rain',
  53: 'rain',
  55: 'rain',
  56: 'rain',
  57: 'rain',
  61: 'rain',
  63: 'rain',
  65: 'rain',
  66: 'rain',
  67: 'rain',
  71: 'snow',
  73: 'snow',
  75: 'snow',
  77: 'snow',
  80: 'rainDay',
  81: 'rainDay',
  82: 'rainDay',
  85: 'snowShowersDay',
  86: 'snowShowersDay',
  95: 'thunder',
  96: 'thunder',
  99: 'thunder',
};

/** The night variant of a day condition; other conditions are unchanged. */
const nightConditions: Partial<Record<WeatherCondition, WeatherCondition>> = {
  clearDay: 'clearNight',
  partlyCloudyDay: 'partlyCloudyNight',
  rainDay: 'rainNight',
  snowShowersDay: 'snowShowersNight',
  thunder: 'thunderShowersNight',
};

/** A Visual Crossing `icons2` name to a condition; unknown names read as clear day. */
export function visualCrossingCondition(name: string): WeatherCondition {
  return Object.hasOwn(visualCrossingConditions, name)
    ? visualCrossingConditions[name]!
    : DEFAULT_CONDITION;
}

/** A WMO code to a condition (night variant when `isDay` is false); unknown codes read as clear. */
export function wmoCondition(code: number, isDay: boolean): WeatherCondition {
  const day = Object.hasOwn(wmoConditions, code) ? wmoConditions[code]! : DEFAULT_CONDITION;

  return isDay ? day : (nightConditions[day] ?? day);
}

/** The icon of a condition; an unknown value uses the clear-day icon. */
export function conditionIcon(condition: string): IconSvgElement {
  return Object.hasOwn(conditionIcons, condition)
    ? conditionIcons[condition as WeatherCondition]
    : conditionIcons[DEFAULT_CONDITION];
}
