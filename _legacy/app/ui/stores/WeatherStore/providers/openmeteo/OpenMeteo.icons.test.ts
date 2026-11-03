import { describe, expect, test } from 'vitest';

import { getPrecipType, getWeatherCondition, translateIcon } from './OpenMeteo.icons';

describe('translateIcon', () => {
  test('maps day codes with the default isDay value', () => {
    expect(translateIcon(0)).toBe('clearDay');
    expect(translateIcon(2)).toBe('partlyCloudyDay');
    expect(translateIcon(3)).toBe('cloudy');
    expect(translateIcon(45)).toBe('fog');
    expect(translateIcon(80)).toBe('rainDay');
    expect(translateIcon(85)).toBe('snowShowersDay');
  });

  test('maps night variants', () => {
    expect(translateIcon(0, false)).toBe('clearNight');
    expect(translateIcon(2, false)).toBe('partlyCloudyNight');
    expect(translateIcon(80, false)).toBe('rainNight');
    expect(translateIcon(85, false)).toBe('snowShowersNight');
    expect(translateIcon(95, false)).toBe('thunderShowersNight');
  });

  test('keeps non day-specific icons at night', () => {
    expect(translateIcon(3, false)).toBe('cloudy');
    expect(translateIcon(45, false)).toBe('fog');
  });

  test('falls back to clearDay for unknown codes', () => {
    expect(translateIcon(999)).toBe('clearDay');
  });
});

describe('getWeatherCondition', () => {
  test('maps known weather codes', () => {
    expect(getWeatherCondition(0)).toBe('Clear');
    expect(getWeatherCondition(3)).toBe('Overcast');
    expect(getWeatherCondition(61)).toBe('Slight Rain');
    expect(getWeatherCondition(71)).toBe('Slight Snow');
    expect(getWeatherCondition(95)).toBe('Thunderstorm');
  });

  test('returns Unknown for unknown codes', () => {
    expect(getWeatherCondition(999)).toBe('Unknown');
  });
});

describe('getPrecipType', () => {
  test('classifies thunderstorm, snow, and rain codes', () => {
    expect(getPrecipType(95)).toEqual(['thunderstorm']);
    expect(getPrecipType(71)).toEqual(['snow']);
    expect(getPrecipType(51)).toEqual(['rain']);
  });

  test('returns null for codes without precipitation', () => {
    expect(getPrecipType(0)).toBeNull();
    expect(getPrecipType(2)).toBeNull();
  });
});
