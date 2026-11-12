import type { IconSvgElement } from '@hugeicons/react';

import { conditionIcon, DEFAULT_CONDITION } from './Weather.icons';
import type { CurrentConditions } from './Weather.providers';
import type { WeatherResult, WeatherState } from './Weather.query';

export type WeatherLook = { icon: IconSvgElement; label: string };

/** `N°C`, the feels-like temperature rounded up; a missing value shows `0°C`. */
export function temperatureLabel(conditions: CurrentConditions): string {
  return `${Math.ceil(conditions.feelslike ?? 0)}°C`;
}

/**
 * What the item shows: the condition and temperature once known, `Loading...`
 * while configured without conditions, nothing while unconfigured or unknown.
 */
export function weatherLook(
  result: WeatherResult | undefined,
  state: WeatherState | undefined,
): WeatherLook | null {
  if (result?.kind === 'ready') {
    return {
      icon: conditionIcon(result.conditions.condition),
      label: temperatureLabel(result.conditions),
    };
  }

  if (result?.kind === 'unconfigured' || !state?.configured) {
    return null;
  }

  return { icon: conditionIcon(DEFAULT_CONDITION), label: 'Loading...' };
}
