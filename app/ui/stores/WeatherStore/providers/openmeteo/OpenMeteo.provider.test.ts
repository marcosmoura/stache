import { describe, expect, test, vi } from 'vitest';

import { OpenMeteoProvider } from './OpenMeteo.provider';

vi.mock('fetch', () => ({
  default: vi.fn(),
}));

const createMockOpenMeteoResponse = () => ({
  latitude: 52.52,
  longitude: 13.405,
  timezone: 'Europe/Berlin',
  utc_offset_seconds: 3600,
  timezone_abbreviation: 'CET',
  elevation: 38,
  current: {
    time: '2026-02-18T14:00',
    temperature_2m: 22.5,
    relative_humidity_2m: 65,
    apparent_temperature: 21.0,
    is_day: 1,
    precipitation: 0,
    weather_code: 2,
    wind_speed_10m: 12.0,
    wind_direction_10m: 180,
    wind_gusts_10m: 18.0,
  },
  hourly: {
    time: ['2026-02-18T12:00', '2026-02-18T13:00', '2026-02-18T14:00'],
    temperature_2m: [20, 21, 22.5],
    relative_humidity_2m: [70, 68, 65],
    precipitation_probability: [5, 5, 10],
    precipitation: [0, 0, 0],
    weather_code: [0, 1, 2],
    cloud_cover: [10, 15, 25],
    wind_speed_10m: [10, 11, 12],
    wind_direction_10m: [170, 175, 180],
    wind_gusts_10m: [15, 16, 18],
    visibility: [10000, 10000, 10000],
    pressure_msl: [1013, 1013, 1013],
  },
  daily: {
    time: ['2026-02-18', '2026-02-19'],
    weather_code: [2, 61],
    temperature_2m_max: [24, 18],
    temperature_2m_min: [16, 12],
    precipitation_sum: [0, 5],
    precipitation_probability_max: [10, 60],
    wind_speed_10m_max: [15, 20],
    sunrise: ['2026-02-18T07:30', '2026-02-19T07:28'],
    sunset: ['2026-02-18T17:30', '2026-02-19T17:32'],
  },
});

const createMockGeocodingResponse = () => ({
  results: [
    {
      latitude: 52.52,
      longitude: 13.405,
      name: 'Berlin',
      country: 'Germany',
    },
  ],
});

describe('OpenMeteoProvider', () => {
  describe('constructor', () => {
    test('creates provider without api key', () => {
      const provider = new OpenMeteoProvider();
      expect(provider.name).toBe('Open Meteo');
      expect(provider.type).toBe('open-meteo');
      expect(provider.requiresApiKey).toBe(false);
    });
  });

  describe('translateIcon', () => {
    test('translates weather code 0 (clear) for day', () => {
      const provider = new OpenMeteoProvider();
      expect(provider.translateIcon(0, true)).toBe('clearDay');
    });

    test('translates weather code 0 (clear) for night', () => {
      const provider = new OpenMeteoProvider();
      expect(provider.translateIcon(0, false)).toBe('clearNight');
    });

    test('translates weather code 2 (partly cloudy)', () => {
      const provider = new OpenMeteoProvider();
      expect(provider.translateIcon(2, true)).toBe('partlyCloudyDay');
    });

    test('translates weather code 61 (rain)', () => {
      const provider = new OpenMeteoProvider();
      expect(provider.translateIcon(61, true)).toBe('rain');
    });

    test('translates weather code 71 (snow)', () => {
      const provider = new OpenMeteoProvider();
      expect(provider.translateIcon(71, true)).toBe('snow');
    });

    test('translates weather code 95 (thunderstorm)', () => {
      const provider = new OpenMeteoProvider();
      expect(provider.translateIcon(95, true)).toBe('thunder');
    });

    test('translates weather code 45 (fog)', () => {
      const provider = new OpenMeteoProvider();
      expect(provider.translateIcon(45, true)).toBe('fog');
    });

    test('returns clearDay for unknown weather code', () => {
      const provider = new OpenMeteoProvider();
      expect(provider.translateIcon(999, true)).toBe('clearDay');
    });
  });

  describe('fetch', () => {
    test('throws error when API returns non-ok status', async () => {
      const provider = new OpenMeteoProvider();
      vi.spyOn(globalThis, 'fetch').mockResolvedValue({
        ok: false,
        status: 400,
      } as unknown as Response);

      await expect(
        provider.fetch(
          { displayName: 'Berlin', coordinates: { lat: 52.52, lon: 13.405 } },
          'Berlin',
        ),
      ).rejects.toThrow('Open Meteo API error: 400');
    });

    test('fetches and normalizes weather data with coordinates', async () => {
      const provider = new OpenMeteoProvider();
      vi.spyOn(globalThis, 'fetch').mockResolvedValue({
        ok: true,
        json: () => Promise.resolve(createMockOpenMeteoResponse()),
      } as unknown as Response);

      const result = await provider.fetch(
        { displayName: 'Berlin', coordinates: { lat: 52.52, lon: 13.405 } },
        'Berlin',
      );

      expect(result.address).toBe('Berlin');
      expect(result.currentConditions.temp).toBe(22.5);
      expect(result.currentConditions.icon).toBe('partlyCloudyDay');
      expect(result.days).toHaveLength(2);
    });

    test('geocodes location when no coordinates provided', async () => {
      const provider = new OpenMeteoProvider();
      vi.spyOn(globalThis, 'fetch').mockImplementation((url) => {
        const urlStr = String(url);
        if (urlStr.includes('geocoding')) {
          return Promise.resolve({
            ok: true,
            json: () => Promise.resolve(createMockGeocodingResponse()),
          } as unknown as Response);
        }
        return Promise.resolve({
          ok: true,
          json: () => Promise.resolve(createMockOpenMeteoResponse()),
        } as unknown as Response);
      });

      const result = await provider.fetch({ displayName: 'Berlin' }, 'Berlin');

      expect(result.address).toBe('Berlin');
      expect(result.currentConditions.temp).toBe(22.5);
    });

    test('throws error when geocoding fails', async () => {
      const provider = new OpenMeteoProvider();
      vi.spyOn(globalThis, 'fetch').mockResolvedValue({
        ok: true,
        json: () => Promise.resolve({ results: [] }),
      } as unknown as Response);

      await expect(provider.fetch({ displayName: 'UnknownPlace123' }, 'Default')).rejects.toThrow(
        'Failed to geocode location',
      );
    });
  });
});
