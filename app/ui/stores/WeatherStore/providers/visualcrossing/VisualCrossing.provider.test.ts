import { describe, expect, test, vi } from 'vitest';

import { VisualCrossingProvider } from './VisualCrossing.provider';

vi.mock('fetch', () => ({
  default: vi.fn(),
}));

const createMockVisualCrossingResponse = () => ({
  queryCost: 1,
  latitude: 52.52,
  longitude: 13.405,
  resolvedAddress: 'Berlin, Germany',
  address: 'Berlin',
  timezone: 'Europe/Berlin',
  tzoffset: 2,
  currentConditions: {
    datetime: '14:00:00',
    temp: 22.5,
    feelslike: 21.0,
    humidity: 65,
    dew: 15.5,
    windspeed: 12.0,
    winddir: 180,
    windgust: 18.0,
    precip: 0,
    precipprob: 10,
    preciptype: null,
    snow: 0,
    pressure: 1013,
    visibility: 10,
    cloudcover: 25,
    conditions: 'Partly Cloudy',
    icon: 'partly-cloudy-day',
    moonphase: 0.5,
  },
  days: [
    {
      datetime: '2026-02-18',
      temp: 20,
      tempmax: 24,
      tempmin: 16,
      precip: 0,
      precipprob: 5,
      preciptype: null,
      snow: 0,
      snowdepth: 0,
      conditions: 'Clear',
      icon: 'clear-day',
      hours: [
        {
          datetime: '2026-02-18T12:00:00',
          temp: 20,
          precip: 0,
          precipprob: 5,
          preciptype: null,
          icon: 'clear-day',
          conditions: 'Clear',
        },
      ],
    },
  ],
});

describe('VisualCrossingProvider', () => {
  describe('constructor', () => {
    test('creates provider with api key', () => {
      const provider = new VisualCrossingProvider('test-api-key');
      expect(provider.name).toBe('Visual Crossing');
      expect(provider.type).toBe('visual-crossing');
      expect(provider.requiresApiKey).toBe(true);
    });
  });

  describe('translateIcon', () => {
    test('translates clear-day icon', () => {
      const provider = new VisualCrossingProvider('test-key');
      expect(provider.translateIcon('clear-day')).toBe('clearDay');
    });

    test('translates partly-cloudy-day icon', () => {
      const provider = new VisualCrossingProvider('test-key');
      expect(provider.translateIcon('partly-cloudy-day')).toBe('partlyCloudyDay');
    });

    test('translates rain icon', () => {
      const provider = new VisualCrossingProvider('test-key');
      expect(provider.translateIcon('rain')).toBe('rain');
    });

    test('translates snow icon', () => {
      const provider = new VisualCrossingProvider('test-key');
      expect(provider.translateIcon('snow')).toBe('snow');
    });

    test('translates thunder-rain icon', () => {
      const provider = new VisualCrossingProvider('test-key');
      expect(provider.translateIcon('thunder-rain')).toBe('thunder');
    });

    test('returns clearDay for unknown icon', () => {
      const provider = new VisualCrossingProvider('test-key');
      expect(provider.translateIcon('unknown-icon')).toBe('clearDay');
    });
  });

  describe('fetch', () => {
    test('throws error when API returns non-ok status', async () => {
      const provider = new VisualCrossingProvider('test-key');
      vi.spyOn(globalThis, 'fetch').mockResolvedValue({
        ok: false,
        status: 401,
      } as unknown as Response);

      await expect(provider.fetch({ displayName: 'Berlin' }, 'Berlin')).rejects.toThrow(
        'Visual Crossing API error: 401',
      );
    });

    test('fetches and normalizes weather data', async () => {
      const provider = new VisualCrossingProvider('test-key');
      vi.spyOn(globalThis, 'fetch').mockResolvedValue({
        ok: true,
        json: () => Promise.resolve(createMockVisualCrossingResponse()),
      } as unknown as Response);

      const result = await provider.fetch({ displayName: 'Berlin' }, 'Berlin');

      expect(result.address).toBe('Berlin');
      expect(result.resolvedAddress).toBe('Berlin, Germany');
      expect(result.currentConditions.temp).toBe(22.5);
      expect(result.currentConditions.icon).toBe('partlyCloudyDay');
      expect(result.days).toHaveLength(1);
      expect(result.days?.[0].hours).toHaveLength(1);
    });
  });
});
