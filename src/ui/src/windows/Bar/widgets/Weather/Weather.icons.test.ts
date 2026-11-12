import * as icons from '@hugeicons/core-free-icons';
import { SunIcon } from '@hugeicons/core-free-icons';
import { describe, expect, it } from 'vitest';

import {
  conditionIcon,
  conditionIcons,
  DEFAULT_CONDITION,
  visualCrossingCondition,
  visualCrossingConditions,
  wmoCondition,
  wmoConditions,
  type WeatherCondition,
} from './Weather.icons';

const published = new Set<unknown>(Object.values(icons));

describe('weather icon tables', () => {
  it('maps every condition to an existing Hugeicons free icon', () => {
    const conditions = Object.keys(conditionIcons) as WeatherCondition[];

    expect(conditions).toHaveLength(16);

    for (const condition of conditions) {
      expect(published.has(conditionIcons[condition]), condition).toBe(true);
      expect(conditionIcon(condition)).toBe(conditionIcons[condition]);
    }
  });

  it('maps every Visual Crossing icons2 name to a condition with an icon', () => {
    const names = Object.keys(visualCrossingConditions);

    expect(names).toHaveLength(16);

    for (const name of names) {
      const condition = visualCrossingCondition(name);

      expect(condition).toBe(visualCrossingConditions[name]);
      expect(published.has(conditionIcon(condition)), name).toBe(true);
    }
  });

  it('maps every WMO code by day and night to a condition with an icon', () => {
    const codes = Object.keys(wmoConditions).map(Number);

    expect(codes).toHaveLength(28);

    for (const code of codes) {
      expect(wmoCondition(code, true)).toBe(wmoConditions[code]);
      expect(published.has(conditionIcon(wmoCondition(code, false))), String(code)).toBe(true);
    }
  });

  it('uses night variants when it is not day', () => {
    expect(wmoCondition(0, false)).toBe('clearNight');
    expect(wmoCondition(2, false)).toBe('partlyCloudyNight');
    expect(wmoCondition(80, false)).toBe('rainNight');
    expect(wmoCondition(85, false)).toBe('snowShowersNight');
    expect(wmoCondition(95, false)).toBe('thunderShowersNight');
    expect(wmoCondition(3, false)).toBe('cloudy');
    expect(wmoCondition(61, false)).toBe('rain');
  });

  it('uses the default for unknown values', () => {
    expect(DEFAULT_CONDITION).toBe('clearDay');
    expect(visualCrossingCondition('hail')).toBe('clearDay');
    expect(visualCrossingCondition('toString')).toBe('clearDay');
    expect(wmoCondition(42, true)).toBe('clearDay');
    expect(wmoCondition(42, false)).toBe('clearNight');
    expect(conditionIcon('unknown')).toBe(SunIcon);
    expect(conditionIcon('constructor')).toBe(SunIcon);
  });
});
